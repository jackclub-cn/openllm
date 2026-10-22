use super::*;

/// A stalled stream must not hold shutdown open past the configured grace
/// period: the server future has to resolve once the deadline elapses even
/// though the connection is still open.
#[tokio::test]
async fn shutdown_deadline_drops_a_stalled_stream() {
    use axum::body::{Body, Bytes};
    use axum::response::Response;
    use axum::routing::get;
    use std::time::{Duration, Instant};

    let app = Router::new().route(
        "/hang",
        get(|| async {
            let stream = futures_util::stream::pending::<Result<Bytes, std::convert::Infallible>>();
            Response::new(Body::from_stream(stream))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve_until_shutdown(listener, app, 1, async move {
            let _ = signal_rx.await;
        })
        .await
    });

    // Hold the only connection open with a body that never completes.
    let client = reqwest::Client::new();
    let held = client
        .get(format!("http://{address}/hang"))
        .send()
        .await
        .unwrap();
    assert_eq!(held.status(), 200);

    let started = Instant::now();
    signal_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the drain deadline must end the serve future")
        .unwrap();
    assert!(result.is_ok(), "the deadline closes cleanly");
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "the connection is kept draining for the grace period, took {:?}",
        started.elapsed()
    );

    drop(held);
}

/// With no stalled connection the server still exits promptly once the signal
/// arrives, without waiting for the full grace period.
#[tokio::test]
async fn shutdown_returns_promptly_when_drained() {
    use std::time::{Duration, Instant};

    let app = Router::new().route("/ok", axum::routing::get(|| async { "ok" }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve_until_shutdown(listener, app, 30, async move {
            let _ = signal_rx.await;
        })
        .await
    });

    let response = reqwest::get(format!("http://{address}/ok")).await.unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");

    let started = Instant::now();
    signal_tx.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("an idle server finishes draining immediately")
        .unwrap();
    assert!(result.is_ok());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the server must not wait out the full grace period when idle"
    );
}

#[test]
fn database_pool_size_parsing_clamps_to_a_sane_range() {
    use crate::db::parse_db_max_connections;
    assert_eq!(parse_db_max_connections(None), 4);
    assert_eq!(parse_db_max_connections(Some("")), 4);
    assert_eq!(parse_db_max_connections(Some("oops")), 4);
    assert_eq!(
        parse_db_max_connections(Some("0")),
        1,
        "at least one connection"
    );
    assert_eq!(parse_db_max_connections(Some("24")), 24);
    assert_eq!(
        parse_db_max_connections(Some("100000")),
        256,
        "an oversized value is clamped instead of exhausting the host"
    );
}

#[tokio::test]
async fn sqlite_memory_settings_are_bounded() {
    use crate::db::{SQLITE_CACHE_KIB, sqlite_connect_options};

    let options = sqlite_connect_options("sqlite::memory:", std::time::Duration::from_secs(1))
        .expect("in-memory SQLite URL should be accepted");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("in-memory SQLite pool should connect");

    let cache_size: i64 = sqlx::query_scalar("PRAGMA cache_size")
        .fetch_one(&pool)
        .await
        .expect("cache_size pragma should be readable");
    let temp_store: i64 = sqlx::query_scalar("PRAGMA temp_store")
        .fetch_one(&pool)
        .await
        .expect("temp_store pragma should be readable");

    assert_eq!(cache_size, -(SQLITE_CACHE_KIB as i64));
    assert_eq!(temp_store, 1, "temporary sorting should use files");
}

#[test]
fn serving_off_loopback_requires_an_admin_token() {
    let config = |bind: &str, admin_token: Option<&str>| crate::Config {
        bind: bind.parse().unwrap(),
        data_dir: PathBuf::from("data"),
        database_url: None,
        admin_token: admin_token.map(ToOwned::to_owned),
    };

    assert!(validate_runtime_security(&config("127.0.0.1:8080", None)).is_ok());
    assert!(
        validate_runtime_security(&config("0.0.0.0:8080", None)).is_err(),
        "a non-loopback listener without a token exposes an open admin API"
    );
    assert!(validate_runtime_security(&config("0.0.0.0:8080", Some("secret"))).is_ok());
}

#[cfg(unix)]
#[test]
fn private_exports_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot.json");
    crate::secure_file::write_private(&path, b"{}").unwrap();
    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn sqlite_database_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path().join("data");
    let database = crate::db::Database::connect(&crate::Config {
        bind: "127.0.0.1:8080".parse().unwrap(),
        data_dir: data_dir.clone(),
        database_url: None,
        admin_token: None,
    })
    .await
    .unwrap();
    let mode = std::fs::metadata(data_dir.join("openllm.db"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    database.pool.close().await;
}

#[test]
fn database_timeout_parsing_keeps_a_positive_default() {
    use crate::db::parse_db_timeout_secs;
    assert_eq!(parse_db_timeout_secs(None, 15), 15);
    assert_eq!(parse_db_timeout_secs(Some(""), 15), 15);
    assert_eq!(
        parse_db_timeout_secs(Some("0"), 15),
        15,
        "zero is not a timeout"
    );
    assert_eq!(parse_db_timeout_secs(Some("nope"), 15), 15);
    assert_eq!(parse_db_timeout_secs(Some("30"), 15), 30);
}

/// A panicking background worker must come back: maintenance and scheduled
/// refreshes are spawned once, so an unsupervised panic would silently disable
/// them for the rest of the process's life.
#[tokio::test]
async fn supervised_worker_restarts_after_a_panic() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    let runs = Arc::new(AtomicU32::new(0));
    let counter = runs.clone();
    spawn_supervised("test-worker", move || {
        let counter = counter.clone();
        async move {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("boom");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while runs.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        runs.load(Ordering::SeqCst) >= 2,
        "the worker must be restarted after it panics"
    );
}

/// The readiness probe must leave rotation as soon as the drain starts, while
/// the liveness probe keeps answering so the orchestrator does not kill the
/// process that is still finishing in-flight work.
#[tokio::test]
async fn readiness_probe_reports_draining_before_shutdown() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let state = AppState::new(pool, None);
    let router = crate::build_router(state.clone());

    let probe = |router: axum::Router, uri: &str| {
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        async move { router.oneshot(request).await.unwrap() }
    };

    let healthy = probe(router.clone(), "/api/ready").await;
    assert_eq!(healthy.status(), StatusCode::OK);

    state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let draining = probe(router.clone(), "/api/ready").await;
    assert_eq!(
        draining.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a draining instance must tell the load balancer to stop sending traffic"
    );
    let body = axum::body::to_bytes(draining.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("draining"));
    assert_eq!(
        probe(router, "/api/health").await.status(),
        StatusCode::OK,
        "liveness must stay green while draining, or the process is killed mid-drain"
    );
}

/// An upgrade must not cut a request that is already running: the drain lets it
/// finish and only then closes the listener.
#[tokio::test]
async fn in_flight_request_finishes_during_the_drain() {
    use axum::routing::get;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    let finished = Arc::new(Notify::new());
    let handler_notify = finished.clone();
    let app = Router::new().route(
        "/slow",
        get(move || {
            let finished = handler_notify.clone();
            async move {
                finished.notify_one();
                tokio::time::sleep(Duration::from_millis(300)).await;
                "done"
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve_until_shutdown(listener, app, 5, async move {
            let _ = signal_rx.await;
        })
        .await
    });

    let request = tokio::spawn(async move {
        let response = reqwest::get(format!("http://{address}/slow"))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.text().await.unwrap()
    });
    finished.notified().await;
    // The handler is mid-flight; a rolling upgrade signals shutdown now.
    signal_tx.send(()).unwrap();

    let body = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("the in-flight request must not be cut by the drain")
        .unwrap();
    assert_eq!(body, "done");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server drains and exits")
        .unwrap()
        .unwrap();
}
