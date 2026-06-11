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
}
