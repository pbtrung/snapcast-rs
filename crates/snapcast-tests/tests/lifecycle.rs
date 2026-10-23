//! Stream and client lifecycle: removed streams, deleted clients and
//! stream activity status.

use snapcast_proto::MessageType;
use snapcast_proto::message::factory::MessagePayload;
use snapcast_proto::status::StreamStatus;
use snapcast_server::{ServerCommand, ServerEvent};
use snapcast_tests::*;

/// Regression: removing the first (default) stream left new clients in
/// groups on the removed stream, with no codec header and no audio.
#[tokio::test]
async fn new_client_after_default_stream_removal_gets_a_remaining_stream() {
    let server = start_two_stream_server().await;
    server
        .cmd
        .send(ServerCommand::RemoveStream {
            stream_id: "stream_a".into(),
        })
        .await
        .unwrap();

    let mut raw = RawClient::connect(server.port, "late").await;
    let mut codec_header = false;
    while let Some(msg) = raw.recv(2000).await {
        if let MessagePayload::CodecHeader(_) = msg.payload {
            codec_header = true;
            break;
        }
    }
    assert!(codec_header, "the client gets a codec header");
    let (_, stream) = client_group(&server.cmd, "late").await;
    assert_eq!(stream, "stream_b");
}

/// Regression: deleting a connected client removed it from the status while
/// its session kept streaming audio.
#[tokio::test]
async fn deleting_a_connected_client_ends_its_session() {
    let mut server = start_server().await;
    let mut raw = RawClient::connect(server.port, "doomed").await;
    expect_client_id_connected(&mut server.events, "doomed").await;

    server
        .cmd
        .send(ServerCommand::DeleteClient {
            client_id: "doomed".into(),
        })
        .await
        .unwrap();
    assert!(raw.closed_within(2000).await, "session closed");
    assert!(client_connected(&server.cmd, "doomed").await.is_none());
}

/// Regression: streams stayed `idle` in the status while playing.
#[tokio::test]
async fn stream_status_follows_audio() {
    let mut server = start_server().await;
    let stream_status = |status: StreamStatus| {
        move |e: ServerEvent| match e {
            ServerEvent::StreamStatus {
                stream_id,
                status: s,
            } if stream_id == "default" && StreamStatus::from(s.as_str()) == status => Some(()),
            _ => None,
        }
    };
    let mut ts = 1_000_000_000;
    for _ in 0..5 {
        server.audio_tx.send(tone_frame(ts)).await.unwrap();
        ts += 10_000;
    }
    expect_server_event(
        &mut server.events,
        2000,
        stream_status(StreamStatus::Playing),
    )
    .await;
    let status = server_status(&server.cmd).await;
    assert_eq!(status.server.streams[0].status, StreamStatus::Playing);

    // No more audio: idle after a few seconds.
    expect_server_event(&mut server.events, 6000, stream_status(StreamStatus::Idle)).await;
    let status = server_status(&server.cmd).await;
    assert_eq!(status.server.streams[0].status, StreamStatus::Idle);
}

/// A client that keeps sending but never reads is closed once a write to it
/// stalls for the idle timeout.
#[tokio::test]
async fn client_that_stops_reading_is_closed() {
    use tokio::io::AsyncWriteExt;
    let mut server = start_server_with(snapcast_server::ServerConfig {
        codec: "pcm".into(),
        client_idle_timeout: Some(std::time::Duration::from_millis(500)),
        ..Default::default()
    })
    .await;

    // A tiny receive window, so the server's writes stall quickly.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut stream = socket
        .connect(([127, 0, 0, 1], server.port).into())
        .await
        .unwrap();
    let send = |msg_type, payload| {
        let mut base = snapcast_proto::BaseMessage {
            msg_type,
            id: 1,
            refers_to: 0,
            sent: Default::default(),
            received: Default::default(),
            size: 0,
        };
        snapcast_proto::message::factory::serialize(&mut base, &payload).unwrap()
    };
    let hello = MessagePayload::Hello(snapcast_proto::message::hello::Hello {
        mac: "00:00:00:00:00:00".into(),
        host_name: "deaf".into(),
        version: "0".into(),
        client_name: "deaf".into(),
        os: "test".into(),
        arch: "test".into(),
        instance: 1,
        id: "deaf".into(),
        snap_stream_protocol_version: 2,
        auth: None,
    });
    stream
        .write_all(&send(MessageType::Hello, hello))
        .await
        .unwrap();
    expect_client_id_connected(&mut server.events, "deaf").await;

    // Audio far beyond what the socket buffers hold (1 s PCM frames).
    let audio = server.audio_tx.clone();
    tokio::spawn(async move {
        let mut ts = 1_000_000_000;
        loop {
            let frame = snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::Pcm(vec![1; 48000 * 4]),
                timestamp_usec: ts,
            };
            if audio.send(frame).await.is_err() {
                return;
            }
            ts += 1_000_000;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    // Keep sending time requests, never reading.
    let time = send(
        MessageType::Time,
        MessagePayload::Time(snapcast_proto::message::time::Time::new()),
    );
    let keep_sending = async {
        while stream.write_all(&time).await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        std::future::pending::<()>().await
    };
    tokio::select! {
        () = keep_sending => unreachable!(),
        () = expect_client_disconnected(&mut server.events, "deaf") => {}
    }
}
