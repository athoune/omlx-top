//! Typed GETs for the three endpoints consumed by the monitor.
//!
//! Every struct uses `#[serde(default)]` and `Option` fields: unknown keys are
//! ignored and a missing key becomes `None` (SPECS.md section 8.1). No field
//! is required, so a short payload degrades instead of failing.

use serde::Deserialize;
use serde_json::Value;

use super::{Client, HttpError, HttpReply};

/// `GET /api/status` (SPECS.md section 2.3).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct StatusResponse {
    pub status: Option<String>,
    pub version: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub models_discovered: Option<u64>,
    pub models_loaded: Option<u64>,
    pub models_loading: Option<u64>,
    pub default_model: Option<String>,
    pub loaded_models: Option<Vec<String>>,
    pub total_requests: Option<u64>,
    pub active_requests: Option<u64>,
    pub waiting_requests: Option<u64>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub total_cached_tokens: Option<u64>,
    pub cache_efficiency: Option<f64>,
    pub avg_prefill_tps: Option<f64>,
    pub avg_generation_tps: Option<f64>,
    pub model_memory_used: Option<u64>,
    pub model_memory_max: Option<u64>,
}

/// `engine_pool` from `GET /health`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct EnginePool {
    pub model_count: Option<u64>,
    pub loaded_count: Option<u64>,
    pub final_ceiling: Option<u64>,
    pub current_model_memory: Option<u64>,
}

/// `GET /health` (SPECS.md section 2.3). Answers 503 with
/// `{"status": "loading"}` while the startup preload runs.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct HealthResponse {
    pub status: Option<String>,
    pub default_model: Option<String>,
    pub engine_pool: Option<EnginePool>,
}

/// One entry of the `models` array in `GET /v1/models/status`.
///
/// Only the fields the monitor consumes are declared; the endpoint returns
/// many more and they are ignored by serde.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ModelEntry {
    pub id: Option<String>,
    pub loaded: Option<bool>,
    pub is_loading: Option<bool>,
    pub estimated_size: Option<u64>,
    pub resident_estimated_size: Option<u64>,
    pub actual_size: Option<u64>,
    pub engine_type: Option<String>,
    pub model_type: Option<String>,
    pub model_context_length: Option<u64>,
    pub max_context_window: Option<u64>,
    pub pinned: Option<bool>,
    pub is_helper: Option<bool>,
    pub is_hidden: Option<bool>,
    pub distributed: Option<bool>,
    pub external_engine: Option<Value>,
    pub last_access: Option<Value>,
    pub model_path: Option<String>,
}

/// `GET /v1/models/status`. This document has no top-level `status` key; the
/// pool counters live at the top level instead.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ModelsResponse {
    pub model_count: Option<u64>,
    pub loaded_count: Option<u64>,
    pub current_model_memory: Option<u64>,
    pub final_ceiling: Option<u64>,
    pub models: Option<Vec<ModelEntry>>,
}

impl Client {
    /// Primary status document.
    pub fn status(&self) -> Result<HttpReply<StatusResponse>, HttpError> {
        self.get("/api/status")
    }

    /// Liveness and loading discriminator.
    pub fn health(&self) -> Result<HttpReply<HealthResponse>, HttpError> {
        self.get("/health")
    }

    /// Per-model table, refreshed on a 30 s cadence.
    pub fn models(&self) -> Result<HttpReply<ModelsResponse>, HttpError> {
        self.get("/v1/models/status")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_GOLDEN: &str = include_str!("../../tests/golden/status.json");
    const HEALTH_GOLDEN: &str = include_str!("../../tests/golden/health.json");
    const MODELS_GOLDEN: &str = include_str!("../../tests/golden/models.json");

    #[test]
    fn status_golden_deserialises() {
        let status: StatusResponse = serde_json::from_str(STATUS_GOLDEN).expect("golden parses");
        assert_eq!(status.status.as_deref(), Some("ok"));
        assert_eq!(status.version.as_deref(), Some("0.7.3"));
        assert!(status.uptime_seconds.is_some());
        assert_eq!(status.models_discovered, Some(19));
        assert_eq!(status.total_requests, Some(0));
        assert_eq!(status.model_memory_used, Some(0));
        assert!(status.model_memory_max.is_some());
    }

    #[test]
    fn health_golden_deserialises() {
        let health: HealthResponse = serde_json::from_str(HEALTH_GOLDEN).expect("golden parses");
        assert_eq!(health.status.as_deref(), Some("healthy"));
        let pool = health.engine_pool.expect("engine_pool");
        assert_eq!(pool.model_count, Some(19));
        assert_eq!(pool.loaded_count, Some(0));
        assert!(pool.final_ceiling.is_some());
    }

    #[test]
    fn models_golden_deserialises() {
        let models: ModelsResponse = serde_json::from_str(MODELS_GOLDEN).expect("golden parses");
        // This is the raw endpoint, which includes the built-in converter:
        // 20 entries, 1 of which is MarkItDown. `build_models` filters it down
        // to 19 discovered and 0 loaded.
        assert_eq!(models.model_count, Some(20));
        assert_eq!(models.loaded_count, Some(1));

        let entries = models.models.expect("models array");
        assert_eq!(entries.len(), 20);
        assert!(entries.iter().all(|entry| entry.id.is_some()));
        let loaded: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.loaded == Some(true))
            .filter_map(|entry| entry.id.as_deref())
            .collect();
        assert_eq!(loaded, vec!["MarkItDown"]);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let status: StatusResponse = serde_json::from_str(
            r#"{"status":"ok","brand_new_field":{"nested":[1,2,3]},"another":null}"#,
        )
        .expect("unknown fields do not fail");
        assert_eq!(status.status.as_deref(), Some("ok"));
        assert_eq!(status.version, None);

        let models: ModelsResponse =
            serde_json::from_str(r#"{"models":[{"id":"x","unknown":true}],"unknown_top":1}"#)
                .expect("unknown fields do not fail");
        assert_eq!(models.models.expect("array")[0].id.as_deref(), Some("x"));
        assert_eq!(models.model_count, None);
    }

    #[test]
    fn missing_fields_become_none_not_errors() {
        let status: StatusResponse = serde_json::from_str("{}").expect("empty object parses");
        assert_eq!(status, StatusResponse::default());
        assert_eq!(status.status, None);
        assert_eq!(status.model_memory_max, None);

        let health: HealthResponse =
            serde_json::from_str(r#"{"status":"loading"}"#).expect("short 503 body parses");
        assert_eq!(health.status.as_deref(), Some("loading"));
        assert_eq!(health.engine_pool, None);

        let models: ModelsResponse = serde_json::from_str("{}").expect("empty object parses");
        assert_eq!(models.models, None);
        assert_eq!(models.model_count, None);
    }

    #[test]
    fn null_values_become_none() {
        let models: ModelsResponse = serde_json::from_str(
            r#"{"model_count":3,"models":[{"id":"a","loaded":null,"actual_size":null}]}"#,
        )
        .expect("nulls parse");
        assert_eq!(models.models.expect("array")[0].loaded, None);
    }
}
