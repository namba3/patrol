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
