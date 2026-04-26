use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;
use crate::ui::autocomplete::{AutocompletePopup, SQL_KEYWORDS};
use crate::ui::autocomplete_context::{detect_context, extract_aliases, CompletionContext};

impl App {
    /// Opens the autocomplete popup if there's a word prefix at the cursor
    /// with at least one match, otherwise inserts a 2-space indent. If a
    /// multi-line selection is active, instead block-indents the entire
    /// selected range.
    pub(super) fn handle_editor_tab(&mut self) {
        if let Some((s, e)) = self.editor().selected_line_range() {
            if e > s {
                self.editor_mut().indent_lines(s, e);
                self.mark_active_dirty();
                return;
            }
        }
        let prefix = self.editor().word_prefix_before_cursor();
        let (row, col) = self.editor().cursor_pos();
        // Snapshot the lines before we need a `&self` borrow for
        // candidate building — `lines()` returns a reference into the
        // editor's buffer that would otherwise alias with self for the
        // duration of the call.
        let ctx = {
            let lines = self.editor().lines();
            detect_context(lines, row, col)
        };
        // The popup opens with no prefix when the surrounding clause
        // already narrows the candidate list (after `FROM ` or
        // `qualifier.`), so the user doesn't have to type a starting
        // letter to discover what's available.
        let context_narrows = !matches!(ctx, CompletionContext::Default);
        if prefix.is_empty() && !context_narrows {
            self.editor_mut().insert_spaces(2);
            self.mark_active_dirty();
            return;
        }
        let candidates = self.candidates_for_context(&ctx);
        let popup = if prefix.is_empty() {
            AutocompletePopup::open_anywhere(candidates)
        } else {
            AutocompletePopup::open(prefix, candidates)
        };
        match popup {
            Some(popup) => self.autocomplete = Some(popup),
            None => {
                self.editor_mut().insert_spaces(2);
                self.mark_active_dirty();
            }
        }
    }

    /// Builds the candidate pool for a known cursor context. Narrowing
    /// rules:
    ///
    /// - After `FROM` / `JOIN` / `INTO` / `UPDATE` / `TABLE`: relation
    ///   names only.
    /// - After `qualifier.`: columns of `qualifier`, where `qualifier` is
    ///   resolved as (1) an alias defined in the same buffer, (2) a known
    ///   relation name, or (3) a known schema name (in which case the
    ///   candidates become the relation names in that schema).
    /// - Otherwise: the full keyword + identifier list.
    ///
    /// Falls back to the default list if a context-specific lookup yields
    /// no candidates — better to show *something* than to mis-narrow when
    /// the schema tree hasn't been loaded yet.
    fn candidates_for_context(&self, ctx: &CompletionContext) -> Vec<String> {
        match ctx {
            CompletionContext::TableName => {
                let names = self.tree.relation_names();
                if names.is_empty() {
                    self.default_candidates()
                } else {
                    names
                }
            }
            CompletionContext::Dotted { qualifier } => {
                let cols = self.resolve_dotted(qualifier);
                if cols.is_empty() {
                    self.default_candidates()
                } else {
                    cols
                }
            }
            CompletionContext::Default => self.default_candidates(),
        }
    }

    fn default_candidates(&self) -> Vec<String> {
        let mut out: Vec<String> = SQL_KEYWORDS.iter().map(|s| (*s).to_string()).collect();
        out.extend(self.tree.collect_identifiers());
        out
    }

    /// Resolves `qualifier.` to a column list. Tries alias mapping first
    /// (so `u.` after `FROM users u` lists `users` columns), then a direct
    /// relation match, then schema-qualified relations.
    fn resolve_dotted(&self, qualifier: &str) -> Vec<String> {
        let aliases = extract_aliases(self.editor().lines());
        let alias_target = aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(qualifier))
            .map(|(_, rel)| rel.clone());
        if let Some(rel) = alias_target {
            let cols = self.tree.columns_of_relation(&rel);
            if !cols.is_empty() {
                return cols;
            }
        }
        let direct = self.tree.columns_of_relation(qualifier);
        if !direct.is_empty() {
            return direct;
        }
        self.tree.relation_names_in_schema(qualifier)
    }

    /// Closes the autocomplete popup if the current prefix is empty or
    /// no longer matches any candidate.
    fn close_popup_if_stale(&mut self) {
        let should_close = match self.autocomplete.as_ref() {
            Some(popup) => popup.prefix().is_empty() || popup.is_empty(),
            None => return,
        };
        if should_close {
            self.autocomplete = None;
        }
    }

    /// Returns true if the key was consumed by the popup.
    pub(super) fn handle_autocomplete_key(&mut self, key: KeyEvent) -> bool {
        let Some(popup) = self.autocomplete.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up => {
                popup.move_up();
                true
            }
            KeyCode::Down => {
                popup.move_down();
                true
            }
            KeyCode::Tab | KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(pick) = popup.current().map(str::to_string) {
                    self.editor_mut().replace_word_prefix(&pick);
                    self.mark_active_dirty();
                }
                self.autocomplete = None;
                true
            }
            KeyCode::Esc => {
                self.autocomplete = None;
                true
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                // Let the editor insert the character, then extend the filter.
                if self.editor_mut().handle_key(key) {
                    self.mark_active_dirty();
                }
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.extend_prefix(c);
                }
                self.close_popup_if_stale();
                true
            }
            KeyCode::Backspace => {
                if self.editor_mut().handle_key(key) {
                    self.mark_active_dirty();
                }
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.shrink_prefix();
                }
                self.close_popup_if_stale();
                true
            }
            _ => {
                // Any other key (arrows, F-keys, Ctrl-combos) closes the
                // popup but does NOT consume the key — caller handles it.
                self.autocomplete = None;
                false
            }
        }
    }
}
