use super::*;

fn capabilities(
    output: Option<i64>,
    tools: Option<bool>,
    context: Option<i64>,
) -> ModelCapabilities {
    ModelCapabilities {
        output_limit: output,
        tool_call: tools,
        context_limit: context,
        ..Default::default()
    }
}

#[test]
fn intersection_takes_strictest_common_envelope() {
    let wide = capabilities(Some(128000), Some(true), Some(400000));
    let narrow = capabilities(Some(8000), Some(true), Some(32000));
    let result = ModelCapabilities::intersect([&wide, &narrow]).unwrap();
    assert_eq!(result.output_limit, Some(8000));
    assert_eq!(result.context_limit, Some(32000));
    assert_eq!(result.tool_call, Some(true));
}

#[test]
fn intersection_reports_false_when_any_target_lacks_a_capability() {
    let with_tools = capabilities(Some(8000), Some(true), None);
    let without_tools = capabilities(Some(8000), Some(false), None);
    let result = ModelCapabilities::intersect([&with_tools, &without_tools]).unwrap();
    assert_eq!(result.tool_call, Some(false));
}

#[test]
fn intersection_ignores_unknown_values_instead_of_zeroing_them() {
    let known = capabilities(Some(8000), None, Some(32000));
    let unknown = ModelCapabilities::default();
    let result = ModelCapabilities::intersect([&known, &unknown]).unwrap();
    // An unknown limit must not collapse the known one to zero.
    assert_eq!(result.output_limit, Some(8000));
    assert_eq!(result.context_limit, Some(32000));
    assert_eq!(result.tool_call, None);
}

#[test]
fn intersection_keeps_only_shared_modalities() {
    let multi = ModelCapabilities {
        input_modalities: Some(vec![
            "text".to_string(),
            "image".to_string(),
            "pdf".to_string(),
        ]),
        output_modalities: Some(vec!["text".to_string()]),
        ..Default::default()
    };
    let text_only = ModelCapabilities {
        input_modalities: Some(vec!["text".to_string()]),
        output_modalities: Some(vec!["text".to_string()]),
        ..Default::default()
    };
    let result = ModelCapabilities::intersect([&multi, &text_only]).unwrap();
    assert_eq!(result.input_modalities, Some(vec!["text".to_string()]));
    assert_eq!(result.output_modalities, Some(vec!["text".to_string()]));
}

#[test]
fn empty_intersection_returns_none() {
    assert!(ModelCapabilities::intersect(std::iter::empty()).is_none());
    let empty = ModelCapabilities::default();
    assert!(ModelCapabilities::intersect([&empty]).is_none());
}

#[test]
fn tps_measures_generation_phase_for_streams() {
    // 100 tokens emitted over 2s after a 3s first-token wait:
    // generation phase is 2s, so 50 tok/s (not 20, which the total would give).
    let tps = output_tps_of(100, 5000, Some(3000), true).unwrap();
    assert!((tps - 50.0).abs() < 0.001, "got {tps}");
}

#[test]
fn tps_falls_back_to_full_latency_for_non_streams() {
    // Non-streamed responses use the whole request time even when the log
    // carries the same timestamp in `first_token_ms`.
    let tps = output_tps_of(100, 2000, Some(2000), false).unwrap();
    assert!((tps - 50.0).abs() < 0.001, "got {tps}");
}

#[test]
fn tps_is_absent_when_undefined() {
    // No output tokens means no throughput to report.
    assert_eq!(output_tps_of(0, 1000, Some(100), true), None);
    // Zero elapsed time would divide by zero.
    assert_eq!(output_tps_of(10, 0, None, false), None);
    // A first-token stamp beyond the total falls back to full latency
    // instead of underflowing into a negative duration.
    let tps = output_tps_of(10, 100, Some(5000), true).unwrap();
    assert!((tps - 100.0).abs() < 0.001, "got {tps}");
}

#[test]
fn effective_input_limit_collapses_window_and_input() {
    // The reported bug: window 1050000 but only 922000 inputs accepted.
    // Both context-named keys must agree on the safe value.
    let capabilities = ModelCapabilities {
        context_limit: Some(1_050_000),
        input_limit: Some(922_000),
        ..Default::default()
    }
    .with_effective_input_limit();
    assert_eq!(capabilities.context_limit, Some(922_000));
    assert_eq!(capabilities.input_limit, Some(922_000));
    // The raw window is preserved rather than discarded.
    assert_eq!(capabilities.total_context_tokens, Some(1_050_000));
}

#[test]
fn effective_input_limit_is_conservative_when_input_exceeds_window() {
    // Also reported: window 400000 with a larger declared input (922000).
    // The smaller of the two is the safe ceiling.
    let capabilities = ModelCapabilities {
        context_limit: Some(400_000),
        input_limit: Some(922_000),
        ..Default::default()
    }
    .with_effective_input_limit();
    assert_eq!(capabilities.context_limit, Some(400_000));
    assert_eq!(capabilities.input_limit, Some(400_000));
}

#[test]
fn effective_input_limit_keeps_equal_values_untouched() {
    let capabilities = ModelCapabilities {
        context_limit: Some(128_000),
        input_limit: Some(128_000),
        ..Default::default()
    }
    .with_effective_input_limit();
    assert_eq!(capabilities.context_limit, Some(128_000));
    assert_eq!(capabilities.input_limit, Some(128_000));
    // Nothing was hidden, so no raw window needs recording.
    assert_eq!(capabilities.total_context_tokens, None);
}

#[test]
fn estimates_cost_with_cache_prices() {
    let cost = serde_json::json!({
        "input": 1.0,
        "output": 2.0,
        "cache_read": 0.1,
        "cache_write": 1.25
    });
    let usage = Usage {
        prompt_tokens: 1_000_000,
        completion_tokens: 1_000_000,
        total_tokens: 2_000_000,
        cache_read_tokens: 400_000,
        cache_write_tokens: 100_000,
    };
    // 500K fresh input + 400K cache read + 100K cache write + 1M output.
    assert_eq!(
        estimate_cost_micros(Some(&cost), usage),
        Some(500_000 + 40_000 + 125_000 + 2_000_000)
    );
}

#[test]
fn cost_overrides_apply_to_base_and_tiered_prices() {
    let synced = serde_json::json!({
        "input": 2.0,
        "output": 10.0,
        "tiers": [
            {"input": 4.0, "output": 20.0}
        ]
    });
    let effective = effective_cost_value(Some(&synced), Some(1.5), None, Some(0.25), None).unwrap();
    assert_eq!(cost_base_price(&effective, "input"), Some(1.5));
    assert_eq!(cost_base_price(&effective, "cache_read"), Some(0.25));
    assert_eq!(cost_base_price(&effective, "output"), Some(10.0));
    assert_eq!(effective["tiers"][0]["input"], serde_json::json!(1.5));
    assert_eq!(effective["tiers"][0]["cache_read"], serde_json::json!(0.25));
    assert_eq!(effective["tiers"][0]["output"], serde_json::json!(20.0));
}

#[test]
fn applies_context_price_tier() {
    let cost = serde_json::json!({
        "input": 1.0,
        "output": 2.0,
        "tiers": [
            { "input": 5.0, "output": 10.0, "tier": { "type": "context", "size": 1000 } }
        ]
    });
    let usage = Usage::new(2_000, 0);
    assert_eq!(estimate_cost_micros(Some(&cost), usage), Some(10_000));
}

#[test]
fn cost_is_unknown_without_numeric_prices() {
    let cost = serde_json::json!({ "currency": "USD" });
    assert_eq!(estimate_cost_micros(Some(&cost), Usage::new(100, 50)), None);
    assert_eq!(estimate_cost_micros(None, Usage::new(100, 50)), None);
}
