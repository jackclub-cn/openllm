//! Capability metadata sourced from <https://models.dev>.
//!
//! The catalog is a large (~5 MB) document, so it is fetched at most once per
//! [`CATALOG_TTL`] and shared through [`AppState`]. Enrichment is treated as
//! best-effort: a network or parse failure must never break provider model
//! sync, it only means models keep no capability metadata.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::models::ModelCapabilities;
use crate::state::AppState;

const DEFAULT_API_URL: &str = "https://models.dev/api.json";
const CATALOG_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Overridable so tests (and air-gapped setups) can point at a local mirror.
fn api_url() -> String {
    std::env::var("OPENLLM_MODELS_DEV_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

#[derive(Debug, Clone)]
struct ProviderEntry {
    name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: HashMap<String, ProviderEntry>,
    /// model name -> (models.dev provider id, capabilities)
    by_model: HashMap<String, Vec<(String, ModelCapabilities)>>,
    /// Normalized model name -> keys into `by_model`.
    ///
    /// Vendors spell the same model differently ("Qwen/Qwen3.7-Flash" vs
    /// "qwen3.7-flash"), so an exact-key lookup alone misses metadata that is
    /// actually present. This index lets a normalized match find it.
    by_normalized: HashMap<String, Vec<String>>,
}

impl Catalog {
    pub fn parse(bytes: &[u8]) -> AppResult<Self> {
        let root: Value = serde_json::from_slice(bytes).map_err(|error| {
            AppError::BadRequest(format!("invalid models.dev payload: {error}"))
        })?;
        let Some(providers) = root.as_object() else {
            return Err(AppError::BadRequest(
                "models.dev payload must be a JSON object".to_string(),
            ));
        };
        let mut catalog = Self::default();
        for (key, entry) in providers {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_string();
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_string();
            catalog.providers.insert(id.clone(), ProviderEntry { name });
            if let Some(models) = entry.get("models").and_then(Value::as_object) {
                for (model_name, model) in models {
                    catalog
                        .by_model
                        .entry(model_name.clone())
                        .or_default()
                        .push((id.clone(), parse_capabilities(model)));
                    // Index by the normalized full name and by the vendor-less
                    // leaf, since the same model appears as "qwen3.7-flash"
                    // upstream and "Qwen/Qwen3.7-Flash" (or vice versa).
                    for candidate in [model_name.as_str(), model_leaf(model_name.as_str())] {
                        let keys = catalog
                            .by_normalized
                            .entry(normalize(candidate))
                            .or_default();
                        if !keys.contains(model_name) {
                            keys.push(model_name.clone());
                        }
                    }
                }
            }
        }
        Ok(catalog)
    }

    pub fn is_empty(&self) -> bool {
        self.by_model.is_empty()
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn model_count(&self) -> usize {
        self.by_model.len()
    }

    /// Best-effort match of a configured provider to a models.dev provider id,
    /// used only to prefer that vendor's copy of a model when several vendors
    /// publish the same model name.
    pub fn match_provider(&self, name: &str, base_url: &str) -> Option<String> {
        let mut ids = self.providers.keys().cloned().collect::<Vec<_>>();
        ids.sort();

        // Host labels first: api.openai.com -> "openai", generativelanguage.googleapis.com
        // has no exact "google" label but does start with it.
        let host = base_url
            .split("://")
            .nth(1)
            .unwrap_or(base_url)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let labels = host
            .split(['.', '-'])
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>();
        for id in &ids {
            if id.len() >= 3 && labels.iter().any(|label| *label == id) {
                return Some(id.clone());
            }
            if id.len() >= 4 && labels.iter().any(|label| label.starts_with(id.as_str())) {
                return Some(id.clone());
            }
        }

        // Then display-name/id equality.
        let normalized = normalize(name);
        if normalized.is_empty() {
            return None;
        }
        for id in &ids {
            let entry = &self.providers[id];
            if normalize(&entry.name) == normalized || normalize(id) == normalized {
                return Some(id.clone());
            }
        }
        None
    }

    /// Capability metadata for `model`. When `provider_hint` is present that
    /// vendor's entry wins; otherwise the most authoritative copy is chosen
    /// deterministically (canonical entries first, then provider id).
    pub fn lookup(&self, provider_hint: Option<&str>, model: &str) -> Option<ModelCapabilities> {
        // Prefer an exact key match; fall back to a normalized one so vendors
        // that spell a model differently still resolve to the same metadata.
        let candidates = match self.by_model.get(model) {
            Some(candidates) => candidates,
            None => {
                let keys = self
                    .by_normalized
                    .get(&normalize(model_leaf(model)))
                    .or_else(|| self.by_normalized.get(&normalize(model)))?;
                // Collect every candidate behind the matched names, preserving
                // the provider-preference logic below.
                let merged = keys
                    .iter()
                    .filter_map(|key| self.by_model.get(key))
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>();
                if merged.is_empty() {
                    return None;
                }
                return Self::pick(&merged, provider_hint);
            }
        };
        Self::pick(candidates, provider_hint)
    }

    /// Chooses the best candidate for a model: the caller's own vendor when it
    /// is known, otherwise a canonical entry, then a deterministic fallback.
    fn pick(
        candidates: &[(String, ModelCapabilities)],
        provider_hint: Option<&str>,
    ) -> Option<ModelCapabilities> {
        if let Some(hint) = provider_hint
            && let Some((_, capabilities)) =
                candidates.iter().find(|(provider, _)| provider == hint)
        {
            return Some(capabilities.clone());
        }
        let mut sorted = candidates.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            let a_canonical = a.1.canonical_model_id.is_some();
            let b_canonical = b.1.canonical_model_id.is_some();
            b_canonical.cmp(&a_canonical).then_with(|| a.0.cmp(&b.0))
        });
        sorted.first().map(|(_, capabilities)| capabilities.clone())
    }
}

/// The model name with any vendor prefix removed, e.g. `Qwen/Qwen3.7-Flash`
/// -> `Qwen3.7-Flash`. Providers are inconsistent about including the vendor
/// prefix, so matching happens on the leaf.
fn model_leaf(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse_capabilities(model: &Value) -> ModelCapabilities {
    let limit = model.get("limit");
    let number = |key: &str| -> Option<i64> {
        limit
            .and_then(|limit| limit.get(key))
            .and_then(Value::as_i64)
    };
    ModelCapabilities {
        context_limit: number("context"),
        output_limit: number("output"),
        input_limit: number("input"),
        attachment: model.get("attachment").and_then(Value::as_bool),
        reasoning: model.get("reasoning").and_then(Value::as_bool),
        tool_call: model.get("tool_call").and_then(Value::as_bool),
        structured_output: model.get("structured_output").and_then(Value::as_bool),
        temperature: model.get("temperature").and_then(Value::as_bool),
        open_weights: model.get("open_weights").and_then(Value::as_bool),
        input_modalities: modality_list(model, "input"),
        output_modalities: modality_list(model, "output"),
        cost: model.get("cost").filter(|v| v.is_object()).cloned(),
        family: string_field(model, "family"),
        knowledge: string_field(model, "knowledge"),
        release_date: string_field(model, "release_date"),
        last_updated: string_field(model, "last_updated"),
        canonical_model_id: string_field(model, "canonical_model_id"),
        total_context_tokens: None,
    }
    // models.dev also publishes both a window and a smaller input cap; collapse
    // them so the gateway never advertises two different context numbers.
    .with_effective_input_limit()
}

/// Reads `modalities.input` / `modalities.output` and flattens each to a list of
/// strings, so the public payload never nests a list under an `input`/`output`
/// key (which crashes some OpenAI-compatible clients).
fn modality_list(model: &Value, key: &str) -> Option<Vec<String>> {
    let items = model.get("modalities")?.get(key)?.as_array()?;
    let values = items
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}

fn string_field(model: &Value, key: &str) -> Option<String> {
    model
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// Returns a cached catalog, refreshing it when the TTL has elapsed.
pub async fn load(state: &AppState) -> AppResult<Arc<Catalog>> {
    if let Some((fetched_at, catalog)) = state.models_dev.read().await.as_ref()
        && fetched_at.elapsed() < CATALOG_TTL
    {
        return Ok(catalog.clone());
    }

    let url = api_url();
    let response = state
        .client
        .get(&url)
        .send()
        .await
        .map_err(|error| AppError::Upstream(format!("failed to fetch models.dev: {error}")))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!("models.dev returned {status}")));
    }
    let catalog = Arc::new(Catalog::parse(&bytes)?);
    *state.models_dev.write().await = Some((Instant::now(), catalog.clone()));
    Ok(catalog)
}

/// Best-effort enrichment: failures are logged and reported as "no metadata"
/// rather than propagated, so provider sync keeps working offline.
pub async fn try_load(state: &AppState) -> Option<Arc<Catalog>> {
    match load(state).await {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            tracing::warn!(%error, "models.dev metadata unavailable; syncing models without capabilities");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        br#"{
          "openai": {
            "id": "openai",
            "name": "OpenAI",
            "models": {
              "gpt-5.4": {
                "id": "gpt-5.4",
                "limit": {"context": 400000, "output": 128000},
                "attachment": true,
                "reasoning": true,
                "tool_call": true,
                "modalities": {"input": ["text","image"], "output": ["text"]},
                "cost": {"input": 1.25, "output": 10},
                "canonical_model_id": "openai/gpt-5.4"
              }
            }
          },
          "openrouter": {
            "id": "openrouter",
            "name": "OpenRouter",
            "models": {
              "gpt-5.4": {
                "id": "gpt-5.4",
                "limit": {"context": 400000, "output": 64000},
                "tool_call": true,
                "modalities": {"input": ["text"], "output": ["text"]}
              }
            }
          }
        }"#
        .to_vec()
    }

    #[test]
    fn parses_and_matches_provider_from_host() {
        let catalog = Catalog::parse(&sample()).unwrap();
        assert_eq!(catalog.provider_count(), 2);
        assert_eq!(
            catalog.match_provider("My Gateway", "https://api.openai.com/v1"),
            Some("openai".to_string())
        );
        assert_eq!(
            catalog.match_provider("OpenRouter", "https://openrouter.ai/api/v1"),
            Some("openrouter".to_string())
        );
    }

    #[test]
    fn provider_hint_wins_over_other_vendors() {
        let catalog = Catalog::parse(&sample()).unwrap();
        let preferred = catalog.lookup(Some("openai"), "gpt-5.4").unwrap();
        assert_eq!(preferred.output_limit, Some(128000));
        let other = catalog.lookup(Some("openrouter"), "gpt-5.4").unwrap();
        assert_eq!(other.output_limit, Some(64000));
        // No hint: canonical entry wins deterministically.
        assert_eq!(
            catalog.lookup(None, "gpt-5.4").unwrap().output_limit,
            Some(128000)
        );
        assert!(catalog.lookup(None, "missing-model").is_none());
    }

    #[test]
    fn matches_models_across_naming_styles() {
        // models.dev stores "qwen3.7-flash"; the provider exposes
        // "Qwen/Qwen3.7-Flash". An exact lookup misses it and the model wrongly
        // appeared to have no metadata.
        let bytes = br#"{
          "alibaba": {
            "id": "alibaba",
            "name": "Alibaba",
            "models": {
              "qwen3.7-flash": {
                "id": "qwen3.7-flash",
                "limit": {"context": 1000000, "output": 65536}
              }
            }
          }
        }"#
        .to_vec();
        let catalog = Catalog::parse(&bytes).unwrap();

        // Exact key still works.
        assert_eq!(
            catalog.lookup(None, "qwen3.7-flash").unwrap().output_limit,
            Some(65536)
        );
        // Vendor-prefixed and differently-cased/spaced spellings now resolve.
        for spelling in [
            "Qwen/Qwen3.7-Flash",
            "qwen/Qwen3.7-flash",
            "Qwen3.7-Flash",
            "qwen3.7-flash",
        ] {
            let found = catalog
                .lookup(None, spelling)
                .unwrap_or_else(|| panic!("{spelling} should resolve"));
            assert_eq!(found.output_limit, Some(65536), "{spelling}");
        }
        // A genuinely unknown model still reports nothing.
        assert!(catalog.lookup(None, "totally-unknown-model").is_none());
    }
}
