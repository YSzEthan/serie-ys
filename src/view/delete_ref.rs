use rust_i18n::t;
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    thread,
};

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{
        delete_branch, delete_branch_force, delete_remote_branch, delete_remote_tag,
        delete_remote_tracking_ref, delete_tag, remote_tracking_oid, restore_remote_tracking_ref,
        RefType, RemoteState,
    },
    view::RefsOrigin,
    widget::{
        commit_list::{CommitList, CommitListState},
        h, keybind_hint_line,
        ref_list::RefListState,
    },
};

/// 刪 remote branch 時可以勾選的項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOpt {
    /// 本機同名分支（`git branch -D <name>`）。
    Local(String),
    /// GitHub 上的分支（`git push <remote> :refs/heads/<branch>`）。
    Remote,
    /// 本機追蹤 ref（`git branch -Dr <remote>/<branch>`，冪等：
    /// Remote 那步已讓 git 順手清掉時也算成功）。
    Tracking,
}

/// 列出要顯示的列，順序 = 執行順序。預設與舊行為一致：GitHub ✔、追蹤 ref ✔、
/// 本機分支 ✘。Local 排在 Tracking 前面是為了不讓追蹤 ref 先消失影響本機分支那步。
fn build_rows(local_branch: Option<String>, remote_state: RemoteState) -> Vec<(RemoteOpt, bool)> {
    let mut rows = Vec::with_capacity(3);
    if let Some(name) = local_branch {
        rows.push((RemoteOpt::Local(name), false));
    }
    if remote_state == RemoteState::Exists {
        rows.push((RemoteOpt::Remote, true));
    }
    rows.push((RemoteOpt::Tracking, true));
    rows
}

/// 只挑出有勾的列，順序維持 `rows` 的順序（= 執行順序）。
fn plan_steps(rows: &[(RemoteOpt, bool)]) -> Vec<RemoteOpt> {
    rows.iter()
        .filter(|(_, checked)| *checked)
        .map(|(opt, _)| opt.clone())
        .collect()
}

struct StepsOutcome {
    message: String,
    succeeded: usize,
    failed: usize,
}

/// 依序執行勾選的步驟，某一步失敗仍繼續下一步。
fn run_remote_steps(repo_path: &Path, ref_name: &str, steps: &[RemoteOpt]) -> StepsOutcome {
    let want_tracking = steps.contains(&RemoteOpt::Tracking);
    let mut lines = Vec::with_capacity(steps.len());
    let (mut succeeded, mut failed) = (0, 0);

    for step in steps {
        let (kind, name, result) = match step {
            RemoteOpt::Local(name) => (
                "local branch",
                name.as_str(),
                delete_branch_force(repo_path, name),
            ),
            RemoteOpt::Remote => {
                // 使用者不想刪追蹤 ref：push 前記 oid，push 後放回去。
                let saved_oid = if want_tracking {
                    None
                } else {
                    remote_tracking_oid(repo_path, ref_name)
                };
                let result = delete_remote_branch(repo_path, ref_name);
                if let (true, Some(oid)) = (result.is_ok(), saved_oid) {
                    if let Err(e) = restore_remote_tracking_ref(repo_path, ref_name, &oid) {
                        lines.push(e);
                    }
                }
                ("remote branch", ref_name, result)
            }
            RemoteOpt::Tracking => (
                "tracking branch",
                ref_name,
                delete_remote_tracking_ref(repo_path, ref_name),
            ),
        };
        match result {
            Ok(()) => {
                succeeded += 1;
                lines.push(t!("view.delete_ref.step_ok", kind = kind, name = name).into_owned());
            }
            Err(e) => {
                failed += 1;
                lines.push(
                    t!(
                        "view.delete_ref.step_failed",
                        kind = kind,
                        name = name,
                        e = e
                    )
                    .into_owned(),
                );
            }
        }
    }
    StepsOutcome {
        message: lines.join("；"),
        succeeded,
        failed,
    }
}

#[derive(Debug)]
pub struct DeleteRefView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,
    repo_path: PathBuf,

    ref_name: String,
    ref_type: RefType,
    /// 只給 Tag 用（同時刪 origin 上的 tag）。
    delete_from_remote: bool,
    force_delete: bool,

    /// 只給 RemoteBranch 用：要顯示的勾選列與目前游標。
    rows: Vec<(RemoteOpt, bool)>,
    cursor: usize,
    remote_state: RemoteState,

    // 純 passthrough：DeleteRef 本身不關心 Refs 從哪來，
    // 只是為了 close_delete_ref 時把原 origin 還回 RefsView。
    // TODO: DeleteRef 長遠應 demotion 成 RefsView 的 modal dialog state，
    // 就不需要這個欄位。
    refs_origin: RefsOrigin,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DeleteRefView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        repo_path: PathBuf,
        ref_name: String,
        ref_type: RefType,
        local_branch: Option<String>,
        remote_state: RemoteState,
        refs_origin: RefsOrigin,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DeleteRefView<'a> {
        let rows = if ref_type == RefType::RemoteBranch {
            build_rows(local_branch, remote_state)
        } else {
            Vec::new()
        };
        DeleteRefView {
            commit_list_state: Some(commit_list_state),
            ref_list_state,
            repo_path,
            ref_name,
            ref_type,
            delete_from_remote: false,
            force_delete: false,
            rows,
            cursor: 0,
            remote_state,
            refs_origin,
            ctx,
            tx,
        }
    }

    pub fn refs_origin(&self) -> RefsOrigin {
        self.refs_origin
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _key: KeyEvent) {
        let event = event_with_count.event;

        match event {
            UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseDeleteRef);
            }
            UserEvent::Confirm => {
                self.delete_ref();
            }
            UserEvent::NavigateRight | UserEvent::NavigateLeft | UserEvent::NavigateDown
                if self.ref_type != RefType::RemoteBranch =>
            {
                match self.ref_type {
                    RefType::Tag => self.delete_from_remote = !self.delete_from_remote,
                    _ => self.force_delete = !self.force_delete,
                }
            }
            UserEvent::NavigateDown if !self.rows.is_empty() => {
                self.cursor = (self.cursor + 1) % self.rows.len()
            }
            UserEvent::NavigateUp if !self.rows.is_empty() => {
                self.cursor = (self.cursor + self.rows.len() - 1) % self.rows.len()
            }
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some((_, checked)) = self.rows.get_mut(self.cursor) {
                    *checked = !*checked;
                }
            }
            _ => {}
        }
    }

    fn delete_remote_branch_rows(&mut self) {
        let steps = plan_steps(&self.rows);
        self.tx.send(AppEvent::CloseDeleteRef);
        if steps.is_empty() {
            return;
        }
        let ref_name = self.ref_name.clone();
        let repo_path = self.repo_path.clone();
        let tx = self.tx.clone();

        self.tx.send(AppEvent::ShowPendingOverlay {
            message: t!("view.delete_ref.pending_remote_branch", name = ref_name).into_owned(),
        });
        thread::spawn(move || {
            let outcome = run_remote_steps(&repo_path, &ref_name, &steps);
            if outcome.succeeded > 0 {
                tx.send(AppEvent::Refresh);
            }
            tx.send(AppEvent::HidePendingOverlay);
            tx.send(match (outcome.succeeded, outcome.failed) {
                (_, 0) => AppEvent::NotifySuccess(outcome.message),
                (0, _) => AppEvent::NotifyError(outcome.message),
                _ => AppEvent::NotifyWarn(outcome.message),
            });
        });
    }

    fn delete_ref(&mut self) {
        if self.ref_type == RefType::RemoteBranch {
            return self.delete_remote_branch_rows();
        }
        let ref_name = self.ref_name.clone();
        let ref_type = self.ref_type;
        let repo_path = self.repo_path.clone();
        let delete_from_remote = self.delete_from_remote;
        let force_delete = self.force_delete;
        let tx = self.tx.clone();

        let pending_msg = match ref_type {
            RefType::Tag if delete_from_remote => {
                t!("view.delete_tag.pending_remote", tag = ref_name)
            }
            RefType::Tag => t!("view.delete_tag.pending", tag = ref_name),
            RefType::Branch if force_delete => {
                t!("app.delete_branch.pending_force", name = ref_name)
            }
            RefType::Branch => t!("app.delete_branch.pending", name = ref_name),
            RefType::RemoteBranch => unreachable!("由 delete_remote_branch_rows 處理"),
        }
        .into_owned();

        self.tx.send(AppEvent::ShowPendingOverlay {
            message: pending_msg,
        });
        self.tx.send(AppEvent::CloseDeleteRef);

        thread::spawn(move || {
            // 記錄本機刪除是否成功（即使 remote 失敗也要能更新 UI）
            let mut local_deleted = false;

            let result = match ref_type {
                RefType::Tag => {
                    if let Err(e) = delete_tag(&repo_path, &ref_name) {
                        Err(e)
                    } else {
                        local_deleted = true;
                        if delete_from_remote {
                            delete_remote_tag(&repo_path, &ref_name).map_err(|e| {
                                t!("view.delete_tag.remote_failed", detail = e).into_owned()
                            })
                        } else {
                            Ok(())
                        }
                    }
                }
                RefType::Branch => {
                    let res = if force_delete {
                        delete_branch_force(&repo_path, &ref_name)
                    } else {
                        delete_branch(&repo_path, &ref_name)
                    };
                    if res.is_ok() {
                        local_deleted = true;
                    }
                    res
                }
                RefType::RemoteBranch => unreachable!("由 delete_remote_branch_rows 處理"),
            };

            match result {
                Ok(()) => {
                    let msg = match ref_type {
                        RefType::Tag if delete_from_remote => {
                            t!("view.delete_tag.done_remote", tag = ref_name)
                        }
                        RefType::Tag => t!("view.delete_tag.done_local", tag = ref_name),
                        RefType::Branch => t!("app.delete_branch.done", name = ref_name),
                        RefType::RemoteBranch => unreachable!("由 delete_remote_branch_rows 處理"),
                    }
                    .into_owned();
                    tx.send(AppEvent::NotifySuccess(msg));
                    tx.send(AppEvent::HidePendingOverlay);
                    tx.send(AppEvent::Refresh);
                }
                Err(e) => {
                    // 如果本機刪除成功，仍然要 refresh UI
                    if local_deleted {
                        tx.send(AppEvent::Refresh);
                    }
                    tx.send(AppEvent::HidePendingOverlay);
                    tx.send(AppEvent::NotifyError(e));
                }
            }
        });
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let Some(list_state) = self.commit_list_state.as_mut() else {
            return;
        };

        let graph_width = list_state.panel_left_edge();
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs_width);

        let [list_area, refs_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(refs_width)]).areas(area);

        let commit_list = CommitList::new(self.ctx.clone(), 0);
        f.render_stateful_widget(commit_list, list_area, list_state);

        let ref_list = crate::widget::ref_list::RefList::new(self.ctx.clone());
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);

        let dialog_width = 50u16.min(area.width.saturating_sub(4));
        // remote branch 的勾選列數不固定（最多 3 列，加一行「無法連線」提示）。
        let unknown_note =
            self.remote_state == RemoteState::Unknown && self.ref_type == RefType::RemoteBranch;
        let checkbox_rows = match self.ref_type {
            RefType::RemoteBranch => self.rows.len() as u16 + u16::from(unknown_note),
            _ => 2,
        };
        let dialog_height = (4 + checkbox_rows).min(area.height.saturating_sub(2));

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        f.render_widget(Clear, dialog_area);

        let title = match self.ref_type {
            RefType::Tag => t!("view.delete_tag.title"),
            RefType::Branch => t!("view.delete_ref.title_branch"),
            RefType::RemoteBranch => t!("view.delete_ref.title_remote_branch"),
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .style(
                Style::default()
                    .bg(self.ctx.color_theme.bg)
                    .fg(self.ctx.color_theme.fg),
            )
            .padding(Padding::horizontal(1));

        let inner_area = block.inner(dialog_area);
        f.render_widget(block, dialog_area);

        let [name_area, checkbox_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(checkbox_rows),
            Constraint::Min(1),
        ])
        .areas(inner_area);

        let name_line = Line::from(vec![Span::raw(&self.ref_name)
            .fg(self.ctx.color_theme.fg)
            .add_modifier(Modifier::BOLD)]);
        f.render_widget(Paragraph::new(name_line), name_area);

        let fg = self.ctx.color_theme.fg;
        let single_checkbox = |checked: bool, label: String| {
            let checkbox = if checked { "[x]" } else { "[ ]" };
            vec![Line::from(vec![
                Span::styled(checkbox, Style::default().fg(fg)),
                Span::raw(label).fg(fg),
            ])]
        };
        let lines: Vec<Line> = match self.ref_type {
            RefType::Tag => single_checkbox(
                self.delete_from_remote,
                t!("view.delete_tag.from_origin").into_owned(),
            ),
            RefType::Branch => single_checkbox(
                self.force_delete,
                t!("view.delete_ref.force_delete").into_owned(),
            ),
            RefType::RemoteBranch => {
                let branch = self.ref_name.split_once('/').map_or("", |(_, b)| b);
                let mut lines: Vec<Line> = self
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, (opt, checked))| {
                        let label = match opt {
                            RemoteOpt::Local(name) => t!("view.delete_ref.opt_local", name = name),
                            RemoteOpt::Remote => t!("view.delete_ref.opt_remote", name = branch),
                            RemoteOpt::Tracking => {
                                t!("view.delete_ref.opt_tracking", name = self.ref_name)
                            }
                        };
                        let checkbox = if *checked { "[x]" } else { "[ ]" };
                        let mut style = Style::default().fg(fg);
                        if i == self.cursor {
                            style = style.add_modifier(Modifier::REVERSED);
                        }
                        Line::from(Span::styled(format!("{checkbox}{label}"), style))
                    })
                    .collect();
                if unknown_note {
                    lines.push(Line::from(
                        Span::raw(t!("view.delete_ref.remote_unknown"))
                            .fg(self.ctx.color_theme.divider_fg),
                    ));
                }
                lines
            }
        };
        f.render_widget(Paragraph::new(lines), checkbox_area);

        let mut hint_items = vec![
            h(&[UserEvent::Confirm], t!("common.hint.delete")),
            h(&[UserEvent::Cancel], t!("common.hint.cancel")),
            h(
                &[UserEvent::NavigateLeft, UserEvent::NavigateRight],
                t!("common.hint.toggle"),
            ),
        ];
        // 只有 remote branch 有多列可移動；Tag／Branch 只有單一勾選框。
        if self.ref_type == RefType::RemoteBranch {
            hint_items.push(h(
                &[UserEvent::NavigateUp, UserEvent::NavigateDown],
                t!("common.hint.move"),
            ));
        }
        let hints = keybind_hint_line(&self.ctx.color_theme, &self.ctx.keybind, &hint_items);
        f.render_widget(Paragraph::new(hints).centered(), hint_area);
    }
}

impl<'a> DeleteRefView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub(super) fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state
            .as_mut()
            .expect("commit_list_state already taken")
    }

    pub fn take_ref_list_state(&mut self) -> RefListState {
        std::mem::take(&mut self.ref_list_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// bare remote + 一個 clone；remote 與本機都有 `001`，也有 `origin/001`。
    fn setup() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bare = root.path().join("bare.git");
        let work = root.path().join("work");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "A"]);
        git(&work, &["branch", "001"]);
        git(&work, &["push", "-q", "origin", "main", "001"]);
        git(&work, &["fetch", "-q", "origin"]);
        (root, work)
    }

    fn has_ref(dir: &Path, name: &str) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", "-q", name])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    }

    fn remote_has_001(dir: &Path) -> bool {
        !git(dir, &["ls-remote", "--heads", "origin", "refs/heads/001"])
            .trim()
            .is_empty()
    }

    #[test]
    fn build_rows_only_lists_existing_items() {
        use RemoteOpt::*;
        assert_eq!(build_rows(None, RemoteState::Gone), vec![(Tracking, true)]);
        assert_eq!(
            build_rows(None, RemoteState::Unknown),
            vec![(Tracking, true)]
        );
        assert_eq!(
            build_rows(Some("001".into()), RemoteState::Exists),
            vec![
                (Local("001".into()), false),
                (Remote, true),
                (Tracking, true)
            ]
        );
    }

    #[test]
    fn plan_steps_keeps_order_and_skips_unchecked() {
        use RemoteOpt::*;
        let rows = vec![(Local("a".into()), true), (Remote, false), (Tracking, true)];
        assert_eq!(plan_steps(&rows), vec![Local("a".into()), Tracking]);
    }

    #[test]
    fn remote_only_keeps_tracking_ref() {
        let (_root, work) = setup();
        let out = run_remote_steps(&work, "origin/001", &[RemoteOpt::Remote]);
        assert_eq!((out.succeeded, out.failed), (1, 0), "{}", out.message);
        assert!(has_ref(&work, "refs/remotes/origin/001"));
        assert!(!remote_has_001(&work));
    }

    #[test]
    fn tracking_only_keeps_remote_branch() {
        let (_root, work) = setup();
        let out = run_remote_steps(&work, "origin/001", &[RemoteOpt::Tracking]);
        assert_eq!((out.succeeded, out.failed), (1, 0), "{}", out.message);
        assert!(!has_ref(&work, "refs/remotes/origin/001"));
        assert!(remote_has_001(&work));
    }

    /// 擋刪 `refs/remotes/origin/*` 的 reference-transaction hook，除非
    /// `GIT_ALLOW_ORIGIN_REF_DELETE=1`。掛在 repo 層級的 `core.hooksPath`，
    /// 不受全域設定影響。
    #[cfg(unix)]
    fn install_origin_guard_hook(root: &Path, work: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let hooks = root.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("reference-transaction");
        std::fs::write(
            &hook,
            "#!/bin/sh\n\
             [ \"$1\" = prepared ] || exit 0\n\
             [ \"$GIT_ALLOW_ORIGIN_REF_DELETE\" = 1 ] && exit 0\n\
             while read -r old new ref; do\n\
             \x20 case \"$new $ref\" in\n\
             \x20   0000*\" refs/remotes/origin/\"*) echo \"blocked: $ref\" >&2; exit 1;;\n\
             \x20 esac\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(work, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    }

    #[cfg(unix)]
    #[test]
    fn guarded_repo_still_deletes_tracking_ref() {
        let (root, work) = setup();
        install_origin_guard_hook(root.path(), &work);
        // 前置：hook 確實有擋（不帶 env 刪不掉）
        let blocked = Command::new("git")
            .args(["update-ref", "-d", "refs/remotes/origin/001"])
            .current_dir(&work)
            .output()
            .unwrap();
        assert!(!blocked.status.success(), "hook 沒生效：{blocked:?}");

        let steps = [
            RemoteOpt::Local("001".into()),
            RemoteOpt::Remote,
            RemoteOpt::Tracking,
        ];
        let out = run_remote_steps(&work, "origin/001", &steps);
        assert_eq!((out.succeeded, out.failed), (3, 0), "{}", out.message);
        assert!(!has_ref(&work, "refs/heads/001"));
        assert!(!has_ref(&work, "refs/remotes/origin/001"));
        assert!(!remote_has_001(&work));
    }

    #[test]
    fn all_three_deletes_everything() {
        let (_root, work) = setup();
        let steps = [
            RemoteOpt::Local("001".into()),
            RemoteOpt::Remote,
            RemoteOpt::Tracking,
        ];
        let out = run_remote_steps(&work, "origin/001", &steps);
        assert_eq!((out.succeeded, out.failed), (3, 0), "{}", out.message);
        assert!(!has_ref(&work, "refs/heads/001"));
        assert!(!has_ref(&work, "refs/remotes/origin/001"));
        assert!(!remote_has_001(&work));
    }
}
