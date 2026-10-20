use super::*;

#[test]
fn gateway_tuning_env_parsers_fall_back_safely() {
    use crate::state::{
        DEFAULT_MAX_BODY_MIB, DEFAULT_SSE_KEEPALIVE_SECS, DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS,
        parse_concurrency_limit, parse_keepalive_secs, parse_max_body_mib, parse_positive_secs,
    };

    // Missing, empty, unparsable, and non-positive values keep the default so a
    // typo can never shrink the body cap or disable the idle timeout.
    assert_eq!(parse_max_body_mib(None), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("   ")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("0")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("-8")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some("lots")), DEFAULT_MAX_BODY_MIB);
    assert_eq!(parse_max_body_mib(Some(" 64 ")), 64);

    assert_eq!(
        parse_positive_secs(None, DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS),
        DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS
    );
    assert_eq!(parse_positive_secs(Some("0"), 300), 300);
    assert_eq!(parse_positive_secs(Some("bad"), 300), 300);
    assert_eq!(parse_positive_secs(Some(" 900 "), 300), 900);

    // The keep-alive interval treats 0 as "off" while a typo keeps the default
    // so the protection cannot be disabled by accident.
    assert_eq!(
        parse_keepalive_secs(None),
        Some(Duration::from_secs(DEFAULT_SSE_KEEPALIVE_SECS))
    );
    assert_eq!(
        parse_keepalive_secs(Some("  ")),
        Some(Duration::from_secs(DEFAULT_SSE_KEEPALIVE_SECS))
    );
    assert_eq!(
        parse_keepalive_secs(Some("nope")),
        Some(Duration::from_secs(DEFAULT_SSE_KEEPALIVE_SECS))
    );
    assert_eq!(parse_keepalive_secs(Some("0")), None);
    assert_eq!(
        parse_keepalive_secs(Some(" 5 ")),
        Some(Duration::from_secs(5))
    );

    // The global cap is opt-in: only a positive integer turns it on.
    assert_eq!(parse_concurrency_limit(None), None);
    assert_eq!(parse_concurrency_limit(Some("")), None);
    assert_eq!(parse_concurrency_limit(Some("  ")), None);
    assert_eq!(parse_concurrency_limit(Some("0")), None);
    assert_eq!(parse_concurrency_limit(Some("-3")), None);
    assert_eq!(parse_concurrency_limit(Some("many")), None);
    assert_eq!(parse_concurrency_limit(Some(" 12 ")), Some(12));

    // The upstream-body cap shares the MiB parser but keeps its own default.
    use crate::state::{DEFAULT_MAX_UPSTREAM_BODY_MIB, parse_body_mib};
    assert_eq!(
        parse_body_mib(None, DEFAULT_MAX_UPSTREAM_BODY_MIB),
        DEFAULT_MAX_UPSTREAM_BODY_MIB
    );
    assert_eq!(parse_body_mib(Some("0"), DEFAULT_MAX_UPSTREAM_BODY_MIB), 64);
    assert_eq!(
        parse_body_mib(Some("128"), DEFAULT_MAX_UPSTREAM_BODY_MIB),
        128
    );

    // The shutdown grace defaults to 30s; an explicit 0 means "wait forever".
    use crate::state::{DEFAULT_SHUTDOWN_GRACE_SECS, parse_shutdown_grace_secs};
    assert_eq!(parse_shutdown_grace_secs(None), DEFAULT_SHUTDOWN_GRACE_SECS);
    assert_eq!(
        parse_shutdown_grace_secs(Some("  ")),
        DEFAULT_SHUTDOWN_GRACE_SECS
    );
    assert_eq!(
        parse_shutdown_grace_secs(Some("soon")),
        DEFAULT_SHUTDOWN_GRACE_SECS
    );
    assert_eq!(parse_shutdown_grace_secs(Some("0")), 0);
    assert_eq!(parse_shutdown_grace_secs(Some(" 45 ")), 45);
}

#[tokio::test]
async fn buffered_upstream_body_is_capped() {
    // A body that stays under the cap is returned intact.
    let mut small: UpstreamByteStream = Box::pin(futures_util::stream::iter([
        Ok::<_, io::Error>(Bytes::from_static(b"hello")),
        Ok(Bytes::from_static(b" world")),
    ]));
    let body = collect_bytes_limited(&mut small, 64).await.unwrap();
    assert_eq!(&body[..], b"hello world");

    // Crossing the cap stops the read rather than buffering without bound.
    let mut big: UpstreamByteStream = Box::pin(futures_util::stream::iter([
        Ok::<_, io::Error>(Bytes::from_static(b"1234")),
        Ok(Bytes::from_static(b"5678")),
    ]));
    let error = collect_bytes_limited(&mut big, 7).await.unwrap_err();
    assert!(error.to_string().contains("exceeded"), "{error}");
}
