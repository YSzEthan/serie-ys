# Keybindings

<!-- Generated from `src/view/help.rs` by `cargo test`. Do not edit by hand. -->
<!-- To regenerate: UPDATE_KEYBINDINGS_DOC=1 cargo test -->

Press <kbd>?</kbd> in the app to see this list at any time, with your own
overrides already applied.

The keys below are the defaults; see
[Custom Keybindings](./custom-keybindings.md) for how to change them.

## Default keybindings

### Common

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>Ctrl-c</kbd> | Force quit | `force_quit` |
| <kbd>q</kbd> | Quit (press twice) | `quit` |
| <kbd>F1</kbd> <kbd>?</kbd> | Open help | `help_toggle` |

### Help

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>F1</kbd> <kbd>?</kbd> <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Left</kbd> <kbd>h</kbd> | Close help | `help_toggle` `cancel` `close` `navigate_left` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | Scroll down | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | Scroll up | `navigate_up` `select_up` |

### Commit List

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | Move down | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | Move up | `navigate_up` `select_up` |
| <kbd>i</kbd> | Go to top | `go_to_top` |
| <kbd>G</kbd> | Go to bottom | `go_to_bottom` |
| <kbd>.</kbd> | Go to HEAD | `go_to_head` |
| <kbd>,</kbd> | Scroll down | `scroll_down` |
| <kbd>m</kbd> | Select parent commit | `go_to_parent` |
| <kbd>Enter</kbd> <kbd>y</kbd> <kbd>Right</kbd> <kbd>l</kbd> | Show commit details | `confirm` `navigate_right` |
| <kbd>Tab</kbd> | Open refs list | `ref_list` |
| <kbd>:</kbd> | Start search | `search` |
| <kbd>'</kbd> | Start filter | `filter` |
| <kbd>n</kbd> <kbd>Esc</kbd> | Cancel search/filter | `cancel` |
| <kbd>]</kbd> | Go to next search match | `go_to_next` |
| <kbd>[</kbd> | Go to previous search match | `go_to_previous` |
| <kbd>x</kbd> | Toggle fuzzy match | `fuzzy_toggle` |
| <kbd>Alt-c</kbd> | Toggle ignore case | `ignore_case_toggle` |
| <kbd>c</kbd> | Copy commit short hash | `short_copy` |
| <kbd>C</kbd> | Copy commit subject | `full_copy` |
| <kbd>b</kbd> | Copy branch name (prefer local) | `branch_copy` |
| <kbd>B</kbd> | Copy remote branch name | `full_branch_copy` |
| <kbd>v</kbd> | Copy tag name | `tag_copy` |
| <kbd>t</kbd> | Create tag on commit | `create_tag` |
| <kbd>Ctrl-t</kbd> | Delete tag from commit | `delete_tag` |
| <kbd>d</kbd> | Delete local branch from commit | `delete_ref` |
| <kbd>o</kbd> | Toggle remote refs | `remote_refs_toggle` |
| <kbd>g</kbd> | Open GitHub issues/PRs | `github_toggle` |
| <kbd>f</kbd> | Fetch all remotes | `fetch` |
| <kbd>Space</kbd> | Checkout selected commit/ref | `checkout` |
| <kbd>r</kbd> | Refresh | `refresh` |

### Commit Detail

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Enter</kbd> <kbd>y</kbd> | Close commit details | `cancel` `close` `confirm` |
| <kbd>u</kbd> | Toggle detail pane | `detail_pane_toggle` |
| <kbd>Down</kbd> <kbd>j</kbd> | Scroll down | `navigate_down` |
| <kbd>Up</kbd> <kbd>k</kbd> | Scroll up | `navigate_up` |
| <kbd>Right</kbd> <kbd>l</kbd> | Select older commit | `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | Select newer commit | `navigate_left` |
| <kbd>m</kbd> | Select parent commit | `go_to_parent` |
| <kbd>c</kbd> | Copy commit short hash | `short_copy` |
| <kbd>C</kbd> | Copy commit subject | `full_copy` |
| <kbd>b</kbd> | Copy branch name (prefer local) | `branch_copy` |
| <kbd>B</kbd> | Copy remote branch name | `full_branch_copy` |
| <kbd>v</kbd> | Copy tag name | `tag_copy` |
| <kbd>o</kbd> | Toggle remote refs | `remote_refs_toggle` |
| <kbd>Tab</kbd> | Open refs list | `ref_list` |
| <kbd>F1</kbd> <kbd>?</kbd> | Open help | `help_toggle` |
| <kbd>r</kbd> | Refresh | `refresh` |

### Refs List

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> | Close refs list | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | Move down | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | Move up | `navigate_up` `select_up` |
| <kbd>Right</kbd> <kbd>l</kbd> | Open node | `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | Close node / Close refs | `navigate_left` |
| <kbd>Space</kbd> | Checkout selected branch | `checkout` |
| <kbd>d</kbd> <kbd>Ctrl-t</kbd> | Delete ref | `delete_ref` `delete_tag` |
| <kbd>r</kbd> | Refresh | `refresh` |

### GitHub View

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>g</kbd> <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> | Close GitHub view | `github_toggle` `cancel` `close` |
| <kbd>Tab</kbd> | Switch issue/PR tab | `ref_list` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | Move down | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | Move up | `navigate_up` `select_up` |
| <kbd>PageDown</kbd> <kbd>Ctrl-f</kbd> | Page down | `page_down` |
| <kbd>PageUp</kbd> <kbd>Ctrl-b</kbd> | Page up | `page_up` |
| <kbd>Ctrl-d</kbd> | Half page down | `half_page_down` |
| <kbd>Ctrl-u</kbd> | Half page up | `half_page_up` |
| <kbd>i</kbd> | Go to top | `go_to_top` |
| <kbd>G</kbd> | Go to bottom | `go_to_bottom` |
| <kbd>Enter</kbd> <kbd>y</kbd> <kbd>Right</kbd> <kbd>l</kbd> | Preview / toggle checkbox | `confirm` `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | Back / cancel | `navigate_left` |
| <kbd>:</kbd> | Search / type number to jump to #N | `search` |
| <kbd>'</kbd> | Filter | `filter` |
| <kbd>c</kbd> | Copy issue/PR URL | `short_copy` |
| <kbd>C</kbd> | Open issue/PR in browser | `full_copy` |
| <kbd>v</kbd> | Copy issue/PR number (#N) | `tag_copy` |
| <kbd>u</kbd> | Open related issue/PR picker | `detail_pane_toggle` |
| <kbd>r</kbd> | Refresh | `refresh` |
| <kbd>p</kbd> | 3-stage merge PR: pick method, delete branch, confirm | `merge_pr` |
| <kbd>X</kbd> | Close/reopen issue or PR | `toggle_issue_state` |
| <kbd>P</kbd> | Mark PR ready / back to draft | `toggle_pr_draft` |

### Create Tag

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | Confirm create | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | Cancel and close | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>Up</kbd> <kbd>k</kbd> | Switch input field | `navigate_down` `navigate_up` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> | Toggle push option | `navigate_right` `navigate_left` |

### Delete Tag

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | Confirm delete | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | Cancel and close | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | Select next tag | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | Select previous tag | `navigate_up` `select_up` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> | Toggle delete from remote | `navigate_right` `navigate_left` |

### Delete Ref

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | Confirm delete ref | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | Cancel | `cancel` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> <kbd>Down</kbd> <kbd>j</kbd> | Toggle yes/no | `navigate_right` `navigate_left` `navigate_down` |

### User Command

| Key | Description | Config key |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> | Close user command | `cancel` `close` |
| <kbd>Down</kbd> <kbd>j</kbd> | Scroll down | `navigate_down` |
| <kbd>Up</kbd> <kbd>k</kbd> | Scroll up | `navigate_up` |
| <kbd>PageDown</kbd> <kbd>Ctrl-f</kbd> | Scroll page down | `page_down` |
| <kbd>PageUp</kbd> <kbd>Ctrl-b</kbd> | Scroll page up | `page_up` |
| <kbd>Ctrl-d</kbd> | Scroll half page down | `half_page_down` |
| <kbd>Ctrl-u</kbd> | Scroll half page up | `half_page_up` |
| <kbd>i</kbd> | Go to top | `go_to_top` |
| <kbd>G</kbd> | Go to bottom | `go_to_bottom` |
| <kbd>J</kbd> | Select older commit | `select_down` |
| <kbd>K</kbd> | Select newer commit | `select_up` |
| <kbd>m</kbd> | Select parent commit | `go_to_parent` |
| <kbd>r</kbd> | Refresh | `refresh` |
| <kbd>Enter</kbd> <kbd>y</kbd> | Show commit details | `confirm` |
| <kbd>F1</kbd> <kbd>?</kbd> | Open help | `help_toggle` |

## Hardcoded keys

These keys are fixed and cannot be changed via config, because they belong to
transient prompts rather than a view's keymap.

| Key | Where | Action |
| --- | ----- | ------ |
| <kbd>1</kbd>–<kbd>9</kbd> | Ref / checkout / related / branch pickers | Pick the n-th entry |
| <kbd>m</kbd> <kbd>s</kbd> <kbd>r</kbd> | Merge PR prompt (step 1) | Merge / squash / rebase |
| <kbd>y</kbd> <kbd>n</kbd> | Merge PR prompt (step 2) | Delete the branch after merging, or not |
| <kbd>f</kbd> | Delete branch confirmation | Force delete |
| <kbd>Tab</kbd> <kbd>Shift-Tab</kbd> | Create tag dialog | Move between fields |
| <kbd>Space</kbd> | Create tag dialog (checkbox) | Toggle the checkbox |
