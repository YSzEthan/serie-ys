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
    event::{AppEvent, BranchKind, Sender},
    git::Ref,
};

/// 依 `b` / `B` 規則從 local/remote 清單挑候選，候選 1 個直接送 CopyToClipboard，
/// 候選 >=2 送 OpenBranchPicker（最多前 9 個）；沒候選靜默略過。
pub(crate) fn dispatch_branch_copy(tx: &Sender, local: &[&str], remote: &[&str], full: bool) {
    let (candidates, kind) = if full {
        if remote.is_empty() {
            return;
        }
        (remote, BranchKind::Remote)
    } else if !local.is_empty() {
        (local, BranchKind::Local)
    } else if !remote.is_empty() {
        (remote, BranchKind::Remote)
    } else {
        return;
    };
    if candidates.len() == 1 {
        tx.send(AppEvent::CopyToClipboard {
            name: kind.copy_label().into(),
            value: candidates[0].to_owned(),
        });
    } else {
        let options: Vec<String> = candidates.iter().take(9).map(|s| (*s).to_owned()).collect();
        tx.send(AppEvent::OpenBranchPicker { options, kind });
    }
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
}
