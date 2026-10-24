use super::*;

struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn busy_wait_sse_releases_a_silent_upstream_after_client_disconnect() {
    let (dropped, dropped_rx) = tokio::sync::oneshot::channel();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let body = Body::from_stream(async_stream::stream! {
        let _signal = DropSignal(Some(dropped));
        let _ = started.send(());
        futures_util::future::pending::<()>().await;
        #[allow(unreachable_code)]
        yield Ok::<Bytes, io::Error>(Bytes::new());
    });
    let response = stream_sse_until_response(
        Duration::from_secs(1),
        SseErrorProtocol::OpenAi,
        async move { Ok(Response::new(body)) },
    );
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("the SSE stream starts with a keep-alive")
        .expect("the response body stays open")
        .expect("the keep-alive is a valid frame");
    assert_eq!(first, Bytes::from_static(SSE_KEEPALIVE_LINE));
    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("the upstream body must be polled before disconnecting")
        .expect("the upstream body start signal is sent");
    drop(body);

    tokio::time::timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("a disconnected busy-wait stream must release its upstream body")
        .expect("the upstream body drop signal is sent");
}

#[test]
fn sse_line_buffer_discards_oversized_lines_and_recovers() {
    let mut buffer = SseLineBuffer::default();
    assert!(buffer.push(&vec![b'x'; MAX_SSE_LINE_BYTES + 1]).is_empty());

    let lines = buffer.push(b"\ndata: {\"ok\":true}\n");
    assert_eq!(lines, vec![b"data: {\"ok\":true}\n".to_vec()]);
}

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
