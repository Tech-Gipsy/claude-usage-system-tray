//! Shared HTTP client: one connection pool for all Anthropic API calls.

use std::sync::OnceLock;

/// Generous enough for the paginated cost report; usage fetches finish well under it.
const TIMEOUT_SECS: u64 = 15;

pub(crate) fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("reqwest client build failed")
    })
}
