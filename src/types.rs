use std::fmt;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
}

impl SslMode {
    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SslMode::Disable => SslMode::Prefer,
            SslMode::Prefer => SslMode::Require,
            SslMode::Require => SslMode::Disable,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone)]
pub enum CellValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Numeric(Decimal),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(NaiveDateTime),
    TimestampTz(DateTime<Utc>),
    Json(String),
    Bytes(usize),
    Unsupported(String),
}

impl fmt::Display for CellValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellValue::Null => f.write_str("NULL"),
            CellValue::Bool(v) => write!(f, "{v}"),
            CellValue::Int(v) => write!(f, "{v}"),
            CellValue::Float(v) => write!(f, "{v}"),
            CellValue::Text(v) => f.write_str(v),
            CellValue::Numeric(v) => write!(f, "{v}"),
            CellValue::Date(v) => write!(f, "{v}"),
            CellValue::Time(v) => write!(f, "{v}"),
            CellValue::Timestamp(v) => write!(f, "{v}"),
            CellValue::TimestampTz(v) => write!(f, "{}", v.format("%Y-%m-%d %H:%M:%S%.fZ")),
            CellValue::Json(v) => f.write_str(v),
            CellValue::Bytes(n) => write!(f, "<{n} bytes>"),
            CellValue::Unsupported(name) => write!(f, "<{name}>"),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<CellValue>>,
    pub truncated_at: Option<usize>,
    pub command_tag: Option<String>,
    pub elapsed_ms: u128,
}

impl ResultSet {
    pub fn empty_with_tag(tag: impl Into<String>, elapsed_ms: u128) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated_at: None,
            command_tag: Some(tag.into()),
            elapsed_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerVersion {
    /// Pre-14 servers may still work but are not officially supported.
    Legacy(u32),
    Supported(u32),
}

impl ServerVersion {
    pub fn from_num(num: u32) -> Self {
        if num >= 140000 {
            ServerVersion::Supported(num)
        } else {
            ServerVersion::Legacy(num)
        }
    }

    pub fn is_supported(self) -> bool {
        matches!(self, ServerVersion::Supported(_))
    }

    pub fn display(self) -> String {
        let num = match self {
            ServerVersion::Legacy(n) | ServerVersion::Supported(n) => n,
        };
        let major = num / 10000;
        let minor = (num % 10000) / 100;
        let patch = num % 100;
        if major >= 10 {
            format!("{major}.{patch}")
        } else {
            format!("{major}.{minor}.{patch}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_mode_cycles() {
        assert_eq!(SslMode::Disable.next(), SslMode::Prefer);
        assert_eq!(SslMode::Prefer.next(), SslMode::Require);
        assert_eq!(SslMode::Require.next(), SslMode::Disable);
    }

    #[test]
    fn server_version_classifies_14_plus_as_supported() {
        assert!(ServerVersion::from_num(140000).is_supported());
        assert!(ServerVersion::from_num(170002).is_supported());
        assert!(!ServerVersion::from_num(130014).is_supported());
    }

    #[test]
    fn server_version_display_matches_modern_scheme() {
        assert_eq!(ServerVersion::from_num(140005).display(), "14.5");
        assert_eq!(ServerVersion::from_num(170000).display(), "17.0");
    }

    #[test]
    fn cell_value_display_covers_every_variant() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let time = NaiveTime::from_hms_opt(12, 34, 56).unwrap();
        let ts = NaiveDateTime::new(date, time);
        let tstz = DateTime::<Utc>::from_naive_utc_and_offset(ts, Utc);

        assert_eq!(CellValue::Null.to_string(), "NULL");
        assert_eq!(CellValue::Bool(true).to_string(), "true");
        assert_eq!(CellValue::Bool(false).to_string(), "false");
        assert_eq!(CellValue::Int(-7).to_string(), "-7");
        assert_eq!(CellValue::Float(3.5).to_string(), "3.5");
        assert_eq!(CellValue::Text("hi".into()).to_string(), "hi");
        assert_eq!(
            CellValue::Numeric(Decimal::new(12345, 2)).to_string(),
            "123.45"
        );
        assert_eq!(CellValue::Date(date).to_string(), "2026-04-22");
        assert_eq!(CellValue::Time(time).to_string(), "12:34:56");
        assert_eq!(CellValue::Timestamp(ts).to_string(), "2026-04-22 12:34:56");
        assert_eq!(
            CellValue::TimestampTz(tstz).to_string(),
            "2026-04-22 12:34:56Z"
        );
        assert_eq!(CellValue::Json("[1,2]".into()).to_string(), "[1,2]");
        assert_eq!(CellValue::Bytes(1024).to_string(), "<1024 bytes>");
        assert_eq!(CellValue::Unsupported("inet".into()).to_string(), "<inet>");
    }
}
