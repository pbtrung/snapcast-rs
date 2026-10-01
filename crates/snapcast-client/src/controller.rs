//! Controller — main orchestrator that wires connection, decoder, stream, and player.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::factory::{MessagePayload, TypedMessage};
use snapcast_proto::message::hello::{Auth, Hello};
use snapcast_proto::message::server_settings::ServerSettings;
use snapcast_proto::message::time::Time;
use snapcast_proto::{MessageType, SampleFormat};
use tokio::sync::mpsc;

use crate::connection::SnapConnection;
use crate::decoder::{self, Decoder, PcmDecoder};
use crate::stream::{PcmChunk, SampleEncoding, Stream};
use crate::time_provider::TimeProvider;
use crate::{ClientCommand, ClientEvent};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_RECONNECT_DELAY_SECS: u32 = 30;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const SYNC_INTERVAL: Duration = Duration::from_secs(1);
const QUICK_SYNC_INTERVAL: Duration = Duration::from_millis(100);

/// Main orchestrator wiring connection, decoder, stream, and audio output.
pub struct Controller {
    settings: crate::ClientConfig,
    connection: SnapConnection,
    time_provider: Arc<Mutex<TimeProvider>>,
    stream: Option<Arc<Mutex<Stream>>>,
    decoder: Option<Box<dyn Decoder>>,
    sample_format: SampleFormat,
    sample_encoding: SampleEncoding,
    server_settings: Option<ServerSettings>,
    event_tx: mpsc::Sender<ClientEvent>,
    audio_tx: mpsc::Sender<crate::AudioFrame>,
    command_rx: mpsc::Receiver<ClientCommand>,
}

impl Controller {
    /// Create a new controller with the given settings and event channels.
    pub fn new(
        settings: crate::ClientConfig,
        event_tx: mpsc::Sender<ClientEvent>,
        command_rx: mpsc::Receiver<ClientCommand>,
        audio_tx: mpsc::Sender<crate::AudioFrame>,
        time_provider: Arc<Mutex<TimeProvider>>,
        stream: Arc<Mutex<Stream>>,
    ) -> Result<Self> {
        let connection = SnapConnection::new(&settings.scheme, &settings.host, settings.port)?;

        Ok(Self {
            connection,
            settings,
            time_provider,
            stream: Some(stream),
            decoder: None,
            sample_format: SampleFormat::default(),
            sample_encoding: SampleEncoding::PcmInt,
            server_settings: None,
            event_tx,
            audio_tx,
            command_rx,
        })
    }

    /// Run the client, reconnecting on errors until stopped.
    pub async fn run(&mut self) -> Result<()> {
        let mut attempts = 0u32;
        loop {
            match self.session().await {
                Ok(()) => {
                    self.cleanup();
                    return Ok(());
                }
                Err(e) => {
                    if attempts == 0 {
                        tracing::warn!("Connection failed: {e}");
                    } else {
                        tracing::debug!("Reconnect attempt {attempts} failed: {e}");
                    }
                    self.emit(ClientEvent::Disconnected {
                        reason: e.to_string(),
                    });
                    attempts = attempts.saturating_add(1);
                }
            }
            self.cleanup();
            let delay = Duration::from_secs(attempts.min(MAX_RECONNECT_DELAY_SECS) as u64);
            tokio::time::sleep(delay).await;
        }
    }

    async fn session(&mut self) -> Result<()> {
        if self.settings.host.is_empty() {
            bail!("No server host configured — specify a server address");
        }

        self.connection.connect().await?;
        tracing::info!(
            scheme = %self.settings.scheme,
            host = %self.settings.host,
            port = self.settings.port,
            "Connected"
        );
        self.emit(ClientEvent::Connected {
            host: self.settings.host.clone(),
            port: self.settings.port,
        });

        self.send_hello().await?;
        self.receive_loop().await
    }

    async fn send_hello(&mut self) -> Result<()> {
        let mac = get_mac_address();
        let host_id = if self.settings.host_id.is_empty() {
            mac.clone()
        } else {
            self.settings.host_id.clone()
        };

        let auth = self.settings.auth.as_ref().map(|a| Auth {
            scheme: a.scheme.clone(),
            param: a.param.clone(),
        });

        let hello = Hello {
            mac: mac.clone(),
            host_name: hostname(),
            version: VERSION.to_string(),
            client_name: self.settings.client_name.clone(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            instance: self.settings.instance,
            id: host_id,
            snap_stream_protocol_version: snapcast_proto::PROTOCOL_VERSION,
            auth,
        };

        self.connection
            .send(MessageType::Hello, &MessagePayload::Hello(hello))
            .await?;

        // Expect ServerSettings as first or one of first messages
        loop {
            let response = self.recv_timeout(HELLO_TIMEOUT).await?;
            match response.payload {
                MessagePayload::ServerSettings(ss) => {
                    self.emit(ClientEvent::ServerSettings {
                        buffer_ms: ss.buffer_ms,
                        latency: ss.latency,
                        volume: ss.volume,
                        muted: ss.muted,
                    });
                    self.server_settings = Some(ss);
                    return Ok(());
                }
                MessagePayload::CodecHeader(ch) => {
                    self.init_audio_pipeline(&ch)?;
                }
                MessagePayload::Error(e) => {
                    bail!("Server rejected Hello: {} (code {})", e.error, e.code)
                }
                _ => tracing::debug!(
                    "Ignoring message during handshake: {:?}",
                    response.base.msg_type
                ),
            }
        }
    }

    async fn receive_loop(&mut self) -> Result<()> {
        let mut sync_timer = tokio::time::interval(SYNC_INTERVAL);
        const INITIAL_QUICK_SYNCS: u32 = 50;
        let mut quick_syncs_remaining = INITIAL_QUICK_SYNCS;
        let mut quick_sync_timer = tokio::time::interval(QUICK_SYNC_INTERVAL);

        self.connection
            .send(MessageType::Time, &MessagePayload::Time(Time::new()))
            .await
            .ok();

        loop {
            tokio::select! {
                msg = self.connection.recv() => {
                    let msg = msg?;
                    self.handle_message(msg)?;
                }
                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(ClientCommand::Stop) | None => {
                            tracing::info!("Stop command received");
                            return Ok(());
                        }
                        Some(ClientCommand::SetVolume { volume, muted }) => {
                            tracing::debug!(volume, muted, "Sending volume change to server");
                            self.connection
                                .send(
                                    MessageType::ClientInfo,
                                    &MessagePayload::ClientInfo(
                                        snapcast_proto::message::client_info::ClientInfo {
                                            volume,
                                            muted,
                                        },
                                    ),
                                )
                                .await?;
                        }
                    }
                }
                _ = quick_sync_timer.tick(), if quick_syncs_remaining > 0 => {
                    quick_syncs_remaining -= 1;
                    self.connection
                        .send(MessageType::Time, &MessagePayload::Time(Time::new()))
                        .await
                        .ok();
                    if quick_syncs_remaining == 0 {
                        let diff = self.time_provider.lock().unwrap_or_else(|e| e.into_inner()).diff_to_server_usec();
                        let diff_ms = diff as f64 / 1000.0;
                        tracing::info!(diff_ms, "Time sync complete");
                        self.emit(ClientEvent::TimeSyncComplete { diff_ms });
                    }
                }
                _ = sync_timer.tick(), if quick_syncs_remaining == 0 => {
                    self.connection
                        .send(MessageType::Time, &MessagePayload::Time(Time::new()))
                        .await
                        .ok();
                }
            }
        }
    }

    fn handle_message(&mut self, msg: TypedMessage) -> Result<()> {
        match msg.payload {
            MessagePayload::WireChunk(wc) => {
                if let Some(ref mut dec) = self.decoder {
                    let mut data = wc.payload;
                    if dec.decode(&mut data)? {
                        let chunk = PcmChunk::new_with_encoding(
                            wc.timestamp,
                            data.clone(),
                            self.sample_format,
                            self.sample_encoding,
                        );
                        if let Some(ref stream) = self.stream {
                            stream
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .add_chunk(chunk);
                        }

                        // Also send to external audio_tx
                        let samples =
                            samples_to_f32(&data, self.sample_format, self.sample_encoding);

                        if !samples.is_empty() {
                            let _ = self.audio_tx.try_send(crate::AudioFrame {
                                samples,
                                sample_rate: self.sample_format.rate(),
                                channels: self.sample_format.channels(),
                                timestamp_usec: wc.timestamp.sec as i64 * 1_000_000
                                    + wc.timestamp.usec as i64,
                            });
                        }
                    }
                }
            }
            MessagePayload::ServerSettings(ss) => {
                self.emit(ClientEvent::ServerSettings {
                    buffer_ms: ss.buffer_ms,
                    latency: ss.latency,
                    volume: ss.volume,
                    muted: ss.muted,
                });
                self.emit(ClientEvent::VolumeChanged {
                    volume: ss.volume,
                    muted: ss.muted,
                });
                self.apply_server_settings(&ss);
                self.server_settings = Some(ss);
            }
            MessagePayload::CodecHeader(ch) => {
                self.init_audio_pipeline(&ch)?;
            }
            MessagePayload::Time(t) => {
                let s2c = msg.base.received - msg.base.sent;
                self.time_provider
                    .lock()
                    .unwrap()
                    .set_diff(&t.latency, &s2c);
            }
            MessagePayload::Error(e) => {
                tracing::error!(code = e.code, error = %e.error, "Server error");
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_server_settings(&mut self, ss: &ServerSettings) {
        if let Some(ref stream) = self.stream {
            let buf_ms = (ss.buffer_ms - ss.latency - self.settings.latency).max(0);
            stream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_buffer_ms(buf_ms as i64);
        }
    }

    fn init_audio_pipeline(&mut self, header: &CodecHeader) -> Result<()> {
        let mut dec: Box<dyn Decoder> = match header.codec.as_str() {
            "pcm" => Box::new(PcmDecoder::new()),
            "flac" => Box::new(decoder::flac::create(header)?),
            "ogg" => Box::new(decoder::vorbis::create(header)?),
            "opus" => Box::new(decoder::opus::create(header)?),
            other => bail!("unsupported codec: {other}"),
        };

        self.sample_format = dec.set_header(header)?;
        self.sample_encoding = dec.output_encoding();
        tracing::info!(codec = %header.codec, format = %self.sample_format, "Codec initialized");

        self.emit(ClientEvent::StreamStarted {
            codec: header.codec.clone(),
            format: self.sample_format,
        });

        // Reinitialize the shared stream (binary's player holds the same Arc)
        if let Some(ref stream) = self.stream {
            let mut s = stream.lock().unwrap_or_else(|e| e.into_inner());
            *s = Stream::with_encoding(self.sample_format, self.sample_encoding);
            if let Some(ref ss) = self.server_settings {
                let buf_ms = (ss.buffer_ms - ss.latency - self.settings.latency).max(0);
                s.set_buffer_ms(buf_ms as i64);
            }
        }

        self.decoder = Some(dec);
        Ok(())
    }

    async fn recv_timeout(&mut self, timeout: Duration) -> Result<TypedMessage> {
        tokio::time::timeout(timeout, self.connection.recv())
            .await
            .map_err(|_| anyhow::anyhow!("receive timed out"))?
    }

    fn cleanup(&mut self) {
        // Don't clear self.stream — it's shared with the binary's player
        self.decoder = None;
        self.connection.disconnect();
    }

    fn emit(&self, event: ClientEvent) {
        let _ = self.event_tx.try_send(event);
    }
}

fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_mac_address() -> String {
    mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.to_string().to_lowercase())
        .unwrap_or_else(|| "00:00:00:00:00:00".to_string())
}

fn samples_to_f32(data: &[u8], format: SampleFormat, encoding: SampleEncoding) -> Vec<f32> {
    match encoding {
        SampleEncoding::Float32 => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        SampleEncoding::PcmInt => match format.bits() {
            16 => data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
                .collect(),
            24 => data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| {
                    i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32
                        / snapcast_proto::PCM_24BIT_MAX
                })
                .collect(),
            32 => data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / i32::MAX as f32)
                .collect(),
            _ => Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::base::BaseMessage;
    use snapcast_proto::message::wire_chunk::WireChunk;
    use snapcast_proto::types::Timeval;

    /// Build a controller wired to in-memory channels (no network is touched until
    /// `connect()` is called, which these tests never do).
    fn make_controller() -> (
        Controller,
        mpsc::Receiver<ClientEvent>,
        mpsc::Receiver<crate::AudioFrame>,
        Arc<Mutex<Stream>>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (_cmd_tx, cmd_rx) = mpsc::channel(64);
        let (audio_tx, audio_rx) = mpsc::channel(64);
        let time_provider = Arc::new(Mutex::new(TimeProvider::new()));
        let stream = Arc::new(Mutex::new(Stream::new(SampleFormat::new(48000, 16, 2))));
        let cfg = crate::ClientConfig {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: "example.invalid".into(),
            ..Default::default()
        };
        let ctrl = Controller::new(
            cfg,
            event_tx,
            cmd_rx,
            audio_tx,
            time_provider,
            Arc::clone(&stream),
        )
        .unwrap();
        (ctrl, event_rx, audio_rx, stream)
    }

    fn base(msg_type: MessageType) -> BaseMessage {
        BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        }
    }

    fn drain_events(rx: &mut mpsc::Receiver<ClientEvent>) -> Vec<ClientEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    // ---- samples_to_f32 ----

    #[test]
    fn samples_to_f32_i16_normalizes_full_scale() {
        let f = SampleFormat::new(48000, 16, 2);
        let out = samples_to_f32(&i16::MAX.to_le_bytes(), f, SampleEncoding::PcmInt);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn samples_to_f32_float32_passthrough() {
        let f = SampleFormat::new(48000, 32, 2);
        let out = samples_to_f32(&0.5f32.to_le_bytes(), f, SampleEncoding::Float32);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn samples_to_f32_24bit_zero() {
        let f = SampleFormat::new(48000, 24, 2);
        let out = samples_to_f32(&0i32.to_le_bytes(), f, SampleEncoding::PcmInt);
        assert_eq!(out, vec![0.0]);
    }

    #[test]
    fn samples_to_f32_unsupported_bit_depth_is_empty() {
        let f = SampleFormat::new(48000, 8, 2);
        assert!(samples_to_f32(&[1, 2, 3, 4], f, SampleEncoding::PcmInt).is_empty());
    }

    // ---- handle_message branches ----

    #[test]
    fn handle_server_settings_emits_settings_and_volume() {
        let (mut ctrl, mut event_rx, _audio, _stream) = make_controller();
        let msg = TypedMessage {
            base: base(MessageType::ServerSettings),
            payload: MessagePayload::ServerSettings(ServerSettings {
                buffer_ms: 1000,
                latency: 0,
                volume: 60,
                muted: false,
            }),
        };
        ctrl.handle_message(msg).unwrap();
        let events = drain_events(&mut event_rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ClientEvent::ServerSettings { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ClientEvent::VolumeChanged { volume: 60, .. }))
        );
    }

    #[test]
    fn handle_time_message_updates_provider_without_panic() {
        let (mut ctrl, _e, _a, _s) = make_controller();
        let mut b = base(MessageType::Time);
        b.sent = Timeval { sec: 1, usec: 0 };
        b.received = Timeval { sec: 1, usec: 500 };
        let msg = TypedMessage {
            base: b,
            payload: MessagePayload::Time(Time {
                latency: Timeval { sec: 0, usec: 200 },
            }),
        };
        ctrl.handle_message(msg).unwrap();
    }

    #[test]
    fn handle_error_message_is_nonfatal() {
        let (mut ctrl, _e, _a, _s) = make_controller();
        let msg = TypedMessage {
            base: base(MessageType::Error),
            payload: MessagePayload::Error(snapcast_proto::message::error::Error {
                code: 500,
                error: "boom".into(),
                message: "server error".into(),
            }),
        };
        assert!(ctrl.handle_message(msg).is_ok());
    }

    #[test]
    fn handle_wirechunk_without_decoder_emits_no_audio() {
        let (mut ctrl, _e, mut audio_rx, _s) = make_controller();
        let msg = TypedMessage {
            base: base(MessageType::WireChunk),
            payload: MessagePayload::WireChunk(WireChunk {
                timestamp: Timeval { sec: 1, usec: 0 },
                payload: vec![0u8; 16],
            }),
        };
        ctrl.handle_message(msg).unwrap();
        assert!(audio_rx.try_recv().is_err(), "no decoder → no audio frame");
    }

    // ---- init_audio_pipeline ----

    #[test]
    fn init_audio_pipeline_rejects_unknown_codec() {
        let (mut ctrl, _e, _a, _s) = make_controller();
        let header = CodecHeader {
            codec: "totally-bogus".into(),
            payload: vec![],
        };
        assert!(ctrl.init_audio_pipeline(&header).is_err());
    }

    // ---- session ----

    #[tokio::test]
    async fn session_without_host_bails_before_connecting() {
        let (event_tx, _event_rx) = mpsc::channel(8);
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let (audio_tx, _audio_rx) = mpsc::channel(8);
        let time_provider = Arc::new(Mutex::new(TimeProvider::new()));
        let stream = Arc::new(Mutex::new(Stream::new(SampleFormat::new(48000, 16, 2))));
        let cfg = crate::ClientConfig {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: String::new(),
            ..Default::default()
        };
        let mut ctrl =
            Controller::new(cfg, event_tx, cmd_rx, audio_tx, time_provider, stream).unwrap();
        assert!(ctrl.session().await.is_err());
    }
}
