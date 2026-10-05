//! Usage query and page shapes; the backend's `UsageStore` answers them.

use serde::{Deserialize, Serialize};

use crate::contracts::{RequestActivity, UsageSummary};

#[derive(Clone, Debug, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct UsageQuery {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub day: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UsagePage {
    pub items: Vec<RequestActivity>,
    pub next_cursor: Option<String>,
    pub summary: UsageSummary,
    pub series: Vec<UsagePoint>,
    pub model_series: Vec<UsageModelPoint>,
    pub agents: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelPoint {
    pub day: String,
    pub model: Option<String>,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}
