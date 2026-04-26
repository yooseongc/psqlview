use super::match_engine::find_in_line;
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

#[test]
fn find_in_line_returns_each_occurrence() {
    let m = find_in_line("select * from t; SELECT 1; SeLeCt 2", "select", true);
    assert_eq!(m, vec![(0, 6), (17, 23), (27, 33)]);
}

#[test]
fn find_in_line_case_sensitive_filters_out_mismatched_case() {
    let m = find_in_line("select Select SELECT", "Select", false);
    assert_eq!(m, vec![(7, 13)]);
}

#[test]
fn find_in_line_does_not_overlap_matches() {
    let m = find_in_line("aaaa", "aa", true);
    // Non-overlapping: 0..2, 2..4. NOT 0,1,2.
    assert_eq!(m, vec![(0, 2), (2, 4)]);
}

#[test]
fn recompute_populates_matches_for_each_line() {
    let mut s = FindState::with_needle("from".into(), false);
    let lines: Vec<String> = vec!["from a".into(), "no m here".into(), "and from b".into()];
    s.recompute(&lines);
    assert_eq!(
        s.matches,
        vec![
            (Cursor::new(0, 0), Cursor::new(0, 4)),
            (Cursor::new(2, 4), Cursor::new(2, 8)),
        ]
    );
    assert_eq!(s.active_idx, Some(0));
}

#[test]
fn recompute_clears_when_needle_empty() {
    let mut s = FindState::new();
    s.recompute(&["any text".to_string()]);
    assert!(s.matches.is_empty());
    assert!(s.active_idx.is_none());
}

#[test]
fn advance_wraps_around() {
    let mut s = FindState::with_needle("a".into(), false);
    s.recompute(&["aaa".to_string()]);
    assert_eq!(s.matches.len(), 3);
    s.active_idx = Some(2);
    assert_eq!(s.advance(), Some(Cursor::new(0, 0)));
}

#[test]
fn retreat_wraps_to_last() {
    let mut s = FindState::with_needle("a".into(), false);
    s.recompute(&["aaa".to_string()]);
    s.active_idx = Some(0);
    assert_eq!(s.retreat(), Some(Cursor::new(0, 2)));
}

#[test]
fn typing_a_char_extends_needle_and_jumps_to_first_match() {
    let lines = vec!["select".to_string()];
    let mut s = FindState::new();
    let out = handle_key(&mut s, k(KeyCode::Char('s'), KeyModifiers::NONE), &lines);
    assert_eq!(out, FindOutcome::JumpTo(Cursor::new(0, 0)));
    assert_eq!(s.needle, "s");
    assert_eq!(s.matches.len(), 1);
}

#[test]
fn enter_advances_through_matches() {
    let lines = vec!["a a a".to_string()];
    let mut s = FindState::with_needle("a".into(), false);
    s.recompute(&lines);
    let out = handle_key(&mut s, k(KeyCode::Enter, KeyModifiers::NONE), &lines);
    assert_eq!(out, FindOutcome::JumpTo(Cursor::new(0, 2)));
}

#[test]
fn esc_cancels() {
    let lines = vec!["x".to_string()];
    let mut s = FindState::with_needle("x".into(), false);
    s.recompute(&lines);
    assert_eq!(
        handle_key(&mut s, k(KeyCode::Esc, KeyModifiers::NONE), &lines),
        FindOutcome::Cancel
    );
}

#[test]
fn alt_c_toggles_case_sensitivity() {
    let lines = vec!["Select select".to_string()];
    let mut s = FindState::with_needle("select".into(), false);
    s.recompute(&lines);
    assert_eq!(s.matches.len(), 2); // case-insensitive: both
    handle_key(&mut s, k(KeyCode::Char('c'), KeyModifiers::ALT), &lines);
    assert!(s.case_sensitive);
    assert_eq!(s.matches.len(), 1); // now only the lowercase one
}

#[test]
fn status_label_formats_index_and_total() {
    let lines = vec!["abab".to_string()];
    let mut s = FindState::with_needle("ab".into(), false);
    s.recompute(&lines);
    assert_eq!(s.status_label(), "[1/2]");
    s.active_idx = Some(1);
    assert_eq!(s.status_label(), "[2/2]");
}

// ---- vim search semantics --------------------------------------

#[test]
fn vim_search_anchored_recompute_picks_match_after_cursor() {
    let lines = vec!["foo bar foo baz foo".to_string()];
    let mut s = FindState::new_vim_search(false, Cursor::new(0, 5));
    s.needle = "foo".into();
    s.recompute(&lines);
    // First foo at col 0 (before cursor), second at col 8, third at 16.
    // forward + anchor (0, 5) → first match at-or-after col 5 = idx 1.
    assert_eq!(s.active_idx, Some(1));
}

#[test]
fn vim_search_backward_anchored_picks_match_before_cursor() {
    let lines = vec!["foo bar foo baz foo".to_string()];
    let mut s = FindState::new_vim_search(true, Cursor::new(0, 14));
    s.needle = "foo".into();
    s.recompute(&lines);
    // backward + anchor (0, 14) → last match strictly before col 14 = idx 1 (col 8).
    assert_eq!(s.active_idx, Some(1));
    assert!(s.backward);
    assert!(s.enter_closes);
}

#[test]
fn vim_search_anchor_after_last_match_wraps_to_first() {
    let lines = vec!["foo".to_string()];
    let mut s = FindState::new_vim_search(false, Cursor::new(0, 99));
    s.needle = "foo".into();
    s.recompute(&lines);
    // Forward + anchor past end → wrap to idx 0.
    assert_eq!(s.active_idx, Some(0));
}

#[test]
fn enter_in_vim_search_returns_jump_and_close() {
    let lines = vec!["foo bar foo".to_string()];
    let mut s = FindState::new_vim_search(false, Cursor::new(0, 0));
    s.needle = "foo".into();
    s.recompute(&lines);
    let out = handle_key(&mut s, k(KeyCode::Enter, KeyModifiers::NONE), &lines);
    // Active match is the first foo at col 0 — JumpAndClose to it.
    assert_eq!(out, FindOutcome::JumpAndClose(Cursor::new(0, 0)));
}

#[test]
fn enter_in_backward_vim_search_jumps_to_active_match() {
    let lines = vec!["foo bar foo".to_string()];
    let mut s = FindState::new_vim_search(true, Cursor::new(0, 10));
    s.needle = "foo".into();
    s.recompute(&lines);
    let out = handle_key(&mut s, k(KeyCode::Enter, KeyModifiers::NONE), &lines);
    // Backward + anchor (0, 10) → active is idx 1 (col 8). Backward
    // Enter calls retreat(), which from idx 1 returns idx 0 (col 0).
    assert_eq!(out, FindOutcome::JumpAndClose(Cursor::new(0, 0)));
}

#[test]
fn ctrl_f_style_enter_keeps_overlay_open() {
    // Ctrl+F entry: enter_closes = false (Ctrl+F path does not call
    // new_vim_search). Enter advances and stays open.
    let lines = vec!["foo bar".to_string()];
    let mut s = FindState::with_needle("foo".into(), false);
    s.recompute(&lines);
    assert!(!s.enter_closes);
    let out = handle_key(&mut s, k(KeyCode::Enter, KeyModifiers::NONE), &lines);
    // advance() wraps from 0 back to 0 (only one match), JumpTo not Close.
    assert!(matches!(out, FindOutcome::JumpTo(_)));
}
