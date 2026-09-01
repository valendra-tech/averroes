use serde::{Deserialize, Serialize};

/// Provider-reported context usage for the latest request.
///
/// Token counts stay optional on purpose. Until a provider returns a usage
/// object, Averroes displays an unknown value rather than presenting a fake
/// precision based on character or byte heuristics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub context_limit: u64,
}

impl ContextUsage {
    pub fn from_usage(input_tokens: u64, output_tokens: u64, context_limit: usize) -> Self {
        Self {
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
            context_limit: context_limit as u64,
        }
    }

    pub fn unknown(context_limit: usize) -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            context_limit: context_limit as u64,
        }
    }

    pub fn percentage(self) -> Option<u8> {
        let input_tokens = self.input_tokens?;
        if self.context_limit == 0 {
            return Some(0);
        }
        Some(
            input_tokens
                .saturating_mul(100)
                .checked_div(self.context_limit)
                .unwrap_or(0)
                .min(100) as u8,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ContextUsage;

    #[test]
    fn percentage_uses_provider_input_usage_without_heuristics() {
        let usage = ContextUsage::from_usage(42_000, 1_000, 100_000);
        assert_eq!(usage.percentage(), Some(42));
    }

    #[test]
    fn percentage_stays_unknown_until_provider_reports_usage() {
        assert_eq!(ContextUsage::unknown(100_000).percentage(), None);
    }
}
