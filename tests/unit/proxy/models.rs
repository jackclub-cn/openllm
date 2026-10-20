use super::*;

#[test]
fn paginates_forward_with_after_id() {
    let models = model_page(&["a", "b", "c", "d", "e"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: Some("b".to_string()),
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["c", "d"]);
    assert!(has_more, "e remains beyond the window");
}

#[test]
fn paginates_backward_with_before_id() {
    let models = model_page(&["a", "b", "c", "d", "e"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: None,
        before_id: Some("d".to_string()),
    };
    let (window, has_more) = paginate_models(models, &query);
    // The page ends just before the cursor, keeping the last two entries.
    assert_eq!(ids_of(&window), vec!["b", "c"]);
    // `d`/`e` still sit after `last_id`, so the client can page forward.
    assert!(has_more);
}

#[test]
fn reports_has_more_on_a_partial_first_page() {
    let models = model_page(&["a", "b", "c"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: None,
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["a", "b"]);
    assert!(has_more);
}

#[test]
fn returns_last_page_without_claiming_more() {
    let models = model_page(&["a", "b", "c"]);
    let query = ModelPageQuery {
        limit: Some(10),
        after_id: None,
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert_eq!(ids_of(&window), vec!["a", "b", "c"]);
    assert!(!has_more);
}

#[test]
fn unknown_cursor_yields_an_empty_page() {
    let models = model_page(&["a", "b"]);
    let query = ModelPageQuery {
        limit: Some(2),
        after_id: Some("missing".to_string()),
        before_id: None,
    };
    let (window, has_more) = paginate_models(models, &query);
    assert!(window.is_empty());
    assert!(!has_more);
}

#[test]
fn page_size_defaults_and_clamps_to_anthropic_bounds() {
    assert_eq!(
        ModelPageQuery::default().page_size(),
        ANTHROPIC_DEFAULT_PAGE_SIZE
    );
    let tiny = ModelPageQuery {
        limit: Some(0),
        ..Default::default()
    };
    assert_eq!(tiny.page_size(), 1, "zero clamps up");
    let huge = ModelPageQuery {
        limit: Some(99999),
        ..Default::default()
    };
    assert_eq!(huge.page_size(), ANTHROPIC_MAX_PAGE_SIZE, "clamps down");
}

#[test]
fn parses_pagination_params_including_slashes() {
    // Model ids contain '/', which clients percent-encode.
    let uri: Uri = "/v1/models?limit=5&after_id=cmd%2Fdeepseek%2Fv4&ignored=x"
        .parse()
        .unwrap();
    let query = ModelPageQuery::parse(&uri);
    assert_eq!(query.limit, Some(5));
    assert_eq!(query.after_id.as_deref(), Some("cmd/deepseek/v4"));
    assert_eq!(query.before_id, None);

    // A malformed limit is ignored rather than rejecting the request.
    let bad: Uri = "/v1/models?limit=abc".parse().unwrap();
    assert_eq!(ModelPageQuery::parse(&bad).limit, None);
}

#[test]
fn joins_base_and_path_without_double_v1() {
    assert_eq!(
        join_upstream_url("https://api.openai.com/v1", "/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("https://api.openai.com", "/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
    assert_eq!(
        join_upstream_url("http://localhost:11434/v1", "/models"),
        "http://localhost:11434/v1/models"
    );
}
