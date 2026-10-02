//! Server state model — clients, groups, streams with JSON persistence.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Volume settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    /// Volume percentage (0–100).
    pub percent: u16,
    /// Mute state.
    pub muted: bool,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            percent: 100,
            muted: false,
        }
    }
}

/// Client configuration (persisted).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Display name (user-assigned).
    #[serde(default)]
    pub name: String,
    /// Volume.
    #[serde(default)]
    pub volume: Volume,
    /// Additional latency in milliseconds.
    #[serde(default)]
    pub latency: i32,
}

/// A connected or previously-seen client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Client {
    /// Unique client ID.
    pub id: String,
    /// Hostname.
    pub host_name: String,
    /// MAC address.
    pub mac: String,
    /// Whether currently connected.
    pub connected: bool,
    /// Client configuration.
    pub config: ClientConfig,
    /// IP address of the last connection.
    #[serde(default)]
    pub ip: String,
    /// Operating system reported in Hello.
    #[serde(default)]
    pub os: String,
    /// CPU architecture reported in Hello.
    #[serde(default)]
    pub arch: String,
    /// Instance number reported in Hello.
    #[serde(default)]
    pub instance: u32,
    /// Client software name reported in Hello (e.g. "Snapclient").
    #[serde(default)]
    pub client_name: String,
    /// Client software version reported in Hello.
    #[serde(default)]
    pub version: String,
    /// Binary protocol version reported in Hello.
    #[serde(default)]
    pub protocol_version: u32,
    /// Wall-clock time the client was last seen (Hello, time sync, disconnect).
    #[serde(default)]
    pub last_seen: crate::status::LastSeen,
}

impl Client {
    /// Record the details a client reports in its Hello and the address it
    /// connected from.
    pub fn update_from_hello(&mut self, hello: &snapcast_proto::message::hello::Hello, ip: &str) {
        self.host_name.clone_from(&hello.host_name);
        self.mac.clone_from(&hello.mac);
        self.ip = ip.to_string();
        self.os.clone_from(&hello.os);
        self.arch.clone_from(&hello.arch);
        self.instance = hello.instance;
        self.client_name.clone_from(&hello.client_name);
        self.version.clone_from(&hello.version);
        self.protocol_version = hello.snap_stream_protocol_version;
        self.touch();
    }

    /// Set `last_seen` to the current wall-clock time.
    pub fn touch(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        self.last_seen = crate::status::LastSeen {
            sec: now.as_secs(),
            usec: u64::from(now.subsec_micros()),
        };
    }
}

/// A group of clients sharing the same stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// Unique group ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Stream ID this group is playing.
    pub stream_id: String,
    /// Group mute state.
    pub muted: bool,
    /// Client IDs in this group.
    pub clients: Vec<String>,
}

/// A stream source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Stream ID (= name from URI).
    pub id: String,
    /// Status: "playing", "idle", "unknown".
    pub status: String,
    /// Source URI.
    pub uri: String,
    /// Stream properties (metadata: artist, title, etc.).
    #[serde(default)]
    pub properties: std::collections::HashMap<String, serde_json::Value>,
}

/// Complete server state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerState {
    /// All known clients (by ID).
    pub clients: HashMap<String, Client>,
    /// All groups.
    pub groups: Vec<Group>,
    /// All streams.
    pub streams: Vec<StreamInfo>,
}

impl ServerState {
    /// Get or create a client entry. Returns mutable reference.
    pub fn get_or_create_client(&mut self, id: &str, host_name: &str, mac: &str) -> &mut Client {
        if !self.clients.contains_key(id) {
            tracing::debug!(client_id = id, host_name, mac, "client created");
            self.clients.insert(
                id.to_string(),
                Client {
                    id: id.to_string(),
                    host_name: host_name.to_string(),
                    mac: mac.to_string(),
                    connected: false,
                    config: ClientConfig::default(),
                    ip: String::new(),
                    os: String::new(),
                    arch: String::new(),
                    instance: 0,
                    client_name: String::new(),
                    version: String::new(),
                    protocol_version: 0,
                    last_seen: Default::default(),
                },
            );
        } else {
            // Update host info on reconnect (may change after OS reinstall etc.)
            let c = self.clients.get_mut(id).unwrap();
            c.host_name = host_name.to_string();
            c.mac = mac.to_string();
        }
        self.clients.get_mut(id).expect("just inserted")
    }

    /// Find which group a client belongs to, or create a new group.
    pub fn group_for_client(&mut self, client_id: &str, default_stream: &str) -> &mut Group {
        // Check if client is already in a group
        let idx = self
            .groups
            .iter()
            .position(|g| g.clients.contains(&client_id.to_string()));

        if let Some(idx) = idx {
            return &mut self.groups[idx];
        }

        // Create new group with this client
        let group = Group {
            id: generate_id(),
            name: String::new(),
            stream_id: default_stream.to_string(),
            muted: false,
            clients: vec![client_id.to_string()],
        };
        self.groups.push(group);
        self.groups.last_mut().expect("just pushed")
    }

    /// Remove a client from all groups.
    pub fn remove_client_from_groups(&mut self, client_id: &str) {
        for group in &mut self.groups {
            group.clients.retain(|c| c != client_id);
        }
        // Remove empty groups
        self.groups.retain(|g| !g.clients.is_empty());
    }

    /// Set the clients of a group (C++ Group.SetClients semantics).
    ///
    /// - Clients removed from the target group get their own new group (inheriting the stream).
    /// - Clients added are moved from their old groups (empty old groups are removed).
    /// - If the target group ends up empty, it is removed.
    pub fn set_group_clients(&mut self, group_id: &str, client_ids: &[String]) {
        // Find the target group's stream for inheritance
        let stream_id = self
            .groups
            .iter()
            .find(|g| g.id == group_id)
            .map(|g| g.stream_id.clone())
            .unwrap_or_default();

        // 1. Evict clients NOT in the new list → create new group for each
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            let evicted: Vec<String> = group
                .clients
                .iter()
                .filter(|c| !client_ids.contains(c))
                .cloned()
                .collect();
            group.clients.retain(|c| client_ids.contains(c));
            for cid in evicted {
                let new_group = Group {
                    id: generate_id(),
                    name: String::new(),
                    stream_id: stream_id.clone(),
                    muted: false,
                    clients: vec![cid],
                };
                self.groups.push(new_group);
            }
        }

        // 2. Add clients to the target group (move from old groups)
        for cid in client_ids {
            let already_in_target = self
                .groups
                .iter()
                .any(|g| g.id == group_id && g.clients.contains(cid));
            if already_in_target {
                continue;
            }
            // Remove from old group
            for group in &mut self.groups {
                group.clients.retain(|c| c != cid);
            }
            // Add to target
            if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
                group.clients.push(cid.clone());
            }
        }

        // 3. Remove empty groups
        self.groups.retain(|g| !g.clients.is_empty());
    }

    /// Set a group's stream.
    pub fn set_group_stream(&mut self, group_id: &str, stream_id: &str) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == group_id) {
            group.stream_id = stream_id.to_string();
        }
    }

    /// Build typed status snapshot.
    pub fn to_status(&self) -> crate::status::ServerStatus {
        use crate::status;
        let groups = self
            .groups
            .iter()
            .map(|g| {
                let clients = g
                    .clients
                    .iter()
                    .filter_map(|cid| self.clients.get(cid))
                    .map(|c| status::Client {
                        id: c.id.clone(),
                        connected: c.connected,
                        config: status::ClientConfig {
                            name: c.config.name.clone(),
                            volume: status::Volume {
                                percent: c.config.volume.percent,
                                muted: c.config.volume.muted,
                            },
                            latency: c.config.latency,
                            instance: c.instance,
                        },
                        host: status::Host {
                            arch: c.arch.clone(),
                            ip: c.ip.clone(),
                            mac: c.mac.clone(),
                            name: c.host_name.clone(),
                            os: c.os.clone(),
                        },
                        snapclient: status::Snapclient {
                            name: c.client_name.clone(),
                            protocol_version: c.protocol_version,
                            version: c.version.clone(),
                        },
                        last_seen: c.last_seen.clone(),
                    })
                    .collect();
                status::Group {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    stream_id: g.stream_id.clone(),
                    muted: g.muted,
                    clients,
                }
            })
            .collect();
        let streams = self
            .streams
            .iter()
            .map(|s| status::Stream {
                id: s.id.clone(),
                status: status::StreamStatus::from(s.status.as_str()),
                uri: status::StreamUri::parse(&s.uri),
                properties: (!s.properties.is_empty())
                    .then(|| {
                        serde_json::from_value(serde_json::Value::Object(
                            s.properties.clone().into_iter().collect(),
                        ))
                        .inspect_err(
                            |e| tracing::warn!(stream = %s.id, "Invalid stream properties: {e}"),
                        )
                        .ok()
                    })
                    .flatten(),
            })
            .collect();
        status::ServerStatus {
            server: status::Server {
                groups,
                streams,
                ..Default::default()
            },
        }
    }
}

fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_lifecycle() {
        let mut state = ServerState::default();
        let c = state.get_or_create_client("abc", "myhost", "aa:bb:cc:dd:ee:ff");
        assert_eq!(c.id, "abc");
        assert_eq!(c.config.volume.percent, 100);

        let g = state.group_for_client("abc", "default");
        assert_eq!(g.clients, vec!["abc"]);
        let gid = g.id.clone();

        // Same client → same group
        let g2 = state.group_for_client("abc", "default");
        assert_eq!(g2.id, gid);
    }

    #[test]
    fn set_group_clients_moves_and_evicts() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        state.get_or_create_client("c2", "h2", "m2");
        state.get_or_create_client("c3", "h3", "m3");
        let g1 = state.group_for_client("c1", "s1").id.clone();
        state.group_for_client("c2", "s1");
        state.group_for_client("c3", "s1");
        assert_eq!(state.groups.len(), 3);

        // Move c2 and c3 into g1 (c1's group)
        state.set_group_clients(&g1, &["c1".into(), "c2".into(), "c3".into()]);
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].clients.len(), 3);

        // Evict c2 — should get its own group inheriting the stream
        state.set_group_clients(&g1, &["c1".into(), "c3".into()]);
        assert_eq!(state.groups.len(), 2);
        let evicted_group = state.groups.iter().find(|g| g.id != g1).unwrap();
        assert_eq!(evicted_group.clients, vec!["c2"]);
        assert_eq!(evicted_group.stream_id, "s1");
    }

    #[test]
    fn json_roundtrip() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        state.group_for_client("c1", "default");
        state.streams.push(StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });

        let json = serde_json::to_string(&state).unwrap();
        let restored: ServerState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.clients.len(), 1);
        assert_eq!(restored.groups.len(), 1);
        assert_eq!(restored.streams.len(), 1);
    }

    #[test]
    fn status_json() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        state.group_for_client("c1", "default");
        let status = state.to_status();
        assert_eq!(status.server.groups.len(), 1);
        assert_eq!(status.server.groups[0].clients.len(), 1);
    }

    #[test]
    fn status_includes_stream_properties() {
        let mut state = ServerState::default();
        state.streams.push(StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });
        state.streams.push(StreamInfo {
            id: "music".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/music".into(),
            properties: [
                ("playbackStatus".into(), serde_json::json!("playing")),
                (
                    "metadata".into(),
                    serde_json::json!({"title": "Song", "artist": ["A"]}),
                ),
            ]
            .into(),
        });

        let json = serde_json::to_value(state.to_status()).unwrap();
        let streams = &json["server"]["streams"];
        assert!(streams[0].get("properties").is_none());
        assert_eq!(streams[0]["uri"]["scheme"], "pipe");
        assert_eq!(streams[0]["uri"]["path"], "/tmp/snapfifo");
        assert_eq!(streams[0]["uri"]["raw"], "pipe:///tmp/snapfifo");
        let props = &streams[1]["properties"];
        assert_eq!(props["playbackStatus"], "playing");
        assert_eq!(props["metadata"]["title"], "Song");
        assert_eq!(props["metadata"]["artist"][0], "A");
    }

    #[test]
    fn generate_id_is_uuid_format() {
        let id = generate_id();
        // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5, "expected 5 UUID parts, got: {id}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        // All hex
        assert!(id.replace('-', "").chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn concurrent_mutations_keep_each_client_in_one_group() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        // ServerState is always reached through Arc<Mutex<_>> in the server, so the
        // realistic concern is that interleaved mutations under the lock keep the
        // routing invariant intact: every client belongs to exactly one group.
        let state = Arc::new(Mutex::new(ServerState::default()));
        {
            let mut s = state.lock().await;
            for i in 0..10 {
                let id = format!("c{i}");
                s.get_or_create_client(&id, "host", "mac");
                s.group_for_client(&id, "default");
            }
        }

        let mut handles = Vec::new();
        for t in 0..8u32 {
            let st = Arc::clone(&state);
            handles.push(tokio::spawn(async move {
                for i in 0..50u32 {
                    let id = format!("c{}", (t + i) % 10);
                    let mut s = st.lock().await;
                    if let Some(c) = s.clients.get_mut(&id) {
                        c.config.volume.muted = i % 2 == 0;
                    }
                    let gid = s.group_for_client(&id, "default").id.clone();
                    s.set_group_stream(&gid, "streamX");
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let s = state.lock().await;
        for cid in s.clients.keys() {
            let count = s.groups.iter().filter(|g| g.clients.contains(cid)).count();
            assert_eq!(count, 1, "client {cid} must be in exactly one group");
        }
        // The churn must not leave any group empty.
        assert!(s.groups.iter().all(|g| !g.clients.is_empty()));
    }
}
