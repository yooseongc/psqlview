//! Writes a [`ResultSet`] to an `io::Write` as a sequence of SQL
//! `INSERT` statements — one row per line, semicolon-terminated,
//! ready to feed back into `psql` or another PostgreSQL client.
//!
//! Target table comes from the export path's `file_stem` so the
//! whole feature stays zero-config: `:w foo.sql` → target
//! `foo`; `:w public.users.sql` → target `public.users` (each
//! component is double-quoted so reserved words / special chars
//! survive). No `serde` / no SQL parser — strings are escaped by
//! doubling embedded single quotes, identifiers by doubling
//! embedded double quotes.
//!
//! Cell rendering rules:
//! * `Null` → `NULL`.
//! * `Bool` → `TRUE` / `FALSE`.
//! * `Int` / `Float` (finite) / `Numeric` → bare literal.
//! * `Float` (NaN / ±∞) → `NULL` (no portable literal).
//! * `Text` / `Date` / `Time` / `Timestamp(Tz)` / `Json` →
//!   single-quoted string (embedded `'` doubled, ISO 8601 for
//!   timestamps).
//! * `Bytes` / `Unsupported` → `NULL` (we don't have the raw
//!   bytes; emitting a placeholder would corrupt the row).

use std::io::{self, Write};

use super::sql_format::{format_value, quote_dotted, quote_ident};
use crate::types::ResultSet;

/// Writes `rs` as one `INSERT INTO target (...) VALUES (...);` per
/// row.
pub fn write_inserts<W: Write>(rs: &ResultSet, target: &str, w: &mut W) -> io::Result<()> {
    let target_quoted = quote_dotted(target);
    let cols_quoted: Vec<String> = rs.columns.iter().map(|c| quote_ident(&c.name)).collect();
    let cols_clause = cols_quoted.join(", ");

    for row in &rs.rows {
        let values: Vec<String> = row.iter().map(format_value).collect();
        let values_clause = values.join(", ");
        writeln!(
            w,
            "INSERT INTO {target_quoted} ({cols_clause}) VALUES ({values_clause});"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CellValue, ColumnMeta};

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

    fn write_to_string(rs: &ResultSet, target: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_inserts(rs, target, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn one_insert_per_row_with_quoted_idents() {
        let rs = rs_for(
            vec![
                vec![CellValue::Int(1), CellValue::Text("hi".into())],
                vec![CellValue::Int(2), CellValue::Text("bye".into())],
            ],
            vec!["id", "msg"],
        );
        assert_eq!(
            write_to_string(&rs, "users"),
            "INSERT INTO \"users\" (\"id\", \"msg\") VALUES (1, 'hi');\n\
             INSERT INTO \"users\" (\"id\", \"msg\") VALUES (2, 'bye');\n"
        );
    }

    #[test]
    fn dotted_target_quotes_each_component() {
        let rs = rs_for(vec![vec![CellValue::Int(1)]], vec!["id"]);
        let out = write_to_string(&rs, "public.users");
        assert!(out.starts_with("INSERT INTO \"public\".\"users\" (\"id\")"));
    }

    #[test]
    fn null_renders_as_sql_null() {
        let rs = rs_for(
            vec![vec![CellValue::Null, CellValue::Text("x".into())]],
            vec!["a", "b"],
        );
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"a\", \"b\") VALUES (NULL, 'x');\n"
        );
    }

    #[test]
    fn bool_renders_as_true_false() {
        let rs = rs_for(
            vec![vec![CellValue::Bool(true), CellValue::Bool(false)]],
            vec!["a", "b"],
        );
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"a\", \"b\") VALUES (TRUE, FALSE);\n"
        );
    }

    #[test]
    fn embedded_single_quote_is_doubled() {
        let rs = rs_for(vec![vec![CellValue::Text("don't".into())]], vec!["msg"]);
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"msg\") VALUES ('don''t');\n"
        );
    }

    #[test]
    fn column_name_with_double_quote_is_doubled() {
        let rs = rs_for(vec![vec![CellValue::Int(1)]], vec!["weird\"col"]);
        let out = write_to_string(&rs, "t");
        assert!(out.contains("(\"weird\"\"col\")"));
    }

    #[test]
    fn float_nan_falls_back_to_null() {
        let rs = rs_for(vec![vec![CellValue::Float(f64::NAN)]], vec!["f"]);
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"f\") VALUES (NULL);\n"
        );
    }

    #[test]
    fn json_cell_is_single_quoted() {
        let rs = rs_for(vec![vec![CellValue::Json("[1,2,3]".into())]], vec!["arr"]);
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"arr\") VALUES ('[1,2,3]');\n"
        );
    }

    #[test]
    fn bytes_renders_as_null() {
        let rs = rs_for(vec![vec![CellValue::Bytes(42)]], vec!["b"]);
        assert_eq!(
            write_to_string(&rs, "t"),
            "INSERT INTO \"t\" (\"b\") VALUES (NULL);\n"
        );
    }

    #[test]
    fn empty_result_emits_no_lines() {
        let rs = rs_for(vec![], vec!["x"]);
        assert_eq!(write_to_string(&rs, "t"), "");
    }
}
