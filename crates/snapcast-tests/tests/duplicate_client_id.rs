//! A reconnect with an id that already has a live session replaces that
//! session: the old connection is closed, and its late cleanup must not
//! unregister or mark disconnected the client's new session.

use snapcast_proto::message::factory::MessagePayload;
use snapcast_server::ServerEvent;
use snapcast_tests::{
    RawClient, client_connected, constant_frame, expect_client_disconnected,
    expect_client_id_connected, expect_no_server_event, start_server,
};

const ID: &str = "dup-client";

#[tokio::test]
async fn reconnect_with_same_id_replaces_previous_session() {
    let mut server = start_server().await;

    let mut first = RawClient::connect(server.port, ID).await;
    expect_client_id_connected(&mut server.events, ID).await;

    let mut second = RawClient::connect(server.port, ID).await;
    expect_client_id_connected(&mut server.events, ID).await;

    assert!(
        first.closed_within(2000).await,
        "server closes the replaced session"
    );
    drop(first);

    // The replaced session's cleanup must neither report a disconnect nor
    // flip the shared client record.
    expect_no_server_event(
        &mut server.events,
        300,
        "ClientDisconnected for a live client",
        |e| matches!(e, ServerEvent::ClientDisconnected { .. }),
    )
    .await;
    assert_eq!(client_connected(&server.cmd, ID).await, Some(true));

    // The new session is still routed: it receives the stream's audio.
    let audio_tx = server.audio_tx.clone();
    tokio::spawn(async move {
        let mut ts = 1_000_000_000i64;
        while audio_tx.send(constant_frame(0.25, ts)).await.is_ok() {
            ts += 10_000;
        }
    });
    let mut got_chunk = false;
    while let Some(msg) = second.recv(3000).await {
        if matches!(msg.payload, MessagePayload::WireChunk(_)) {
            got_chunk = true;
            break;
        }
    }
    assert!(got_chunk, "new session receives audio");

    // Closing the current session is reported normally.
    drop(second);
    expect_client_disconnected(&mut server.events, ID).await;
    assert_eq!(client_connected(&server.cmd, ID).await, Some(false));
}
