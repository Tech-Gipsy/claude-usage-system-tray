use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub limits: Option<Limits>,
    pub local: Option<LocalStats>,
    pub api_spend: Option<ApiSpend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    pub session_pct: f32,
    pub session_resets_at: Option<String>, // ISO 8601
    pub weekly_pct: f32,
    pub weekly_resets_at: Option<String>,
    pub fetched_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayStat {
    pub date: String, // YYYY-MM-DD local
    pub tokens: u64,
    pub cost_usd: Option<f64>, // None when only an estimate is impossible
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalStats {
    pub today_tokens: u64,
    pub today_cost_usd: f64,
    pub days: Vec<DayStat>,        // last 7 days, oldest first, includes today
    pub total_30d_tokens: u64,
    pub total_30d_cost_usd: f64,   // ≈ estimate, blended rate
    pub cost_is_estimate: bool,
    pub fetched_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSpend {
    pub month_to_date_usd: f64,
    pub fetched_at: String,
    pub stale: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_through_json() {
        let snap = UsageSnapshot {
            limits: Some(Limits {
                session_pct: 62.0,
                session_resets_at: Some("2026-06-10T21:20:00+00:00".into()),
                weekly_pct: 34.0,
                weekly_resets_at: None,
                fetched_at: "2026-06-10T10:00:00Z".into(),
                stale: false,
            }),
            local: None,
            api_spend: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: UsageSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.limits.as_ref().unwrap().session_pct, 62.0);
        assert!(back.local.is_none());
    }
}
