use super::*;

#[tokio::test]
async fn upstream_read_cancels_when_the_client_drops_the_stream() {
    let (tx, rx) = mpsc::channel(1);
    let mut upstream: UpstreamByteStream = Box::pin(futures_util::stream::pending());
    drop(rx);

    let result = tokio::time::timeout(
        Duration::from_millis(100),
        next_upstream_chunk(&mut upstream, &tx, None),
    )
    .await
    .expect("a dropped client must wake the upstream read");
    assert!(result.is_none());
}

#[tokio::test]
async fn sse_keepalive_fires_while_the_upstream_is_silent() {
    let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(4);
    let mut upstream: UpstreamByteStream = Box::pin(futures_util::stream::pending());
    let pump = tokio::spawn(async move {
        next_upstream_chunk(&mut upstream, &tx, Some(Duration::from_millis(20))).await
    });

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a silent upstream must still produce a keep-alive")
        .expect("the keep-alive channel stays open")
        .expect("the keep-alive is a valid frame");
    assert_eq!(first, Bytes::from_static(SSE_KEEPALIVE_LINE));

    // Dropping the client wakes the pump and stops the timer.
    drop(rx);
    assert!(pump.await.unwrap().is_none());
}

#[tokio::test]
async fn sse_keepalive_does_not_delay_available_chunks() {
    let (tx, _rx) = mpsc::channel::<Result<Bytes, io::Error>>(4);
    let chunk = Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
    let expected = chunk.clone();
    let mut upstream: UpstreamByteStream =
        Box::pin(futures_util::stream::once(async move { Ok(chunk) }));

    // The interval is far longer than the timeout, so a returned chunk proves
    // the upstream branch won rather than the keep-alive timer.
    let result = tokio::time::timeout(
        Duration::from_millis(200),
        next_upstream_chunk(&mut upstream, &tx, Some(Duration::from_secs(30))),
    )
    .await
    .expect("a ready chunk must not wait for the keep-alive timer")
    .expect("the chunk is forwarded");
    assert_eq!(result.unwrap(), expected);
}

#[tokio::test]
async fn sse_keepalive_is_injected_only_into_event_streams() {
    // An idle SSE body gets a comment frame so proxies do not drop it.
    let stream = futures_util::stream::pending::<Result<Bytes, std::convert::Infallible>>();
    let response = Response::builder()
        .header(
            reqwest::header::CONTENT_TYPE,
            "text/event-stream; charset=utf-8",
        )
        .body(Body::from_stream(stream))
        .unwrap();
    let response = attach_provider_slot(response, None, Some(Duration::from_millis(20)));
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("an idle event stream must emit a keep-alive")
        .expect("the body stays open")
        .expect("the keep-alive is a valid frame");
    assert_eq!(first, Bytes::from_static(SSE_KEEPALIVE_LINE));

    // A JSON body is passed through untouched even when a keep-alive is set.
    let response = Response::builder()
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"ok\":true}"))
        .unwrap();
    let response = attach_provider_slot(response, None, Some(Duration::from_millis(20)));
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"{\"ok\":true}");
}
