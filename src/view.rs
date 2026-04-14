mod views;

mod create_tag;
mod delete_ref;
mod delete_tag;
mod detail;
mod github;
mod help;
mod list;
mod refs;
mod user_command;

pub use refs::RefsOrigin;
pub use views::*;

use crate::{
    event::{AppEvent, RefCopyKind, Sender},
    git::Ref,
};

/// 核心 send-or-picker 邏輯：候選 1 個直接 CopyToClipboard，>=2 送 OpenRefPicker
/// （最多前 9 個），0 個靜默。branch/tag 的 dispatch wrapper 都最終呼這個。
pub(crate) fn dispatch_ref_copy(tx: &Sender, candidates: &[&str], kind: RefCopyKind) {
    if candidates.is_empty() {
        return;
    }
    if candidates.len() == 1 {
        tx.send(AppEvent::CopyToClipboard {
            name: kind.copy_label().into(),
            value: candidates[0].to_owned(),
        });
    } else {
        let options: Vec<String> = candidates.iter().take(9).map(|s| (*s).to_owned()).collect();
        tx.send(AppEvent::OpenRefPicker { options, kind });
    }
}

/// 依 `b` / `B` 規則從 local/remote 清單挑候選，再交給 `dispatch_ref_copy`。
/// `full=true` (Shift+B) 只看 remote；`false` (b) prefer local，fallback remote。
pub(crate) fn dispatch_branch_copy(tx: &Sender, local: &[&str], remote: &[&str], full: bool) {
    let (candidates, kind) = if full {
        (remote, RefCopyKind::Remote)
    } else if !local.is_empty() {
        (local, RefCopyKind::Local)
    } else {
        (remote, RefCopyKind::Remote)
    };
    dispatch_ref_copy(tx, candidates, kind);
}

/// Tag 無 local/remote 之分，直接交給 `dispatch_ref_copy`。
pub(crate) fn dispatch_tag_copy(tx: &Sender, tags: &[&str]) {
    dispatch_ref_copy(tx, tags, RefCopyKind::Tag);
}

/// 把 refs 分拆成 local 和 remote branch 名稱列表，各自按字典序排序。
/// Tag / Stash 會被忽略。
pub(crate) fn partition_branches<'r>(
    refs: impl IntoIterator<Item = &'r Ref>,
) -> (Vec<&'r str>, Vec<&'r str>) {
    let mut local: Vec<&str> = Vec::new();
    let mut remote: Vec<&str> = Vec::new();
    for r in refs {
        match r {
            Ref::Branch { name, .. } => local.push(name.as_str()),
            Ref::RemoteBranch { name, .. } => remote.push(name.as_str()),
            _ => {}
        }
    }
    local.sort_unstable();
    remote.sort_unstable();
    (local, remote)
}

/// 把 refs 裡的 tag 名稱抽出，字典序排序。Branch / Stash 忽略。
/// tag name 已經被 `parse_tag_refs` strip 過 `refs/tags/` 與 `^{}`。
pub(crate) fn partition_tags<'r>(refs: impl IntoIterator<Item = &'r Ref>) -> Vec<&'r str> {
    let mut tags: Vec<&str> = refs
        .into_iter()
        .filter_map(|r| match r {
            Ref::Tag { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    tags.sort_unstable();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Ref;

    fn branch(name: &str) -> Ref {
        Ref::Branch {
            name: name.into(),
            target: "deadbeef".into(),
        }
    }

    fn remote_branch(name: &str) -> Ref {
        Ref::RemoteBranch {
            name: name.into(),
            target: "deadbeef".into(),
        }
    }

    fn tag(name: &str) -> Ref {
        Ref::Tag {
            name: name.into(),
            target: "deadbeef".into(),
        }
    }

    #[test]
    fn partition_local_only() {
        let refs = [branch("foo")];
        let (local, remote) = partition_branches(refs.iter());
        assert_eq!(local, ["foo"]);
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_remote_only() {
        let refs = [remote_branch("origin/foo")];
        let (local, remote) = partition_branches(refs.iter());
        assert!(local.is_empty());
        assert_eq!(remote, ["origin/foo"]);
    }

    #[test]
    fn partition_both() {
        let refs = [branch("foo"), remote_branch("origin/foo")];
        let (local, remote) = partition_branches(refs.iter());
        assert_eq!(local, ["foo"]);
        assert_eq!(remote, ["origin/foo"]);
    }

    #[test]
    fn partition_multiple_local_sorted() {
        let refs = [branch("b"), branch("a"), branch("c")];
        let (local, remote) = partition_branches(refs.iter());
        assert_eq!(local, ["a", "b", "c"]);
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_multiple_mixed_sorted() {
        let refs = [
            remote_branch("origin/z"),
            branch("y"),
            remote_branch("origin/a"),
            branch("x"),
        ];
        let (local, remote) = partition_branches(refs.iter());
        assert_eq!(local, ["x", "y"]);
        assert_eq!(remote, ["origin/a", "origin/z"]);
    }

    #[test]
    fn partition_ignores_tags() {
        let refs = [tag("v1.0"), branch("foo")];
        let (local, remote) = partition_branches(refs.iter());
        assert_eq!(local, ["foo"]);
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_empty() {
        let refs: [Ref; 0] = [];
        let (local, remote) = partition_branches(refs.iter());
        assert!(local.is_empty());
        assert!(remote.is_empty());
    }

    #[test]
    fn partition_tags_only() {
        let refs = [tag("v1.0")];
        let tags = partition_tags(refs.iter());
        assert_eq!(tags, ["v1.0"]);
    }

    #[test]
    fn partition_tags_sorted() {
        let refs = [tag("v2.0"), tag("v1.0"), tag("v1.5")];
        let tags = partition_tags(refs.iter());
        assert_eq!(tags, ["v1.0", "v1.5", "v2.0"]);
    }

    #[test]
    fn partition_tags_ignores_branches() {
        let refs = [branch("foo"), tag("v1.0"), remote_branch("origin/foo")];
        let tags = partition_tags(refs.iter());
        assert_eq!(tags, ["v1.0"]);
    }

    #[test]
    fn partition_tags_empty() {
        let refs: [Ref; 0] = [];
        let tags = partition_tags(refs.iter());
        assert!(tags.is_empty());
    }
}
