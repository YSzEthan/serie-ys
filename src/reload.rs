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

/// 背景重載 working changes 的把手。
#[derive(Debug)]
pub struct Reloader {
    request_tx: mpsc::Sender<()>,
    latest: Arc<Mutex<Option<WorkingChanges>>>,
    want_stats: Arc<AtomicBool>,
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
}

fn worker_loop(
    repo_path: PathBuf,
    request_rx: mpsc::Receiver<()>,
    latest: Arc<Mutex<Option<WorkingChanges>>>,
    want_stats: Arc<AtomicBool>,
    tx: Sender,
) {
    // 一開始就當作「上次跑完」是很久以前，第一個請求不用等。
    let mut last_end = Instant::now() - MIN_INTERVAL;
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
