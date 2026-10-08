//! Command dispatch — handles each [`ServerCommand`] against shared state.
//!
//! Extracted from the `serve()` loop so the state machine is one focused unit and
//! the crate root keeps to API types plus the server facade. `Stop`/`None`
//! terminate the loop and are handled by `serve()` itself; every other command
//! lands here.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::session::SessionServer;
use crate::state::ServerState;
use crate::{ClientSettingsUpdate, ServerCommand, ServerEvent};

/// The handles and config a command handler needs (everything but loop control).
pub(crate) struct Dispatcher {
    /// Shared server state, guarded for concurrent access with the session task.
    pub(crate) shared_state: Arc<Mutex<ServerState>>,
    /// Session server — pushes settings and recomputes audio routing.
    pub(crate) session_srv: Arc<SessionServer>,
    /// Outbound event channel to the embedder (events + state snapshots).
    pub(crate) event_tx: mpsc::Sender<ServerEvent>,
    /// Playout buffer size in milliseconds (pushed to clients).
    pub(crate) buffer_ms: i32,
}

impl Dispatcher {
    /// Delete every client that has been disconnected for at least `after`,
    /// exactly as [`ServerCommand::DeleteClient`] would. Returns the ids
    /// removed.
    pub(crate) async fn remove_disconnected_clients(
        &self,
        after: std::time::Duration,
    ) -> Vec<String> {
        let Some(cutoff) = std::time::SystemTime::now().checked_sub(after) else {
            return Vec::new();
        };
        let mut s = self.shared_state.lock().await;
        let stale = s.disconnected_clients_since(cutoff);
        if stale.is_empty() {
            return stale;
        }
        for id in &stale {
            s.remove_client(id);
        }
        let snapshot = s.clone();
        drop(s);
        tracing::info!(
            clients = ?stale,
            after = ?after,
            "Removed clients disconnected for too long"
        );
        self.announce_removed_clients(snapshot, None).await;
        stale
    }

    /// Report a structural change after clients were deleted.
    async fn announce_removed_clients(&self, snapshot: ServerState, origin: Option<u64>) {
        let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
        // Mirrors Server.OnUpdate in C++ snapserver after Server.DeleteClient.
        self.notify(origin, ServerEvent::ServerUpdated);
        self.session_srv.update_routing_all().await;
    }

    /// Emit a change notification, wrapped in [`ServerEvent::FromControl`]
    /// when the command came from a control connection.
    fn notify(&self, origin: Option<u64>, event: ServerEvent) {
        let event = match origin {
            Some(origin) => ServerEvent::FromControl {
                origin,
                event: Box::new(event),
            },
            None => event,
        };
        let _ = self.event_tx.try_send(event);
    }

    /// Handle one command, already taken out of its
    /// [`ServerCommand::FromControl`] wrapper (see
    /// [`ServerCommand::split_origin`]): `origin` tags the notifications it
    /// causes. [`ServerEvent::StateChanged`] snapshots are never tagged.
    ///
    /// `Stop`/`None` are handled by the run loop and never reach here.
    pub(crate) async fn dispatch(&self, origin: Option<u64>, cmd: ServerCommand) {
        match cmd {
            ServerCommand::Stop => unreachable!("Stop is handled by the run loop"),
            ServerCommand::FromControl { .. } => {
                unreachable!("the run loop unwraps FromControl")
            }
            ServerCommand::SetClientVolume {
                client_id,
                volume,
                muted,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.volume.percent = volume;
                    c.config.volume.muted = muted;
                }
                let latency = s
                    .clients
                    .get(&client_id)
                    .map(|c| c.config.latency)
                    .unwrap_or(0);
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.session_srv
                    .push_settings(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume,
                        muted,
                    })
                    .await;
                self.notify(
                    origin,
                    ServerEvent::ClientVolumeChanged {
                        client_id: client_id.clone(),
                        volume,
                        muted,
                    },
                );
                self.session_srv.update_routing_for_client(&client_id).await;
            }
            ServerCommand::SetClientLatency { client_id, latency } => {
                let mut settings_update = None;
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.latency = latency;
                    settings_update = Some(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume: c.config.volume.percent,
                        muted: c.config.volume.muted,
                    });
                }
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                if let Some(update) = settings_update {
                    self.session_srv.push_settings(update).await;
                }
                self.notify(
                    origin,
                    ServerEvent::ClientLatencyChanged { client_id, latency },
                );
            }
            ServerCommand::SetClientName { client_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.name = name.clone();
                }
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.notify(origin, ServerEvent::ClientNameChanged { client_id, name });
            }
            ServerCommand::SetGroupStream {
                group_id,
                stream_id,
            } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_stream(&group_id, &stream_id);
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.notify(
                    origin,
                    ServerEvent::GroupStreamChanged {
                        group_id: group_id.clone(),
                        stream_id,
                    },
                );
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupMute { group_id, muted } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.muted = muted;
                }
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.notify(
                    origin,
                    ServerEvent::GroupMuteChanged {
                        group_id: group_id.clone(),
                        muted,
                    },
                );
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupName { group_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.name = name.clone();
                }
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.notify(origin, ServerEvent::GroupNameChanged { group_id, name });
            }
            ServerCommand::SetGroupClients { group_id, clients } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_clients(&group_id, &clients);
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                // Structural change — mirrors Server.OnUpdate in C++ snapserver
                self.notify(origin, ServerEvent::ServerUpdated);
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::DeleteClient { client_id } => {
                let mut s = self.shared_state.lock().await;
                s.remove_client(&client_id);
                let snapshot = s.clone();
                drop(s);
                self.announce_removed_clients(snapshot, origin).await;
            }
            ServerCommand::SetStreamMeta {
                stream_id,
                metadata,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(stream) = s.streams.iter_mut().find(|st| st.id == stream_id) {
                    stream.properties = metadata.clone();
                }
                drop(s);
                self.notify(
                    origin,
                    ServerEvent::StreamMetaChanged {
                        stream_id,
                        metadata,
                    },
                );
            }
            ServerCommand::AddStream { uri, response_tx } => {
                tracing::warn!(
                    uri,
                    "Dynamic stream addition requires application-owned stream orchestration"
                );
                let _ = response_tx.send(Err(
                    "dynamic Stream.AddStream is not supported by the embeddable server after serve(); create streams before serve()".into(),
                ));
            }
            ServerCommand::RemoveStream { stream_id } => {
                let mut s = self.shared_state.lock().await;
                s.streams.retain(|st| st.id != stream_id);
                // Clear stream_id on groups that referenced this stream
                for g in &mut s.groups {
                    if g.stream_id == stream_id {
                        g.stream_id.clear();
                    }
                }
                let snapshot = s.clone();
                drop(s);
                let _ = self.event_tx.try_send(ServerEvent::StateChanged(snapshot));
                self.notify(origin, ServerEvent::ServerUpdated);
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::StreamControl {
                stream_id,
                command,
                params,
            } => {
                tracing::debug!(stream_id, command, ?params, "Stream control forwarded");
                // Forward to embedder via event — the library doesn't own stream readers
                let _ = self.event_tx.try_send(ServerEvent::StreamControl {
                    stream_id,
                    command,
                    params,
                });
            }
            ServerCommand::GetStatus { response_tx } => {
                let s = self.shared_state.lock().await;
                let _ = response_tx.send(s.to_status());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionServerConfig;
    use crate::state::{ServerState, StreamInfo};
    use std::collections::HashMap;

    /// Build a dispatcher over `state`, returning it and its event receiver. The
    /// session server has no registered client senders, so `push_settings` /
    /// `update_routing_*` are no-ops — exactly the isolation we want for testing
    /// the command state machine.
    fn dispatcher_with(state: ServerState) -> (Dispatcher, mpsc::Receiver<ServerEvent>) {
        let shared_state = Arc::new(Mutex::new(state));
        let session_srv = Arc::new(SessionServer::new(SessionServerConfig {
            buffer_ms: 1000,
            auth: None,
            client_filter: None,
            shared_state: Arc::clone(&shared_state),
            default_stream: "default".into(),
            send_audio_to_muted: false,
            idle_timeout: None,
        }));
        let (event_tx, event_rx) = mpsc::channel(256);
        (
            Dispatcher {
                shared_state,
                session_srv,
                event_tx,
                buffer_ms: 1000,
            },
            event_rx,
        )
    }

    #[tokio::test]
    async fn removes_only_clients_disconnected_longer_than_threshold() {
        let mut state = ServerState::default();
        for id in ["old", "recent", "live"] {
            state.get_or_create_client(id, id, id);
            state.group_for_client(id, "default");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let three_days = 3 * 24 * 3600;
        let set = |state: &mut ServerState, id: &str, connected: bool, sec: u64| {
            let c = state.clients.get_mut(id).unwrap();
            c.connected = connected;
            c.last_seen.sec = sec;
        };
        set(&mut state, "old", false, now - three_days);
        set(&mut state, "recent", false, now - 3600);
        // Connected clients stay however stale last_seen looks.
        set(&mut state, "live", true, now - three_days);

        let (d, mut events) = dispatcher_with(state);
        let removed = d
            .remove_disconnected_clients(crate::DEFAULT_REMOVE_DISCONNECTED_CLIENTS_AFTER)
            .await;
        assert_eq!(removed, ["old"]);

        let s = d.shared_state.lock().await;
        assert!(!s.clients.contains_key("old"));
        assert!(s.clients.contains_key("recent"));
        assert!(s.clients.contains_key("live"));
        assert!(
            !s.groups
                .iter()
                .any(|g| g.clients.iter().any(|c| c == "old")),
            "removed from its group"
        );
        assert_eq!(s.groups.len(), 2, "the emptied group is gone");
        drop(s);
        assert!(matches!(
            events.try_recv(),
            Ok(ServerEvent::StateChanged(_))
        ));
        assert!(matches!(events.try_recv(), Ok(ServerEvent::ServerUpdated)));

        // Nothing left to remove: no events.
        let removed = d
            .remove_disconnected_clients(crate::DEFAULT_REMOVE_DISCONNECTED_CLIENTS_AFTER)
            .await;
        assert!(removed.is_empty());
        assert!(events.try_recv().is_err());
    }

    /// State with one client `c1` in a group on stream `default`; returns the group id.
    fn state_with_client() -> (ServerState, String) {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        let gid = state.group_for_client("c1", "default").id.clone();
        (state, gid)
    }

    fn drain(rx: &mut mpsc::Receiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[tokio::test]
    async fn set_client_volume_mutates_state_and_emits_events() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetClientVolume {
                client_id: "c1".into(),
                volume: 42,
                muted: true,
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            let c = s.clients.get("c1").unwrap();
            assert_eq!(c.config.volume.percent, 42);
            assert!(c.config.volume.muted);
        }
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::StateChanged(_)))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::ClientVolumeChanged {
                volume: 42,
                muted: true,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn set_client_volume_unknown_client_is_noop_but_still_notifies() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(
            None,
            ServerCommand::SetClientVolume {
                client_id: "ghost".into(),
                volume: 10,
                muted: false,
            },
        )
        .await;
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientVolumeChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_client_latency_updates_config() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetClientLatency {
                client_id: "c1".into(),
                latency: 33,
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            assert_eq!(s.clients.get("c1").unwrap().config.latency, 33);
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientLatencyChanged { latency: 33, .. }))
        );
    }

    #[tokio::test]
    async fn set_client_name_updates_config() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetClientName {
                client_id: "c1".into(),
                name: "Kitchen".into(),
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            assert_eq!(s.clients.get("c1").unwrap().config.name, "Kitchen");
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientNameChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_group_stream_reassigns_and_emits() {
        let (state, gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetGroupStream {
                group_id: gid.clone(),
                stream_id: "living-room".into(),
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert_eq!(g.stream_id, "living-room");
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupStreamChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_group_mute_and_name() {
        let (state, gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetGroupMute {
                group_id: gid.clone(),
                muted: true,
            },
        )
        .await;
        d.dispatch(
            None,
            ServerCommand::SetGroupName {
                group_id: gid.clone(),
                name: "Upstairs".into(),
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert!(g.muted);
            assert_eq!(g.name, "Upstairs");
        }
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupMuteChanged { muted: true, .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupNameChanged { .. }))
        );
    }

    #[tokio::test]
    async fn delete_client_removes_from_state_and_groups() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::DeleteClient {
                client_id: "c1".into(),
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            assert!(!s.clients.contains_key("c1"));
            assert!(
                s.groups
                    .iter()
                    .all(|g| !g.clients.contains(&"c1".to_string()))
            );
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn set_group_clients_reorganizes() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        state.get_or_create_client("c2", "h2", "m2");
        let gid = state.group_for_client("c1", "default").id.clone();
        state.group_for_client("c2", "default");
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::SetGroupClients {
                group_id: gid.clone(),
                clients: vec!["c1".into(), "c2".into()],
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert_eq!(g.clients.len(), 2);
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn set_stream_meta_updates_properties() {
        let mut state = ServerState::default();
        state.streams.push(StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });
        let (d, mut rx) = dispatcher_with(state);
        let mut meta = HashMap::new();
        meta.insert("title".to_string(), serde_json::json!("Song"));
        d.dispatch(
            None,
            ServerCommand::SetStreamMeta {
                stream_id: "default".into(),
                metadata: meta,
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            let st = s.streams.iter().find(|st| st.id == "default").unwrap();
            assert_eq!(st.properties.get("title"), Some(&serde_json::json!("Song")));
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::StreamMetaChanged { .. }))
        );
    }

    #[tokio::test]
    async fn add_stream_is_rejected_for_embeddable_server() {
        let (d, _rx) = dispatcher_with(ServerState::default());
        let (tx, rx) = tokio::sync::oneshot::channel();
        d.dispatch(
            None,
            ServerCommand::AddStream {
                uri: "pipe:///tmp/x".into(),
                response_tx: tx,
            },
        )
        .await;
        assert!(rx.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn remove_stream_clears_group_reference() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        let gid = state.group_for_client("c1", "s1").id.clone();
        state.streams.push(StreamInfo {
            id: "s1".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/x".into(),
            properties: Default::default(),
        });
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(
            None,
            ServerCommand::RemoveStream {
                stream_id: "s1".into(),
            },
        )
        .await;
        {
            let s = d.shared_state.lock().await;
            assert!(s.streams.iter().all(|st| st.id != "s1"));
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert!(g.stream_id.is_empty());
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn stream_control_is_forwarded_as_event() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(
            None,
            ServerCommand::StreamControl {
                stream_id: "s1".into(),
                command: "next".into(),
                params: serde_json::Value::Null,
            },
        )
        .await;
        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            ServerEvent::StreamControl { command, .. } if command == "next"
        )));
    }

    #[tokio::test]
    async fn get_status_responds_with_snapshot() {
        let (state, _gid) = state_with_client();
        let (d, _rx) = dispatcher_with(state);
        let (tx, rx) = tokio::sync::oneshot::channel();
        d.dispatch(None, ServerCommand::GetStatus { response_tx: tx })
            .await;
        let status = rx.await.unwrap();
        assert_eq!(status.server.groups.len(), 1);
    }

    #[tokio::test]
    async fn tagged_command_wraps_its_notifications_only() {
        let (state, gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        let tagged = ServerCommand::FromControl {
            origin: 7,
            command: Box::new(ServerCommand::SetClientVolume {
                client_id: "c1".into(),
                volume: 42,
                muted: false,
            }),
        };
        let (origin, cmd) = tagged.split_origin();
        assert_eq!(origin, Some(7));
        d.dispatch(origin, cmd).await;
        let events = drain(&mut rx);
        assert!(
            matches!(events[0], ServerEvent::StateChanged(_)),
            "untagged"
        );
        assert!(matches!(
            &events[1],
            ServerEvent::FromControl { origin: 7, event }
                if matches!(**event, ServerEvent::ClientVolumeChanged { volume: 42, .. })
        ));
        assert_eq!(events.len(), 2);

        // Structural changes tag their Server.OnUpdate.
        d.dispatch(
            Some(8),
            ServerCommand::SetGroupClients {
                group_id: gid,
                clients: vec!["c1".into()],
            },
        )
        .await;
        let events = drain(&mut rx);
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::FromControl { origin: 8, event }
                if matches!(**event, ServerEvent::ServerUpdated)
        )));

        // An untagged command is reported as before.
        d.dispatch(
            None,
            ServerCommand::SetClientName {
                client_id: "c1".into(),
                name: "Kitchen".into(),
            },
        )
        .await;
        let (origin, event) = drain(&mut rx).pop().unwrap().split_origin();
        assert_eq!(origin, None);
        assert!(matches!(event, ServerEvent::ClientNameChanged { .. }));
    }

    #[test]
    fn split_origin_unwraps_nested_tags() {
        let cmd = ServerCommand::FromControl {
            origin: 1,
            command: Box::new(ServerCommand::FromControl {
                origin: 2,
                command: Box::new(ServerCommand::Stop),
            }),
        };
        let (origin, cmd) = cmd.split_origin();
        assert_eq!(origin, Some(2));
        assert!(matches!(cmd, ServerCommand::Stop));
        assert!(matches!(
            ServerCommand::Stop.split_origin(),
            (None, ServerCommand::Stop)
        ));
    }
}
