//! Writes a [`ResultSet`] to an `io::Write` as JSON.
//!
//! Two flavors:
//! * `write_json_lines` — one JSON object per row, LF-terminated.
//!   Easy to pipe through `jq` (`jq -s .` re-collects to an array).
//! * `write_json_pretty` — a single top-level array with one row
//!   per element and 2-space indentation.
//!
//! Cell rendering rules:
//! * `Null` → `null`.
//! * `Bool` → JSON bool.
//! * `Int` / `Float` (finite) → JSON number.
//! * `Float` (NaN / ±∞) → `null` (JSON has no representation).
//! * `Text` / `Numeric` / `Date` / `Time` / `Timestamp(Tz)` →
//!   JSON string. Numeric becomes a string so its precision survives
//!   round-tripping; JSON numbers can lose digits.
//! * `Json` — emitted as-is (the value is already valid JSON).
//! * `Bytes` / `Unsupported` → string fallback so the output stays
//!   valid even when we don't have the underlying value.

use std::io::{self, Write};

use crate::types::{CellValue, ResultSet};

/// Writes `rs` as JSON Lines (one object per row, LF-terminated).
pub fn write_json_lines<W: Write>(rs: &ResultSet, w: &mut W) -> io::Result<()> {
    for row in &rs.rows {
        let mut line = String::from("{");
        write_object_body(&mut line, rs, row);
        line.push('}');
        writeln!(w, "{}", line)?;
    }
    Ok(())
}

/// Writes `rs` as a pretty-printed JSON array. Two-space indent;
/// one row per line; trailing LF after the closing bracket.
pub fn write_json_pretty<W: Write>(rs: &ResultSet, w: &mut W) -> io::Result<()> {
    if rs.rows.is_empty() {
        writeln!(w, "[]")?;
        return Ok(());
    }
    writeln!(w, "[")?;
    for (i, row) in rs.rows.iter().enumerate() {
        let mut line = String::from("  {");
        write_object_body(&mut line, rs, row);
        line.push('}');
        if i + 1 < rs.rows.len() {
            line.push(',');
        }
        writeln!(w, "{}", line)?;
    }
    writeln!(w, "]")?;
    Ok(())
}

fn write_object_body(out: &mut String, rs: &ResultSet, row: &[CellValue]) {
    for (i, col) in rs.columns.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_quoted_string(out, &col.name);
        out.push_str(": ");
        write_cell(out, row.get(i));
    }
}

fn write_cell(out: &mut String, cell: Option<&CellValue>) {
    match cell {
        None | Some(CellValue::Null) => out.push_str("null"),
        Some(CellValue::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
        Some(CellValue::Int(n)) => out.push_str(&n.to_string()),
        Some(CellValue::Float(f)) => {
            if f.is_finite() {
                out.push_str(&format!("{}", f));
            } else {
                out.push_str("null");
            }
        }
        Some(CellValue::Text(s)) => write_quoted_string(out, s),
        Some(CellValue::Numeric(n)) => write_quoted_string(out, &n.to_string()),
        Some(CellValue::Date(d)) => write_quoted_string(out, &d.to_string()),
        Some(CellValue::Time(t)) => write_quoted_string(out, &t.to_string()),
        Some(CellValue::Timestamp(t)) => write_quoted_string(out, &t.to_string()),
        Some(CellValue::TimestampTz(t)) => {
            write_quoted_string(out, &t.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string())
        }
        // `Json` is already a valid JSON value — inline it raw. Trim
        // trailing whitespace so a server-emitted "{...}\n" doesn't
        // break the surrounding format.
        Some(CellValue::Json(raw)) => out.push_str(raw.trim_end()),
        Some(CellValue::Bytes(n)) => write_quoted_string(out, &format!("<{n} bytes>")),
        Some(CellValue::Unsupported(name)) => write_quoted_string(out, &format!("<{name}>")),
    }
}

fn write_quoted_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ColumnMeta;

    fn col(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            type_name: "text".into(),
        }
    }

    fn rs_for(rows: Vec<Vec<CellValue>>, cols: Vec<&str>) -> ResultSet {
        ResultSet {
            columns: cols.into_iter().map(col).collect(),
            rows,
            ..ResultSet::default()
        }
    }

    fn lines_to_string(rs: &ResultSet) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_json_lines(rs, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn pretty_to_string(rs: &ResultSet) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_json_pretty(rs, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn json_lines_writes_one_object_per_row() {
        let rs = rs_for(
            vec![
                vec![CellValue::Int(1), CellValue::Text("hi".into())],
                vec![CellValue::Int(2), CellValue::Text("bye".into())],
            ],
            vec!["id", "msg"],
        );
        assert_eq!(
            lines_to_string(&rs),
            "{\"id\": 1, \"msg\": \"hi\"}\n{\"id\": 2, \"msg\": \"bye\"}\n"
        );
    }

    #[test]
    fn null_renders_as_json_null() {
        let rs = rs_for(
            vec![vec![CellValue::Null, CellValue::Text("x".into())]],
            vec!["a", "b"],
        );
        assert_eq!(lines_to_string(&rs), "{\"a\": null, \"b\": \"x\"}\n");
    }

    #[test]
    fn bool_int_float_render_as_json_primitives() {
        let rs = rs_for(
            vec![vec![
                CellValue::Bool(true),
                CellValue::Int(-7),
                CellValue::Float(1.5),
            ]],
            vec!["b", "i", "f"],
        );
        assert_eq!(
            lines_to_string(&rs),
            "{\"b\": true, \"i\": -7, \"f\": 1.5}\n"
        );
    }

    #[test]
    fn float_nan_falls_back_to_null() {
        let rs = rs_for(vec![vec![CellValue::Float(f64::NAN)]], vec!["f"]);
        assert_eq!(lines_to_string(&rs), "{\"f\": null}\n");
    }

    #[test]
    fn strings_are_escaped() {
        let rs = rs_for(
            vec![vec![CellValue::Text("she said \"hi\"\nline2".into())]],
            vec!["t"],
        );
        assert_eq!(
            lines_to_string(&rs),
            "{\"t\": \"she said \\\"hi\\\"\\nline2\"}\n"
        );
    }

    #[test]
    fn json_cell_inlined_raw() {
        let rs = rs_for(vec![vec![CellValue::Json("[1,2,3]".into())]], vec!["arr"]);
        assert_eq!(lines_to_string(&rs), "{\"arr\": [1,2,3]}\n");
    }

    #[test]
    fn pretty_emits_array_with_indent() {
        let rs = rs_for(
            vec![
                vec![CellValue::Int(1), CellValue::Text("a".into())],
                vec![CellValue::Int(2), CellValue::Text("b".into())],
            ],
            vec!["id", "v"],
        );
        assert_eq!(
            pretty_to_string(&rs),
            "[\n  {\"id\": 1, \"v\": \"a\"},\n  {\"id\": 2, \"v\": \"b\"}\n]\n"
        );
    }

    #[test]
    fn pretty_empty_emits_empty_array() {
        let rs = rs_for(vec![], vec!["a"]);
        assert_eq!(pretty_to_string(&rs), "[]\n");
    }

    #[test]
    fn control_chars_escape_to_unicode() {
        let rs = rs_for(vec![vec![CellValue::Text("\x01".into())]], vec!["c"]);
        assert_eq!(lines_to_string(&rs), "{\"c\": \"\\u0001\"}\n");
    }
}
