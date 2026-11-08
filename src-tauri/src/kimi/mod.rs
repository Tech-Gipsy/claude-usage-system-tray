pub mod config;

use std::path::PathBuf;

/// `~/.kimi-code` if it exists and is a directory.
pub fn kimi_dir() -> Option<PathBuf> {
    let d = dirs::home_dir()?.join(".kimi-code");
    d.is_dir().then_some(d)
}

const ALLOWED_DOMAINS: [&str; 4] = ["kimi.com", "kimi.ai", "moonshot.ai", "moonshot.cn"];

/// Credentials may only be sent to https hosts that are, or are subdomains of,
/// an allowed Kimi/Moonshot domain.
pub fn host_allowed(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    if u.scheme() != "https" {
        return false;
    }
    let Some(host) = u.host_str() else { return false };
    let host = host.to_ascii_lowercase();
    ALLOWED_DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist() {
        assert!(host_allowed("https://api.kimi.ai/coding/v1"));
        assert!(host_allowed("https://api.kimi.com/coding/v1"));
        assert!(host_allowed("https://api.moonshot.cn/v1"));
        assert!(!host_allowed("http://api.kimi.ai/coding/v1"));
        assert!(!host_allowed("https://evilkimi.ai/v1"));
        assert!(!host_allowed("https://kimi.ai.evil.com/v1"));
        assert!(!host_allowed("not a url"));
    }
}
