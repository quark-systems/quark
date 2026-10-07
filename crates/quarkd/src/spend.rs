//! What tokens cost: list prices per model, for estimating spend from the
//! token counts in session logs.
//!
//! Prices are each provider's public API list price in US dollars per
//! million tokens. Work run under a subscription is not billed per token,
//! so for it the figure is what the same work would cost through the API.
//! A harness that records its own cost (Pi) is believed over this table.
//! A model not listed here is counted in tokens but not priced.

use quark_transcript::ModelUsage;

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

const fn anthropic(input: f64, output: f64, cache_read: f64) -> Price {
    // Five-minute cache writes cost 1.25 times input.
    Price {
        input,
        output,
        cache_write: input * 1.25,
        cache_read,
    }
}

const fn openai(input: f64, output: f64, cache_read: f64) -> Price {
    // OpenAI bills no cache writes.
    Price {
        input,
        output,
        cache_write: input,
        cache_read,
    }
}

/// Model id prefixes and their prices. The longest matching prefix wins, so
/// `claude-opus-4-5` is not priced as `claude-opus-4`.
const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", anthropic(10.0, 50.0, 0.25)),
    ("claude-fable-5", anthropic(10.0, 50.0, 1.00)),
    ("claude-mythos-5-1", anthropic(10.0, 50.0, 0.25)),
    ("claude-mythos-5", anthropic(10.0, 50.0, 1.00)),
    ("claude-opus-5-5", anthropic(4.0, 20.0, 0.20)),
    ("claude-opus-5", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-8", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-7", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-6", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-5", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4", anthropic(15.0, 75.0, 1.50)),
    ("claude-sonnet-5-5", anthropic(2.0, 10.0, 0.20)),
    ("claude-sonnet-5", anthropic(2.0, 10.0, 0.20)),
    ("claude-sonnet-4", anthropic(3.0, 15.0, 0.30)),
    ("claude-3-7-sonnet", anthropic(3.0, 15.0, 0.30)),
    ("claude-haiku-4-5", anthropic(1.0, 5.0, 0.10)),
    ("claude-3-5-haiku", anthropic(0.8, 4.0, 0.08)),
    ("gpt-5", openai(1.25, 10.0, 0.125)),
    ("gpt-5-mini", openai(0.25, 2.0, 0.025)),
    ("gpt-5-nano", openai(0.05, 0.4, 0.005)),
];

/// The list price of `model`, if known. Provider prefixes (`anthropic.`,
/// `openai/`), a context-window suffix (`[1m]`) and case are ignored.
pub fn price(model: &str) -> Option<Price> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.rsplit('/').next().unwrap_or(&m);
    let m = m.strip_prefix("anthropic.").unwrap_or(m);
    let m = m.split('[').next().unwrap_or(m);
    PRICES
        .iter()
        .filter(|(prefix, _)| m.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, p)| *p)
}

/// What `u` cost in US dollars: the harness's own figure when it recorded
/// one, else list price. `None` when neither is known.
pub fn cost(u: &ModelUsage) -> Option<f64> {
    if let Some(c) = u.cost_usd {
        return Some(c);
    }
    let p = price(&u.model)?;
    let fresh = u.input.saturating_sub(u.cache_read + u.cache_write);
    let m = |tokens: u64, per: f64| tokens as f64 * per / 1_000_000.0;
    Some(
        m(fresh, p.input)
            + m(u.cache_write, p.cache_write)
            + m(u.cache_read, p.cache_read)
            + m(u.output, p.output),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins_and_decorations_are_ignored() {
        assert_eq!(price("claude-opus-4-5-20251101").unwrap().input, 5.0);
        assert_eq!(price("claude-opus-4-1-20250805").unwrap().input, 15.0);
        assert_eq!(price("anthropic.claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(price("claude-opus-5-5[1m]").unwrap().output, 20.0);
        assert_eq!(price("gpt-5-mini").unwrap().input, 0.25);
        assert_eq!(price("openai/gpt-5-codex").unwrap().input, 1.25);
        assert_eq!(price("llama-4"), None);
    }

    #[test]
    fn costs_fresh_cached_and_output_tokens_apart() {
        let u = ModelUsage {
            model: "claude-sonnet-5-5".into(),
            input: 3_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
            calls: 3,
            cost_usd: None,
        };
        // 1M fresh at 2, 1M written at 2.5, 1M read at 0.2, 1M out at 10.
        assert!((cost(&u).unwrap() - 14.7).abs() < 1e-9);
        let own = ModelUsage {
            cost_usd: Some(0.5),
            ..u.clone()
        };
        assert_eq!(cost(&own), Some(0.5));
        let unknown = ModelUsage {
            model: "mystery".into(),
            ..u
        };
        assert_eq!(cost(&unknown), None);
    }
}
