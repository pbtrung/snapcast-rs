//! Codec negotiation integration tests.
//!
//! Starts the server with an explicit codec and asserts that the client's
//! `StreamStarted` event reports the codec name the server was configured with,
//! plus a sane sample format. Exercises both default-available codecs: `flac`
//! and `pcm`. Opus is intentionally excluded because it is feature-
//! gated and may not be compiled in.

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::{ServerConfig, SnapServer};
use snapcast_tests::{TestClient, expect_event, spawn_serving};

/// Start a server on a random port with an explicit codec and the default
/// `48000:16:2` sample format. Returns the bound port.
///
/// `start_server()` from the harness always uses the default codec (flac), so
/// codec-negotiation tests need their own builder to override it.
async fn start_server_with_codec(codec: &str) -> u16 {
    let config = ServerConfig {
        codec: codec.into(),
        ..ServerConfig::default()
    };
    let (mut server, _events) = SnapServer::new(config);
    // The harness default stream is named "default"; keep parity.
    let _audio_tx = server.add_stream("default");
    spawn_serving(server).await
}

/// Connect a client to `port`. Mirrors the harness `connect_client`, duplicated
/// locally so this file does not depend on harness internals beyond the public
/// `TestClient` struct.
async fn connect(port: u16) -> TestClient {
    let config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        ..ClientConfig::default()
    };
    let (mut client, events, audio_rx) = SnapClient::new(config);
    let cmd = client.command_sender();
    tokio::spawn(async move {
        client.run().await.ok();
    });
    TestClient {
        events,
        audio_rx,
        cmd,
    }
}

/// Drive one negotiation round: start a server pinned to `codec`, connect a
/// client, and assert the `StreamStarted` event reports exactly that codec name
/// along with the configured `48000:16:2` sample format.
async fn assert_negotiates(codec: &str) {
    let port = start_server_with_codec(codec).await;
    let mut client = connect(port).await;

    let (negotiated_codec, rate, bits, channels) =
        expect_event(&mut client.events, 3000, |e| match e {
            ClientEvent::StreamStarted { codec, format } => {
                Some((codec, format.rate(), format.bits(), format.channels()))
            }
            _ => None,
        })
        .await;

    // Primary assertion: the negotiated codec string tracks the server config.
    assert_eq!(
        negotiated_codec, codec,
        "client reported codec {negotiated_codec:?} but server was configured with {codec:?}"
    );

    // The server was configured with the default "48000:16:2" sample format.
    // Rate and channel count are carried through unchanged by every codec.
    assert_eq!(rate, 48000, "unexpected sample rate for codec {codec}");
    assert_eq!(channels, 2, "unexpected channel count for codec {codec}");

    // Bit depth is the *decoded* output width the client reports, which is
    // codec-dependent: PCM passes the raw 16-bit format through, while FLAC's
    // decoder reports the f32 (32-bit) width it decodes into. Assert the exact
    // value expected for each codec rather than assuming they match.
    let expected_bits = match codec {
        c if c == snapcast_proto::CODEC_PCM => 16,
        c if c == snapcast_proto::CODEC_FLAC => 32,
        other => panic!("test only covers pcm/flac, got {other}"),
    };
    assert_eq!(
        bits, expected_bits,
        "unexpected decoded bit depth for codec {codec}"
    );
}

#[tokio::test]
async fn negotiates_flac() {
    assert_negotiates(snapcast_proto::CODEC_FLAC).await;
}

#[tokio::test]
async fn negotiates_pcm() {
    assert_negotiates(snapcast_proto::CODEC_PCM).await;
}

/// Iterate over both default-available codecs in a single test to prove the
/// negotiated string tracks the server configuration rather than a constant.
#[tokio::test]
async fn negotiates_each_default_codec() {
    for codec in [snapcast_proto::CODEC_FLAC, snapcast_proto::CODEC_PCM] {
        assert_negotiates(codec).await;
    }
}
