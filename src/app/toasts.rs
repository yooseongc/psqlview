use std::time::{Duration, Instant};

use super::{App, Toast};

impl App {
    pub(super) fn toast_info(&mut self, message: String) {
        self.toast = Some(Toast {
            message,
            until: Instant::now() + Duration::from_secs(3),
            is_error: false,
        });
    }

    pub(super) fn toast_error(&mut self, message: String) {
        tracing::warn!(%message, "error toast");
        // Multi-line errors (e.g., structured Postgres errors with DETAIL/HINT)
        // need more time to read — scale timeout with line count.
        let lines = message.lines().count().max(1) as u64;
        let ttl = 6 + 3 * (lines.saturating_sub(1));
        self.toast = Some(Toast {
            message,
            until: Instant::now() + Duration::from_secs(ttl),
            is_error: true,
        });
    }
}
