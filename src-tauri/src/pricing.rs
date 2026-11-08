/// (substring to match in model id, $/MTok input, $/MTok output)
/// Cache pricing: write = 1.25x base input, read = 0.1x base input (Anthropic standard, all models).
/// Order matters: first match wins, so more specific substrings go first.
/// Known limitation: a hypothetical "opus-4-10" would match the "opus-4-1" row.
const PRICES: &[(&str, f64, f64)] = &[
    ("fable-5", 10.0, 50.0),
    ("opus-4-8", 5.0, 25.0),
    ("opus-4-7", 5.0, 25.0),
    ("opus-4-6", 5.0, 25.0),
    ("opus-4-5", 5.0, 25.0),
    ("opus-4-1", 15.0, 75.0),
    ("opus-4", 15.0, 75.0),
    ("3-opus", 15.0, 75.0),
    ("sonnet", 3.0, 15.0),
    ("3-5-haiku", 0.8, 4.0),
    ("3-haiku", 0.25, 1.25),
    ("haiku-4-5", 1.0, 5.0),
    ("haiku", 1.0, 5.0),
];

const MTOK: f64 = 1_000_000.0;

/// Moonshot list prices, USD per MTok: (model substring, input cache-miss, output, cache-hit input).
/// Checked 2026-11-09. The official page (https://platform.kimi.ai/docs/pricing/chat) renders its
/// tables with JS and could not be fetched, so rows are cross-checked from two trackers that
/// agree and state they were verified against Moonshot's pages:
/// https://developer.puter.com/tutorials/kimi-api-pricing/ and https://benchlm.ai/moonshot/api-pricing
/// Models with no row (e.g. *-highspeed variants) are deliberately unpriced, never guessed.
/// Most specific substrings first.
const KIMI_PRICES: &[(&str, f64, f64, f64)] = &[
    ("kimi-k3.1-pro", 2.0, 10.0, 0.20),
    ("kimi-k3", 3.0, 15.0, 0.30),
    ("kimi-k2.7-code", 0.95, 4.0, 0.19),
    ("kimi-k2.6", 0.95, 4.0, 0.16),
];

/// Cost for a Kimi model id like `moonshot-ai/kimi-k2.6`. `kimi-code/*` is the subscription
/// (no per-token cost) and unknown or unlisted models return None.
pub fn kimi_cost_usd(model: &str, input: u64, output: u64, cache_read: u64) -> Option<f64> {
    if model.starts_with("kimi-code/") {
        return None;
    }
    let name = model.rsplit('/').next().unwrap_or(model);
    let (key, inp, out, hit) = KIMI_PRICES.iter().find(|(m, ..)| name.contains(*m))?;
    // A highspeed variant is priced differently from its base row; never reuse the base price.
    if name.contains("highspeed") && !key.contains("highspeed") {
        return None;
    }
    Some(input as f64 / MTOK * inp + output as f64 / MTOK * out + cache_read as f64 / MTOK * hit)
}

pub fn cost_usd(
    model: &str,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
) -> Option<f64> {
    let (_, inp, out) = PRICES.iter().find(|(m, _, _)| model.contains(*m))?;
    Some(
        input as f64 / MTOK * inp
            + output as f64 / MTOK * out
            + cache_write as f64 / MTOK * (inp * 1.25)
            + cache_read as f64 / MTOK * (inp * 0.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_4_5_costs() {
        // 1M input tokens on opus 4.5 = $5
        let c = cost_usd("claude-opus-4-5-20251101", 1_000_000, 0, 0, 0).unwrap();
        assert!((c - 5.0).abs() < 1e-9);
    }

    #[test]
    fn full_token_mix() {
        // sonnet 4.6: in $3, out $15, write $3.75, read $0.30 per MTok
        let c = cost_usd("claude-sonnet-4-6", 1_000_000, 1_000_000, 1_000_000, 1_000_000).unwrap();
        assert!((c - (3.0 + 15.0 + 3.75 + 0.30)).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(cost_usd("claude-zeppelin-9", 1000, 1000, 0, 0).is_none());
    }

    #[test]
    fn haiku_3_costs() {
        let c = cost_usd("claude-3-haiku-20240307", 1_000_000, 0, 0, 0).unwrap();
        assert!((c - 0.25).abs() < 1e-9);
    }

    #[test]
    fn haiku_3_5_does_not_match_other_rows() {
        let c = cost_usd("claude-3-5-haiku-20241022", 1_000_000, 0, 0, 0).unwrap();
        assert!((c - 0.8).abs() < 1e-9);
    }

    #[test]
    fn claude_3_opus_costs() {
        let c = cost_usd("claude-3-opus-20240229", 1_000_000, 0, 0, 0).unwrap();
        assert!((c - 15.0).abs() < 1e-9);
    }

    #[test]
    fn subscription_models_are_unpriced() {
        assert!(kimi_cost_usd("kimi-code/kimi-for-coding", 1_000_000, 0, 0).is_none());
    }

    #[test]
    fn priced_model_uses_table_row() {
        let r = KIMI_PRICES[0];
        let c = kimi_cost_usd(&format!("moonshot-ai/{}", r.0), 1_000_000, 1_000_000, 1_000_000).unwrap();
        assert!((c - (r.1 + r.2 + r.3)).abs() < 1e-9);
    }

    #[test]
    fn highspeed_variants_are_not_priced_as_base() {
        assert!(kimi_cost_usd("moonshot-ai/kimi-k3.1-pro-highspeed", 1_000_000, 0, 0).is_none());
        assert!(kimi_cost_usd("moonshot-ai/kimi-k3", 1_000_000, 0, 0).is_some());
    }
}
