//! Detects the *completion context* under the cursor so the autocomplete
//! popup can narrow its candidate list.
//!
//! Three contexts are recognized:
//!
//! - [`CompletionContext::TableName`] — the cursor follows a clause that
//!   wants a relation name (`FROM`, `JOIN`, `INTO`, `UPDATE`, `TABLE`).
//! - [`CompletionContext::Dotted`] — the cursor sits right after
//!   `qualifier.`, so we want columns of `qualifier` (where `qualifier`
//!   is a relation name, an alias defined in the same statement, or a
//!   schema name).
//! - [`CompletionContext::Default`] — fall back to the full keyword +
//!   identifier list.
//!
//! Detection reuses [`crate::ui::sql_lexer::tokenize_line`] so string
//! literals and comments are skipped consistently with the highlighter.

use crate::ui::sql_lexer::{tokenize_line, LexState, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// A relation (table / view / matview) name is expected.
    TableName,
    /// A qualified name is being typed: candidates are the columns of
    /// `qualifier` (or the relations in `qualifier` if it names a schema).
    Dotted { qualifier: String },
    /// No clause-level context; the popup uses keywords + all known
    /// identifiers.
    Default,
}

#[derive(Debug, Clone)]
struct MeaningfulTok {
    kind: TokenKind,
    text: String,
}

/// Classifies the cursor's completion context. `lines` is the editor's
/// raw text; `(row, col)` is the 0-indexed cursor position.
pub fn detect_context(lines: &[String], row: usize, col: usize) -> CompletionContext {
    let toks = tokens_until(lines, row, col);
    let in_word = cursor_in_word(lines, row, col);
    classify(&toks, in_word)
}

/// True when the character immediately to the left of the cursor is part
/// of an identifier (alphanumeric or `_`). Mirrors the rule used by
/// `EditorState::word_prefix_before_cursor` so the popup's prefix and the
/// completion context agree on what counts as "the prefix".
fn cursor_in_word(lines: &[String], row: usize, col: usize) -> bool {
    if col == 0 {
        return false;
    }
    let Some(line) = lines.get(row) else {
        return false;
    };
    let chars: Vec<char> = line.chars().collect();
    if col > chars.len() {
        return false;
    }
    let prev = chars[col - 1];
    prev.is_alphanumeric() || prev == '_'
}

/// Returns `(alias, relation_name)` pairs found in the buffer's FROM /
/// JOIN clauses. Single-pass, best-effort — handles the common
/// `relation alias` and `relation AS alias` forms; does not attempt to
/// parse subqueries or schema-qualified relations.
pub fn extract_aliases(lines: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut state = LexState::default();
    // Walk every line, but treat each line independently for offset; we
    // only care about token *order*, not absolute position.
    let mut flat: Vec<MeaningfulTok> = Vec::new();
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        for t in tokenize_line(line, &mut state) {
            if matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            ) {
                continue;
            }
            let end = (t.start_col + t.len).min(chars.len());
            let text: String = chars[t.start_col..end].iter().collect();
            flat.push(MeaningfulTok { kind: t.kind, text });
        }
    }

    let n = flat.len();
    let mut i = 0;
    while i < n {
        let kw_match = flat[i].kind == TokenKind::Keyword
            && matches!(flat[i].text.to_ascii_uppercase().as_str(), "FROM" | "JOIN");
        if !kw_match {
            i += 1;
            continue;
        }
        // After FROM / JOIN, walk the following identifiers until we hit a
        // keyword that ends the relation list (WHERE, ON, GROUP, ORDER, ...).
        i += 1;
        while i < n {
            if flat[i].kind == TokenKind::Keyword {
                let upper = flat[i].text.to_ascii_uppercase();
                if upper == "AS" {
                    // `relation AS alias` — the previous identifier is the
                    // relation, the next identifier is the alias.
                    if let (Some(prev), Some(next)) = (flat.get(i.wrapping_sub(1)), flat.get(i + 1))
                    {
                        if is_identlike(prev) && is_identlike(next) {
                            out.push((unquote(&next.text), unquote(&prev.text)));
                        }
                    }
                    i += 2;
                    continue;
                }
                if matches!(
                    upper.as_str(),
                    "JOIN"
                        | "LEFT"
                        | "RIGHT"
                        | "INNER"
                        | "OUTER"
                        | "FULL"
                        | "CROSS"
                        | "ON"
                        | "USING"
                ) {
                    // Re-enter the FROM/JOIN scan at the next iteration.
                    break;
                }
                // Any other keyword (WHERE, GROUP, ORDER, ...) ends the clause.
                break;
            }
            // Implicit alias: `relation alias` (two identifiers in a row,
            // not separated by `.` or `,`).
            if is_identlike(&flat[i]) {
                if let Some(next) = flat.get(i + 1) {
                    if is_identlike(next) {
                        out.push((unquote(&next.text), unquote(&flat[i].text)));
                        i += 2;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    out
}

fn is_identlike(t: &MeaningfulTok) -> bool {
    matches!(t.kind, TokenKind::Identifier | TokenKind::QuotedIdent)
}

fn unquote(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        return inner.replace("\"\"", "\"");
    }
    s.to_string()
}

/// Collects meaningful tokens (non-whitespace, non-comment) from the
/// buffer up to and including any token that lies entirely before the
/// cursor. A token straddling the cursor is truncated to the part on
/// the left side, so e.g. `FROM us|` yields a final token `us`.
fn tokens_until(lines: &[String], row: usize, col: usize) -> Vec<MeaningfulTok> {
    let mut state = LexState::default();
    let mut out = Vec::new();
    for (r, line) in lines.iter().enumerate() {
        if r > row {
            break;
        }
        let chars: Vec<char> = line.chars().collect();
        let limit = if r == row {
            col.min(chars.len())
        } else {
            chars.len()
        };
        let line_toks = tokenize_line(line, &mut state);
        for t in line_toks {
            if t.start_col >= limit {
                break;
            }
            if matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            ) {
                continue;
            }
            let end = (t.start_col + t.len).min(limit);
            let text: String = chars[t.start_col..end].iter().collect();
            out.push(MeaningfulTok { kind: t.kind, text });
        }
    }
    out
}

fn classify(toks: &[MeaningfulTok], cursor_in_word: bool) -> CompletionContext {
    // If the cursor sits at the end of an identifier-like token, that
    // token is the prefix being typed — drop it from the scope. When the
    // cursor is in whitespace, every token is "complete" and stays in.
    let mut end = toks.len();
    if cursor_in_word {
        if let Some(last) = toks.last() {
            if matches!(
                last.kind,
                TokenKind::Identifier | TokenKind::Keyword | TokenKind::QuotedIdent
            ) {
                end = end.saturating_sub(1);
            }
        }
    }
    let scope = &toks[..end];

    // Dotted: `<ident> .` immediately before the prefix.
    if scope.len() >= 2 {
        let last = &scope[scope.len() - 1];
        let prev = &scope[scope.len() - 2];
        if last.kind == TokenKind::Operator && last.text == "." && is_identlike(prev) {
            return CompletionContext::Dotted {
                qualifier: unquote(&prev.text),
            };
        }
    }

    // Walk back for the most recent keyword. Identifiers, dots, and commas
    // don't reset the context (`SELECT a, b FROM t1, t|` still wants a
    // table). Other operators (=, <, etc.) reset to Default.
    for t in scope.iter().rev() {
        match t.kind {
            TokenKind::Keyword => {
                let upper = t.text.to_ascii_uppercase();
                return if matches!(
                    upper.as_str(),
                    "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE"
                ) {
                    CompletionContext::TableName
                } else {
                    CompletionContext::Default
                };
            }
            TokenKind::Identifier | TokenKind::QuotedIdent => continue,
            TokenKind::Operator if t.text == "," || t.text == "." => continue,
            _ => return CompletionContext::Default,
        }
    }
    CompletionContext::Default
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(|s| s.to_string()).collect()
    }

    fn cursor_at_end(buf: &[String]) -> (usize, usize) {
        let row = buf.len().saturating_sub(1);
        let col = buf[row].chars().count();
        (row, col)
    }

    #[test]
    fn after_from_is_table_name() {
        let l = lines("SELECT * FROM us");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn after_from_with_no_prefix_is_table_name() {
        // Cursor right after a space following FROM.
        let l = lines("SELECT * FROM ");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn after_join_is_table_name() {
        let l = lines("SELECT * FROM users JOIN ord");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn after_update_is_table_name() {
        let l = lines("UPDATE us");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn second_table_in_comma_list_is_table_name() {
        let l = lines("SELECT * FROM users, ord");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn after_where_is_default() {
        let l = lines("SELECT * FROM users WHERE id");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::Default);
    }

    #[test]
    fn dotted_after_alias_returns_qualifier() {
        let l = lines("SELECT u.na");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(
            detect_context(&l, r, c),
            CompletionContext::Dotted {
                qualifier: "u".into()
            }
        );
    }

    #[test]
    fn dotted_immediately_after_dot_returns_qualifier() {
        let l = lines("SELECT u.");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(
            detect_context(&l, r, c),
            CompletionContext::Dotted {
                qualifier: "u".into()
            }
        );
    }

    #[test]
    fn dotted_with_quoted_identifier_strips_quotes() {
        let l = lines("SELECT \"User\".na");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(
            detect_context(&l, r, c),
            CompletionContext::Dotted {
                qualifier: "User".into()
            }
        );
    }

    #[test]
    fn comments_do_not_disturb_context() {
        let l = lines("SELECT *\n-- pick a table\nFROM us");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::TableName);
    }

    #[test]
    fn token_inside_string_does_not_count() {
        // The 'FROM' inside the string literal must not trigger TableName.
        let l = lines("SELECT 'FROM' || x");
        let (r, c) = cursor_at_end(&l);
        assert_eq!(detect_context(&l, r, c), CompletionContext::Default);
    }

    #[test]
    fn extract_aliases_handles_implicit_form() {
        let l = lines("SELECT u.id FROM users u");
        let pairs = extract_aliases(&l);
        assert!(pairs.contains(&("u".to_string(), "users".to_string())));
    }

    #[test]
    fn extract_aliases_handles_as_form() {
        let l = lines("SELECT u.id FROM users AS u");
        let pairs = extract_aliases(&l);
        assert!(pairs.contains(&("u".to_string(), "users".to_string())));
    }

    #[test]
    fn extract_aliases_handles_join() {
        let l = lines("SELECT * FROM users u JOIN orders o ON u.id = o.user_id");
        let pairs = extract_aliases(&l);
        assert!(pairs.contains(&("u".to_string(), "users".to_string())));
        assert!(pairs.contains(&("o".to_string(), "orders".to_string())));
    }

    #[test]
    fn extract_aliases_strips_quoted_idents() {
        let l = lines("SELECT * FROM \"My Table\" AS \"t\"");
        let pairs = extract_aliases(&l);
        assert!(pairs.contains(&("t".to_string(), "My Table".to_string())));
    }
}
