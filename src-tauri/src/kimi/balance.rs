//! Moonshot Open Platform account balance (optional; needs a Moonshot API key).

use super::config::KimiConfig;
use crate::snapshot::KimiBalance;
use serde_json::Value;

const KEYRING_SERVICE: &str = "claude-usage-meter";
const KEYRING_USER: &str = "moonshot-api-key";
const DEFAULT_BASE: &str = "https://api.moonshot.ai/v1";

pub fn currency_for(base_url: &str) -> &'static str {
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if host.ends_with("moonshot.cn") {
        "CNY"
    } else {
        "USD"
    }
}

/// Saved key wins (with the configured or default base); else the config's key and base.
pub fn resolve_key(saved: Option<String>, cfg: &KimiConfig) -> Option<(String, String)> {
    if let Some(k) = saved.filter(|k| !k.is_empty()) {
        let base = cfg
            .moonshot_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        return Some((base, k));
    }
    let key = cfg.moonshot_api_key.clone().filter(|k| !k.is_empty())?;
    let base = cfg
        .moonshot_base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    Some((base, key))
}

pub fn parse_balance(json: &Value, base_url: &str) -> Result<KimiBalance, String> {
    let available = json
        .pointer("/data/available_balance")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or("missing data.available_balance")?;
    Ok(KimiBalance {
        available,
        currency: currency_for(base_url).to_string(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        stale: false,
    })
}

pub async fn fetch_balance(base_url: &str, api_key: &str) -> Result<KimiBalance, String> {
    if !super::host_allowed(base_url) {
        return Err("host not allowed".to_string());
    }
    fetch_balance_unchecked(base_url, api_key).await
}

pub(crate) async fn fetch_balance_unchecked(
    base_url: &str,
    api_key: &str,
) -> Result<KimiBalance, String> {
    let url = format!("{}/users/me/balance", base_url.trim_end_matches('/'));
    let resp = crate::http::client()
        .get(url)
        .bearer_auth(api_key)
        .header(reqwest::header::USER_AGENT, super::USER_AGENT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("balance status {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    parse_balance(&body, base_url)
}

// ---------- OS secret-store storage ----------

pub fn save_moonshot_key(key: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|e| e.set_password(key))
        .map_err(|e| e.to_string())
}

pub fn load_moonshot_key() -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .ok()?
        .get_password()
        .ok()
}

pub fn clear_moonshot_key() -> Result<(), String> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).and_then(|e| e.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture_json() -> Value {
        let p = crate::test_fixture("kimi/balance.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn cfg(base: Option<&str>, key: Option<&str>) -> KimiConfig {
        KimiConfig {
            moonshot_base_url: base.map(str::to_string),
            moonshot_api_key: key.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn parses_balance_fixture() {
        let b = parse_balance(&fixture_json(), "https://api.moonshot.ai/v1").unwrap();
        assert_eq!(b.available, 12.5);
        assert_eq!(b.currency, "USD");
        assert!(!b.stale);
    }

    #[test]
    fn missing_balance_is_error() {
        assert!(parse_balance(&serde_json::json!({"data":{}}), "https://x").is_err());
    }

    #[test]
    fn currency_from_host() {
        assert_eq!(currency_for("https://api.moonshot.cn/v1"), "CNY");
        assert_eq!(currency_for("https://api.moonshot.ai/v1"), "USD");
    }

    #[test]
    fn saved_key_wins_over_config() {
        let c = cfg(Some("https://api.moonshot.cn/v1"), Some("cfg-key"));
        assert_eq!(
            resolve_key(Some("saved".into()), &c),
            Some(("https://api.moonshot.cn/v1".into(), "saved".into()))
        );
        assert_eq!(
            resolve_key(Some("saved".into()), &cfg(None, None)),
            Some((DEFAULT_BASE.into(), "saved".into()))
        );
    }

    #[test]
    fn config_key_used_when_none_saved() {
        let c = cfg(Some("https://api.moonshot.cn/v1"), Some("cfg-key"));
        assert_eq!(
            resolve_key(None, &c),
            Some(("https://api.moonshot.cn/v1".into(), "cfg-key".into()))
        );
    }

    #[test]
    fn no_key_anywhere_is_none() {
        assert_eq!(resolve_key(None, &cfg(Some("https://api.moonshot.ai/v1"), None)), None);
        assert_eq!(resolve_key(Some(String::new()), &cfg(None, None)), None);
    }

    #[tokio::test]
    async fn fetch_balance_unchecked_reads_available_balance() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/users/me/balance"))
            .and(header("authorization", "Bearer sk-test"))
            .and(header("user-agent", crate::kimi::USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_json()))
            .expect(1)
            .mount(&server)
            .await;
        let b = fetch_balance_unchecked(&format!("{}/v1/", server.uri()), "sk-test")
            .await
            .unwrap();
        assert_eq!(b.available, 12.5);
    }

    #[tokio::test]
    async fn disallowed_host_is_not_contacted() {
        assert!(fetch_balance("https://proxy.example.com/v1", "k").await.is_err());
    }
}
