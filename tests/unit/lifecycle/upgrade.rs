use super::*;

// -------------------------------------------------------------------------
// Upgrade check
// -------------------------------------------------------------------------

#[tokio::test]
async fn the_upgrade_check_compares_against_a_given_version() {
    let state = memory_state().await;

    let newer = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("v0.4.0".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(newer.current_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(newer.latest_version.as_deref(), Some("0.4.0"));
    assert!(newer.update_available);
    assert_eq!(newer.status, "update_available");
    assert_eq!(newer.source, "query");
    assert!(!newer.message.is_empty());

    // A tag that cannot be parsed must not be reported as "up to date".
    let garbage = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("latest-release".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(garbage.status, "unknown");
    assert!(!garbage.update_available);
    assert!(
        garbage.message.contains("latest-release"),
        "the message should say what could not be parsed: {}",
        garbage.message
    );

    let same = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!same.update_available);
    assert_eq!(same.status, "up_to_date");

    let older = upgrade_check(
        State(state.clone()),
        Query(UpgradeCheckQuery {
            latest: Some("0.1.0".to_string()),
        }),
    )
    .await
    .unwrap()
    .0;
    assert!(!older.update_available);
    assert_eq!(older.status, "ahead");

    if std::env::var("OPENLLM_UPGRADE_FEED_URL").is_err() {
        let unknown = upgrade_check(
            State(state.clone()),
            Query(UpgradeCheckQuery { latest: None }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(unknown.status, "unknown");
        assert_eq!(unknown.source, "none");
        assert!(unknown.latest_version.is_none());
        assert!(!unknown.message.is_empty());
    }
}

#[test]
fn version_comparison_is_numeric_and_tolerates_a_v_prefix() {
    use std::cmp::Ordering;

    assert_eq!(compare_versions("0.3.14", "0.4.0"), Ordering::Less);
    assert_eq!(compare_versions("v0.3.14", "0.3.14"), Ordering::Equal);
    assert_eq!(compare_versions("0.3.14", "v0.3.13"), Ordering::Greater);
    assert_eq!(compare_versions("0.10.0", "0.9.9"), Ordering::Greater);
    assert_eq!(compare_versions("0.3.14", "0.3.14.1"), Ordering::Less);
    assert_eq!(compare_versions("0.4", "0.4.0"), Ordering::Equal);
    // A version the comparison cannot read keeps the current version: the
    // upgrade check turns that into "unknown" instead of "up to date".
    assert_eq!(compare_versions("0.3.14", "not-a-version"), Ordering::Equal);
    assert!(!is_numeric_version("not-a-version"));
    assert!(!is_numeric_version("1.0.0-rc1"));
    assert!(!is_numeric_version(""));
    assert!(is_numeric_version("v1.2.3"));
    assert!(is_numeric_version(" 2 "));
}
