use serde::{Deserialize, Serialize};

use crate::{domain::CHANGE_HISTORY_CONTENT_LIMIT_BYTES, infrastructure::TomlFileProxy};

pub const HISTORY_LIMIT: usize = 100;
pub const MAX_HISTORY_LIMIT: usize = 1_000;
pub const CONTENT_LIMIT_BYTES: usize = CHANGE_HISTORY_CONTENT_LIMIT_BYTES;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeHistoryEntry {
    pub id: String,
    pub timestamp_unix_ms: i64,
    pub previous_content: Option<String>,
    pub previous_truncated: bool,
    pub content: String,
    pub content_truncated: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ChangeHistoryDocument {
    #[serde(default)]
    entries: Vec<ChangeHistoryEntry>,
}

pub struct TomlChangeHistoryRepository {
    proxy: TomlFileProxy<ChangeHistoryDocument>,
    history_limit: usize,
}

impl TomlChangeHistoryRepository {
    pub async fn new(path: &str) -> Result<Self, crate::infrastructure::toml_file_proxy::Error> {
        Self::with_limit(path, HISTORY_LIMIT).await
    }

    pub async fn with_limit(
        path: &str,
        history_limit: usize,
    ) -> Result<Self, crate::infrastructure::toml_file_proxy::Error> {
        let history_limit = history_limit.clamp(1, MAX_HISTORY_LIMIT);
        let mut proxy = TomlFileProxy::<ChangeHistoryDocument>::new(path).await?;
        proxy.get_cache_or_load().await?;
        let entries = &mut proxy.get_cache_mut().unwrap().entries;
        let entries_to_remove = entries.len().saturating_sub(history_limit);
        if entries_to_remove > 0 {
            entries.drain(..entries_to_remove);
        }
        Ok(Self {
            proxy,
            history_limit,
        })
    }

    pub fn entries(&self) -> &[ChangeHistoryEntry] {
        self.proxy
            .get_cache()
            .map(|document| document.entries.as_slice())
            .unwrap_or_default()
    }

    pub async fn record(
        &mut self,
        id: String,
        timestamp_unix_ms: i64,
        content: String,
    ) -> Result<(), crate::infrastructure::toml_file_proxy::Error> {
        self.record_batch(std::iter::once((id, timestamp_unix_ms, content, false)))
            .await
    }

    pub async fn record_batch(
        &mut self,
        changes: impl IntoIterator<Item = (String, i64, String, bool)>,
    ) -> Result<(), crate::infrastructure::toml_file_proxy::Error> {
        let mut changes = changes.into_iter().peekable();
        if changes.peek().is_none() {
            return Ok(());
        }

        let original_len = self.entries().len();
        let mut latest_entry_by_id = self
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect::<std::collections::HashMap<_, _>>();
        for (id, timestamp_unix_ms, content, already_truncated) in changes {
            let previous = latest_entry_by_id.get(&id).map(|index| {
                let entry = &self.entries()[*index];
                (entry.content.clone(), entry.content_truncated)
            });
            let (content, content_truncated) = bounded_content(content);
            let content_truncated = content_truncated || already_truncated;
            let (previous_content, previous_truncated) = match previous {
                Some((content, truncated)) => (Some(content), truncated),
                None => (None, false),
            };
            let entries = &mut self
                .proxy
                .get_cache_mut()
                .ok_or(crate::infrastructure::toml_file_proxy::Error::CacheEmpty)?
                .entries;
            latest_entry_by_id.insert(id.clone(), entries.len());
            entries.push(ChangeHistoryEntry {
                id,
                timestamp_unix_ms,
                previous_content,
                previous_truncated,
                content,
                content_truncated,
            });
        }
        let removed_entries = {
            let entries = &mut self
                .proxy
                .get_cache_mut()
                .ok_or(crate::infrastructure::toml_file_proxy::Error::CacheEmpty)?
                .entries;
            let entries_to_remove = entries.len().saturating_sub(self.history_limit);
            entries.drain(..entries_to_remove).collect::<Vec<_>>()
        };
        let result = self.proxy.save().await;
        if result.is_err() {
            let entries = &mut self
                .proxy
                .get_cache_mut()
                .ok_or(crate::infrastructure::toml_file_proxy::Error::CacheEmpty)?
                .entries;
            let removed_original_entries = removed_entries
                .into_iter()
                .take(original_len)
                .collect::<Vec<_>>();
            entries.truncate(original_len - removed_original_entries.len());
            entries.splice(0..0, removed_original_entries);
        }
        result
    }
}

fn bounded_content(content: String) -> (String, bool) {
    if content.len() <= CONTENT_LIMIT_BYTES {
        return (content, false);
    }
    let mut end = CONTENT_LIMIT_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (content[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::{
        ChangeHistoryEntry, TomlChangeHistoryRepository, CONTENT_LIMIT_BYTES, HISTORY_LIMIT,
    };

    fn temp_path() -> String {
        std::env::temp_dir()
            .join(format!("patrol-history-{}.toml", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn history_keeps_previous_snapshot_and_survives_reload() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        repository
            .record("page".into(), 10, "first".into())
            .await
            .unwrap();
        repository
            .record("page".into(), 20, "second".into())
            .await
            .unwrap();

        assert_eq!(
            repository.entries()[1].previous_content.as_deref(),
            Some("first")
        );
        drop(repository);
        let repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        assert_eq!(repository.entries().len(), 2);
        assert_eq!(repository.entries()[1].content, "second");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn history_batch_links_same_target_changes_and_keeps_global_order() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        repository
            .record_batch(vec![
                ("a".into(), 10, "first".into(), false),
                ("b".into(), 11, "other".into(), false),
                ("a".into(), 12, "second".into(), false),
            ])
            .await
            .unwrap();

        assert_eq!(repository.entries().len(), 3);
        assert_eq!(
            repository.entries()[2].previous_content.as_deref(),
            Some("first")
        );
        assert_eq!(repository.entries()[2].timestamp_unix_ms, 12);
        drop(repository);
        let repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        assert_eq!(repository.entries().len(), 3);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn configured_history_limit_controls_retention_and_reload() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::with_limit(&path, 2)
            .await
            .unwrap();
        repository
            .record_batch(vec![
                ("page".into(), 1, "first".into(), false),
                ("page".into(), 2, "second".into(), false),
                ("page".into(), 3, "third".into(), false),
            ])
            .await
            .unwrap();

        assert_eq!(repository.entries().len(), 2);
        assert_eq!(repository.entries()[0].timestamp_unix_ms, 2);
        drop(repository);
        let repository = TomlChangeHistoryRepository::with_limit(&path, 2)
            .await
            .unwrap();
        assert_eq!(repository.entries().len(), 2);
        assert_eq!(repository.entries()[1].content, "third");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn configured_history_limit_is_clamped_to_safe_bounds() {
        let path = temp_path();
        let repository = TomlChangeHistoryRepository::with_limit(&path, 0)
            .await
            .unwrap();
        assert_eq!(repository.history_limit, 1);
        drop(repository);

        let repository = TomlChangeHistoryRepository::with_limit(&path, usize::MAX)
            .await
            .unwrap();
        assert_eq!(repository.history_limit, super::MAX_HISTORY_LIMIT);
        drop(repository);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn failed_history_save_restores_cache_before_a_later_write() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        assert!(repository
            .record("failed".into(), 1, "not persisted".into())
            .await
            .is_err());
        assert!(repository.entries().is_empty());

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "").unwrap();
        repository
            .record("saved".into(), 2, "persisted".into())
            .await
            .unwrap();
        assert_eq!(repository.entries().len(), 1);
        assert_eq!(repository.entries()[0].id, "saved");

        drop(repository);
        let repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        assert_eq!(repository.entries().len(), 1);
        assert_eq!(repository.entries()[0].id, "saved");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn failed_save_restores_entries_removed_by_history_limit() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::with_limit(&path, 2)
            .await
            .unwrap();
        repository
            .record_batch(vec![
                ("first".into(), 1, "one".into(), false),
                ("second".into(), 2, "two".into(), false),
            ])
            .await
            .unwrap();
        let before = repository.entries().to_vec();

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let result = repository
            .record_batch(vec![
                ("third".into(), 3, "three".into(), false),
                ("fourth".into(), 4, "four".into(), false),
                ("fifth".into(), 5, "five".into(), false),
            ])
            .await;

        assert!(result.is_err());
        assert_eq!(repository.entries(), before);
        std::fs::remove_dir(&path).unwrap();
    }

    #[tokio::test]
    async fn history_is_bounded_and_truncates_at_utf8_boundary() {
        let path = temp_path();
        let mut repository = TomlChangeHistoryRepository::new(&path).await.unwrap();
        let long_content = "界".repeat(CONTENT_LIMIT_BYTES);
        repository
            .record("page".into(), 1, long_content)
            .await
            .unwrap();

        assert_eq!(
            repository.entries()[0].content.len(),
            CONTENT_LIMIT_BYTES - 1
        );
        assert!(repository.entries()[0].content_truncated);
        for timestamp in 2..=HISTORY_LIMIT as i64 + 2 {
            repository
                .record("page".into(), timestamp, "x".into())
                .await
                .unwrap();
        }
        assert_eq!(repository.entries().len(), HISTORY_LIMIT);
        assert_eq!(repository.entries()[0].timestamp_unix_ms, 3);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn history_entry_serde_round_trip() {
        let entry = ChangeHistoryEntry {
            id: "page".into(),
            timestamp_unix_ms: 123,
            previous_content: Some("old".into()),
            previous_truncated: false,
            content: "new".into(),
            content_truncated: false,
        };
        let encoded = toml::to_string(&entry).unwrap();
        assert_eq!(
            toml::from_str::<ChangeHistoryEntry>(&encoded).unwrap(),
            entry
        );
    }
}
