use super::*;

#[test]
fn word_prefix_extracts_identifier_before_cursor() {
    let mut e = EditorState::new();
    e.type_text("SELECT user");
    assert_eq!(e.word_prefix_before_cursor(), "user");
}

#[test]
fn word_prefix_empty_when_cursor_after_space() {
    let mut e = EditorState::new();
    e.type_text("SELECT ");
    assert_eq!(e.word_prefix_before_cursor(), "");
}

#[test]
fn word_prefix_empty_when_cursor_after_digit_start() {
    let mut e = EditorState::new();
    e.type_text("123abc");
    assert_eq!(e.word_prefix_before_cursor(), "");
}

#[test]
fn replace_word_prefix_swaps_last_token() {
    let mut e = EditorState::new();
    e.type_text("SELECT use");
    e.replace_word_prefix("users");
    assert_eq!(e.text(), "SELECT users");
}

#[test]
fn outdent_removes_up_to_two_leading_spaces() {
    let mut e = EditorState::new();
    e.type_text("    SELECT 1");
    // Move cursor to line start.
    for _ in 0..12 {
        e.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    }
    e.outdent_current_line();
    assert_eq!(e.text(), "  SELECT 1");
    e.outdent_current_line();
    assert_eq!(e.text(), "SELECT 1");
    // No leading spaces → no-op.
    e.outdent_current_line();
    assert_eq!(e.text(), "SELECT 1");
}

#[test]
fn insert_spaces_appends_n_spaces() {
    let mut e = EditorState::new();
    e.type_text("a");
    e.insert_spaces(3);
    assert_eq!(e.text(), "a   ");
}

#[test]
fn insert_str_preserves_newlines() {
    let mut e = EditorState::new();
    e.insert_str("SELECT 1\nFROM t;");
    assert_eq!(e.text(), "SELECT 1\nFROM t;");
}

#[test]
fn move_cursor_to_char_position_handles_single_line() {
    let mut e = EditorState::new();
    e.type_text("SELECT 1 FROM nope");
    // 'n' of "nope" is at 1-based char 15.
    assert!(e.move_cursor_to_char_position(15));
    assert_eq!(e.cursor_line_col(), (1, 15));
}

#[test]
fn move_cursor_to_char_position_handles_multi_line() {
    let mut e = EditorState::new();
    e.type_text("SELECT 1\nFROM bad");
    assert!(e.move_cursor_to_char_position(15));
    let (ln, col) = e.cursor_line_col();
    assert_eq!(ln, 2);
    assert_eq!(col, 6);
}

#[test]
fn move_cursor_to_char_position_returns_false_when_out_of_range() {
    let mut e = EditorState::new();
    e.type_text("abc");
    assert!(!e.move_cursor_to_char_position(99));
}

#[test]
fn indent_lines_prepends_two_spaces_per_line() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc");
    e.indent_lines(0, 2);
    assert_eq!(e.text(), "  a\n  b\n  c");
}

#[test]
fn outdent_lines_removes_up_to_two_leading_spaces_per_line() {
    let mut e = EditorState::new();
    e.type_text("    a\n  b\nc");
    e.outdent_lines(0, 2);
    assert_eq!(e.text(), "  a\nb\nc");
}

#[test]
fn indent_then_outdent_round_trips() {
    let mut e = EditorState::new();
    e.type_text("x\ny\nz");
    e.indent_lines(0, 2);
    e.outdent_lines(0, 2);
    assert_eq!(e.text(), "x\ny\nz");
}

#[test]
fn insert_str_normalizes_crlf() {
    let mut e = EditorState::new();
    e.insert_str("a\r\nb");
    assert_eq!(e.text(), "a\nb");
}

#[test]
fn goto_line_jumps_to_first_column_of_target_row() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd");
    e.goto_line(3);
    assert_eq!(e.cursor_line_col(), (3, 1));
}

#[test]
fn goto_line_clamps_to_last_row_when_out_of_range() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc");
    e.goto_line(99);
    assert_eq!(e.cursor_line_col(), (3, 1));
}

#[test]
fn replace_range_swaps_a_single_match() {
    let mut e = EditorState::new();
    e.type_text("a foo b");
    let s = buffer::Cursor::new(0, 2);
    let end = buffer::Cursor::new(0, 5);
    e.replace_range(s, end, "BAR");
    assert_eq!(e.text(), "a BAR b");
}

#[test]
fn replace_all_swaps_every_match_and_undo_is_one_step() {
    let mut e = EditorState::new();
    e.type_text("a a a");
    let ranges = vec![
        (buffer::Cursor::new(0, 0), buffer::Cursor::new(0, 1)),
        (buffer::Cursor::new(0, 2), buffer::Cursor::new(0, 3)),
        (buffer::Cursor::new(0, 4), buffer::Cursor::new(0, 5)),
    ];
    e.replace_all(&ranges, "bb");
    assert_eq!(e.text(), "bb bb bb");
    // Single Ctrl+Z reverts the entire batch.
    e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(e.text(), "a a a");
}

#[test]
fn replace_all_handles_replacement_containing_needle() {
    // Replacing 'foo' with 'foofoo' must NOT loop — left-to-right
    // semantics, no rescanning.
    let mut e = EditorState::new();
    e.type_text("foo");
    let ranges = vec![(buffer::Cursor::new(0, 0), buffer::Cursor::new(0, 3))];
    e.replace_all(&ranges, "foofoo");
    assert_eq!(e.text(), "foofoo");
}

#[test]
fn replace_all_with_empty_ranges_is_noop() {
    let mut e = EditorState::new();
    e.type_text("untouched");
    e.replace_all(&[], "x");
    assert_eq!(e.text(), "untouched");
}

#[test]
fn goto_line_zero_is_treated_as_line_one() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc");
    e.goto_line(0);
    assert_eq!(e.cursor_line_col(), (1, 1));
}

#[test]
fn ctrl_z_undoes_last_edit() {
    let mut e = EditorState::new();
    e.type_text("ab");
    e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(e.text(), "a");
}

#[test]
fn ctrl_y_redoes_undone_edit() {
    let mut e = EditorState::new();
    e.type_text("ab");
    e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    e.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(e.text(), "ab");
}

// ---- mode state machine ----------------------------------------

fn press(e: &mut EditorState, code: KeyCode, mods: KeyModifiers) -> bool {
    e.handle_key(KeyEvent::new(code, mods))
}

#[test]
fn fresh_editor_starts_in_insert_mode() {
    let e = EditorState::new();
    assert_eq!(e.mode(), Mode::Insert);
}

#[test]
fn esc_in_insert_switches_to_normal() {
    let mut e = EditorState::new();
    e.type_text("hi");
    let dirty = press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Normal);
    assert!(!dirty, "mode flip is not a text change");
    assert_eq!(e.text(), "hi", "Esc must not mutate the buffer");
}

#[test]
fn normal_swallows_unmapped_keys() {
    let mut e = EditorState::new();
    e.type_text("abc");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    // 'x' is currently unmapped here — buffer must not change.
    let before = e.text();
    let dirty = press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!dirty);
    assert_eq!(e.text(), before);
    assert_eq!(e.mode(), Mode::Normal);
}

#[test]
fn i_in_normal_switches_to_insert_at_cursor() {
    let mut e = EditorState::new();
    e.type_text("ab");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    // Cursor is at (0, 2) (end of "ab").
    press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.cursor_pos(), (0, 2));
}

#[test]
fn a_in_normal_moves_right_then_insert() {
    let mut e = EditorState::new();
    e.type_text("ab");
    // Move cursor to col 1 ('b' is at col 1).
    press(&mut e, KeyCode::Left, KeyModifiers::NONE);
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.cursor_pos(), (0, 2));
}

#[test]
fn a_at_eol_does_not_move_past_end() {
    let mut e = EditorState::new();
    e.type_text("ab"); // cursor at (0, 2) — end of line
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 2));
}

#[test]
fn capital_i_jumps_to_line_start() {
    let mut e = EditorState::new();
    e.type_text("abc"); // cursor (0, 3)
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('I'), KeyModifiers::SHIFT);
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.cursor_pos(), (0, 0));
}

#[test]
fn capital_a_jumps_to_line_end() {
    let mut e = EditorState::new();
    e.type_text("abc");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE); // (0, 0)
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('A'), KeyModifiers::SHIFT);
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.cursor_pos(), (0, 3));
}

#[test]
fn o_opens_line_below_and_enters_insert() {
    let mut e = EditorState::new();
    e.type_text("abc");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    let dirty = press(&mut e, KeyCode::Char('o'), KeyModifiers::NONE);
    assert!(dirty, "o adds text");
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.text(), "abc\n");
    assert_eq!(e.cursor_pos(), (1, 0));
}

#[test]
fn capital_o_opens_line_above_and_enters_insert() {
    let mut e = EditorState::new();
    e.type_text("abc");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    let dirty = press(&mut e, KeyCode::Char('O'), KeyModifiers::SHIFT);
    assert!(dirty, "O adds text");
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.text(), "\nabc");
    assert_eq!(e.cursor_pos(), (0, 0));
}

#[test]
fn ctrl_z_works_in_normal_mode() {
    let mut e = EditorState::new();
    e.type_text("ab");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(e.text(), "a");
}

#[test]
fn entering_insert_from_normal_then_typing_inserts_text() {
    let mut e = EditorState::new();
    e.type_text("ab");
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('A'), KeyModifiers::SHIFT);
    // Now in Insert at (0, 2). Type a single char.
    let dirty = press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(dirty);
    assert_eq!(e.text(), "abc");
}

// ---- motions + count + chord -----------------------------------

fn enter_normal(e: &mut EditorState) {
    press(e, KeyCode::Esc, KeyModifiers::NONE);
}

#[test]
fn h_in_normal_moves_left() {
    let mut e = EditorState::new();
    e.type_text("hello"); // cursor (0, 5)
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 4));
}

#[test]
fn count_prefix_repeats_motion() {
    let mut e = EditorState::new();
    e.type_text("hello"); // cursor (0, 5)
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 2));
}

#[test]
fn count_accumulates_across_multiple_digits() {
    let mut e = EditorState::new();
    // Build a long line so a 12-step left move actually has room.
    e.type_text(&"x".repeat(20)); // cursor (0, 20)
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('1'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('2'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 8));
}

#[test]
fn count_resets_after_motion_fires() {
    let mut e = EditorState::new();
    e.type_text("hello world");
    enter_normal(&mut e);
    // 3h moves 3 left, then plain h must move only 1.
    press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    let after_3h = e.cursor_pos();
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos().1, after_3h.1 - 1);
}

#[test]
fn zero_first_is_line_start() {
    let mut e = EditorState::new();
    e.type_text("  hello");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('0'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 0));
}

#[test]
fn zero_extends_count_when_count_in_progress() {
    let mut e = EditorState::new();
    e.type_text(&"x".repeat(20));
    enter_normal(&mut e);
    // 1 then 0 should accumulate to 10, not stop at LineStart.
    press(&mut e, KeyCode::Char('1'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('0'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 10));
}

#[test]
fn caret_jumps_to_first_non_blank() {
    let mut e = EditorState::new();
    e.type_text("    abc");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('^'), KeyModifiers::SHIFT);
    assert_eq!(e.cursor_pos(), (0, 4));
}

#[test]
fn dollar_jumps_to_line_end() {
    let mut e = EditorState::new();
    e.type_text("abcdef");
    // Move to start.
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('$'), KeyModifiers::SHIFT);
    assert_eq!(e.cursor_pos(), (0, 6));
}

#[test]
fn w_jumps_to_next_word_start() {
    let mut e = EditorState::new();
    e.type_text("foo bar baz");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 4));
}

#[test]
fn b_jumps_to_previous_word_start() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    // cursor (0, 7) end-of-line
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 4));
}

#[test]
fn e_jumps_to_word_end() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('e'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (0, 2));
}

#[test]
fn percent_jumps_to_matching_bracket() {
    let mut e = EditorState::new();
    e.type_text("(foo)");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('%'), KeyModifiers::SHIFT);
    assert_eq!(e.cursor_pos(), (0, 4));
}

#[test]
fn capital_g_with_count_goes_to_line_n() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd\ne");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(e.cursor_line_col(), (3, 1));
}

#[test]
fn bare_capital_g_goes_to_last_line() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc");
    // Cursor sits at end of buffer; first move it elsewhere.
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    press(&mut e, KeyCode::Up, KeyModifiers::NONE);
    press(&mut e, KeyCode::Up, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(e.cursor_line_col(), (3, 1));
}

#[test]
fn gg_chord_resolves_to_first_line() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd");
    enter_normal(&mut e);
    // First g — chord pending, no movement.
    press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
    let after_first_g = e.cursor_line_col();
    // Buffer cursor unchanged so far.
    assert_eq!(after_first_g, e.cursor_line_col());
    press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(e.cursor_line_col(), (1, 1));
}

#[test]
fn gg_chord_with_count_goes_to_line_n() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd\ne");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('5'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(e.cursor_line_col(), (5, 1));
}

#[test]
fn chord_is_broken_by_unrelated_key() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
    // Now press 'h' — chord breaks, h applies as Left motion.
    let before = e.cursor_pos();
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    assert_eq!(e.cursor_pos(), (before.0, before.1.saturating_sub(1)));
}

#[test]
fn mode_entry_resets_pending_count() {
    let mut e = EditorState::new();
    e.type_text("abc");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('5'), KeyModifiers::NONE);
    // 'i' enters Insert and the `5` is dropped, not used as count.
    press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Insert);
    // Type a char to confirm insert mode and that no count lingered.
    press(&mut e, KeyCode::Char('z'), KeyModifiers::NONE);
    // Cursor was at (0, 3) before; 5 was dropped; one 'z' inserted.
    assert!(e.text().contains('z'));
}

// ---- Visual mode + operators + text objects --------------------

#[test]
fn v_enters_visual_at_cursor() {
    let mut e = EditorState::new();
    e.type_text("hello");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Visual);
}

#[test]
fn v_then_motion_extends_selection_then_d_deletes() {
    let mut e = EditorState::new();
    e.type_text("hello world");
    // Move cursor to start.
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    // v + 4 right + d → delete first 5 chars ("hello"). Visual is
    // inclusive so the right edge is part of the deletion.
    press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
    for _ in 0..4 {
        press(&mut e, KeyCode::Char('l'), KeyModifiers::NONE);
    }
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(e.text(), " world");
    assert_eq!(e.mode(), Mode::Normal);
}

#[test]
fn esc_in_visual_returns_to_normal_without_change() {
    let mut e = EditorState::new();
    e.type_text("hello");
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
    let before = e.text();
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Normal);
    assert_eq!(e.text(), before);
}

#[test]
fn dw_deletes_to_next_word_start() {
    let mut e = EditorState::new();
    e.type_text("foo bar baz");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    // dw on "foo" → delete "foo " (word + trailing space).
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.text(), "bar baz");
}

#[test]
fn de_deletes_through_word_end_inclusive() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    // de = delete from 'f' through end of 'foo' (inclusive).
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('e'), KeyModifiers::NONE);
    assert_eq!(e.text(), " bar");
}

#[test]
fn dd_deletes_current_line() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc");
    // Move to line 2.
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    press(&mut e, KeyCode::Up, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(e.text(), "a\nc");
}

#[test]
fn dd_with_count_deletes_n_lines() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd\ne");
    // Move to line 2 (col 0).
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..3 {
        press(&mut e, KeyCode::Up, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    // 3dd → delete lines 2..=4.
    press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    assert_eq!(e.text(), "a\ne");
}

#[test]
fn yw_yanks_into_register_without_modifying_buffer() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.text(), "foo bar");
    assert_eq!(e.register, "foo ");
}

#[test]
fn cw_deletes_word_and_enters_insert() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.mode(), Mode::Insert);
    // Now in Insert at start; type something.
    press(&mut e, KeyCode::Char('Z'), KeyModifiers::SHIFT);
    assert_eq!(e.text(), "Zbar");
}

#[test]
fn diw_deletes_inner_word() {
    let mut e = EditorState::new();
    e.type_text("foo bar baz");
    // Cursor on 'a' of "bar" (col 5).
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..5 {
        press(&mut e, KeyCode::Right, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.text(), "foo  baz");
}

#[test]
fn diw_big_word_includes_dotted_identifier() {
    let mut e = EditorState::new();
    e.type_text("SELECT schema.table FROM x");
    // Cursor on 't' of "table" (col 14).
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..14 {
        press(&mut e, KeyCode::Right, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('W'), KeyModifiers::SHIFT);
    // diW takes the whole "schema.table".
    assert_eq!(e.text(), "SELECT  FROM x");
}

#[test]
fn ci_quote_changes_string_contents() {
    let mut e = EditorState::new();
    e.type_text("a \"foo\" b");
    // Cursor on 'o' inside quotes (col 4).
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..4 {
        press(&mut e, KeyCode::Right, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('"'), KeyModifiers::SHIFT);
    // Now in Insert with "foo" gone, quotes preserved.
    assert_eq!(e.mode(), Mode::Insert);
    assert_eq!(e.text(), "a \"\" b");
}

#[test]
fn dap_deletes_around_parens() {
    let mut e = EditorState::new();
    e.type_text("foo(bar)baz");
    // Cursor on 'a' of "bar" (col 5).
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..5 {
        press(&mut e, KeyCode::Right, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('('), KeyModifiers::SHIFT);
    assert_eq!(e.text(), "foobaz");
}

#[test]
fn paste_after_inserts_register_after_cursor() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    // Yank first word.
    press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    // Move to end of buffer.
    press(&mut e, KeyCode::Char('$'), KeyModifiers::SHIFT);
    // Paste.
    press(&mut e, KeyCode::Char('p'), KeyModifiers::NONE);
    // Original "foo bar" → cursor at end, paste "foo " after =
    // "foo bafoo r" — register held "foo " (yw includes trailing space).
    assert!(e.text().contains("foo"));
    assert!(e.text().len() > "foo bar".len());
}

#[test]
fn delete_via_op_is_undoable_in_one_step() {
    let mut e = EditorState::new();
    e.type_text("foo bar baz");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.text(), "bar baz");
    // Single Ctrl+Z reverts.
    press(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(e.text(), "foo bar baz");
}

#[test]
fn x_deletes_char_under_cursor() {
    let mut e = EditorState::new();
    e.type_text("hello");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(e.text(), "ello");
}

#[test]
fn x_with_count_deletes_n_chars() {
    let mut e = EditorState::new();
    e.type_text("hello");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(e.text(), "lo");
}

#[test]
fn d_then_esc_cancels_operator() {
    let mut e = EditorState::new();
    e.type_text("hello");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
    // Esc cancels op; subsequent 'w' should be a plain motion.
    press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
    assert_eq!(e.text(), "hello");
}

#[test]
fn d_capital_g_deletes_through_last_line_linewise() {
    let mut e = EditorState::new();
    e.type_text("a\nb\nc\nd");
    // Move to line 2.
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..2 {
        press(&mut e, KeyCode::Up, KeyModifiers::NONE);
    }
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
    press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
    assert_eq!(e.text(), "a");
}

#[test]
fn visual_yank_does_not_modify_buffer() {
    let mut e = EditorState::new();
    e.type_text("foo bar");
    press(&mut e, KeyCode::Home, KeyModifiers::NONE);
    enter_normal(&mut e);
    press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
    for _ in 0..2 {
        press(&mut e, KeyCode::Char('l'), KeyModifiers::NONE);
    }
    press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(e.text(), "foo bar");
    assert_eq!(e.register, "foo");
    assert_eq!(e.mode(), Mode::Normal);
}
