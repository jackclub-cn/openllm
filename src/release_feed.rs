use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub(crate) const OFFICIAL_RELEASE_FEED: &str =
    "https://api.github.com/repos/jackclub-cn/openllm/releases/latest";

#[derive(Default)]
pub(crate) struct ReleaseFeed {
    cached: Mutex<Option<CachedRelease>>,
}

struct CachedRelease {
    url: String,
    fetched: Instant,
    result: Result<String, String>,
}

impl ReleaseFeed {
    pub(crate) async fn latest(
        &self,
        client: &reqwest::Client,
        url: &str,
    ) -> Result<String, String> {
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref() {
            let ttl = if entry.result.is_ok() { 3600 } else { 60 };
            if entry.url == url && entry.fetched.elapsed() < Duration::from_secs(ttl) {
                return entry.result.clone();
            }
        }
        let result = fetch(client, url)
            .await
            .map_err(|error| format!("{error:#}"));
        *cached = Some(CachedRelease {
            url: url.to_string(),
            fetched: Instant::now(),
            result: result.clone(),
        });
        result
    }
}

async fn fetch(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(5))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("OpenLLM/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = response.json().await?;
    if body.get("draft").and_then(serde_json::Value::as_bool) == Some(true)
        || body.get("prerelease").and_then(serde_json::Value::as_bool) == Some(true)
    {
        anyhow::bail!("发布源返回了草稿或预发布版本");
    }
    body.get("tag_name")
        .or_else(|| body.get("version"))
        .or_else(|| body.get("latest"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("响应里没有 tag_name/version 字段"))
}
