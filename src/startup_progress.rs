//! 進 TUI 之前的載入提示：初始 `Repository::load`＋`calc_graph` 在大 repo
//! 要跑好幾秒，這段時間還沒進 alt screen，終端機一片空白會像當掉。
//!
//! 這時還在一般（cooked）模式，Ctrl-C 照預設直接結束 process，不需要還原
//! 終端機。提示寫在 stderr，stderr 不是 TTY（重導到檔案、管線）就完全不畫。

use std::{
    convert::Infallible,
    io::{self, IsTerminal},
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ratatui::crossterm::{
    cursor::MoveToColumn,
    execute,
    style::Print,
    terminal::{Clear, ClearType},
};

/// 第一次畫之前先等一下：小 repo 在這之前就載完，完全不會閃一下；
/// 大 repo 仍能在 0.1 s 內看到秒數。
const FIRST_DRAW_DELAY: Duration = Duration::from_millis(80);
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// RAII guard：drop 時停掉 thread 並清掉提示那一行。`run()` 中途 `?`
/// 提早回傳（例如不是 git repo）也會走 drop，錯誤訊息因此印在乾淨的一行。
/// stderr 不是 TTY 時是 `None`，什麼都不畫。
pub struct StartupProgress(Option<(mpsc::Sender<Infallible>, JoinHandle<()>)>);

impl StartupProgress {
    pub fn start() -> Self {
        if !io::stderr().is_terminal() {
            return Self(None);
        }
        let (tx, rx) = mpsc::channel::<Infallible>();
        let handle = thread::spawn(move || {
            let start = Instant::now();
            let mut wait = FIRST_DRAW_DELAY;
            // 靠 sender 被 drop 時的 `Disconnected` 喚醒
            while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(wait) {
                // `UntilNewLine`：一般模式下使用者打的字會 echo 在這一行，要一起蓋掉
                let _ = execute!(
                    io::stderr(),
                    MoveToColumn(0),
                    Print(render(start.elapsed())),
                    Clear(ClearType::UntilNewLine)
                );
                wait = REDRAW_INTERVAL;
            }
            let _ = execute!(io::stderr(), MoveToColumn(0), Clear(ClearType::CurrentLine));
        });
        Self(Some((tx, handle)))
    }
}

impl Drop for StartupProgress {
    fn drop(&mut self) {
        // 先 drop sender 讓 thread 醒來清行，再 join 等它清完——之後才會
        // 進 alt screen 或印錯誤訊息，兩者都不能跟提示搶同一行。
        if let Some((tx, handle)) = self.0.take() {
            drop(tx);
            let _ = handle.join();
        }
    }
}

/// 一位小數：整數秒的話第一秒會一直停在「0 秒」，看起來像卡住。
fn render(elapsed: Duration) -> String {
    format!("載入中… {:.1} 秒", elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_shows_one_decimal() {
        assert_eq!(render(Duration::from_millis(100)), "載入中… 0.1 秒");
        assert_eq!(render(Duration::from_millis(1349)), "載入中… 1.3 秒");
        assert_eq!(render(Duration::from_secs(12)), "載入中… 12.0 秒");
    }
}
