//! Kimi Code subscription quota, read from the `/usages` endpoint.

use crate::snapshot::KimiLimits;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("host not allowed")]
    Disallowed,
    #[error("http: {0}")]
    Http(String),
    #[error("parse: {0}")]
    Parse(String),
}

/// Mirrors the CLI's `ratioValue`: accepts numbers and numeric strings.
fn ratio_value(v: &Value) -> Option<f64> {
    let r = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    r.is_finite().then_some(r)
}

fn entry(usages: &Value, key: &str) -> (Option<f32>, Option<String>) {
    let Some(e) = usages.get(key) else { return (None, None) };
    let pct = e
        .get("used_ratio")
        .and_then(ratio_value)
        .map(|r| (r * 100.0).clamp(0.0, 100.0) as f32);
    let reset = e
        .get("reset_time")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    (pct, reset)
}

pub fn parse_usages(json: &Value) -> Result<KimiLimits, QuotaError> {
    let empty = Value::Null;
    let u = json.get("usages").unwrap_or(&empty);
    let (five_hour_pct, five_hour_resets_at) = entry(u, "limit_5h");
    let (weekly_pct, weekly_resets_at) = entry(u, "limit_7d");
    // A 200 with neither window is not "no usage": it is an unexpected body, and
    // reporting it as data would wipe the last-known values.
    if five_hour_pct.is_none()
        && five_hour_resets_at.is_none()
        && weekly_pct.is_none()
        && weekly_resets_at.is_none()
    {
        return Err(QuotaError::Parse("no usage windows in response".into()));
    }
    Ok(KimiLimits {
        five_hour_pct,
        five_hour_resets_at,
        weekly_pct,
        weekly_resets_at,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        stale: false,
    })
}

pub async fn fetch_usages(base_url: &str, access_token: &str) -> Result<KimiLimits, QuotaError> {
    if !super::host_allowed(base_url) {
        return Err(QuotaError::Disallowed);
    }
    fetch_usages_unchecked(base_url, access_token).await
}

pub(crate) async fn fetch_usages_unchecked(
    base_url: &str,
    access_token: &str,
) -> Result<KimiLimits, QuotaError> {
    let url = format!("{}/usages", base_url.trim_end_matches('/'));
    let resp = crate::http::client()
        .get(url)
        .bearer_auth(access_token)
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .send()
        .await
        .map_err(|e| QuotaError::Http(e.to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(QuotaError::Unauthorized);
    }
    if !status.is_success() {
        return Err(QuotaError::Http(format!("status {status}")));
    }
    let body: Value = resp.json().await.map_err(|e| QuotaError::Parse(e.to_string()))?;
    parse_usages(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_json() -> Value {
        let p = crate::test_fixture("kimi/usages.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn parses_fixture() {
        let l = parse_usages(&fixture_json()).unwrap();
        assert_eq!(l.five_hour_pct, Some(0.25f64.mul_add(100.0, 0.0) as f32));
        assert_eq!(l.weekly_pct, Some((0.4f64 * 100.0) as f32));
        assert_eq!(l.five_hour_resets_at.as_deref(), Some("2026-11-08T18:00:00Z"));
        assert_eq!(l.weekly_resets_at.as_deref(), Some("2026-11-12T00:00:00Z"));
        assert!(!l.stale);
    }

    #[test]
    fn missing_weekly_is_none() {
        let v = serde_json::json!({"usages":{"limit_5h":{"used_ratio":0.3,"reset_time":"2026-11-08T18:10:00Z"}}});
        let l = parse_usages(&v).unwrap();
        assert_eq!(l.five_hour_pct, Some(30.0));
        assert_eq!(l.weekly_pct, None);
    }

    #[test]
    fn ratio_is_clamped() {
        let v = serde_json::json!({"usages":{"limit_7d":{"used_ratio":1.7},"limit_5h":{"used_ratio":-0.2}}});
        let l = parse_usages(&v).unwrap();
        assert_eq!(l.weekly_pct, Some(100.0));
        assert_eq!(l.five_hour_pct, Some(0.0));
    }

    #[test]
    fn empty_reset_time_is_none() {
        let v = serde_json::json!({"usages":{"limit_5h":{"used_ratio":0.5,"reset_time":""}}});
        assert_eq!(parse_usages(&v).unwrap().five_hour_resets_at, None);
    }

    #[test]
    fn numeric_string_ratio_and_junk() {
        let v = serde_json::json!({"usages":{"limit_5h":{"used_ratio":"0.5"},"limit_7d":"oops"}});
        let l = parse_usages(&v).unwrap();
        assert_eq!(l.five_hour_pct, Some(50.0));
        assert_eq!(l.weekly_pct, None);
        assert!(parse_usages(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn both_windows_absent_is_parse_error() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({"usages":{}}),
            serde_json::json!({"usages":{"other":{"used_ratio":0.1}}}),
            serde_json::json!({"usages":{"limit_5h":"x","limit_7d":null}}),
        ] {
            assert!(matches!(parse_usages(&v), Err(QuotaError::Parse(_))), "{v}");
        }
    }

    #[tokio::test]
    async fn fetch_hits_usages_with_bearer() {
        let server = MockServer::start().await;
        let ua = concat!("claude-usage-meter/", env!("CARGO_PKG_VERSION"));
        Mock::given(method("GET"))
            .and(path("/coding/v1/usages"))
            .and(header("authorization", "Bearer tok"))
            .and(header("user-agent", ua))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_json()))
            .expect(1)
            .mount(&server)
            .await;
        let l = fetch_usages_unchecked(&format!("{}/coding/v1/", server.uri()), "tok")
            .await
            .unwrap();
        assert_eq!(l.weekly_resets_at.as_deref(), Some("2026-11-12T00:00:00Z"));
    }

    #[tokio::test]
    async fn fetch_401_is_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let r = fetch_usages_unchecked(&format!("{}/coding/v1", server.uri()), "tok").await;
        assert!(matches!(r, Err(QuotaError::Unauthorized)));
    }

    #[tokio::test]
    async fn disallowed_host_is_not_contacted() {
        let r = fetch_usages("https://proxy.example.com/v1", "tok").await;
        assert!(matches!(r, Err(QuotaError::Disallowed)));
    }
}
