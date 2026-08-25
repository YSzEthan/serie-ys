use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 把 pipe 讀到底丟進 channel；呼叫端只能用 `recv_timeout` 有界地等，
/// 不能 `join`（理由見 `run_with_timeout`）。
fn spawn_reader<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// 逾時就砍掉的子行程執行，`github.rs::run_gh`（純讀，`stdin_data = None`）跟
/// `update_body`（要把 body 灌進 stdin，`stdin_data = Some(body)`）、
/// `app.rs::spawn_git_task`（背景 `git fetch`／`checkout`）共用。
///
/// **函式內部唯一的等待機制是 `deadline`，沒有任何 `join()`**——子行程常會
/// fork 出孫行程（`gh` 底下的 `git`、或使用者的 shell hook），`kill()` 只
/// 殺得掉直接子行程，孫行程繼承的 pipe 寫端沒關，reader thread 永遠等不到
/// EOF，`join()` 會卡死——這正好是「有子行程掛在網路上」的情境，也就是這
/// 個函式最該生效的那個情境。改用 `spawn_reader` + `recv_timeout(剩餘預算)`，
/// 不管子行程是被我們 kill、還是自己結束但孫行程仍握著 pipe，都保證在
/// `timeout` 內返回。
pub fn run_with_timeout(
    mut cmd: Command,
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute {program}: {e}"))?;

    if let Some(data) = stdin_data {
        // 射後不理：唯一的副作用是寫完 drop 掉 `stdin`，讓子行程收到
        // EOF。不能等它——它可能跟 reader thread 一樣卡在孫行程手上。
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin is piped when stdin_data is Some");
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }

    let stdout_rx = spawn_reader(child.stdout.take().expect("stdout is piped"));
    let stderr_rx = spawn_reader(child.stderr.take().expect("stderr is piped"));

    let deadline = Instant::now() + timeout;
    let status = loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            Ok(None) => format!(
                "{program} command timed out after {}s (network issue?)",
                timeout.as_secs()
            ),
            // try_wait 本身失敗（極罕見）也走同一條收尾。
            Err(e) => format!("Failed to wait for {program}: {e}"),
        };
        let _ = child.kill();
        let _ = child.wait(); // reap，不留 zombie；不等 reader thread
        return Err(failure);
    };

    // 子行程自己結束了（不是被我們 kill），但孫行程可能仍握著 pipe 寫端
    // 不放——用剩餘的 timeout 預算等 reader，逾時就當空輸出，不能無界等
    // （這條路徑對應 `Command::output()`／`wait_with_output()` 今天就有
    // 的同一個理論限制，這裡額外把它也收進同一個 deadline 預算裡）。
    //
    // 每次都要重算剩餘預算：算一次餵給兩個 `recv_timeout` 的話，子行程
    // 一結束就 break（剩餘 ≈ 整個 timeout）時兩次各可耗滿，總共變成
    // 2×timeout。（`Receiver::recv_deadline()` 能一步到位，但還是
    // unstable，rust-lang/rust#46316，不能用。）
    //
    // 逾時當空輸出、不是錯誤：`github.rs::run_gh` 會把空字串餵給
    // `parse_issues_graphql`，使用者看到的是 JSON parse error 而不是
    // timed out 訊息；但 `github.rs::update_body` 根本不讀 stdout，只看
    // `status`，改成報錯反而會對「其實成功了」的編輯回報假錯誤——而
    // checkbox toggle 不冪等，使用者重試會把狀態弄反。回假成功比回假
    // 失敗安全，這個取捨對所有呼叫端一致套用，不各自決定。
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let stdout = stdout_rx.recv_timeout(remaining()).unwrap_or_default();
    let stderr = stderr_rx.recv_timeout(remaining()).unwrap_or_default();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 所有逾時測試共用的預算與斷言上限。上限抓 3 倍而不是「反正很大」的
    // 2 秒：這幾條測試唯一的價值就是證明「有界」，斷言鬆到跟預算無關就
    // 什麼都沒證明。

    const BUDGET: Duration = Duration::from_millis(150);
    const MAX_ELAPSED: Duration = Duration::from_millis(450);

    #[test]
    fn run_with_timeout_pipes_stdin_and_captures_stdout() {
        let output = run_with_timeout(
            Command::new("cat"),
            Some(b"hello\n"),
            Duration::from_secs(5),
        )
        .expect("cat should succeed");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello\n");
    }

    #[test]
    fn run_with_timeout_kills_a_directly_hanging_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let err = run_with_timeout(cmd, None, BUDGET).expect_err("sleep 30 should time out");
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
    }

    #[test]
    fn run_with_timeout_does_not_hang_when_a_killed_childs_orphan_still_holds_the_pipe() {
        // sh 自己因為 `wait` 卡住 30 秒（觸發 kill），但它背景起的 sleep
        // 繼承了同一組 stdout/stderr 寫端——kill 掉 sh 不會連帶殺掉 sleep，
        // 這正是 join() 版本會卡死的情境，即使子行程本身已經被砍掉、reap 掉。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        let err = run_with_timeout(cmd, None, BUDGET)
            .expect_err("should time out even though a grandchild still holds the pipe open");
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
    }

    #[test]
    fn run_with_timeout_does_not_exceed_budget_when_child_exits_but_orphan_holds_the_pipe() {
        // sh 沒有 `wait`，立刻結束（成功路徑，不會觸發 kill）；但背景的
        // sleep 一樣繼承了 pipe 寫端還活著。這條是「remaining 算一次用
        // 兩次」的迴歸測試：那個寫法在這裡會花掉 2×BUDGET，每次重算才會
        // 落在 BUDGET 附近。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 &"]);
        let start = Instant::now();
        let output = run_with_timeout(cmd, None, BUDGET)
            .expect("sh itself exits immediately, this should not error");
        assert!(output.status.success());
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
    }
}
