//! 背景重新整理 working changes（`git status`）。跟 `auto_fetch` 那種
//! 「事件觸發、跑完用 `send_after` 自我重新武裝」不同，這裡是常駐 worker
//! thread + request channel——debounce 視窗要「上一次跑完後至少等 N」而
//! 不是「上一次排程後等 N」，用事件鏈重新武裝表達不出這個語意，見
//! `worker_loop` 內的等待計算。
//!
//! 由 `lib.rs` 的主迴圈持有、跟 `Repository`／`graph` 活得一樣久；`App`
//! 只借用 `&Reloader`，不擁有它——`Ret::Refresh` 重建 `App` 不該連背景
//! worker 也重開一次。

use std::{
    cell::Cell,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    event::{AppEvent, Sender},
    git::{self, WorkingChanges},
};

/// worker 兩次 `git status` 之間至少間隔多久——避免存檔工具連續觸發多次
/// fs 事件時，每個事件都各自跑一次 `git status`。跟 `classify_event` 的
/// 500ms debounce 是兩層不同的節流：debounce 決定一批 fs 事件何時送進
/// 來，這裡決定 worker 自己跑多快。
const MIN_INTERVAL: Duration = Duration::from_millis(200);

/// Full 重載兩次之間至少間隔多久——rebase 每一步都動 HEAD，沒有這條會
/// 一路凍結到 rebase 結束。跟 `MIN_INTERVAL` 是同一種節流手法，但套用在
/// `lib.rs` 的完整重載上，數字也大得多（Full 本身就貴很多）。
const FULL_COOLDOWN: Duration = Duration::from_secs(1);

/// 背景重載 working changes 的把手，**也**是 `lib.rs` 的 Full 重載冷卻狀態
/// 的唯一擁有者（`App`／`lib.rs` 都要讀寫，放這裡比另開一個結構、多傳一個
/// 參數簡單——兩者本來就是「背景重新整理」同一個主題的兩個層級）。
#[derive(Debug)]
pub struct Reloader {
    request_tx: mpsc::Sender<()>,
    latest: Arc<Mutex<Option<WorkingChanges>>>,
    want_stats: Arc<AtomicBool>,
    /// 下一次 Full 重載最早能開始的時間，`full_finished` 唯一寫入點。
    full_not_before: Cell<Instant>,
    /// 冷卻中收到的 Full 請求是否已經排過一個延遲的 `AutoRefresh(Full)`
    /// ——一次性旗標，`arm_owed_full` 設它、`full_finished` 清它，見兩者
    /// 文件。
    full_owed: Cell<bool>,
    /// 上一次（或這一次正在跑的）Full 重載的開始時間，`full_started` 唯一
    /// 寫入點。`App` 拿它跟 `AutoRefresh::at` 比對，濾掉「已經被這次 Full
    /// 涵蓋」的 watcher 事件——見 `last_full_start` 文件。
    last_full_start: Cell<Instant>,
}

impl Reloader {
    /// 啟動 worker thread。`repo_path` 全程不變——`Reloader` 活得跟主迴圈
    /// 一樣久，repo 路徑（CLI 傳入的 `args.path`）啟動後不會變。
    pub fn spawn(repo_path: PathBuf, tx: Sender) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<()>();
        let latest = Arc::new(Mutex::new(None));
        let want_stats = Arc::new(AtomicBool::new(false));

        {
            let latest = Arc::clone(&latest);
            let want_stats = Arc::clone(&want_stats);
            thread::spawn(move || worker_loop(repo_path, request_rx, latest, want_stats, tx));
        }

        Self {
            request_tx,
            latest,
            want_stats,
            // `full_ready()` 比較的是 `now >= full_not_before`，設成
            // `now` 本身就已經是「現在可以」，不必再往前推、也不用像下面
            // 那樣擔心減出負的 `Instant`。
            full_not_before: Cell::new(Instant::now()),
            full_owed: Cell::new(false),
            // 啟動時「上一次 Full」等於從來沒發生過，設成很久以前——
            // 這樣啟動瞬間收到的任何 watcher 事件都不會被誤判成「已經被
            // 涵蓋」而濾掉。`checked_sub`：系統剛開機、單調時鐘起點還不到
            // `FULL_COOLDOWN` 前時，直接減會 panic，跟 `event.rs` 的
            // debounce 起點算法同一個理由。
            last_full_start: Cell::new(
                Instant::now()
                    .checked_sub(FULL_COOLDOWN)
                    .unwrap_or_else(Instant::now),
            ),
        }
    }

    /// 請求重新跑一次 `git status`。多次呼叫會被 worker 合併成一次——
    /// `mpsc::Sender` 本身就是佇列，worker 醒來後一次排空，見
    /// `worker_loop`。
    pub fn request(&self) {
        let _ = self.request_tx.send(());
    }

    /// 目前最新一次背景 `git status` 的結果；worker 還沒跑完第一輪時是
    /// `None`。
    pub fn latest(&self) -> Option<WorkingChanges> {
        self.latest.lock().unwrap().clone()
    }

    /// 開啟 working changes detail 時呼叫，讓背景重載順便算檔案行數增減
    /// （`diff --numstat`）；關閉時呼叫 `false` 關掉——一般瀏覽只看檔案數
    /// （`file_count()`），不值得為了看不到的行數多跑兩個子行程。
    pub fn set_want_stats(&self, want: bool) {
        self.want_stats.store(want, Ordering::Release);
    }

    /// 啟動時呼叫一次，同步等第一個 `git status` 結果。這個等待幾乎是
    /// 免費的——`status` 通常比 `git log` 快很多，`Repository::load` 早就
    /// 跑完了——換來的是「開起來就選在虛擬列」這個既有行為不必碰運氣。
    /// 等不到（`timeout` 到期，或 worker 掛了）就回 `None`，呼叫端當
    /// 「暫時沒有 working changes」處理，不阻塞啟動；真正的結果隨後會用
    /// `AppEvent::WorkingChangesReady` 補上。
    pub fn wait_first(&self, timeout: Duration) -> Option<WorkingChanges> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(wc) = self.latest() {
                return Some(wc);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Full 重載現在能不能立刻觸發——不在冷卻視窗內。
    pub fn full_ready(&self) -> bool {
        Instant::now() >= self.full_not_before.get()
    }

    /// Full 重載即將開始時呼叫：記下起點供 `full_finished` 算耗時，也寫入
    /// `last_full_start`（`App` 拿它濾掉已經被這次重載涵蓋的 watcher
    /// 事件）。冷卻狀態（`full_not_before`）本身不在這裡改，要等重載真的
    /// 結束才推進。
    pub fn full_started(&self) {
        self.last_full_start.set(Instant::now());
    }

    /// 上一次（或這一次正在跑的）Full 重載的開始時間，見該欄位文件。
    pub fn last_full_start(&self) -> Instant {
        self.last_full_start.get()
    }

    /// Full 重載結束時呼叫，不論成功或失敗——失敗沒呼叫的話，`mv .git`
    /// 之類的錯誤會讓 watcher 一直回報，變成 Full → 失敗 → Full 的緊密
    /// 迴圈。耗時直接從 `last_full_start` 算（`full_started` 剛設過），不用
    /// 讓呼叫端自己帶著起點跑一圈再交回來。下一次至少要等
    /// `max(FULL_COOLDOWN, 這次耗時)`；`full_owed` 一併清掉——這次真的跑完
    /// 了，不欠了。
    pub fn full_finished(&self) {
        let cost = self.last_full_start.get().elapsed();
        self.full_not_before
            .set(Instant::now() + FULL_COOLDOWN.max(cost));
        self.full_owed.set(false);
    }

    /// 冷卻中收到一次 Full 請求時呼叫。`full_owed` 是一次性旗標：第一次
    /// 呼叫回 `true`（呼叫端該排一個 `send_after(AutoRefresh(Full), ..)`），
    /// 之後在同一個冷卻視窗內再呼叫都回 `false`（已經排過，不必排第二個）
    /// ——`full_finished` 才會把它清掉，不是誰讀了就清。
    pub fn arm_owed_full(&self) -> bool {
        !self.full_owed.replace(true)
    }

    /// 純讀取、不清除，給 `App::close_help` 等三個 overlay 關閉時檢查用：
    /// 冷卻期間吞掉過一次 Full 請求、冷卻計時器到期時 overlay 還開著
    /// （它們的 `refresh()` 是 no-op）就會卡在這裡，直到清掉這個旗標的
    /// `full_finished` 真的跑完一次 Full 重載——關閉時用同一個
    /// `request_full` 補一次，見 `App::retry_owed_full`。
    pub fn full_owed(&self) -> bool {
        self.full_owed.get()
    }

    /// 冷卻視窗還剩多久，`arm_owed_full` 回 `true` 之後呼叫端拿去
    /// `send_after`。
    pub fn full_remaining(&self) -> Duration {
        self.full_not_before
            .get()
            .saturating_duration_since(Instant::now())
    }
}

fn worker_loop(
    repo_path: PathBuf,
    request_rx: mpsc::Receiver<()>,
    latest: Arc<Mutex<Option<WorkingChanges>>>,
    want_stats: Arc<AtomicBool>,
    tx: Sender,
) {
    // 一開始就當作「上次跑完」是很久以前，第一個請求不用等。`checked_sub`：
    // 系統剛開機、單調時鐘起點還不到 `MIN_INTERVAL` 前時，直接減會 panic。
    let mut last_end = Instant::now()
        .checked_sub(MIN_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_cost = Duration::ZERO;

    loop {
        // 等下一個請求；request channel 斷線（`Reloader` 被 drop）就收工。
        if request_rx.recv().is_err() {
            return;
        }

        // 睡到「上次跑完後至少 max(MIN_INTERVAL, 上次耗時)」——用上次
        // *結束* 時間當基準，不是排程時間：呼應存檔工具連續觸發的情境，
        // 睡覺期間累積的請求會在下面一次排空，不會因為還在等待就重新起算。
        let wait = MIN_INTERVAL
            .max(last_cost)
            .saturating_sub(last_end.elapsed());
        if !wait.is_zero() {
            thread::sleep(wait);
        }

        // 排空這段時間（recv 之後、睡醒之後）累積的請求，合併成這一次。
        while request_rx.try_recv().is_ok() {}

        let start = Instant::now();
        match git::load_working_changes(&repo_path) {
            Ok(mut wc) => {
                if want_stats.load(Ordering::Acquire) {
                    git::fill_working_changes_stats(&repo_path, &mut wc);
                }
                *latest.lock().unwrap() = Some(wc);
                tx.send(AppEvent::WorkingChangesReady);
            }
            Err(_) => {
                // 保留舊值、不通知——錯誤交給 Full 重載回報
                // （`NotifyError`），這裡吞掉避免跟它搶狀態列。
            }
        }
        last_cost = start.elapsed();
        last_end = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Sender;

    /// worker thread 不影響這裡測的邏輯（不呼叫 `request()` 就不會跑
    /// `git status`），路徑不必是真的 repo。
    fn spawn_for_test() -> Reloader {
        let (tx, _rx) = Sender::channel_for_test();
        Reloader::spawn(PathBuf::from("/nonexistent"), tx)
    }

    #[test]
    fn full_ready_right_after_spawn() {
        // `full_not_before` 初始化成很久以前，第一次 Full 不用等。
        assert!(spawn_for_test().full_ready());
    }

    #[test]
    fn full_started_advances_last_full_start() {
        let reloader = spawn_for_test();
        let before = reloader.last_full_start();
        reloader.full_started();
        assert!(reloader.last_full_start() >= before);
    }

    #[test]
    fn full_finished_starts_cooldown_and_clears_owed() {
        let reloader = spawn_for_test();
        reloader.arm_owed_full();
        assert!(reloader.full_owed());

        reloader.full_started();
        reloader.full_finished();

        assert!(
            !reloader.full_ready(),
            "剛結束一次 Full，下一次至少要等 FULL_COOLDOWN"
        );
        assert!(!reloader.full_owed(), "跑完一次 Full 就不欠了");
    }

    /// 一次性旗標：第一次呼叫回 true（該排一個 `send_after`），冷卻視窗內
    /// 再呼叫都回 false（已經排過，不必排第二個）。
    #[test]
    fn arm_owed_full_is_one_shot_within_cooldown() {
        let reloader = spawn_for_test();

        assert!(reloader.arm_owed_full());
        assert!(!reloader.arm_owed_full());
        assert!(!reloader.arm_owed_full());
    }

    #[test]
    fn full_remaining_is_positive_right_after_full_finished() {
        let reloader = spawn_for_test();
        reloader.full_started();
        reloader.full_finished();

        let remaining = reloader.full_remaining();
        assert!(remaining > Duration::ZERO);
        assert!(remaining <= FULL_COOLDOWN);
    }
}
