//! Keystroke router for the Find / Find-Replace overlay.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{FindMode, FindOutcome, FindState, ReplaceFocus};

/// Routes a key into a Find overlay. The needle / matches mutate
/// in-place; the caller refreshes `recompute(lines)` whenever the
/// needle changes (handled inside this function for char/backspace,
/// callers don't have to repeat).
pub fn handle_key(state: &mut FindState, key: KeyEvent, lines: &[String]) -> FindOutcome {
    // Replace-mode-only keys come first.
    if state.mode == FindMode::Replace {
        match key.code {
            KeyCode::Tab => {
                state.focus = match state.focus {
                    ReplaceFocus::Needle => ReplaceFocus::Replacement,
                    ReplaceFocus::Replacement => ReplaceFocus::Needle,
                };
                return FindOutcome::Stay;
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if state.matches.is_empty() {
                    return FindOutcome::Stay;
                }
                let ranges = state.matches.clone();
                let text = state.replacement.clone();
                return FindOutcome::ReplaceAll { ranges, text };
            }
            KeyCode::Enter if state.focus == ReplaceFocus::Replacement => {
                let Some(idx) = state.active_idx else {
                    return FindOutcome::Stay;
                };
                let range = state.matches[idx];
                let text = state.replacement.clone();
                return FindOutcome::ReplaceOne { range, text };
            }
            _ => {}
        }
    }

    let mutating_replacement =
        state.mode == FindMode::Replace && state.focus == ReplaceFocus::Replacement;

    match key.code {
        KeyCode::Esc => FindOutcome::Cancel,
        // F3 / Enter advance; Shift+F3 retreats. In Replace mode the
        // Enter key on the Needle field also advances; Enter on the
        // Replacement field is handled above. In vim-search mode
        // (`enter_closes`), Enter closes the overlay after jumping
        // (and uses `retreat()` if the search direction is backward).
        KeyCode::Enter if state.enter_closes => {
            let jumped = if state.backward {
                state.retreat()
            } else {
                // active_idx is already positioned at the nearest
                // match by recompute — the active position is what
                // Enter should jump to, not the *next* one. So
                // return active match directly without advancing.
                state
                    .active_idx
                    .and_then(|i| state.matches.get(i).map(|(s, _)| *s))
            };
            match jumped {
                Some(c) => FindOutcome::JumpAndClose(c),
                None => FindOutcome::Cancel,
            }
        }
        KeyCode::F(3) | KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            match state.advance() {
                Some(c) => FindOutcome::JumpTo(c),
                None => FindOutcome::Stay,
            }
        }
        KeyCode::F(3) if key.modifiers.contains(KeyModifiers::SHIFT) => match state.retreat() {
            Some(c) => FindOutcome::JumpTo(c),
            None => FindOutcome::Stay,
        },
        KeyCode::Backspace => {
            if mutating_replacement {
                state.replacement.pop();
                FindOutcome::Stay
            } else {
                state.needle.pop();
                jump_to_first_after_needle_change(state, lines)
            }
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if key.modifiers.contains(KeyModifiers::ALT) && !mutating_replacement =>
        {
            state.case_sensitive = !state.case_sensitive;
            state.recompute(lines);
            FindOutcome::Stay
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            if mutating_replacement {
                state.replacement.push(c);
                FindOutcome::Stay
            } else {
                state.needle.push(c);
                jump_to_first_after_needle_change(state, lines)
            }
        }
        _ => FindOutcome::Stay,
    }
}

/// Recomputes matches after the needle changed and returns
/// `JumpTo(first_match)` if any match exists, else `Stay`. Used by
/// the Backspace and char-input paths.
fn jump_to_first_after_needle_change(state: &mut FindState, lines: &[String]) -> FindOutcome {
    state.recompute(lines);
    if let Some(c) = state.matches.first().map(|(s, _)| *s) {
        state.active_idx = Some(0);
        FindOutcome::JumpTo(c)
    } else {
        FindOutcome::Stay
    }
}
