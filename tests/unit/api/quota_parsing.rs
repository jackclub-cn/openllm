use super::*;

#[test]
fn recognizes_provider_quota_endpoints() {
    assert_eq!(
        provider_quota_kind("https://api.commandcode.ai/provider/v1"),
        Some("command_code")
    );
    assert_eq!(
        provider_quota_kind("https://opencode.ai/zen/go/v1"),
        Some("opencode_go")
    );
    assert_eq!(
        provider_quota_kind("https://api.deepseek.com/v1"),
        Some("deepseek")
    );
    assert_eq!(provider_quota_kind("https://api.openai.com/v1"), None);
}

#[test]
fn maps_command_code_plans() {
    assert_eq!(command_code_plan_name("individual-goat"), "GOAT");
    assert_eq!(command_code_monthly_total("individual-goat"), Some(70.0));
    assert_eq!(command_code_plan_name("individual-pro"), "Pro");
    assert_eq!(command_code_monthly_total("individual-pro"), Some(30.0));
    assert_eq!(command_code_monthly_total("individual-pro-v1"), Some(80.0));
    assert_eq!(command_code_monthly_total("individual-max"), Some(150.0));
    assert_eq!(command_code_plan_name("unknown"), "unknown");
    assert_eq!(
        quota_reset_at(Some(&json!(1790774552836_i64))),
        epoch_millis_to_rfc3339(1790774552836)
    );
    assert_eq!(
        quota_reset_at(Some(&json!("2026-10-01T00:00:00Z"))).as_deref(),
        Some("2026-10-01T00:00:00Z")
    );
}

#[test]
fn derives_command_code_monthly_total_from_the_ledger_and_snaps_races() {
    // Separate usage and remaining reads can race by a few cents during a
    // request; the published plan total must still win.
    assert_eq!(
        command_code_monthly_total_value(Some(62.53), Some(7.46), Some("individual-goat")),
        Some(70.0)
    );
    // A real plan or promotion change larger than the race tolerance is kept.
    assert_eq!(
        command_code_monthly_total_value(Some(72.5), Some(7.5), Some("individual-goat")),
        Some(80.0)
    );
    // An unlisted plan still gets a total from the live ledger.
    assert_eq!(
        command_code_monthly_total_value(Some(12.25), Some(7.75), Some("future-plan")),
        Some(20.0)
    );
    // With no ledger reads, the published fallback still works.
    assert_eq!(
        command_code_monthly_total_value(None, None, Some("individual-pro")),
        Some(30.0)
    );
}

#[test]
fn parses_command_code_model_monthly_credits() {
    let html = r#"
        <table>
          <thead>
            <tr><th>Model</th><th>Input</th><th>Monthly credits</th></tr>
          </thead>
          <tbody>
            <tr>
              <td><button>DeepSeek V4.1 Flash</button></td>
              <td>$0.15</td>
              <td><button>$80 <span>$100</span> through Oct 16th</button></td>
            </tr>
            <tr>
              <td>GLM-5.2</td>
              <td>$1.40</td>
              <td>$70</td>
            </tr>
          </tbody>
        </table>
    "#;
    let models = parse_command_code_plan_credits(html);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_name, "DeepSeek V4.1 Flash");
    assert_eq!(models[0].monthly_credits, Some(80.0));
    assert_eq!(models[0].promo_credits, Some(100.0));
    assert_eq!(models[0].note.as_deref(), Some("through Oct 16th"));
    assert_eq!(models[1].model_name, "GLM-5.2");
    assert_eq!(models[1].monthly_credits, Some(70.0));
    assert_eq!(models[1].promo_credits, None);
    assert_eq!(models[1].note, None);
}

#[test]
fn maps_command_code_model_names_to_provider_ids() {
    let mut models = vec![ProviderQuotaModel {
        model_name: "DeepSeek V4.1 Flash".to_string(),
        model_id: None,
        monthly_credits: Some(80.0),
        promo_credits: Some(100.0),
        note: Some("through Oct 16th".to_string()),
    }];
    apply_command_code_model_ids(
        &mut models,
        &json!({
            "data": [
                {
                    "id": "deepseek/deepseek-v4.1-flash",
                    "name": "DeepSeek V4.1 Flash"
                }
            ]
        }),
    );
    assert_eq!(
        models[0].model_id.as_deref(),
        Some("deepseek/deepseek-v4.1-flash")
    );
}
