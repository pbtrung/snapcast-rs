use snapcast_client::ClientEvent;
use snapcast_server::ServerEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

#[tokio::test]
async fn server_sees_client_connect_and_disconnect() {
    let mut server = start_server().await;
    let client = connect_client(server.port).await;

    // Server should see ClientConnected
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    let client_id = loop {
        match tokio::time::timeout_at(deadline, server.events.recv()).await {
            Ok(Some(ServerEvent::ClientConnected { id, .. })) => break id,
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for ClientConnected"),
        }
    };
    assert!(!client_id.is_empty());

    // Stop client
    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();

    // Server should see ClientDisconnected (event-driven wait below)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, server.events.recv()).await {
            Ok(Some(ServerEvent::ClientDisconnected { id })) if id == client_id => break,
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for ClientDisconnected"),
        }
    }
}

#[tokio::test]
async fn audio_round_trip() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    // Wait for stream to start
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, .. } => {
            // Default codec depends on server features (flac or pcm)
            assert!(!codec.is_empty());
            Some(())
        }
        _ => None,
    })
    .await;

    // Wait for time sync
    expect_event(&mut client.events, 10_000, |e| match e {
        ClientEvent::TimeSyncComplete { .. } => Some(()),
        _ => None,
    })
    .await;

    // Push audio frames
    let samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
    let mut ts = 1_000_000_000; // Start at 1000s to avoid being near 0
    for _ in 0..20 {
        server
            .audio_tx
            .send(snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::F32(samples.clone()),
                timestamp_usec: ts,
            })
            .await
            .unwrap();
        ts += 10_000; // 10ms increment
    }

    // Verify client's stream buffer has data
    let mut total_samples = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);

    while total_samples < 9600 {
        let frame = tokio::time::timeout_at(deadline, client.audio_rx.recv())
            .await
            .expect("Timed out waiting for audio frame")
            .expect("Audio channel closed");

        total_samples += frame.samples.len();
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.channels, 2);
    }

    assert!(total_samples >= 9600);

    // The audio went through: server encoded → wire → client decoded
}
