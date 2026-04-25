//! Incremental SQL tokenizer for the editor's syntax highlighter.
//!
//! Designed for partial input — incomplete strings and unclosed block
//! comments are reported as their respective token kinds, so the
//! highlighter can keep coloring the buffer correctly while the user is
//! mid-typing.
//!
//! Multi-line constructs (block comments, string literals) carry their
//! state across lines via `LexState`; the renderer threads a single
//! `LexState` from the top of the buffer down through every visible
//! line so that a `'string'` opened on row 5 still colors the body of
//! row 6 correctly.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    QuotedIdent,
    Number,
    StringLit,
    BlockComment,
    LineComment,
    Operator,
    Whitespace,
}

#[derive(Debug, Clone, Copy)]
pub struct Token {
    pub kind: TokenKind,
    /// Start column in chars (not bytes).
    pub start_col: usize,
    /// Length in chars.
    pub len: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LexState {
    #[default]
    Normal,
    InBlockComment,
    InString,
}

/// Tokenizes a single line, advancing `state` as multi-line constructs
/// open or close. Tokens are emitted with char-based offsets (`start_col`,
/// `len`) so the renderer can splice them with `chars().nth()`-style
/// indexing.
pub fn tokenize_line(line: &str, state: &mut LexState) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    // First, finish any multi-line construct that was open at end of
    // the previous line.
    match *state {
        LexState::InBlockComment => {
            let start = i;
            while i < n {
                if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    i += 2;
                    *state = LexState::Normal;
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::BlockComment, start, i - start);
        }
        LexState::InString => {
            let start = i;
            while i < n {
                if chars[i] == '\'' {
                    if i + 1 < n && chars[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    *state = LexState::Normal;
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::StringLit, start, i - start);
        }
        LexState::Normal => {}
    }

    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            push_token(&mut tokens, TokenKind::Whitespace, start, i - start);
        } else if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            // Line comment to end of line.
            let start = i;
            i = n;
            push_token(&mut tokens, TokenKind::LineComment, start, i - start);
        } else if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            *state = LexState::InBlockComment;
            while i < n {
                if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    i += 2;
                    *state = LexState::Normal;
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::BlockComment, start, i - start);
        } else if c == '\'' {
            let start = i;
            i += 1;
            *state = LexState::InString;
            while i < n {
                if chars[i] == '\'' {
                    if i + 1 < n && chars[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    *state = LexState::Normal;
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::StringLit, start, i - start);
        } else if c == '"' {
            // Quoted identifier; doesn't carry state (enforce single-line
            // for simplicity — PG technically allows multi-line "..." but
            // it's exceedingly rare).
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '"' {
                    if i + 1 < n && chars[i + 1] == '"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::QuotedIdent, start, i - start);
        } else if c.is_ascii_digit() {
            let start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            // Decimal point + fractional digits.
            if i < n && chars[i] == '.' && i + 1 < n && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // Optional exponent.
            if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                i += 1;
                if i < n && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            push_token(&mut tokens, TokenKind::Number, start, i - start);
        } else if c.is_alphabetic() || c == '_' {
            // PG accepts Unicode identifiers (e.g. Hangul, CJK).
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if is_keyword(&word) {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            push_token(&mut tokens, kind, start, i - start);
        } else {
            // Punctuation / operator.
            let start = i;
            i += 1;
            while i < n {
                let nc = chars[i];
                if nc.is_alphanumeric()
                    || nc.is_whitespace()
                    || nc == '_'
                    || nc == '\''
                    || nc == '"'
                {
                    break;
                }
                if nc == '-' && i + 1 < n && chars[i + 1] == '-' {
                    break;
                }
                if nc == '/' && i + 1 < n && chars[i + 1] == '*' {
                    break;
                }
                i += 1;
            }
            push_token(&mut tokens, TokenKind::Operator, start, i - start);
        }
    }
    tokens
}

fn push_token(out: &mut Vec<Token>, kind: TokenKind, start_col: usize, len: usize) {
    if len == 0 {
        return;
    }
    out.push(Token {
        kind,
        start_col,
        len,
    });
}

fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    SQL_KEYWORDS.binary_search(&upper.as_str()).is_ok()
}

/// Sorted (binary searchable) list of SQL keywords we color. Sorted ASCII
/// so `is_keyword` can use `binary_search`. Mirrors
/// [`crate::ui::autocomplete::SQL_KEYWORDS`] in coverage; the autocomplete
/// list is the canonical user-facing reference, this one is duplicated
/// here for syntax highlighting only.
const SQL_KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "AS",
    "BETWEEN",
    "BY",
    "CASE",
    "COLUMN",
    "CREATE",
    "CROSS",
    "DELETE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXPLAIN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "RETURNING",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "UNION",
    "UPDATE",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_one(line: &str) -> Vec<Token> {
        let mut state = LexState::default();
        tokenize_line(line, &mut state)
    }

    fn kinds(line: &str) -> Vec<TokenKind> {
        lex_one(line).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn keyword_is_recognized_case_insensitively() {
        assert_eq!(kinds("SELECT"), vec![TokenKind::Keyword]);
        assert_eq!(kinds("select"), vec![TokenKind::Keyword]);
        assert_eq!(kinds("Select"), vec![TokenKind::Keyword]);
    }

    #[test]
    fn identifier_distinct_from_keyword() {
        assert_eq!(kinds("foo"), vec![TokenKind::Identifier]);
        assert_eq!(
            kinds("foo_bar"),
            vec![TokenKind::Identifier],
            "underscore is part of ident"
        );
        assert_eq!(
            kinds("foo$"),
            vec![TokenKind::Identifier],
            "PG allows $ in idents"
        );
    }

    #[test]
    fn number_handles_int_float_exponent() {
        assert_eq!(kinds("123"), vec![TokenKind::Number]);
        assert_eq!(kinds("3.14"), vec![TokenKind::Number]);
        assert_eq!(kinds("1e10"), vec![TokenKind::Number]);
        assert_eq!(kinds("1.5e-2"), vec![TokenKind::Number]);
    }

    #[test]
    fn string_literal_with_escaped_quote() {
        assert_eq!(kinds("'a''b'"), vec![TokenKind::StringLit]);
    }

    #[test]
    fn quoted_identifier_with_escaped_quote() {
        assert_eq!(kinds(r#""User""Names""#), vec![TokenKind::QuotedIdent]);
    }

    #[test]
    fn line_comment_runs_to_end_of_line() {
        let toks = lex_one("SELECT -- comment");
        assert_eq!(
            toks.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![
                TokenKind::Keyword,
                TokenKind::Whitespace,
                TokenKind::LineComment
            ]
        );
        assert_eq!(toks.last().unwrap().len, "-- comment".chars().count());
    }

    #[test]
    fn block_comment_within_one_line() {
        assert_eq!(
            kinds("/* hi */"),
            vec![TokenKind::BlockComment],
            "single-line block comment"
        );
    }

    #[test]
    fn block_comment_carries_to_next_line() {
        let mut state = LexState::default();
        let toks1 = tokenize_line("/* still", &mut state);
        assert_eq!(state, LexState::InBlockComment);
        assert!(toks1.iter().all(|t| t.kind == TokenKind::BlockComment));

        let toks2 = tokenize_line("open */ SELECT", &mut state);
        assert_eq!(state, LexState::Normal);
        let kinds2: Vec<_> = toks2.iter().map(|t| t.kind).collect();
        assert_eq!(kinds2[0], TokenKind::BlockComment);
        assert!(kinds2.contains(&TokenKind::Keyword));
    }

    #[test]
    fn string_carries_to_next_line() {
        let mut state = LexState::default();
        let toks1 = tokenize_line("'multi", &mut state);
        assert_eq!(state, LexState::InString);
        assert!(toks1.iter().all(|t| t.kind == TokenKind::StringLit));
        let toks2 = tokenize_line("line' SELECT", &mut state);
        assert_eq!(state, LexState::Normal);
        let kinds2: Vec<_> = toks2.iter().map(|t| t.kind).collect();
        assert!(matches!(kinds2[0], TokenKind::StringLit));
        assert!(kinds2.contains(&TokenKind::Keyword));
    }

    #[test]
    fn operator_run_is_grouped() {
        let toks = lex_one("a >= b");
        let kinds: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::Whitespace,
                TokenKind::Operator,
                TokenKind::Whitespace,
                TokenKind::Identifier,
            ]
        );
    }

    #[test]
    fn cast_double_colon_is_operator() {
        let toks = lex_one("a::int");
        let kinds: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Identifier,
                TokenKind::Operator,
                TokenKind::Identifier,
            ]
        );
    }

    #[test]
    fn token_offsets_reference_chars_not_bytes() {
        let toks = lex_one("\u{ac00}\u{ac01} foo");
        // Two CJK chars, then a space, then "foo".
        assert_eq!(toks[0].kind, TokenKind::Identifier);
        assert_eq!(toks[0].start_col, 0);
        assert_eq!(toks[0].len, 2);
        assert_eq!(toks[1].kind, TokenKind::Whitespace);
        assert_eq!(toks[2].kind, TokenKind::Identifier);
        assert_eq!(toks[2].start_col, 3);
        assert_eq!(toks[2].len, 3);
    }
}
