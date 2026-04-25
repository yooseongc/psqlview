//! Writes a [`ResultSet`] to an `io::Write` as RFC 4180 CSV.
//!
//! NULL cells render as empty fields (matches psql's default).
//! Everything else uses `CellValue`'s `Display` impl, then quotes the
//! field if it contains a comma, double quote, newline, or carriage
//! return — embedded `"` becomes `""` per the RFC.

use std::io::{self, Write};

use crate::types::{CellValue, ResultSet};

/// Writes `rs` as CSV to `w`. Fields are comma-separated, rows are LF-
/// terminated (CRLF would break round-tripping through the editor's
/// CRLF normalization on Open).
pub fn write_csv<W: Write>(rs: &ResultSet, w: &mut W) -> io::Result<()> {
    let header: Vec<String> = rs.columns.iter().map(|c| escape(&c.name)).collect();
    writeln!(w, "{}", header.join(","))?;

    for row in &rs.rows {
        let cells: Vec<String> = row.iter().map(format_cell).collect();
        writeln!(w, "{}", cells.join(","))?;
    }
    Ok(())
}

fn format_cell(v: &CellValue) -> String {
    match v {
        CellValue::Null => String::new(),
        other => escape(&other.to_string()),
    }
}

fn escape(s: &str) -> String {
    let needs_quote = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs_quote {
        return s.to_string();
    }
    let escaped = s.replace('"', "\"\"");
    format!("\"{escaped}\"")
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

    fn write_to_string(rs: &ResultSet) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_csv(rs, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn header_row_then_data_rows() {
        let rs = rs_for(
            vec![
                vec![CellValue::Int(1), CellValue::Text("hi".into())],
                vec![CellValue::Int(2), CellValue::Text("bye".into())],
            ],
            vec!["id", "msg"],
        );
        assert_eq!(write_to_string(&rs), "id,msg\n1,hi\n2,bye\n");
    }

    #[test]
    fn null_renders_as_empty_field() {
        let rs = rs_for(
            vec![vec![CellValue::Null, CellValue::Text("x".into())]],
            vec!["a", "b"],
        );
        assert_eq!(write_to_string(&rs), "a,b\n,x\n");
    }

    #[test]
    fn fields_with_commas_or_quotes_get_quoted() {
        let rs = rs_for(
            vec![vec![
                CellValue::Text("a,b".into()),
                CellValue::Text("she said \"hi\"".into()),
            ]],
            vec!["c1", "c2"],
        );
        assert_eq!(
            write_to_string(&rs),
            "c1,c2\n\"a,b\",\"she said \"\"hi\"\"\"\n"
        );
    }

    #[test]
    fn fields_with_newlines_get_quoted() {
        let rs = rs_for(
            vec![vec![CellValue::Text("line1\nline2".into())]],
            vec!["c"],
        );
        assert_eq!(write_to_string(&rs), "c\n\"line1\nline2\"\n");
    }

    #[test]
    fn header_with_special_chars_is_quoted_too() {
        let rs = rs_for(vec![], vec!["a,b"]);
        assert_eq!(write_to_string(&rs), "\"a,b\"\n");
    }

    #[test]
    fn empty_result_emits_only_header() {
        let rs = rs_for(vec![], vec!["x", "y"]);
        assert_eq!(write_to_string(&rs), "x,y\n");
    }
}
