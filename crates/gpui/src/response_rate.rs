use std::time::{Duration, Instant};

const TOKEN_CHARS: u64 = 4;
const MIN_DISPLAY_ELAPSED: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenRate {
    pub(crate) tokens_per_second: u64,
    pub(crate) exact: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ResponseRate {
    started_at: Option<Instant>,
    estimated_chars: u64,
    last_rate: Option<TokenRate>,
    turn_finalized: bool,
}

impl ResponseRate {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn record_delta(&mut self, text: &str, now: Instant) {
        if text.is_empty() {
            return;
        }
        if self.turn_finalized {
            self.started_at = None;
            self.estimated_chars = 0;
            self.turn_finalized = false;
        }
        let started_at = *self.started_at.get_or_insert(now);
        self.estimated_chars = self
            .estimated_chars
            .saturating_add(text.chars().count() as u64);
        if let Some(rate) = rate_from_tokens(
            estimated_tokens(self.estimated_chars),
            now.saturating_duration_since(started_at),
            false,
        ) {
            self.last_rate = Some(rate);
        }
    }

    pub(crate) fn finalize(&mut self, output_tokens: Option<u64>, now: Instant) {
        let Some(started_at) = self.started_at else {
            return;
        };
        let tokens = output_tokens.unwrap_or_else(|| estimated_tokens(self.estimated_chars));
        if let Some(rate) = rate_from_tokens(
            tokens,
            now.saturating_duration_since(started_at),
            output_tokens.is_some(),
        ) {
            self.last_rate = Some(rate);
        }
        self.turn_finalized = true;
    }

    pub(crate) fn display_rate(&self, now: Instant) -> Option<TokenRate> {
        if self.turn_finalized || self.started_at.is_none() {
            return self.last_rate;
        }
        let started_at = self.started_at.expect("checked above");
        rate_from_tokens(
            estimated_tokens(self.estimated_chars),
            now.saturating_duration_since(started_at),
            false,
        )
        .or(self.last_rate)
    }
}

fn estimated_tokens(chars: u64) -> u64 {
    chars.saturating_add(TOKEN_CHARS - 1) / TOKEN_CHARS
}

fn rate_from_tokens(tokens: u64, elapsed: Duration, exact: bool) -> Option<TokenRate> {
    if tokens == 0 || elapsed < MIN_DISPLAY_ELAPSED {
        return None;
    }
    let seconds = elapsed.as_secs_f64();
    let tokens_per_second = ((tokens as f64) / seconds).round().max(1.0) as u64;
    Some(TokenRate {
        tokens_per_second,
        exact,
    })
}

#[cfg(test)]
mod tests {
    use super::{ResponseRate, TokenRate};
    use std::time::{Duration, Instant};

    #[test]
    fn deltas_are_estimated_and_rate_is_available_after_a_short_window() {
        let start = Instant::now();
        let mut rate = ResponseRate::default();

        rate.record_delta("12345678", start);

        assert_eq!(rate.display_rate(start + Duration::from_millis(99)), None);
        assert_eq!(
            rate.display_rate(start + Duration::from_secs(1)),
            Some(TokenRate {
                tokens_per_second: 2,
                exact: false,
            })
        );
    }

    #[test]
    fn reasoning_and_text_deltas_share_the_same_measurement() {
        let start = Instant::now();
        let mut rate = ResponseRate::default();

        rate.record_delta("1234", start);
        rate.record_delta("5678", start + Duration::from_secs(1));

        assert_eq!(
            rate.display_rate(start + Duration::from_secs(1)),
            Some(TokenRate {
                tokens_per_second: 2,
                exact: false,
            })
        );
    }

    #[test]
    fn exact_provider_usage_replaces_the_estimate_and_is_retained() {
        let start = Instant::now();
        let mut rate = ResponseRate::default();

        rate.record_delta("12345678", start);
        rate.finalize(Some(12), start + Duration::from_secs(2));

        assert_eq!(
            rate.display_rate(start + Duration::from_secs(2)),
            Some(TokenRate {
                tokens_per_second: 6,
                exact: true,
            })
        );
    }

    #[test]
    fn reset_clears_the_previous_value_for_a_new_request() {
        let start = Instant::now();
        let mut rate = ResponseRate::default();

        rate.record_delta("12345678", start);
        rate.finalize(None, start + Duration::from_secs(1));
        rate.reset();

        assert_eq!(rate.display_rate(start + Duration::from_secs(2)), None);
    }
}
