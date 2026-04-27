//! Shared SQL value / identifier formatting + user-input parsing for
//! the INSERT export and the cell-edit UPDATE generation.
//!
//! Quoting rules match `sql_export`'s INSERT writer (the behavior was
//! lifted here so the cell-edit `UPDATE` path can reuse it). The
//! parsing direction (user text → CellValue) lives here too because it
//! is the natural inverse of the formatting direction.

use crate::types::CellValue;

/// Wraps a Postgres identifier in double quotes, doubling any
/// embedded `"` per the standard.
pub fn quote_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Quotes a possibly-dotted target like `schema.table` by quoting
/// each component separately.
pub fn quote_dotted(target: &str) -> String {
    target
        .split('.')
        .map(quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}

/// Single-quoted string literal — embedded `'` doubled.
pub fn quote_string(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// Renders a [`CellValue`] as a SQL literal. Same rules used by both
/// INSERT export (`sql_export::write_inserts`) and UPDATE generation
/// (`format_update_one`). Bytes / Unsupported / non-finite floats fall
/// back to `NULL` because there is no portable literal we can emit.
pub fn format_value(v: &CellValue) -> String {
    match v {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(true) => "TRUE".into(),
        CellValue::Bool(false) => "FALSE".into(),
        CellValue::Int(n) => n.to_string(),
        CellValue::Float(f) if f.is_finite() => format!("{f}"),
        CellValue::Float(_) => "NULL".into(),
        CellValue::Numeric(n) => n.to_string(),
        CellValue::Text(s) => quote_string(s),
        CellValue::Date(d) => quote_string(&d.to_string()),
        CellValue::Time(t) => quote_string(&t.to_string()),
        CellValue::Timestamp(t) => quote_string(&t.to_string()),
        CellValue::TimestampTz(t) => quote_string(&t.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string()),
        CellValue::Json(s) => quote_string(s),
        CellValue::Bytes(_) => "NULL".into(),
        CellValue::Unsupported(_) => "NULL".into(),
    }
}

/// Builds `UPDATE "schema"."table" SET "col" = <val> WHERE "pk" = <pk_val>;`.
/// Identifiers double-quoted, values formatted via [`format_value`].
pub fn format_update_one(
    target: &str,
    pk_col: &str,
    pk_val: &CellValue,
    set_col: &str,
    set_val: &CellValue,
) -> String {
    format!(
        "UPDATE {} SET {} = {} WHERE {} = {};",
        quote_dotted(target),
        quote_ident(set_col),
        format_value(set_val),
        quote_ident(pk_col),
        format_value(pk_val),
    )
}

/// Parses user-typed text into a [`CellValue`] of the same kind as
/// `template`. Empty input becomes `Null`. The original CellValue's
/// variant drives the parse — the user is editing an existing typed
/// value so the type is known up front.
pub fn parse_cell_input(template: &CellValue, input: &str) -> Result<CellValue, String> {
    if input.is_empty() {
        return Ok(CellValue::Null);
    }
    match template {
        CellValue::Bool(_) => match input.to_ascii_lowercase().as_str() {
            "t" | "true" => Ok(CellValue::Bool(true)),
            "f" | "false" => Ok(CellValue::Bool(false)),
            _ => Err(format!("cannot parse '{input}' as bool (true/false/t/f)")),
        },
        CellValue::Int(_) => input
            .parse::<i64>()
            .map(CellValue::Int)
            .map_err(|_| format!("cannot parse '{input}' as integer")),
        CellValue::Float(_) => input
            .parse::<f64>()
            .map(CellValue::Float)
            .map_err(|_| format!("cannot parse '{input}' as float")),
        CellValue::Numeric(_) => input
            .parse::<rust_decimal::Decimal>()
            .map(CellValue::Numeric)
            .map_err(|_| format!("cannot parse '{input}' as numeric")),
        CellValue::Date(_) => chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
            .map(CellValue::Date)
            .map_err(|_| format!("cannot parse '{input}' as date (YYYY-MM-DD)")),
        CellValue::Time(_) => chrono::NaiveTime::parse_from_str(input, "%H:%M:%S")
            .or_else(|_| chrono::NaiveTime::parse_from_str(input, "%H:%M:%S%.f"))
            .map(CellValue::Time)
            .map_err(|_| format!("cannot parse '{input}' as time (HH:MM:SS)")),
        CellValue::Timestamp(_) => {
            chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S%.f"))
                .map(CellValue::Timestamp)
                .map_err(|_| format!("cannot parse '{input}' as timestamp (YYYY-MM-DD HH:MM:SS)"))
        }
        CellValue::TimestampTz(_) => {
            // MVP: accept naive `YYYY-MM-DD HH:MM:SS` as UTC.
            chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S")
                .map(|t| {
                    CellValue::TimestampTz(
                        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(t, chrono::Utc),
                    )
                })
                .map_err(|_| {
                    format!("cannot parse '{input}' as timestamptz (YYYY-MM-DD HH:MM:SS UTC)")
                })
        }
        CellValue::Text(_) => Ok(CellValue::Text(input.to_string())),
        CellValue::Json(_) => serde_json::from_str::<serde_json::Value>(input)
            .map(|v| CellValue::Json(v.to_string()))
            .map_err(|e| format!("invalid JSON: {e}")),
        // Null cells default to text when the user types something —
        // backend columns are typed but the local cell representation
        // doesn't carry the column type for NULLs.
        CellValue::Null => Ok(CellValue::Text(input.to_string())),
        CellValue::Bytes(_) | CellValue::Unsupported(_) => Err("type not editable".into()),
    }
}

/// `true` when `v` is a CellValue kind the cell-edit modal can edit.
/// Bytes / Unsupported are excluded — we don't have round-trippable
/// representations for those.
pub fn is_editable(v: &CellValue) -> bool {
    !matches!(v, CellValue::Bytes(_) | CellValue::Unsupported(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_doubles_embedded_quotes() {
        assert_eq!(quote_ident("plain"), "\"plain\"");
        assert_eq!(quote_ident("weird\"name"), "\"weird\"\"name\"");
    }

    #[test]
    fn quote_dotted_quotes_each_component() {
        assert_eq!(quote_dotted("public.users"), "\"public\".\"users\"");
        assert_eq!(quote_dotted("solo"), "\"solo\"");
    }

    #[test]
    fn format_update_one_quotes_idents_and_values() {
        let sql = format_update_one(
            "public.users",
            "id",
            &CellValue::Int(42),
            "name",
            &CellValue::Text("O'Hara".into()),
        );
        assert_eq!(
            sql,
            "UPDATE \"public\".\"users\" SET \"name\" = 'O''Hara' WHERE \"id\" = 42;"
        );
    }

    #[test]
    fn format_update_one_with_null_set_value() {
        let sql = format_update_one("t", "id", &CellValue::Int(1), "note", &CellValue::Null);
        assert_eq!(sql, "UPDATE \"t\" SET \"note\" = NULL WHERE \"id\" = 1;");
    }

    #[test]
    fn format_value_handles_each_supported_kind() {
        assert_eq!(format_value(&CellValue::Bool(true)), "TRUE");
        assert_eq!(format_value(&CellValue::Bool(false)), "FALSE");
        assert_eq!(format_value(&CellValue::Int(-7)), "-7");
        assert_eq!(format_value(&CellValue::Float(3.5)), "3.5");
        assert_eq!(format_value(&CellValue::Float(f64::NAN)), "NULL");
        assert_eq!(format_value(&CellValue::Bytes(8)), "NULL");
        assert_eq!(format_value(&CellValue::Null), "NULL");
        assert_eq!(format_value(&CellValue::Text("a'b".into())), "'a''b'");
        assert_eq!(format_value(&CellValue::Json("[1]".into())), "'[1]'");
    }

    #[test]
    fn parse_cell_input_round_trips_each_type() {
        // Bool
        match parse_cell_input(&CellValue::Bool(false), "true").unwrap() {
            CellValue::Bool(b) => assert!(b),
            _ => panic!(),
        }
        match parse_cell_input(&CellValue::Bool(false), "F").unwrap() {
            CellValue::Bool(b) => assert!(!b),
            _ => panic!(),
        }
        // Int
        match parse_cell_input(&CellValue::Int(0), "42").unwrap() {
            CellValue::Int(n) => assert_eq!(n, 42),
            _ => panic!(),
        }
        // Numeric
        match parse_cell_input(&CellValue::Numeric(rust_decimal::Decimal::ZERO), "3.14").unwrap() {
            CellValue::Numeric(d) => assert_eq!(d.to_string(), "3.14"),
            _ => panic!(),
        }
        // Text
        match parse_cell_input(&CellValue::Text(String::new()), "hello").unwrap() {
            CellValue::Text(s) => assert_eq!(s, "hello"),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_cell_input_empty_becomes_null() {
        match parse_cell_input(&CellValue::Int(0), "").unwrap() {
            CellValue::Null => {}
            _ => panic!(),
        }
    }

    #[test]
    fn parse_cell_input_rejects_invalid_int() {
        let err = parse_cell_input(&CellValue::Int(0), "abc").unwrap_err();
        assert!(err.contains("integer"));
    }

    #[test]
    fn parse_cell_input_rejects_invalid_json() {
        let err = parse_cell_input(&CellValue::Json(String::new()), "not json").unwrap_err();
        assert!(err.to_lowercase().contains("json"));
    }

    #[test]
    fn parse_cell_input_rejects_uneditable_template() {
        assert!(parse_cell_input(&CellValue::Bytes(0), "anything").is_err());
        assert!(parse_cell_input(&CellValue::Unsupported("inet".into()), "1.2.3.4").is_err());
    }

    #[test]
    fn is_editable_excludes_bytes_and_unsupported() {
        assert!(is_editable(&CellValue::Int(0)));
        assert!(is_editable(&CellValue::Text(String::new())));
        assert!(is_editable(&CellValue::Null));
        assert!(!is_editable(&CellValue::Bytes(8)));
        assert!(!is_editable(&CellValue::Unsupported("inet".into())));
    }
}
