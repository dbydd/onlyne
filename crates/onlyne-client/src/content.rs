//! Client-owned fan-out for content journalled by the session backend.

use anyhow::{Context, Result, anyhow};
use onlyne_proto::{ContentFrame, WatchContentArgs};
use onlyne_session::{ContentRecord, ContentSink, read_content_records};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, watch};

const SUBSCRIBER_CAPACITY: usize = 64;

pub(crate) struct ContentHub {
    workspace: PathBuf,
    state: Mutex<HubState>,
}

struct HubState {
    next_id: u64,
    /// Highest cursor whose journal append reached `publish` in this process.
    published_head: u64,
    subscribers: BTreeMap<u64, Subscriber>,
}

struct Subscriber {
    task_id: Option<String>,
    after: u64,
    tx: mpsc::Sender<ContentRecord>,
    cancel: watch::Sender<bool>,
}

pub(crate) struct ContentSubscription {
    id: u64,
    hub: Weak<ContentHub>,
    pub(crate) replay: Vec<ContentRecord>,
    pub(crate) receiver: mpsc::Receiver<ContentRecord>,
    pub(crate) cancel: watch::Receiver<bool>,
}

impl ContentHub {
    pub(crate) fn new(workspace: impl Into<PathBuf>) -> Arc<Self> {
        let workspace = workspace.into();
        let indexed_head = match read_content_records(&workspace) {
            Ok(records) => records.last().map(|record| record.seq).unwrap_or(0),
            Err(error) => {
                tracing::warn!(error = %error, "content index was not recovered");
                0
            }
        };
        Arc::new(Self {
            workspace,
            state: Mutex::new(HubState {
                next_id: 1,
                published_head: indexed_head,
                subscribers: BTreeMap::new(),
            }),
        })
    }

    /// Atomically cut replay from live delivery and register the live half.
    pub(crate) fn subscribe(
        self: &Arc<Self>,
        args: &WatchContentArgs,
    ) -> Result<ContentSubscription> {
        let (id, cutoff, receiver, cancel) = {
            let mut state = self.state.lock();
            let id = state.next_id;
            state.next_id = state
                .next_id
                .checked_add(1)
                .ok_or_else(|| anyhow!("content subscriber id exhausted"))?;
            let cutoff = state.published_head;
            let after = args.since.map_or(cutoff, |since| since.max(cutoff));
            let (tx, receiver) = mpsc::channel(SUBSCRIBER_CAPACITY);
            let (cancel_tx, cancel) = watch::channel(false);
            state.subscribers.insert(
                id,
                Subscriber {
                    task_id: args.task_id.clone(),
                    after,
                    tx,
                    cancel: cancel_tx,
                },
            );
            (id, cutoff, receiver, cancel)
        };

        let replay = match self.replay(args, cutoff) {
            Ok(replay) => replay,
            Err(error) => {
                self.remove(id);
                return Err(error);
            }
        };
        Ok(ContentSubscription {
            id,
            hub: Arc::downgrade(self),
            replay,
            receiver,
            cancel,
        })
    }

    fn replay(&self, args: &WatchContentArgs, cutoff: u64) -> Result<Vec<ContentRecord>> {
        let mut records = read_content_records(&self.workspace)
            .with_context(|| format!("read content history at {}", self.workspace.display()))?;
        records.retain(|record| {
            record.seq <= cutoff
                && args
                    .task_id
                    .as_deref()
                    .is_none_or(|task_id| record.task_id == task_id)
        });
        match args.since {
            Some(since) => records.retain(|record| record.seq > since),
            None => {
                if let Some(head) = records.pop() {
                    records.clear();
                    records.push(head);
                }
            }
        }
        Ok(records)
    }

    fn remove(&self, id: u64) {
        self.state.lock().subscribers.remove(&id);
    }

    #[cfg(test)]
    pub(crate) fn subscriber_count(&self) -> usize {
        self.state.lock().subscribers.len()
    }
}

impl ContentSink for ContentHub {
    fn publish(&self, record: ContentRecord) {
        let mut state = self.state.lock();
        state.published_head = state.published_head.max(record.seq);
        let mut remove = Vec::new();
        for (id, subscriber) in &state.subscribers {
            if record.seq <= subscriber.after
                || subscriber
                    .task_id
                    .as_deref()
                    .is_some_and(|task_id| task_id != record.task_id)
            {
                continue;
            }
            if let Err(error) = subscriber.tx.try_send(record.clone()) {
                let _ = subscriber.cancel.send(true);
                tracing::warn!(
                    subscriber = *id,
                    seq = record.seq,
                    reason = if matches!(error, mpsc::error::TrySendError::Full(_)) {
                        "slow"
                    } else {
                        "closed"
                    },
                    "content subscriber was removed"
                );
                remove.push(*id);
            }
        }
        for id in remove {
            state.subscribers.remove(&id);
        }
    }
}

impl Drop for ContentSubscription {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            hub.remove(self.id);
        }
    }
}

pub(crate) fn frame(record: ContentRecord) -> ContentFrame {
    ContentFrame {
        seq: record.seq,
        task_id: record.task_id,
        session_id: record.session_id,
        at: record.at,
        record: record.record,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn hub() -> (tempfile::TempDir, Arc<ContentHub>) {
        let dir = tempdir().unwrap();
        let hub = ContentHub::new(dir.path());
        (dir, hub)
    }

    #[tokio::test]
    async fn full_subscriber_is_evicted_without_blocking_publish() {
        let (_dir, hub) = hub();
        let args = WatchContentArgs {
            task_id: None,
            since: Some(0),
        };
        let _subscription = hub.subscribe(&args).unwrap();
        for seq in 1..=(SUBSCRIBER_CAPACITY as u64 + 1) {
            hub.publish(ContentRecord {
                seq,
                task_id: "task".into(),
                session_id: None,
                at: "now".into(),
                record: serde_json::json!({"seq": seq}),
            });
        }
        assert_eq!(hub.subscriber_count(), 0);
    }
}
