//! A reconnect with an id that already has a live session replaces that
//! session: the old connection is closed, and its late cleanup must not
//! unregister or mark disconnected the client's new session.

use snapcast_proto::message::factory::MessagePayload;
use snapcast_server::{AudioData, AudioFrame, ServerEvent};
use snapcast_tests::{RawClient, client_connected, expect_server_event, start_server};

const ID: &str = "dup-client";

#[tokio::test]
async fn reconnect_with_same_id_replaces_previous_session() {
    let mut server = start_server().await;

    let mut first = RawClient::connect(server.port, ID).await;
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientConnected { ref id, .. } if id == ID).then_some(())
    })
    .await;

    let mut second = RawClient::connect(server.port, ID).await;
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientConnected { ref id, .. } if id == ID).then_some(())
    })
    .await;

    assert!(
        first.closed_within(2000).await,
        "server closes the replaced session"
    );
    drop(first);

    // The replaced session's cleanup must neither report a disconnect nor
    // flip the shared client record.
    let spurious = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        loop {
            if let Some(ServerEvent::ClientDisconnected { .. }) = server.events.recv().await {
                return;
            }
        }
    })
    .await;
    assert!(spurious.is_err(), "no ClientDisconnected for a live client");
    assert_eq!(client_connected(&server.cmd, ID).await, Some(true));

    // The new session is still routed: it receives the stream's audio.
    let audio_tx = server.audio_tx.clone();
    tokio::spawn(async move {
        let mut ts = 1_000_000_000i64;
        while audio_tx
            .send(AudioFrame {
                data: AudioData::F32(vec![0.25; 960]),
                timestamp_usec: ts,
            })
            .await
            .is_ok()
        {
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
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientDisconnected { ref id } if id == ID).then_some(())
    })
    .await;
    assert_eq!(client_connected(&server.cmd, ID).await, Some(false));
}
