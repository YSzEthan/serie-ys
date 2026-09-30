//! 背景重新整理：working changes（只跑 `git status`）跟 Full（`git log` +
//! 重算 graph）各自一條常駐 worker thread + request channel——debounce
//! 視窗要「上一次跑完後至少等 N」而不是「上一次排程後等 N」，用事件鏈重新
//! 武裝表達不出這個語意，見兩個 `*_worker_loop` 內的等待計算。
//!
//! 由 `lib.rs` 的主迴圈持有、跟 `Repository`／`graph` 活得一樣久；`App`
//! 只借用 `&Reloader`，不擁有它——換資料重建 `App` 不該連背景 worker 也
//! 重開一次。

use std::{
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

use rust_i18n::t;

use crate::{
    event::{AppEvent, Sender},
    git::{self, Repository, SortCommit, WorkingChanges},
    graph::{self, Graph},
    RemoteOnly,
};

/// worker 兩次 `git status` 之間至少間隔多久——避免存檔工具連續觸發多次
/// fs 事件時，每個事件都各自跑一次 `git status`。跟 `classify_event` 的
/// 500ms debounce 是兩層不同的節流：debounce 決定一批 fs 事件何時送進
/// 來，這裡決定 worker 自己跑多快。
const MIN_INTERVAL: Duration = Duration::from_millis(200);

/// Full 重載兩次之間至少間隔多久——rebase 每一步都動 HEAD，沒有這條會
/// 一路凍結到 rebase 結束。跟 `MIN_INTERVAL` 是同一種節流手法，但套用在
/// Full 重載上，數字也大得多（Full 本身就貴很多）。
const FULL_COOLDOWN: Duration = Duration::from_secs(1);

/// Full 重載一次的完整結果，全部是 `Send` 的裸資料（沒有 `Rc`）——worker
/// thread 建好之後跨執行緒交給主執行緒，主執行緒才把 `graph`／`filtered`
/// 包成 `Rc`（`CommitListState` 要的型別）。
#[derive(Debug)]
pub struct Loaded {
    pub repository: Repository,
    pub graph: Graph,
    pub filtered: Option<Graph>,
    pub remote_only: RemoteOnly,
    /// 這次載入結果的內容指紋，`take_full()` 取走時寫回 `Reloader::applied`。
    fingerprint: u64,
}

/// Full 重載的狀態機。`Loading` 與 `Ready` 不可能同時成立——用 enum 讓這個
/// 不變式在型別層面直接不存在第二種可能，不用另外拿兩個欄位（`bool` +
/// `Option`）維持同步。
#[derive(Debug)]
enum FullState {
    Idle,
    Loading,
    Ready(Box<Loaded>),
}

/// `App::run()` 迴圈頂端讀的摘要——只關心 discriminant，不需要（也不該）
/// 把 `Loaded` 借出來看一眼就還回去，`take_full()` 才是唯一的消費入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullStatus {
    Idle,
    Loading,
    Ready,
}

/// `Reloader::spawn` 啟動 Full worker要用的載入設定，收成一包跟
/// `app.rs::GitTask` 同一個理由：好幾個同型別的參數相鄰，位置參數容易
/// 傳反。
pub struct FullLoadConfig {
    /// Full 重載呼叫 `Repository::load` 用的路徑——刻意跟啟動時
    /// `lib.rs::run()` 呼叫初始 `Repository::load` 的那個字串一致（CLI
    /// 傳入的 `args.path`，例如 `.`），不要傳 canonicalize 過的版本：
    /// `Repository::path()` 因此在每次重載後都維持同一個值，不會因為
    /// 換成絕對路徑而跟啟動時不一致。
    pub path: PathBuf,
    pub order: SortCommit,
    pub max_count: Option<usize>,
    pub trunc: graph::Truncation,
}

/// 背景重載的把手，working changes 與 Full 共用同一個結構——兩者本來就是
/// 「背景重新整理」同一個主題的兩個層級，各自的 worker thread、channel、
/// 節流常數分開放在下面兩個區塊裡。
#[derive(Debug)]
pub struct Reloader {
    // ---- working changes（只跑 `git status`）----
    request_tx: mpsc::Sender<()>,
    latest: Arc<Mutex<Option<WorkingChanges>>>,
    want_stats: Arc<AtomicBool>,

    // ---- Full（`git log` + 重算 graph）----
    full_tx: mpsc::Sender<Instant>,
    full_state: Arc<Mutex<FullState>>,
    /// App 手上那份資料的指紋——`take_full()` 唯一寫入點。Full worker 拿它
    /// 跟剛載入的新資料比對：相同就代表沒變，不用觸發換資料。
    applied: Arc<AtomicU64>,
}

impl Reloader {
    /// 啟動兩條 worker thread。`repo_path`（working changes 用）與
    /// `full.path`（Full 用）全程不變——`Reloader` 活得跟主迴圈一樣久。
    pub fn spawn(repo_path: PathBuf, full: FullLoadConfig, tx: Sender) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<()>();
        let latest = Arc::new(Mutex::new(None));
        let want_stats = Arc::new(AtomicBool::new(false));

        {
            let latest = Arc::clone(&latest);
            let want_stats = Arc::clone(&want_stats);
            let tx = tx.clone();
            // 具名 thread：見 `QUIET_PANIC_THREAD_PREFIX`。
            thread::Builder::new()
                .name(crate::QUIET_PANIC_THREAD_PREFIX.to_string())
                .spawn(move || {
                    working_tree_worker_loop(repo_path, request_rx, latest, want_stats, tx)
                })
                .expect("spawn working tree reload worker thread");
        }

        let (full_tx, full_rx) = mpsc::channel::<Instant>();
        let full_state = Arc::new(Mutex::new(FullState::Idle));
        // 先用哨兵值起步——這裡（啟動時）不知道初始 `Repository` 的指紋，
        // 算出來要等它那筆昂貴的 `git log` 跑完。安全：這個階段不可能有
        // 任何 `request_full()` 呼叫搶在 `set_initial_fingerprint` 之前
        // 執行——`App` 都還沒建出來，見該方法文件。
        let applied = Arc::new(AtomicU64::new(0));

        {
            let full_state = Arc::clone(&full_state);
            let applied = Arc::clone(&applied);
            // 具名 thread：見 `QUIET_PANIC_THREAD_PREFIX`。
            thread::Builder::new()
                .name(crate::QUIET_PANIC_THREAD_PREFIX.to_string())
                .spawn(move || full_worker_loop(full, full_rx, full_state, applied, tx))
                .expect("spawn full reload worker thread");
        }

        Self {
            request_tx,
            latest,
            want_stats,
            full_tx,
            full_state,
            applied,
        }
    }

    /// 啟動時，初始 `Repository::load` 完成之後呼叫一次，把它的指紋設成
    /// Full worker 比對的起點。呼叫時機必須在任何 `request_full` 可能被
    /// 觸發之前——`lib.rs::run()` 這個呼叫緊接在初始載入之後、`App` 建立
    /// 之前，這時不可能有 watcher 事件或使用者操作搶先呼叫 `request_full`。
    pub fn set_initial_fingerprint(&self, fingerprint: u64) {
        self.applied.store(fingerprint, Ordering::Release);
    }

    /// 請求重新跑一次 `git status`。多次呼叫會被 worker 合併成一次——
    /// `mpsc::Sender` 本身就是佇列，worker 醒來後一次排空，見
    /// `working_tree_worker_loop`。
    pub fn request(&self) {
        let _ = self.request_tx.send(());
    }

    /// 目前最新一次背景 `git status` 的結果；worker 還沒跑完第一輪時是
    /// `None`。
    pub fn latest(&self) -> Option<WorkingChanges> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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

    /// 請求一次 Full 重載。`at` 是這次變化實際發生的時間（watcher 用
    /// debounce 視窗起點，其餘呼叫端用送出當下），worker 拿它濾掉已經被
    /// 上一次成功載入涵蓋的請求，取代舊版 `App` 裡 `at >=
    /// last_full_start()` 的過濾。
    pub fn request_full(&self, at: Instant) {
        let _ = self.full_tx.send(at);
    }

    /// 這一輪迴圈要不要去看一眼 Full 結果——便宜，`App::run()` 每輪都問得起。
    pub fn full_status(&self) -> FullStatus {
        match *self
            .full_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            FullState::Idle => FullStatus::Idle,
            FullState::Loading => FullStatus::Loading,
            FullState::Ready(_) => FullStatus::Ready,
        }
    }

    /// 取走目前的 Full 結果並把狀態收回 `Idle`；沒有 `Ready` 就回 `None`,
    /// 不動狀態——`full_status()` 之後緊接著呼叫這個才有意義，兩者之間
    /// worker 可能已經把 `Ready` 換掉，這裡永遠以當下鎖到的狀態為準。
    ///
    /// **有副作用**：取走的同時把 `applied` 設成這份結果的指紋，讓 Full
    /// worker 下一輪能跟「App 現在手上這份」比對。呼叫端必須先做完所有
    /// 不帶副作用的檢查（`pending_message`、狀態列、view 能不能換）再呼叫
    /// 這個，拿到 `None` 就整輪放棄，不能事後回頭。
    pub fn take_full(&self) -> Option<Box<Loaded>> {
        let mut guard = self
            .full_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let loaded = match std::mem::replace(&mut *guard, FullState::Idle) {
            FullState::Ready(loaded) => loaded,
            other => {
                *guard = other;
                return None;
            }
        };
        drop(guard);
        self.applied.store(loaded.fingerprint, Ordering::Release);
        Some(loaded)
    }
}

fn working_tree_worker_loop(
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
        // 這條 thread 的 panic 被 `QUIET_PANIC_THREAD_PREFIX` 靜音，不接住
        // 的話 worker 會無聲死掉、working changes 從此不再更新；接住後比照
        // `full_worker_loop` 轉成 `NotifyError`。
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let mut wc = git::load_working_changes(&repo_path)?;
            if want_stats.load(Ordering::Acquire) {
                git::fill_working_changes_stats(&repo_path, &mut wc);
            }
            crate::Result::Ok(wc)
        }));
        match result {
            Ok(Ok(wc)) => {
                *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(wc);
                tx.send(AppEvent::WorkingChangesReady);
            }
            Ok(Err(_)) => {
                // 保留舊值、不通知——錯誤交給 Full 重載回報
                // （`NotifyError`），這裡吞掉避免跟它搶狀態列。
            }
            Err(payload) => {
                tx.send(AppEvent::NotifyError(
                    t!(
                        "app.reload.working_changes_panicked",
                        detail = panic_message(payload)
                    )
                    .into_owned(),
                ));
            }
        }
        last_cost = start.elapsed();
        last_end = Instant::now();
    }
}

/// 一次 Full 載入的結果：`Unchanged` 代表指紋跟 App 手上那份相同——連
/// `calc_graph` 都不用跑，比舊版 fast path 更早止血（舊版是 `git log` 跑完
/// 才比對，這裡指紋在 `Repository::load` 一結束就能算）。
enum Outcome {
    Unchanged,
    Changed(Box<Loaded>),
}

fn load_full(config: &FullLoadConfig, applied: &AtomicU64) -> crate::Result<Outcome> {
    let repository = Repository::load(&config.path, config.order, config.max_count)?;
    let fingerprint = repository.fingerprint();
    if fingerprint == applied.load(Ordering::Acquire) {
        return Ok(Outcome::Unchanged);
    }

    let head = crate::resolve_head_commit_hash(&repository);
    let graph = graph::calc_graph(
        &repository,
        head.as_ref(),
        crate::head_has_named_ref(&repository),
        config.trunc,
    );
    let remote_only = crate::find_remote_only_commits(&repository);
    let filtered = crate::compute_filtered_graph_from(&repository, &remote_only, config.trunc);

    Ok(Outcome::Changed(Box::new(Loaded {
        repository,
        graph,
        filtered,
        remote_only,
        fingerprint,
    })))
}

/// panic payload 轉成訊息——`catch_unwind` 接住的 panic 幾乎都是
/// `&'static str`（`panic!("literal")`）或 `String`（`panic!("{e}")`），
/// 其餘型別（極罕見的 `panic_any`）退回固定文案。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        t!("app.reload.unknown_panic").into_owned()
    }
}

fn full_worker_loop(
    config: FullLoadConfig,
    request_rx: mpsc::Receiver<Instant>,
    state: Arc<Mutex<FullState>>,
    applied: Arc<AtomicU64>,
    tx: Sender,
) {
    // 「上一次成功載入的開始時間」，`request_full` 帶的 `at` 早於這個時間
    // 就代表那批變化已經被那次載入涵蓋，不需要再跑一次。一開始當作很久
    // 以前，第一個請求不用等；`checked_sub` 理由同 working changes worker。
    let mut covered = Instant::now()
        .checked_sub(FULL_COOLDOWN)
        .unwrap_or_else(Instant::now);
    let mut last_end = covered;
    let mut last_cost = Duration::ZERO;

    loop {
        let Ok(first_at) = request_rx.recv() else {
            return;
        };
        let mut max_at = first_at;
        while let Ok(at) = request_rx.try_recv() {
            max_at = max_at.max(at);
        }
        if max_at < covered {
            continue;
        }

        // 立刻把狀態換成 Loading，丟掉舊的 Ready（若有）——冷卻期間 slot
        // 不會留著已知過期的結果，主執行緒只換一次、拿到的一定是最新的。
        // 舊值在放掉鎖之後才 drop，不在持鎖期間釋放（上 GB 的 `Repository`
        // 釋放很貴，鎖著它會讓每 100ms 讀一次 `full_status()` 的主執行緒
        // 卡在鎖上）。
        {
            let old = std::mem::replace(
                &mut *state.lock().unwrap_or_else(PoisonError::into_inner),
                FullState::Loading,
            );
            drop(old);
        }
        tx.send(AppEvent::FullReloadStatus);

        // 睡到「上次跑完後至少 max(FULL_COOLDOWN, 上次耗時)」，理由同
        // working changes worker；rebase 這類每一步都動 HEAD 的操作，
        // 沒有這條會一路凍結到結束。睡覺期間累積的請求併進這一輪。
        let wait = FULL_COOLDOWN
            .max(last_cost)
            .saturating_sub(last_end.elapsed());
        if !wait.is_zero() {
            thread::sleep(wait);
            while let Ok(at) = request_rx.try_recv() {
                max_at = max_at.max(at);
            }
        }

        let start = Instant::now();
        let result = panic::catch_unwind(AssertUnwindSafe(|| load_full(&config, &applied)));

        let mut next_state = FullState::Idle;
        match result {
            Ok(Ok(Outcome::Unchanged)) => {
                covered = start;
            }
            Ok(Ok(Outcome::Changed(loaded))) => {
                covered = start;
                next_state = FullState::Ready(loaded);
            }
            Ok(Err(e)) => {
                tx.send(AppEvent::NotifyError(e.to_string()));
            }
            Err(payload) => {
                tx.send(AppEvent::NotifyError(
                    t!("app.reload.panicked", detail = panic_message(payload)).into_owned(),
                ));
            }
        }
        *state.lock().unwrap_or_else(PoisonError::into_inner) = next_state;
        tx.send(AppEvent::FullReloadStatus);

        last_cost = start.elapsed();
        last_end = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Sender;

    fn full_config() -> FullLoadConfig {
        FullLoadConfig {
            path: PathBuf::from("/nonexistent"),
            order: SortCommit::Chronological,
            max_count: None,
            trunc: graph::Truncation::new(0),
        }
    }

    fn spawn_for_test() -> Reloader {
        let (tx, _rx) = Sender::channel_for_test();
        Reloader::spawn(PathBuf::from("/nonexistent"), full_config(), tx)
    }

    #[test]
    fn full_status_starts_idle() {
        assert_eq!(spawn_for_test().full_status(), FullStatus::Idle);
    }

    #[test]
    fn take_full_on_idle_returns_none() {
        assert!(spawn_for_test().take_full().is_none());
    }

    /// 路徑不是 repo：`Repository::load` 失敗，狀態回 `Idle`，收到
    /// `NotifyError`——不會一直卡在 `Loading`。
    #[test]
    fn request_on_nonexistent_repo_settles_to_idle_and_notifies() {
        let (tx, rx) = Sender::channel_for_test();
        let reloader = Reloader::spawn(PathBuf::from("/nonexistent"), full_config(), tx);

        reloader.request_full(Instant::now());

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if reloader.full_status() == FullStatus::Idle
                && matches!(rx.try_recv(), Ok(AppEvent::NotifyError(_)))
            {
                break;
            }
            assert!(Instant::now() < deadline, "逾時：worker 沒有回到 Idle");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(reloader.take_full().is_none());
    }
}
