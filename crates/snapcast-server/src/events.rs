//! Delivery of [`ServerEvent`]s that must not be lost to a full channel.
//!
//! Change notifications and [`ServerEvent::StateChanged`] snapshots are
//! emitted from the command dispatcher and the client sessions, which must
//! never wait for a slow embedder. They are sent at once when the channel
//! has room. When it is full:
//!
//! - a snapshot is kept, replaced by any later one, and sent by a background
//!   task as soon as there is room: the embedder always ends up with the
//!   latest state;
//! - a dropped notification is followed by [`ServerEvent::ServerUpdated`]
//!   once there is room, telling the embedder to re-read the whole status.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::ServerEvent;
use crate::state::ServerState;

/// Shared sender of server events. See the module docs.
#[derive(Clone)]
pub(crate) struct EventSink {
    tx: mpsc::Sender<ServerEvent>,
    backlog: Arc<Mutex<Backlog>>,
}

/// What waits for room in the channel.
#[derive(Default)]
struct Backlog {
    /// Latest snapshot not yet sent.
    state: Option<ServerState>,
    /// A notification was dropped: send `ServerUpdated`.
    resync: bool,
    /// A task is sending the backlog.
    draining: bool,
}

impl EventSink {
    pub(crate) fn new(tx: mpsc::Sender<ServerEvent>) -> Self {
        Self {
            tx,
            backlog: Arc::default(),
        }
    }

    /// The underlying channel, for events a task may wait to send.
    pub(crate) fn sender(&self) -> &mpsc::Sender<ServerEvent> {
        &self.tx
    }

    fn backlog(&self) -> std::sync::MutexGuard<'_, Backlog> {
        self.backlog.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Report the state after a mutation. Never waits; never lost.
    pub(crate) fn state_changed(&self, snapshot: ServerState) {
        let mut backlog = self.backlog();
        // Behind an older snapshot still waiting: replace it, so snapshots
        // stay in order.
        if backlog.draining {
            backlog.state = Some(snapshot);
            return;
        }
        match self.tx.try_send(ServerEvent::StateChanged(snapshot)) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(ServerEvent::StateChanged(snapshot))) => {
                backlog.state = Some(snapshot);
                self.drain(backlog);
            }
            Err(mpsc::error::TrySendError::Full(_)) => unreachable!("sent StateChanged"),
        }
    }

    /// Emit a change notification. Never waits: when the channel is full it
    /// is dropped, and `ServerUpdated` follows once there is room.
    pub(crate) fn notify(&self, event: ServerEvent) {
        if let Err(mpsc::error::TrySendError::Full(event)) = self.tx.try_send(event) {
            tracing::warn!(
                ?event,
                "Event channel full, notification dropped; a full update follows"
            );
            let mut backlog = self.backlog();
            backlog.resync = true;
            self.drain(backlog);
        }
    }

    /// Start the task sending the backlog, unless it runs already.
    fn drain(&self, mut backlog: std::sync::MutexGuard<'_, Backlog>) {
        if backlog.draining {
            return;
        }
        backlog.draining = true;
        drop(backlog);
        let sink = self.clone();
        tokio::spawn(async move {
            loop {
                let Ok(permit) = sink.tx.reserve().await else {
                    return;
                };
                let mut backlog = sink.backlog();
                if let Some(state) = backlog.state.take() {
                    permit.send(ServerEvent::StateChanged(state));
                } else if std::mem::take(&mut backlog.resync) {
                    permit.send(ServerEvent::ServerUpdated);
                } else {
                    backlog.draining = false;
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(name: &str) -> ServerState {
        let mut s = ServerState::default();
        s.get_or_create_client(name, name, name);
        s
    }

    /// Regression: with the channel full, the last snapshot was dropped and
    /// the embedder kept an obsolete state.
    #[tokio::test]
    async fn latest_snapshot_survives_a_full_channel() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = EventSink::new(tx);
        sink.notify(ServerEvent::ServerUpdated);
        sink.state_changed(state_with("a"));
        sink.state_changed(state_with("b"));
        sink.notify(ServerEvent::GroupMuteChanged {
            group_id: "g".into(),
            muted: true,
        });

        let mut events = Vec::new();
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
        {
            events.push(ev);
        }
        assert!(matches!(events[0], ServerEvent::ServerUpdated));
        let ServerEvent::StateChanged(state) = &events[1] else {
            panic!("expected the latest snapshot, got {:?}", events[1]);
        };
        assert!(state.clients.contains_key("b"), "the latest one");
        assert!(
            matches!(events[2], ServerEvent::ServerUpdated),
            "the dropped notification is replaced by a full update"
        );
        assert_eq!(events.len(), 3);

        // The backlog is empty again: events go straight through.
        sink.state_changed(state_with("c"));
        assert!(matches!(rx.try_recv(), Ok(ServerEvent::StateChanged(_))));
    }
}
