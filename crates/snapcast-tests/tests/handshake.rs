use snapcast_client::ClientEvent;
use snapcast_server::ServerConfig;
use snapcast_tests::{connect_client, expect_event, start_server, start_server_with};

#[tokio::test]
async fn client_connects_and_receives_settings() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    let port = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::Connected { port, .. } => Some(port),
        _ => None,
    })
    .await;
    assert_eq!(port, server.port);

    // A new client: the server's buffer, full volume, unmuted, no latency.
    let settings = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings {
            buffer_ms,
            latency,
            volume,
            muted,
        } => Some((buffer_ms, latency, volume, muted)),
        _ => None,
    })
    .await;
    assert_eq!(
        settings,
        (snapcast_proto::DEFAULT_BUFFER_MS as i32, 0, 100, false)
    );
}

#[tokio::test]
async fn server_settings_carry_the_configured_buffer() {
    let server = start_server_with(ServerConfig {
        buffer_ms: 1234,
        ..ServerConfig::default()
    })
    .await;
    let mut client = connect_client(server.port).await;
    let buffer_ms = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings { buffer_ms, .. } => Some(buffer_ms),
        _ => None,
    })
    .await;
    assert_eq!(buffer_ms, 1234);
}

#[tokio::test]
async fn client_receives_codec_header() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    let (codec, format) = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => Some((codec, format)),
        _ => None,
    })
    .await;
    assert_eq!(codec, snapcast_proto::CODEC_FLAC);
    assert_eq!((format.rate(), format.channels()), (48000, 2));
}
