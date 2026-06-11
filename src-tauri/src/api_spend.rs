use serde::Deserialize;

pub const ADMIN_BASE: &str = "https://api.anthropic.com";
const KEYRING_SERVICE: &str = "claude-usage-meter";
const KEYRING_USER: &str = "anthropic-admin-key";

#[derive(Deserialize)]
struct CostResult {
    amount: String, // decimal string, CENTS
}

#[derive(Deserialize)]
struct CostBucket {
    #[serde(default)]
    results: Vec<CostResult>,
}

#[derive(Deserialize)]
struct CostReport {
    #[serde(default)]
    data: Vec<CostBucket>,
    #[serde(default)]
    has_more: bool,
    next_page: Option<String>,
}

pub async fn fetch_month_to_date(
    base: &str,
    admin_key: &str,
    starting_at: &str,
    ending_at: &str,
) -> Result<f64, String> {
    const MAX_PAGES: usize = 50;
    let mut total_cents = 0.0f64;
    let mut page: Option<String> = None;
    let mut page_count = 0usize;
    loop {
        let mut req = crate::http::client()
            .get(format!("{base}/v1/organizations/cost_report"))
            .query(&[("starting_at", starting_at), ("ending_at", ending_at)])
            .header("x-api-key", admin_key)
            .header("anthropic-version", "2023-06-01");
        if let Some(p) = &page {
            req = req.query(&[("page", p.as_str())]);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("cost_report status {}", resp.status()));
        }
        let report: CostReport = resp.json().await.map_err(|e| e.to_string())?;
        for bucket in &report.data {
            for r in &bucket.results {
                // unparseable amounts are skipped (display app; better to undercount than error out)
                total_cents += r.amount.parse::<f64>().unwrap_or(0.0);
            }
        }
        page_count += 1;
        if page_count >= MAX_PAGES {
            break;
        }
        if report.has_more && report.next_page.is_some() {
            page = report.next_page;
        } else {
            break;
        }
    }
    Ok(total_cents / 100.0)
}

// ---------- Windows Credential Manager storage ----------

pub fn save_admin_key(key: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|e| e.set_password(key))
        .map_err(|e| e.to_string())
}

pub fn load_admin_key() -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .ok()?
        .get_password()
        .ok()
}

pub fn clear_admin_key() -> Result<(), String> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|e| e.delete_credential())
    {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// First moment of the current month in UTC, RFC3339 — the cost report window start.
pub fn month_start_utc() -> String {
    let now = chrono::Utc::now();
    format!("{}-01T00:00:00Z", now.format("%Y-%m"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn sums_cost_report_amounts_in_cents() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/cost_report"))
            .and(header("x-api-key", "sk-ant-admin-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":[
                     {"starting_at":"2026-06-01T00:00:00Z","ending_at":"2026-06-02T00:00:00Z",
                      "results":[{"amount":"1234.56","currency":"USD"}]},
                     {"starting_at":"2026-06-02T00:00:00Z","ending_at":"2026-06-03T00:00:00Z",
                      "results":[{"amount":"100","currency":"USD"},{"amount":"5.5","currency":"USD"}]}
                   ],"has_more":false,"next_page":null}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let usd = fetch_month_to_date(&server.uri(), "sk-ant-admin-test",
            "2026-06-01T00:00:00Z", "2026-06-10T00:00:00Z").await.unwrap();
        // (1234.56 + 100 + 5.5) cents = $13.4006
        assert!((usd - 13.4006).abs() < 1e-9);
    }

    #[tokio::test]
    async fn follows_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/cost_report"))
            .and(query_param("page", "p2"))
            .and(header("x-api-key", "k"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":[{"starting_at":"x","ending_at":"y","results":[{"amount":"100","currency":"USD"}]}],"has_more":false,"next_page":null}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/organizations/cost_report"))
            .and(wiremock::matchers::query_param_is_missing("page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":[{"starting_at":"x","ending_at":"y","results":[{"amount":"100","currency":"USD"}]}],"has_more":true,"next_page":"p2"}"#,
                "application/json",
            ))
            .mount(&server)
            .await;

        let usd = fetch_month_to_date(&server.uri(), "k", "a", "b").await.unwrap();
        assert!((usd - 2.0).abs() < 1e-9);
    }
}
