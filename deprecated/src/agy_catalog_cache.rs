//! Small profile-local cache for AGY's metadata-only model discovery.
//!
//! The snapshot is public catalog metadata; credentials and process environment
//! are deliberately excluded. The caller supplies an invocation-context key
//! and serializes refreshes so two concurrent catalog requests share one query.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::future::Future;

use crate::agy::{Catalog, ModelOption};

const CACHE_FILE: &str = "agy-model-catalog.json";
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct CatalogCache {
    path: PathBuf,
    context_key: String,
}

pub(crate) struct CatalogLookup {
    pub catalog: Option<Catalog>,
    pub warning: Option<String>,
}

impl CatalogCache {
    pub(crate) fn new(state_dir: &Path, context_key: String) -> Self {
        Self {
            path: state_dir.join(CACHE_FILE),
            context_key,
        }
    }

    /// Return a fresh matching snapshot or discover and persist a new one.
    /// Failed discoveries never modify the previous on-disk snapshot.
    pub(crate) async fn get_or_refresh<F, Fut>(
        &self,
        now_unix_seconds: u64,
        force: bool,
        discover: F,
    ) -> Result<CatalogLookup>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Catalog>>,
    {
        let (previous, cache_warning) = self.load_record()?;
        if let Some(record) = previous.as_ref()
            && record.context_key == self.context_key
        {
            if !force
                && record
                    .successful_at
                    .is_some_and(|time| within_ttl(now_unix_seconds, time))
            {
                return Ok(CatalogLookup {
                    catalog: record.catalog.clone(),
                    warning: cache_warning,
                });
            }
            if !force
                && record
                    .last_attempt_at
                    .is_some_and(|time| within_ttl(now_unix_seconds, time))
            {
                return Ok(CatalogLookup {
                    catalog: record.catalog.clone(),
                    warning: Some(join_warnings(
                        cache_warning,
                        "AGY model refresh recently failed; using the last successful catalog."
                            .to_owned(),
                    )),
                });
            }
        }

        match discover().await.context("discover AGY model catalog") {
            Ok(discovered) => {
                self.save(&CacheRecord {
                    context_key: self.context_key.clone(),
                    successful_at: Some(now_unix_seconds),
                    last_attempt_at: Some(now_unix_seconds),
                    catalog: Some(discovered.clone()),
                })?;
                Ok(CatalogLookup {
                    catalog: Some(discovered),
                    warning: cache_warning,
                })
            }
            Err(error) if force => match cache_warning {
                Some(warning) => Err(error.context(warning)),
                None => Err(error),
            },
            Err(error) => {
                let prior_catalog = previous
                    .as_ref()
                    .filter(|record| record.context_key == self.context_key)
                    .and_then(|record| record.catalog.clone());
                self.save(&CacheRecord {
                    context_key: self.context_key.clone(),
                    successful_at: previous
                        .as_ref()
                        .filter(|record| record.context_key == self.context_key)
                        .and_then(|record| record.successful_at),
                    last_attempt_at: Some(now_unix_seconds),
                    catalog: prior_catalog.clone(),
                })?;
                Ok(CatalogLookup {
                    catalog: prior_catalog,
                    warning: Some(join_warnings(
                        cache_warning,
                        format!("AGY model discovery failed: {error:#}"),
                    )),
                })
            }
        }
    }

    fn load_record(&self) -> Result<(Option<CacheRecord>, Option<String>)> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read AGY model catalog cache at {}", self.path.display())
                });
            }
        };
        Ok(decode_record_bytes(&bytes))
    }

    fn save(&self, record: &CacheRecord) -> Result<()> {
        let value = encode_record(record);
        let bytes = serde_json::to_vec(&value).context("encode AGY model catalog cache")?;
        let parent = self
            .path
            .parent()
            .context("AGY model catalog cache has no parent directory")?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create AGY model catalog cache directory {}",
                parent.display()
            )
        })?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)
            .with_context(|| format!("write AGY model catalog cache {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "replace AGY model catalog cache {} with {}",
                self.path.display(),
                temporary.display()
            )
        })?;
        Ok(())
    }
}

fn decode_record_bytes(bytes: &[u8]) -> (Option<CacheRecord>, Option<String>) {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            return (
                None,
                Some(format!(
                    "AGY model catalog cache is invalid JSON ({error}); refreshing."
                )),
            );
        }
    };
    match decode_record(&value) {
        Ok(record) => (Some(record), None),
        Err(error) => (
            None,
            Some(format!(
                "AGY model catalog cache is invalid ({error:#}); refreshing."
            )),
        ),
    }
}

fn join_warnings(previous: Option<String>, next: String) -> String {
    match previous {
        Some(previous) => format!("{previous} {next}"),
        None => next,
    }
}

fn within_ttl(now: u64, then: u64) -> bool {
    now >= then && now - then < TTL.as_secs()
}

struct CacheRecord {
    context_key: String,
    successful_at: Option<u64>,
    last_attempt_at: Option<u64>,
    catalog: Option<Catalog>,
}

fn encode_record(record: &CacheRecord) -> Value {
    json!({
        "version": 1,
        "contextKey": record.context_key,
        "successfulAtUnixSeconds": record.successful_at,
        "lastAttemptAtUnixSeconds": record.last_attempt_at,
        "catalog": record.catalog.as_ref().map(|catalog| json!({
            "models": catalog.models.iter().map(|model| json!({"id": model.id, "label": model.label})).collect::<Vec<_>>(),
            "efforts": catalog.efforts,
        })),
    })
}

fn decode_record(value: &Value) -> Result<CacheRecord> {
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        anyhow::bail!("AGY model catalog cache has an unsupported version");
    }
    let context_key = value
        .get("contextKey")
        .and_then(Value::as_str)
        .context("AGY model catalog cache has no context key")?
        .to_owned();
    let successful_at = value.get("successfulAtUnixSeconds").and_then(Value::as_u64);
    let last_attempt_at = value
        .get("lastAttemptAtUnixSeconds")
        .and_then(Value::as_u64);
    let catalog = value
        .get("catalog")
        .filter(|value| !value.is_null())
        .map(decode_catalog)
        .transpose()?;
    Ok(CacheRecord {
        context_key,
        successful_at,
        last_attempt_at,
        catalog,
    })
}

fn decode_catalog(value: &Value) -> Result<Catalog> {
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .context("AGY model catalog cache has no models array")?
        .iter()
        .map(|model| {
            Ok(ModelOption {
                id: model
                    .get("id")
                    .and_then(Value::as_str)
                    .context("AGY model catalog cache row has no ID")?
                    .to_owned(),
                label: model
                    .get("label")
                    .and_then(Value::as_str)
                    .context("AGY model catalog cache row has no label")?
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let efforts = value
        .get("efforts")
        .and_then(Value::as_array)
        .context("AGY model catalog cache has no efforts array")?
        .iter()
        .map(|effort| {
            effort
                .as_str()
                .map(str::to_owned)
                .context("AGY model catalog cache effort is not text")
        })
        .collect::<Result<Vec<_>>>()?;
    if models.is_empty() || efforts.is_empty() {
        anyhow::bail!("AGY model catalog cache is incomplete");
    }
    Ok(Catalog { models, efforts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(id: &str) -> Catalog {
        Catalog {
            models: vec![ModelOption {
                id: id.to_owned(),
                label: format!("{id} label"),
            }],
            efforts: vec!["low".to_owned(), "high".to_owned()],
        }
    }

    #[tokio::test]
    async fn initial_lookup_persists_and_restart_reuses_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/shared".into());
        let mut calls = 0;
        let first = cache
            .get_or_refresh(100, false, || async {
                calls += 1;
                Ok(catalog("first"))
            })
            .await
            .unwrap();
        assert_eq!(first.catalog, Some(catalog("first")));

        let restarted = CatalogCache::new(directory.path(), "profile-a/shared".into());
        let again = restarted
            .get_or_refresh(101, false, || async {
                calls += 1;
                Ok(catalog("unexpected"))
            })
            .await
            .unwrap();
        assert_eq!(again.catalog, Some(catalog("first")));
        assert_eq!(calls, 1);
    }

    #[test]
    fn corrupt_cache_bytes_are_reported_as_a_miss() {
        let (record, warning) = decode_record_bytes(b"not a catalog");
        assert!(record.is_none());
        assert!(warning.unwrap().contains("invalid JSON"));
    }

    #[tokio::test]
    async fn expires_at_24_hours_and_forced_refresh_bypasses_fresh_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/separate".into());
        cache
            .get_or_refresh(100, false, || async { Ok(catalog("first")) })
            .await
            .unwrap();

        let mut calls = 0;
        let refreshed = cache
            .get_or_refresh(100 + TTL.as_secs(), false, || async {
                calls += 1;
                Ok(catalog("expired"))
            })
            .await
            .unwrap();
        assert_eq!(refreshed.catalog, Some(catalog("expired")));
        let forced = cache
            .get_or_refresh(100 + TTL.as_secs() + 1, true, || async {
                calls += 1;
                Ok(catalog("manual"))
            })
            .await
            .unwrap();
        assert_eq!(forced.catalog, Some(catalog("manual")));
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn failed_forced_refresh_preserves_last_good_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/shared".into());
        cache
            .get_or_refresh(100, false, || async { Ok(catalog("good")) })
            .await
            .unwrap();
        let error = cache
            .get_or_refresh(101, true, || async {
                anyhow::bail!("discovery unavailable")
            })
            .await;
        assert!(error.is_err());

        let restarted = CatalogCache::new(directory.path(), "profile-a/shared".into());
        let after_failure = restarted
            .get_or_refresh(102, false, || async {
                anyhow::bail!("fresh snapshot should avoid rediscovery")
            })
            .await
            .unwrap();
        assert_eq!(after_failure.catalog, Some(catalog("good")));
        assert_eq!(after_failure.warning, None);
    }

    #[tokio::test]
    async fn different_profile_or_command_context_does_not_reuse_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let original = CatalogCache::new(directory.path(), "profile-a/shared".into());
        original
            .get_or_refresh(100, false, || async { Ok(catalog("profile-a")) })
            .await
            .unwrap();

        let other_context = CatalogCache::new(directory.path(), "profile-b/shared".into());
        let result = other_context
            .get_or_refresh(101, false, || async { Ok(catalog("profile-b")) })
            .await
            .unwrap();
        assert_eq!(result.catalog, Some(catalog("profile-b")));
    }

    #[tokio::test]
    async fn failed_expired_refresh_is_attempted_once_per_day_and_keeps_stale_catalog() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/shared".into());
        cache
            .get_or_refresh(100, false, || async { Ok(catalog("good")) })
            .await
            .unwrap();
        let mut calls = 0;
        let first = cache
            .get_or_refresh(100 + TTL.as_secs(), false, || async {
                calls += 1;
                anyhow::bail!("temporary failure")
            })
            .await
            .unwrap();
        assert_eq!(first.catalog, Some(catalog("good")));
        assert!(first.warning.unwrap().contains("temporary failure"));
        let second = cache
            .get_or_refresh(100 + TTL.as_secs() + 1, false, || async {
                calls += 1;
                Ok(catalog("unexpected"))
            })
            .await
            .unwrap();
        assert_eq!(second.catalog, Some(catalog("good")));
        assert!(second.warning.is_some());
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn failed_initial_refresh_is_not_retried_before_the_daily_window() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/shared".into());
        let mut calls = 0;
        let first = cache
            .get_or_refresh(100, false, || async {
                calls += 1;
                anyhow::bail!("not signed in")
            })
            .await
            .unwrap();
        assert!(first.catalog.is_none());
        assert!(first.warning.unwrap().contains("not signed in"));

        let second = cache
            .get_or_refresh(101, false, || async {
                calls += 1;
                Ok(catalog("unexpected"))
            })
            .await
            .unwrap();
        assert!(second.catalog.is_none());
        assert!(second.warning.is_some());
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn future_attempt_timestamp_is_invalidated_and_rewritten_once() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path(), "profile-a/shared".into());
        cache
            .get_or_refresh(500, false, || async { Ok(catalog("future")) })
            .await
            .unwrap();
        let mut calls = 0;
        let rewound = cache
            .get_or_refresh(100, false, || async {
                calls += 1;
                Ok(catalog("rewritten"))
            })
            .await
            .unwrap();
        assert_eq!(rewound.catalog, Some(catalog("rewritten")));
        let next = cache
            .get_or_refresh(101, false, || async {
                calls += 1;
                Ok(catalog("unexpected"))
            })
            .await
            .unwrap();
        assert_eq!(next.catalog, Some(catalog("rewritten")));
        assert_eq!(calls, 1);
    }

    #[test]
    fn malformed_cache_record_values_are_reported_as_misses() {
        let (malformed, malformed_warning) =
            decode_record_bytes(br#"{"contextKey":"synthetic","catalog":{}}"#);
        assert!(malformed.is_none());
        assert!(malformed_warning.unwrap().contains("invalid ("));

        let (unsupported, unsupported_warning) =
            decode_record_bytes(br#"{"version":99,"contextKey":"synthetic","catalog":null}"#);
        assert!(unsupported.is_none());
        assert!(unsupported_warning.unwrap().contains("unsupported version"));
    }
}
