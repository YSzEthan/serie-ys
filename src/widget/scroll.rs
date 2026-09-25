//! 標準 vim 風格 scrolloff 的共用計算。`commit_detail` 的檔案樹視窗與
//! `commit_list` 的游標邊距都是同一套數學，只是「邊距要留幾列」的值不同
//! （前者固定 `FILE_TREE_SCROLLOFF`，後者來自 `ui.list.scrolloff` 設定），
//! 抽成這裡讓兩邊共用同一份實作，不要各自維護一份。

/// 邊距值不可能超過視窗高度的一半——上下各留一半後中間至少要留一列給
/// 游標本身，否則上下邊距互相矛盾。`height == 0` 時 `saturating_sub`
/// 保護不會 underflow，回傳 0。
pub(crate) fn effective_scrolloff(scrolloff: usize, height: usize) -> usize {
    scrolloff.min(height.saturating_sub(1) / 2)
}

/// 游標移動或視窗大小改變後，計算「最小捲動」的新 offset：只在游標即將
/// 超出 `[offset+margin, offset+height-1-margin]` 這個可視範圍時才捲動，
/// 捲動量剛好讓游標回到邊界，其餘情況維持 `prev_offset` 不動。這是唯一
/// 的定位計算點——游標移動與 render 時的 resize 安全網都呼叫這裡。
pub(crate) fn scrolled_offset(
    cursor: usize,
    window_height: usize,
    rows_len: usize,
    prev_offset: usize,
    scrolloff: usize,
) -> usize {
    if window_height == 0 {
        return 0;
    }
    let max_offset = rows_len.saturating_sub(window_height);
    if max_offset == 0 {
        return 0;
    }

    let margin = effective_scrolloff(scrolloff, window_height);
    let min_offset_for_cursor = cursor.saturating_sub(window_height - 1 - margin);
    let max_offset_for_cursor = cursor.saturating_sub(margin).min(max_offset);

    let mut offset = prev_offset.min(max_offset);
    if offset > max_offset_for_cursor {
        offset = max_offset_for_cursor;
    }
    if offset < min_offset_for_cursor {
        offset = min_offset_for_cursor.min(max_offset);
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_scrolloff_is_capped_by_half_the_height() {
        assert_eq!(effective_scrolloff(15, 6), 2);
        assert_eq!(effective_scrolloff(15, 46), 15);
        assert_eq!(effective_scrolloff(15, 45), 15);
        assert_eq!(effective_scrolloff(15, 44), 15);
        assert_eq!(effective_scrolloff(2, 0), 0);
        assert_eq!(effective_scrolloff(0, 100), 0);
    }

    #[test]
    fn scrolled_offset_keeps_prev_offset_when_cursor_stays_in_margin() {
        // rows=12, h=6, scrolloff=2, prev_offset=4：可視範圍 [4+2, 4+3] = [6, 7]。
        // 游標在 6，落在範圍內，不動。
        assert_eq!(scrolled_offset(6, 6, 12, 4, 2), 4);
    }

    #[test]
    fn scrolled_offset_scrolls_minimally_when_cursor_exits_top_margin() {
        assert_eq!(scrolled_offset(4, 6, 12, 6, 2), 2);
    }

    #[test]
    fn scrolled_offset_scrolls_minimally_when_cursor_exits_bottom_margin() {
        assert_eq!(scrolled_offset(8, 6, 12, 2, 2), 5);
    }

    #[test]
    fn scrolled_offset_is_zero_when_rows_fit_in_window() {
        assert_eq!(scrolled_offset(3, 10, 6, 3, 2), 0);
    }

    #[test]
    fn scrolled_offset_is_zero_when_window_height_is_zero() {
        assert_eq!(scrolled_offset(5, 0, 12, 3, 2), 0);
    }

    #[test]
    fn scrolled_offset_caps_offset_at_max_offset() {
        assert_eq!(scrolled_offset(11, 6, 12, usize::MAX, 2), 6);
    }

    #[test]
    fn scrolled_offset_invariants_hold_across_the_value_space() {
        for rows_len in [0usize, 1, 2, 6, 12, 30] {
            for window_height in [0usize, 1, 2, 3, 6, 10] {
                for scrolloff in [0usize, 1, 2, 5, 100] {
                    let max_offset = rows_len.saturating_sub(window_height);
                    for cursor in 0..rows_len.max(1) {
                        for prev_offset in [0usize, max_offset, usize::MAX] {
                            let offset = scrolled_offset(
                                cursor,
                                window_height,
                                rows_len,
                                prev_offset,
                                scrolloff,
                            );
                            assert!(
                                offset <= max_offset,
                                "offset {offset} > max_offset {max_offset} \
                                 (rows_len={rows_len}, h={window_height}, so={scrolloff}, cursor={cursor}, prev={prev_offset})"
                            );
                            if window_height > 0 {
                                assert!(
                                    offset <= cursor,
                                    "offset {offset} > cursor {cursor} \
                                     (rows_len={rows_len}, h={window_height}, so={scrolloff}, prev={prev_offset})"
                                );
                                assert!(
                                    cursor - offset < window_height,
                                    "cursor {cursor} - offset {offset} >= h {window_height} \
                                     (rows_len={rows_len}, so={scrolloff}, prev={prev_offset})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
