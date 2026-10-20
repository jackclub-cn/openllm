use super::*;

#[tokio::test]
async fn upstream_stream_deadline_ends_a_silent_stream() {
    // Emits one chunk, then stays silent past a short lifetime bound.
    let stream: UpstreamByteStream = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(Bytes::from_static(b"data: hi\n\n"))
    })
    .chain(futures_util::stream::pending())
    .boxed();
    let mut guarded = with_stream_deadline(stream, Some(Duration::from_millis(80)));

    let first = guarded.next().await.unwrap().unwrap();
    assert_eq!(&first[..], b"data: hi\n\n", "bytes pass through untouched");

    let error = guarded
        .next()
        .await
        .expect("the stream must surface the timeout")
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        guarded.next().await.is_none(),
        "a stream trimmed by the deadline ends immediately"
    );
}

#[tokio::test]
async fn upstream_stream_deadline_passes_a_completed_stream() {
    let stream: UpstreamByteStream = futures_util::stream::iter(vec![
        Ok(Bytes::from_static(b"a")),
        Ok(Bytes::from_static(b"b")),
    ])
    .boxed();
    let mut guarded = with_stream_deadline(stream, Some(Duration::from_secs(5)));
    assert_eq!(&guarded.next().await.unwrap().unwrap()[..], b"a");
    assert_eq!(&guarded.next().await.unwrap().unwrap()[..], b"b");
    assert!(guarded.next().await.is_none());
}

#[tokio::test]
async fn body_read_deadline_fails_a_silent_body() {
    let silent =
        Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    let bounded = with_body_read_deadline(silent, Some(Duration::from_millis(60)));
    let collected = axum::body::to_bytes(bounded, usize::MAX).await;
    assert!(
        collected.is_err(),
        "a body that never completes must fail once the deadline passes"
    );
}

#[tokio::test]
async fn body_read_deadline_passes_a_complete_body() {
    let body = Body::from(Bytes::from_static(b"{\"model\":\"m\"}"));
    let bounded = with_body_read_deadline(body, Some(Duration::from_secs(5)));
    let collected = axum::body::to_bytes(bounded, usize::MAX).await.unwrap();
    assert_eq!(&collected[..], b"{\"model\":\"m\"}");
}

/// A client that never finishes its body must be cut off instead of pinning
/// the admission slot forever.
#[tokio::test]
async fn admission_bounds_a_hanging_request_body() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut state = AppState::new(pool, None);
    state.body_read_timeout = Some(Duration::from_millis(80));
    let router = crate::build_router(state);

    let hanging = Body::from_stream(futures_util::stream::pending::<
        Result<Bytes, std::convert::Infallible>,
    >());
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(OPENAI_CHAT_COMPLETIONS)
        .header("content-type", "application/json")
        .body(hanging)
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(2), router.oneshot(request))
        .await
        .expect("a hanging body must be cut off, not held forever")
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "a body that never arrives must be rejected, got {}",
        response.status()
    );
}
