use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct KimiConfig {
    pub managed_base_url: Option<String>,
    pub oauth_key: Option<String>,
    pub oauth_storage_file: bool,
    pub moonshot_base_url: Option<String>,
    pub moonshot_api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KimiToken {
    pub access_token: String,
    /// Epoch seconds.
    pub expires_at: i64,
}

fn non_empty(t: Option<&toml::Table>, k: &str) -> Option<String> {
    t?.get(k)?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn table<'a>(t: Option<&'a toml::Table>, k: &str) -> Option<&'a toml::Table> {
    t?.get(k)?.as_table()
}

pub fn read_config(kimi_dir: &Path) -> Option<KimiConfig> {
    let raw = std::fs::read_to_string(kimi_dir.join("config.toml")).ok()?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let root: toml::Table = raw.parse().ok()?;
    let providers = table(Some(&root), "providers");
    let managed = table(providers, "managed:kimi-code");
    let oauth = table(managed, "oauth");
    let moonshot = table(providers, "moonshot-ai");
    Some(KimiConfig {
        managed_base_url: non_empty(managed, "base_url"),
        oauth_key: non_empty(oauth, "key"),
        oauth_storage_file: oauth
            .and_then(|o| o.get("storage")?.as_str())
            .is_some_and(|s| s == "file"),
        moonshot_base_url: non_empty(moonshot, "base_url"),
        moonshot_api_key: non_empty(moonshot, "api_key"),
    })
}

pub fn credentials_path(kimi_dir: &Path, cfg: &KimiConfig) -> Option<PathBuf> {
    let dir = kimi_dir.join("credentials");
    if let Some(key) = cfg.oauth_key.as_deref() {
        let name = key.strip_prefix("oauth/").unwrap_or(key);
        // Reject anything that could escape the credentials dir.
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            return None;
        }
        return Some(dir.join(format!("{name}.json")));
    }
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

pub fn read_token(path: &Path) -> Option<KimiToken> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value =
        serde_json::from_str(raw.strip_prefix('\u{feff}').unwrap_or(&raw)).ok()?;
    let access_token = v.get("access_token")?.as_str().filter(|s| !s.is_empty())?.to_string();
    let expires_at = v.get("expires_at")?.as_f64()? as i64;
    Some(KimiToken { access_token, expires_at })
}

pub fn has_subscription(kimi_dir: &Path, cfg: &KimiConfig) -> bool {
    cfg.managed_base_url.as_deref().is_some_and(super::host_allowed)
        && cfg.oauth_storage_file
        && credentials_path(kimi_dir, cfg)
            .and_then(|p| read_token(&p))
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture;

    fn fixture() -> PathBuf {
        test_fixture("kimi")
    }

    fn fixture_cred() -> String {
        std::fs::read_to_string(fixture().join("credentials/kimi-code-env-test.json")).unwrap()
    }

    fn fixture_config_text() -> String {
        std::fs::read_to_string(fixture().join("config.toml")).unwrap()
    }

    fn tmp_with(config: &str, cred: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), config).unwrap();
        if let Some(c) = cred {
            std::fs::create_dir(d.path().join("credentials")).unwrap();
            std::fs::write(d.path().join("credentials/kimi-code-env-test.json"), c).unwrap();
        }
        d
    }

    #[test]
    fn reads_fixture_config() {
        let cfg = read_config(&fixture()).unwrap();
        assert_eq!(cfg.managed_base_url.as_deref(), Some("https://api.kimi.ai/coding/v1"));
        assert_eq!(cfg.oauth_key.as_deref(), Some("oauth/kimi-code-env-test"));
        assert!(cfg.oauth_storage_file);
        assert_eq!(cfg.moonshot_base_url.as_deref(), Some("https://api.moonshot.ai/v1"));
        assert_eq!(cfg.moonshot_api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn subscription_detected_from_fixture() {
        let cfg = read_config(&fixture()).unwrap();
        assert!(has_subscription(&fixture(), &cfg));
        let p = credentials_path(&fixture(), &cfg).unwrap();
        let t = read_token(&p).unwrap();
        assert_eq!(t.access_token, "tok-test");
        assert_eq!(t.expires_at, 1794069442);
    }

    #[test]
    fn missing_or_corrupt_credentials_means_no_subscription() {
        let d = tmp_with(&fixture_config_text(), None);
        let cfg = read_config(d.path()).unwrap();
        assert!(!has_subscription(d.path(), &cfg));
        let d = tmp_with(&fixture_config_text(), Some("{not json"));
        let cfg = read_config(d.path()).unwrap();
        assert!(!has_subscription(d.path(), &cfg));
    }

    #[test]
    fn non_file_storage_means_no_subscription() {
        let text = fixture_config_text().replace("storage = \"file\"", "storage = \"keyring\"");
        let d = tmp_with(&text, Some(&fixture_cred()));
        let cfg = read_config(d.path()).unwrap();
        assert!(!cfg.oauth_storage_file);
        assert!(!has_subscription(d.path(), &cfg));
    }

    #[test]
    fn disallowed_base_url_means_no_subscription() {
        let text = fixture_config_text().replace(
            "base_url = \"https://api.kimi.ai/coding/v1\"",
            "base_url = \"https://proxy.example.com/v1\"",
        );
        let d = tmp_with(&text, Some(&fixture_cred()));
        let cfg = read_config(d.path()).unwrap();
        assert_eq!(cfg.managed_base_url.as_deref(), Some("https://proxy.example.com/v1"));
        assert!(!has_subscription(d.path(), &cfg));
    }

    #[test]
    fn config_without_managed_provider() {
        let d = tmp_with("[providers.moonshot-ai]\napi_key = \"sk-x\"\n", None);
        let cfg = read_config(d.path()).unwrap();
        assert_eq!(cfg.managed_base_url, None);
        assert_eq!(cfg.moonshot_api_key.as_deref(), Some("sk-x"));
        assert!(!has_subscription(d.path(), &cfg));
    }

    #[test]
    fn newest_credentials_file_used_without_key() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("credentials")).unwrap();
        std::fs::write(d.path().join("credentials/a.json"), "{}").unwrap();
        let cfg = KimiConfig::default();
        assert_eq!(credentials_path(d.path(), &cfg), Some(d.path().join("credentials/a.json")));
    }

    #[test]
    fn bom_and_path_traversal_handled() {
        let d = tmp_with(&format!("\u{feff}{}", fixture_config_text()), None);
        assert!(read_config(d.path()).is_some());
        let cfg = KimiConfig { oauth_key: Some("oauth/../../x".into()), ..Default::default() };
        assert_eq!(credentials_path(d.path(), &cfg), None);
    }
}
