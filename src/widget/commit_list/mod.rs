mod layout;
mod render;
mod search;
mod state;

pub use render::CommitList;
pub use search::{FilterState, MatchQuery, SearchState};
pub use state::{ChildJump, CommitListState};

use ratatui::style::Color;

use crate::git::{Commit, CommitHash, Ref};

/// 索引到 `commits: Vec<CommitInfo>` 與 `search_matches: Vec<SearchMatch>` 的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RawCommitIdx(pub(crate) usize);

/// `filtered_indices` 內的位置；filter 空時 alias 到 raw（語意上仍是獨立座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilteredIdx(usize);

/// 可視清單內的位置（= FilteredIdx + virtual_row_offset）。
/// 對應 `self.offset + self.selected` 的空間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleIdx(usize);

#[derive(Debug, Clone, Copy)]
enum MatchStep {
    Next,
    Prev,
}

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    graph_color: Color,
}

impl<'a> CommitInfo<'a> {
    pub fn new(commit: &'a Commit, refs: Vec<&'a Ref>, graph_color: Color) -> Self {
        Self {
            commit,
            refs,
            graph_color,
        }
    }

    pub fn commit_hash(&self) -> &CommitHash {
        &self.commit.commit_hash
    }

    pub fn refs(&self) -> &[&'a Ref] {
        &self.refs
    }

    pub fn subject(&self) -> &str {
        &self.commit.subject
    }
}

/// `select_child` 遇到分支點（2 個以上 child）時回報的候選項目。
///
/// 用 owned `String`/`CommitHash` 而非 `&'a Ref`，不是風格選擇：
/// - `Sender::send_after`（`event.rs`）把 `AppEvent` move 進 `thread::spawn` 的
///   closure，要求 `AppEvent: Send + 'static`。
/// - `EventController` 活得比任何一次 `App::new` 借用的 `Repository` 都久（見
///   `lib.rs` 的主迴圈），`&'a Ref` 那個 `'a` 撐不到事件被消費的那一刻。
/// - 同樣的約束 `CommitHash` 選擇 `Arc<str>` 時已經寫過一次（`git.rs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildPickOption {
    pub label: String,
    pub commit_hash: CommitHash,
}
