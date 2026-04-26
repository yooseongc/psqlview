use super::explain::parse_actual_total_ms;
use super::render::{truncate_for_cell, MAX_CELL_WIDTH, MIN_CELL_WIDTH};
use super::*;
use crate::types::{CellValue, ColumnMeta, ResultSet};
use crossterm::event::KeyModifiers;
use unicode_width::UnicodeWidthStr;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn sample_result() -> ResultSet {
    ResultSet {
        columns: vec![
            ColumnMeta {
                name: "a".into(),
                type_name: "int4".into(),
            },
            ColumnMeta {
                name: "b".into(),
                type_name: "text".into(),
            },
        ],
        rows: vec![
            vec![CellValue::Int(1), CellValue::Text("x".into())],
            vec![CellValue::Int(2), CellValue::Text("y".into())],
            vec![CellValue::Int(3), CellValue::Text("z".into())],
        ],
        truncated_at: None,
        command_tag: Some("3 rows".into()),
        elapsed_ms: 1,
    }
}

#[test]
fn compute_widths_respects_bounds() {
    let cols = vec![
        ColumnMeta {
            name: "a".into(),
            type_name: "int4".into(),
        },
        ColumnMeta {
            name: "verbose_column_header_should_be_capped_to_40".into(),
            type_name: "text".into(),
        },
    ];
    let rows = vec![vec![CellValue::Int(123), CellValue::Text("x".into())]];
    let widths = compute_widths(&cols, &rows, 0, 2);
    // MIN_CELL_WIDTH=4 (for "a"), MAX_CELL_WIDTH=40 (for long header)
    assert_eq!(widths.len(), 2);
}

#[test]
fn truncate_keeps_short_strings() {
    assert_eq!(truncate_for_cell("hi"), "hi");
    let s = "x".repeat(100);
    let t = truncate_for_cell(&s);
    assert!(t.ends_with('…'));
    assert!(UnicodeWidthStr::width(t.as_str()) <= MAX_CELL_WIDTH as usize);
}

#[test]
fn handle_key_is_safe_on_empty_state() {
    let mut s = ResultsState::default();
    for code in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Char('h'),
        KeyCode::Char('l'),
    ] {
        s.handle_key(key(code));
    }
    assert_eq!(s.selected_row, 0);
    assert_eq!(s.x_offset, 0);
    assert!(s.current.is_none());
}

#[test]
fn handle_key_respects_row_and_col_bounds() {
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    // PageUp/PageDown now step by visible_rows, which is set by draw.
    // Simulate a "screenful" large enough to overshoot the 3-row sample.
    s.visible_rows = 20;

    for _ in 0..10 {
        s.handle_key(key(KeyCode::Down));
    }
    assert_eq!(s.selected_row, 2);

    s.handle_key(key(KeyCode::Home));
    assert_eq!(s.selected_row, 0);

    s.handle_key(key(KeyCode::PageDown));
    assert_eq!(s.selected_row, 2); // capped at max

    s.handle_key(key(KeyCode::End));
    assert_eq!(s.selected_row, 2);

    s.handle_key(key(KeyCode::PageUp));
    assert_eq!(s.selected_row, 0);

    for _ in 0..10 {
        s.handle_key(key(KeyCode::Right));
    }
    assert_eq!(s.x_offset, 1); // col_count - 1

    for _ in 0..10 {
        s.handle_key(key(KeyCode::Left));
    }
    assert_eq!(s.x_offset, 0);
}

fn result_with_rows(n: usize) -> ResultSet {
    let rows: Vec<Vec<CellValue>> = (0..n).map(|i| vec![CellValue::Int(i as i64)]).collect();
    ResultSet {
        columns: vec![ColumnMeta {
            name: "a".into(),
            type_name: "int4".into(),
        }],
        rows,
        truncated_at: None,
        command_tag: Some(format!("{n} rows")),
        elapsed_ms: 1,
    }
}

#[test]
fn page_down_uses_visible_rows_step() {
    let mut s = ResultsState::default();
    s.set_result(result_with_rows(100));
    s.visible_rows = 10;
    s.handle_key(key(KeyCode::PageDown));
    assert_eq!(s.selected_row, 10);
    s.handle_key(key(KeyCode::PageDown));
    assert_eq!(s.selected_row, 20);
}

#[test]
fn page_up_clamps_to_zero() {
    let mut s = ResultsState::default();
    s.set_result(result_with_rows(100));
    s.visible_rows = 25;
    s.selected_row = 5;
    s.handle_key(key(KeyCode::PageUp));
    assert_eq!(s.selected_row, 0);
}

#[test]
fn extract_explain_detects_query_plan_column() {
    let set = ResultSet {
        columns: vec![ColumnMeta {
            name: "QUERY PLAN".into(),
            type_name: "text".into(),
        }],
        rows: vec![
            vec![CellValue::Text(
                "Seq Scan on t  (cost=0.00..1.00 rows=1)".into(),
            )],
            vec![CellValue::Text("Planning Time: 0.1 ms".into())],
        ],
        truncated_at: None,
        command_tag: Some("2 rows".into()),
        elapsed_ms: 1,
    };
    let mut s = ResultsState::default();
    s.set_result(set);
    let lines = s.explain_lines.as_ref().expect("explain detected");
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("Seq Scan"));
}

#[test]
fn extract_explain_ignores_normal_results() {
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    assert!(s.explain_lines.is_none());
}

#[test]
fn parse_actual_total_ms_extracts_upper_bound() {
    assert_eq!(
        parse_actual_total_ms("  (actual time=0.013..0.014 rows=3 loops=1)"),
        Some(0.014)
    );
    assert_eq!(
        parse_actual_total_ms("  (actual time=10..123.4 rows=99)"),
        Some(123.4)
    );
    assert_eq!(parse_actual_total_ms("  (cost=0..1 rows=10)"), None);
}

#[test]
fn sort_cycles_asc_desc_off() {
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    // Three rows: ints 1, 2, 3 in column a.
    s.cycle_sort_on_current_column();
    let cur = s.current.as_ref().unwrap();
    assert!(matches!(cur.rows[0][0], CellValue::Int(1)));
    assert_eq!(s.sort.unwrap().dir, SortDir::Asc);
    s.cycle_sort_on_current_column();
    let cur = s.current.as_ref().unwrap();
    assert!(matches!(cur.rows[0][0], CellValue::Int(3)));
    assert_eq!(s.sort.unwrap().dir, SortDir::Desc);
    s.cycle_sort_on_current_column();
    let cur = s.current.as_ref().unwrap();
    // Off → restored to original (1, 2, 3).
    assert!(matches!(cur.rows[0][0], CellValue::Int(1)));
    assert!(s.sort.is_none());
}

#[test]
fn sort_on_different_column_starts_asc() {
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    s.cycle_sort_on_current_column();
    s.cycle_sort_on_current_column(); // Desc on col 0
    s.x_offset = 1;
    s.cycle_sort_on_current_column(); // Asc on col 1
    let st = s.sort.unwrap();
    assert_eq!(st.col, 1);
    assert_eq!(st.dir, SortDir::Asc);
}

#[test]
fn ctrl_left_jumps_to_first_column() {
    use crossterm::event::KeyModifiers;
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    s.x_offset = 1;
    s.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(s.x_offset, 0);
}

#[test]
fn ctrl_right_jumps_to_last_column() {
    use crossterm::event::KeyModifiers;
    let mut s = ResultsState::default();
    s.set_result(sample_result());
    s.x_offset = 0;
    s.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    // sample_result has 2 columns; last index is 1.
    assert_eq!(s.x_offset, 1);
}

#[test]
fn page_down_falls_back_to_single_step_when_visible_rows_unset() {
    let mut s = ResultsState::default();
    s.set_result(result_with_rows(5));
    // visible_rows defaults to 0 — step is clamped to 1.
    s.handle_key(key(KeyCode::PageDown));
    assert_eq!(s.selected_row, 1);
}

#[test]
fn compute_widths_handles_offset_slice() {
    let cols = vec![
        ColumnMeta {
            name: "a".into(),
            type_name: "int".into(),
        },
        ColumnMeta {
            name: "b".into(),
            type_name: "int".into(),
        },
        ColumnMeta {
            name: "c".into(),
            type_name: "int".into(),
        },
    ];
    let rows = vec![vec![
        CellValue::Int(1),
        CellValue::Int(2),
        CellValue::Int(3),
    ]];
    let widths = compute_widths(&cols, &rows, 1, 2);
    assert_eq!(widths.len(), 2);
    for w in widths {
        // Each Constraint::Min(w) should have w in [MIN_CELL_WIDTH, MAX_CELL_WIDTH].
        match w {
            ratatui::layout::Constraint::Min(n) => {
                assert!((MIN_CELL_WIDTH..=MAX_CELL_WIDTH).contains(&n));
            }
            other => panic!("unexpected constraint: {other:?}"),
        }
    }
}
