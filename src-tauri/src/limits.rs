use crate::snapshot::Limits;
use serde::Deserialize;
use std::path::PathBuf;

pub const USAGE_BASE: &str = "https://api.anthropic.com";
pub const TOKEN_BASE: &str = "https://console.anthropic.com";
/// Claude Code's public OAuth client id (PKCE public client; widely documented).
/// If the refresh flow returns 400 invalid_client during live verification,
/// re-extract the id from a Claude Code OAuth flow and update this constant.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(thiserror::Error, Debug)]
pub enum LimitsError {
    #[error("credentials file missing or unreadable")]
    NoCredentials,
    #[error("unauthorized (token expired)")]
    Unauthorized,
    #[error("forbidden (access denied)")]
    Forbidden,
    #[error("no refresh token available")]
    NoRefreshToken,
    #[error("network/http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected response: {0}")]
    Bad(String),
}

/// OAuth credentials. Debug is intentionally NOT derived to prevent accidental token logging.
#[derive(Clone)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
}

/// Where Claude Code's OAuth credentials live for this platform.
///
/// Windows / Linux: the plaintext `~/.claude/.credentials.json` file.
/// macOS: Claude Code stores them in the login Keychain instead (the file does not
/// exist), as a generic password under [`MACOS_KEYCHAIN_SERVICE`].
pub enum CredStore {
    File(PathBuf),
    #[cfg(target_os = "macos")]
    Keychain,
}

/// Generic-password service name Claude Code uses for its credentials on macOS.
#[cfg(target_os = "macos")]
const MACOS_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

impl CredStore {
    /// Read the raw credentials JSON from the backing store.
    fn read_raw(&self) -> Result<String, LimitsError> {
        match self {
            CredStore::File(path) => {
                std::fs::read_to_string(path).map_err(|_| LimitsError::NoCredentials)
            }
            #[cfg(target_os = "macos")]
            CredStore::Keychain => {
                // `security` prints the secret (the same JSON) to stdout, plus a newline.
                let out = std::process::Command::new("/usr/bin/security")
                    .args(["find-generic-password", "-s", MACOS_KEYCHAIN_SERVICE, "-w"])
                    .output()
                    .map_err(|_| LimitsError::NoCredentials)?;
                if !out.status.success() {
                    return Err(LimitsError::NoCredentials);
                }
                String::from_utf8(out.stdout).map_err(|_| LimitsError::NoCredentials)
            }
        }
    }

    /// Persist the raw credentials JSON back to the backing store. Best-effort:
    /// callers still return the in-memory fresh token if this fails.
    fn write_raw(&self, json: &str) -> Result<(), LimitsError> {
        match self {
            CredStore::File(path) => {
                // Atomic: write to a temp file in the same dir, then rename.
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, json).map_err(|e| LimitsError::Bad(e.to_string()))?;
                std::fs::rename(&tmp, path).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    LimitsError::Bad(e.to_string())
                })
            }
            #[cfg(target_os = "macos")]
            CredStore::Keychain => keychain_write(json),
        }
    }
}

/// Parse the account from `security find-generic-password` attribute output.
/// The relevant line looks like:    "acct"<blob>="the-account"
/// (Pure/testable; compiled on macOS and under test.)
#[cfg(any(target_os = "macos", test))]
fn parse_keychain_account(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"acct\"") {
            if let Some(eq) = rest.find("=\"") {
                let val = &rest[eq + 2..];
                if let Some(end) = val.rfind('"') {
                    return Some(val[..end].to_string());
                }
            }
        }
    }
    None
}

/// The account ("-a") of Claude Code's existing Keychain item. Needed so an update
/// targets the same item instead of creating a duplicate; if it can't be determined
/// we skip the write rather than risk a dup.
#[cfg(target_os = "macos")]
fn keychain_account() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", MACOS_KEYCHAIN_SERVICE])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_keychain_account(&String::from_utf8_lossy(&out.stdout))
}

/// Update Claude Code's Keychain item in place (`-U`) with new credentials JSON.
#[cfg(target_os = "macos")]
fn keychain_write(json: &str) -> Result<(), LimitsError> {
    let account = keychain_account()
        .ok_or_else(|| LimitsError::Bad("keychain account not found".into()))?;
    let status = std::process::Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            &account,
            "-s",
            MACOS_KEYCHAIN_SERVICE,
            "-w",
            json,
        ])
        .status()
        .map_err(|e| LimitsError::Bad(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(LimitsError::Bad("keychain update failed".into()))
    }
}

fn parse_credentials(raw: &str) -> Result<Credentials, LimitsError> {
    let v: serde_json::Value =
        serde_json::from_str(raw.trim()).map_err(|_| LimitsError::NoCredentials)?;
    let oauth = &v["claudeAiOauth"];
    Ok(Credentials {
        access_token: oauth["accessToken"].as_str().ok_or(LimitsError::NoCredentials)?.into(),
        refresh_token: oauth["refreshToken"].as_str().unwrap_or_default().into(),
    })
}

pub fn read_credentials(store: &CredStore) -> Result<Credentials, LimitsError> {
    parse_credentials(&store.read_raw()?)
}

#[derive(Deserialize)]
struct UsageWindow {
    utilization: Option<f32>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

pub async fn fetch_limits(base: &str, access_token: &str) -> Result<Limits, LimitsError> {
    let resp = crate::http::client()
        .get(format!("{base}/api/oauth/usage"))
        .header("authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(LimitsError::Unauthorized);
    }
    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(LimitsError::Forbidden);
    }
    if !resp.status().is_success() {
        return Err(LimitsError::Bad(format!("status {}", resp.status())));
    }
    let body: UsageResponse = resp.json().await?;
    let five = body.five_hour.unwrap_or(UsageWindow { utilization: None, resets_at: None });
    let seven = body.seven_day.unwrap_or(UsageWindow { utilization: None, resets_at: None });
    Ok(Limits {
        session_pct: five.utilization.unwrap_or(0.0),
        session_resets_at: five.resets_at,
        weekly_pct: seven.utilization.unwrap_or(0.0),
        weekly_resets_at: seven.resets_at,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        stale: false,
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

/// Refresh the OAuth token and persist it back to the store, preserving fields we
/// don't model. Works for both the file (Windows/Linux) and the macOS Keychain.
///
/// Safety rules (the store is ALSO written by Claude Code itself):
/// 1. Fails fast if refresh_token is empty (NoRefreshToken).
/// 2. After POST succeeds, re-reads the store; if refreshToken changed (Claude Code
///    already refreshed), skips the write but still returns fresh Credentials.
/// 3. Persists via the store (atomic temp+rename for files; `security -U` for Keychain).
/// 4. If persistence fails the fresh Credentials are still returned (in-flight retry
///    keeps working); the error is not propagated.
pub async fn refresh_credentials(
    token_base: &str,
    store: &CredStore,
) -> Result<Credentials, LimitsError> {
    let creds = read_credentials(store)?;

    // (1) Fail fast on empty refresh token — no network call needed.
    if creds.refresh_token.is_empty() {
        return Err(LimitsError::NoRefreshToken);
    }

    let resp = crate::http::client()
        .post(format!("{token_base}/v1/oauth/token"))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": creds.refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send()
        .await?;

    let status = resp.status();

    // Only 400/401 map to Unauthorized; everything else is Bad.
    if !status.is_success() {
        if status == reqwest::StatusCode::BAD_REQUEST
            || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(LimitsError::Unauthorized);
        }
        return Err(LimitsError::Bad(format!("token endpoint status {}", status)));
    }

    let tok: TokenResponse = resp.json().await.map_err(LimitsError::Http)?;

    let fresh = Credentials {
        access_token: tok.access_token.clone(),
        refresh_token: tok.refresh_token.clone().unwrap_or(creds.refresh_token.clone()),
    };

    // (2) Re-read the store; if refreshToken already changed, another writer (Claude
    //     Code) won — skip the write entirely but still return the fresh token.
    let raw = match store.read_raw() {
        Ok(r) => r,
        Err(_) => return Ok(fresh),
    };
    let mut v: serde_json::Value = match serde_json::from_str(raw.trim()) {
        Ok(v) => v,
        Err(_) => return Ok(fresh),
    };

    let on_store_rt = v["claudeAiOauth"]["refreshToken"].as_str().unwrap_or_default();
    if on_store_rt != creds.refresh_token {
        // Another writer already refreshed — skip to avoid clobbering.
        return Ok(fresh);
    }

    // Update fields in the JSON object (preserving all other fields).
    v["claudeAiOauth"]["accessToken"] = tok.access_token.clone().into();
    if let Some(rt) = &tok.refresh_token {
        v["claudeAiOauth"]["refreshToken"] = rt.clone().into();
    }
    if let Some(exp) = tok.expires_in {
        let ms = chrono::Utc::now().timestamp_millis() + (exp as i64) * 1000;
        v["claudeAiOauth"]["expiresAt"] = ms.into();
    }

    // (3,4) Best-effort persist; on failure the fresh token is still returned.
    let serialized = serde_json::to_string(&v).unwrap();
    let _ = store.write_raw(&serialized);

    Ok(fresh)
}

/// High-level: read creds → fetch; on 401 check for a newer token on disk first,
/// then refresh once and retry. 403 is propagated without refreshing.
pub async fn get_limits(usage_base: &str, token_base: &str, store: &CredStore) -> Result<Limits, LimitsError> {
    let creds = read_credentials(store)?;
    match fetch_limits(usage_base, &creds.access_token).await {
        Err(LimitsError::Unauthorized) => {
            // (2) Check if Claude Code already refreshed the token (file or Keychain).
            if let Ok(latest) = read_credentials(store) {
                if latest.access_token != creds.access_token {
                    // A fresher token is already stored — retry without refreshing.
                    return fetch_limits(usage_base, &latest.access_token).await;
                }
            }
            // Token is unchanged — refresh (a no-op error on macOS Keychain).
            let fresh = refresh_credentials(token_base, store).await?;
            fetch_limits(usage_base, &fresh.access_token).await
        }
        // (3) 403 is not refreshable — propagate immediately.
        Err(LimitsError::Forbidden) => Err(LimitsError::Forbidden),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture as fixture;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn reads_credentials_file() {
        let creds = read_credentials(&CredStore::File(fixture("credentials.json"))).unwrap();
        assert_eq!(creds.access_token, "sk-ant-oat01-test");
        assert_eq!(creds.refresh_token, "sk-ant-ort01-test");
    }

    #[test]
    fn parses_keychain_account_from_security_output() {
        // Representative `security find-generic-password -s ...` attribute dump.
        let out = "keychain: \"/Users/me/Library/Keychains/login.keychain-db\"\n\
                   class: \"genp\"\n\
                   attributes:\n    \
                   \"acct\"<blob>=\"cyberstudio\"\n    \
                   \"svce\"<blob>=\"Claude Code-credentials\"\n";
        assert_eq!(parse_keychain_account(out).as_deref(), Some("cyberstudio"));
        assert_eq!(parse_keychain_account("no account here"), None);
    }

    #[tokio::test]
    async fn fetches_limits_from_usage_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer sk-ant-oat01-test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"five_hour":{"utilization":62.0,"resets_at":"2026-06-10T21:20:00+00:00"},
                    "seven_day":{"utilization":34.5,"resets_at":"2026-06-15T11:00:00+00:00"},
                    "seven_day_opus":null,"extra_usage":{"is_enabled":false}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let limits = fetch_limits(&server.uri(), "sk-ant-oat01-test").await.unwrap();
        assert_eq!(limits.session_pct, 62.0);
        assert_eq!(limits.weekly_pct, 34.5);
        assert!(limits.session_resets_at.unwrap().starts_with("2026-06-10"));
    }

    #[tokio::test]
    async fn unauthorized_returns_unauthorized_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = fetch_limits(&server.uri(), "expired").await.unwrap_err();
        assert!(matches!(err, LimitsError::Unauthorized));
    }

    #[tokio::test]
    async fn refresh_token_round_trip_updates_credentials_file() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join(".credentials.json");
        std::fs::copy(fixture("credentials.json"), &creds_path).unwrap();

        let creds = refresh_credentials(&server.uri(), &CredStore::File(creds_path.clone()))
            .await
            .unwrap();
        assert_eq!(creds.access_token, "new-access");

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&creds_path).unwrap()).unwrap();
        assert_eq!(raw["claudeAiOauth"]["accessToken"], "new-access");
        assert_eq!(raw["claudeAiOauth"]["refreshToken"], "new-refresh");
        assert_eq!(raw["claudeAiOauth"]["subscriptionType"], "max");
    }

    #[tokio::test]
    async fn get_limits_refreshes_once_on_401_and_retries() {
        let server = MockServer::start().await;
        // expired token -> 401
        Mock::given(method("GET")).and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer sk-ant-oat01-test"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server).await;
        // refresh endpoint
        Mock::given(method("POST")).and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
                "application/json")).mount(&server).await;
        // fresh token -> 200
        Mock::given(method("GET")).and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer new-access"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"five_hour":{"utilization":10.0,"resets_at":null},"seven_day":{"utilization":5.0,"resets_at":null}}"#,
                "application/json")).mount(&server).await;

        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join(".credentials.json");
        std::fs::copy(fixture("credentials.json"), &creds_path).unwrap();

        let limits = get_limits(&server.uri(), &server.uri(), &CredStore::File(creds_path.clone()))
            .await
            .unwrap();
        assert_eq!(limits.session_pct, 10.0);
        // refreshed token persisted
        let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&creds_path).unwrap()).unwrap();
        assert_eq!(raw["claudeAiOauth"]["accessToken"], "new-access");
    }

    #[tokio::test]
    async fn forbidden_does_not_trigger_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/api/oauth/usage"))
            .respond_with(ResponseTemplate::new(403)).mount(&server).await;
        // NOTE: no /v1/oauth/token mock — a refresh attempt would 404 and the test
        // would fail the match below
        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join(".credentials.json");
        std::fs::copy(fixture("credentials.json"), &creds_path).unwrap();
        let err = get_limits(&server.uri(), &server.uri(), &CredStore::File(creds_path.clone()))
            .await
            .unwrap_err();
        assert!(matches!(err, LimitsError::Forbidden));
    }

    #[tokio::test]
    async fn empty_refresh_token_fails_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let creds_path = tmp.path().join(".credentials.json");
        std::fs::write(&creds_path, r#"{"claudeAiOauth":{"accessToken":"x","refreshToken":""}}"#).unwrap();
        let err = refresh_credentials("http://127.0.0.1:1", &CredStore::File(creds_path.clone()))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, LimitsError::NoRefreshToken));
    }
}
