use super::*;

#[derive(Debug, Serialize)]
pub struct PublicModel {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: &'static str,
    /// Upstream provider the model resolves to. Absent on route entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The upstream model name this entry forwards to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// Effective capability envelope. For routes this is the barrel (strictest
    /// common) intersection across every enabled target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
    /// Number of enabled targets behind a route entry. Absent for a model that
    /// resolves directly to a single provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_count: Option<usize>,
    /// Present on route entries. `false` means at least one target lacks
    /// metadata, so `capabilities` is a lower bound rather than a verified
    /// guarantee that every target accepts the advertised limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits_verified: Option<bool>,
    /// Flat, de-facto-standard limit names.
    ///
    /// OpenAI-compatible clients (Hermes, LiteLLM, assorted routers) read
    /// these specific keys, and several only walk top-level fields. They mirror
    /// the nested `capabilities` values so both styles of client work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
    /// Friendly label for clients that show one (Anthropic's `display_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Endpoint paths this model is known to support.
    ///
    /// Omitted means the provider did not declare endpoint capabilities and
    /// the gateway treats the model as eligible for any compatible endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_endpoints: Option<Vec<String>>,
}

impl PublicModel {
    /// Fills the flat limit fields from a capability envelope.
    ///
    /// `context_limit` is the total window, so it doubles as the input ceiling
    /// when no separate input limit is published; that matches how clients
    /// interpret `max_input_tokens`.
    pub fn with_flat_limits(mut self) -> Self {
        if let Some(capabilities) = self.capabilities.as_ref() {
            self.context_length = capabilities.context_limit;
            self.max_input_tokens = capabilities.input_limit.or(capabilities.context_limit);
            self.max_output_tokens = capabilities.output_limit;
            self.max_completion_tokens = capabilities.output_limit;
        }
        self
    }
}

/// Capability metadata mirrored from models.dev. Every field is optional so an
/// unknown value stays distinguishable from a known `false` or `0`, which
/// matters when intersecting several targets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_weights: Option<bool>,
    /// Accepted input modalities, e.g. `["text", "image"]`.
    ///
    /// Deliberately a top-level list rather than nested under an
    /// `input`/`output` key: OpenAI-compatible clients walk nested dicts
    /// looking for `input`/`output` *price* fields, and a list value there
    /// raises `TypeError: unhashable type: 'list'`, which silently discards the
    /// whole endpoint's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_model_id: Option<String>,
    /// The model's full context window, when it is larger than the input the
    /// client may actually send.
    ///
    /// `context_limit` reports the safe input capacity so every context-named
    /// key tells a client the same thing. The raw window is kept here so the
    /// information is not lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_context_tokens: Option<i64>,
}

impl ModelCapabilities {
    /// Collapses the window/input distinction into one conservative input cap.
    ///
    /// Providers report both a total window and a (smaller) maximum input, and
    /// clients read either key to decide how much to send. Exposing different
    /// numbers invites a client to pick the optimistic one and exceed the real
    /// limit, so both are published as the smaller value; the untouched window
    /// moves to `total_context_tokens`.
    pub fn with_effective_input_limit(mut self) -> Self {
        if let Some(window) = self.context_limit {
            // A declared input limit is authoritative when it is the stricter
            // of the two; otherwise the window is the cap.
            let effective = match self.input_limit {
                Some(input) => input.min(window),
                None => window,
            };
            if effective != window {
                self.total_context_tokens = Some(window);
            }
            self.context_limit = Some(effective);
            self.input_limit = Some(effective);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Barrel/intersection: the strictest common envelope across `targets`.
    ///
    /// Numeric limits take the minimum of the known values, booleans are only
    /// true when every target that reports the flag says true, and modality
    /// lists keep only what all targets accept. Descriptive fields (family,
    /// cost, release dates) are intentionally dropped because a route can span
    /// unrelated models where they have no common meaning.
    pub fn intersect<'a>(targets: impl IntoIterator<Item = &'a ModelCapabilities>) -> Option<Self> {
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() {
            return None;
        }
        let min = |f: fn(&ModelCapabilities) -> Option<i64>| -> Option<i64> {
            targets.iter().filter_map(|c| f(c)).min()
        };
        let all_true = |f: fn(&ModelCapabilities) -> Option<bool>| -> Option<bool> {
            let known = targets.iter().filter_map(|c| f(c)).collect::<Vec<_>>();
            if known.is_empty() {
                None
            } else {
                Some(known.iter().all(|v| *v))
            }
        };
        let capabilities = Self {
            context_limit: min(|c| c.context_limit),
            output_limit: min(|c| c.output_limit),
            input_limit: min(|c| c.input_limit),
            attachment: all_true(|c| c.attachment),
            reasoning: all_true(|c| c.reasoning),
            tool_call: all_true(|c| c.tool_call),
            structured_output: all_true(|c| c.structured_output),
            temperature: all_true(|c| c.temperature),
            open_weights: all_true(|c| c.open_weights),
            input_modalities: intersect_modalities(&targets, |c| c.input_modalities.as_ref()),
            output_modalities: intersect_modalities(&targets, |c| c.output_modalities.as_ref()),
            cost: None,
            family: None,
            knowledge: None,
            release_date: None,
            last_updated: None,
            canonical_model_id: None,
            // The raw window is intersected like any other ceiling so a route
            // never advertises a larger window than its narrowest target.
            total_context_tokens: min(|c| c.total_context_tokens),
        };
        (!capabilities.is_empty()).then_some(capabilities)
    }
}

/// Keeps only the modalities supported by every target that declares them.
/// Targets with no modality data are ignored rather than treated as "nothing",
/// and if none declare anything the result is `None`.
pub(crate) fn intersect_modalities(
    targets: &[&ModelCapabilities],
    get: fn(&ModelCapabilities) -> Option<&Vec<String>>,
) -> Option<Vec<String>> {
    let declared = targets.iter().filter_map(|c| get(c)).collect::<Vec<_>>();
    if declared.is_empty() {
        return None;
    }
    let mut sets = declared.iter().map(|items| {
        items
            .iter()
            .map(|item| item.to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>()
    });
    let mut result = sets.next().unwrap_or_default();
    for group in sets {
        result = result.intersection(&group).cloned().collect();
    }
    Some(result.into_iter().collect())
}

#[derive(Debug, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<PublicModel>,
}
