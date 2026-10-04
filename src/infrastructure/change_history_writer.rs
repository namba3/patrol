use tokio::sync::{mpsc, watch};

use crate::{
    application::app::DocChangeBatch,
    infrastructure::{ChangeHistoryEntry, TomlChangeHistoryRepository},
};

const WRITE_ATTEMPTS: usize = 3;

pub async fn run_change_history_writer(
    mut rx_history: mpsc::Receiver<DocChangeBatch>,
    mut history_repo: TomlChangeHistoryRepository,
    tx_history_snapshot: watch::Sender<Vec<ChangeHistoryEntry>>,
) {
    while let Some(change_batch) = rx_history.recv().await {
        let changes = change_batch
            .changes
            .into_iter()
            .map(|entry| {
                (
                    entry.id,
                    entry.timestamp_unix_ms,
                    entry.content,
                    entry.content_truncated,
                )
            })
            .collect::<Vec<_>>();
        let mut saved = false;
        for attempt in 1..=WRITE_ATTEMPTS {
            match history_repo.record_batch(changes.clone()).await {
                Ok(()) => {
                    tx_history_snapshot.send_replace(history_repo.entries().to_vec());
                    saved = true;
                    break;
                }
                Err(error) if attempt < WRITE_ATTEMPTS => {
                    log::warn!("failed to save change history (attempt {attempt}): {error}");
                    tokio::time::sleep(std::time::Duration::from_millis(100 * attempt as u64))
                        .await;
                }
                Err(error) => {
                    log::error!("failed to save change history after {attempt} attempts: {error}")
                }
            }
        }
        let _ = change_batch.persisted.send(saved);
    }
}

#[cfg(test)]
mod tests {
    use super::run_change_history_writer;
    use crate::{
        application::app::{DocChangeBatch, DocChangeContent},
        infrastructure::TomlChangeHistoryRepository,
    };

    #[tokio::test]
    async fn history_writer_drains_queued_changes_before_stopping() {
        let path = std::env::temp_dir().join(format!(
            "patrol-history-writer-{}.toml",
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (snapshot_tx, _snapshot_rx) = tokio::sync::watch::channel(Vec::new());
        let writer = tokio::spawn(run_change_history_writer(rx, repository, snapshot_tx));
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();
        tx.send(DocChangeBatch {
            changes: vec![
                DocChangeContent {
                    id: "page".into(),
                    timestamp_unix_ms: 1,
                    content: "first".into(),
                    content_truncated: false,
                },
                DocChangeContent {
                    id: "page".into(),
                    timestamp_unix_ms: 2,
                    content: "second".into(),
                    content_truncated: false,
                },
            ],
            persisted: persisted_tx,
        })
        .await
        .unwrap();
        drop(tx);
        writer.await.unwrap();
        assert!(persisted_rx.await.unwrap());

        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        assert_eq!(repository.entries().len(), 2);
        assert_eq!(repository.entries()[1].content, "second");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn history_writer_reports_persistence_failure_after_retries() {
        let path = std::env::temp_dir().join(format!(
            "patrol-history-writer-error-{}.toml",
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let repository = TomlChangeHistoryRepository::new(&path_string)
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let (snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(Vec::new());
        let writer = tokio::spawn(run_change_history_writer(rx, repository, snapshot_tx));
        let (persisted_tx, persisted_rx) = tokio::sync::oneshot::channel();
        tx.send(DocChangeBatch {
            changes: vec![DocChangeContent {
                id: "page".into(),
                timestamp_unix_ms: 1,
                content: "content".into(),
                content_truncated: false,
            }],
            persisted: persisted_tx,
        })
        .await
        .unwrap();
        drop(tx);
        writer.await.unwrap();

        assert!(!persisted_rx.await.unwrap());
        assert!(snapshot_rx.borrow().is_empty());
        std::fs::remove_dir(path).unwrap();
    }
}
