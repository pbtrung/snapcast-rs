//! End-to-end integration test against a real C++ snapserver.
//!
//! Requires `snapserver` in PATH.
//! Run with: cargo test --test integration -- --ignored

use std::process::{Child, Command};
use std::time::Duration;

use snapcast_client::connection::TcpConnection;
use snapcast_proto::MessageType;
use snapcast_proto::message::factory::MessagePayload;
use snapcast_proto::message::hello::Hello;
use snapcast_proto::message::time::Time;

const SNAPSERVER_PORT: u16 = 11704;

struct SnapserverGuard {
    child: Child,
}

impl SnapserverGuard {
    fn start() -> Option<Self> {
        let config = format!(
            "[stream]\n\
             port = {SNAPSERVER_PORT}\n\
             source = pipe:///tmp/snapcast_rs_test_fifo?name=test\n\
             [http]\n\
             enabled = false\n\
             [logging]\n\
             sink = null\n"
        );
        std::fs::write("/tmp/snapcast_rs_test.conf", config).ok()?;
        let _ = std::process::Command::new("mkfifo")
            .arg("/tmp/snapcast_rs_test_fifo")
            .output();

        let child = Command::new("snapserver")
            .args(["-c", "/tmp/snapcast_rs_test.conf"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;

        std::thread::sleep(Duration::from_secs(2));
        Some(Self { child })
    }
}

impl Drop for SnapserverGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file("/tmp/snapcast_rs_test_fifo");
        let _ = std::fs::remove_file("/tmp/snapcast_rs_test.conf");
    }
}

fn make_hello(id_suffix: &str) -> Hello {
    Hello {
        mac: "aa:bb:cc:dd:ee:ff".into(),
        host_name: "integration-test".into(),
        version: "0.1.0".into(),
        client_name: "snapclient-rs-test".into(),
        os: "test".into(),
        arch: "x86_64".into(),
        instance: 1,
        id: format!("aa:bb:cc:dd:ee:ff#{id_suffix}"),
        snap_stream_protocol_version: 2,
        auth: None,
    }
}

#[tokio::test]
#[ignore]
async fn hello_handshake() {
    let _server = SnapserverGuard::start().expect("snapserver not found");

    let mut conn = TcpConnection::new("127.0.0.1", SNAPSERVER_PORT);
    conn.connect().await.expect("connect failed");

    // Send Hello and read response directly (no concurrent recv loop needed)
    conn.send(
        MessageType::Hello,
        &MessagePayload::Hello(make_hello("hello")),
    )
    .await
    .expect("send failed");

    let response = tokio::time::timeout(Duration::from_secs(5), conn.recv())
        .await
        .expect("timeout")
        .expect("recv failed");

    match response.payload {
        MessagePayload::ServerSettings(ss) => {
            eprintln!(
                "ServerSettings: buffer={}ms, volume={}, muted={}",
                ss.buffer_ms, ss.volume, ss.muted
            );
            assert!(ss.buffer_ms > 0);
            assert!(ss.volume <= 100);
        }
        MessagePayload::Error(e) => panic!("Error: {} ({})", e.error, e.code),
        _ => panic!("Unexpected: {:?}", response.base.msg_type),
    }
}

#[tokio::test]
#[ignore]
async fn time_sync() {
    let _server = SnapserverGuard::start().expect("snapserver not found");

    let mut conn = TcpConnection::new("127.0.0.1", SNAPSERVER_PORT);
    conn.connect().await.expect("connect failed");

    // Hello first
    conn.send(
        MessageType::Hello,
        &MessagePayload::Hello(make_hello("time")),
    )
    .await
    .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.recv())
        .await
        .unwrap()
        .unwrap();

    // Time sync
    conn.send(MessageType::Time, &MessagePayload::Time(Time::new()))
        .await
        .unwrap();

    for _ in 0..10 {
        let response = tokio::time::timeout(Duration::from_secs(5), conn.recv())
            .await
            .expect("timeout")
            .expect("recv failed");

        if let MessagePayload::Time(t) = response.payload {
            let latency_usec = t.latency.sec as i64 * 1_000_000 + t.latency.usec as i64;
            eprintln!("Time sync latency: {}us", latency_usec);
            return;
        }
        eprintln!("Skipping {:?}", response.base.msg_type);
    }
    panic!("never received Time response");
}

#[tokio::test]
#[ignore]
async fn receives_codec_header() {
    let _server = SnapserverGuard::start().expect("snapserver not found");

    let mut conn = TcpConnection::new("127.0.0.1", SNAPSERVER_PORT);
    conn.connect().await.expect("connect failed");

    conn.send(
        MessageType::Hello,
        &MessagePayload::Hello(make_hello("codec")),
    )
    .await
    .unwrap();

    // Read ServerSettings, then CodecHeader
    let mut got_codec = false;
    for _ in 0..5 {
        let msg = tokio::time::timeout(Duration::from_secs(5), conn.recv())
            .await
            .expect("timeout")
            .expect("recv failed");

        match msg.payload {
            MessagePayload::CodecHeader(ch) => {
                eprintln!(
                    "CodecHeader: codec={}, payload={}B",
                    ch.codec,
                    ch.payload.len()
                );
                assert!(["pcm", "flac", "opus"].contains(&ch.codec.as_str()));
                assert!(!ch.payload.is_empty());
                got_codec = true;
                break;
            }
            MessagePayload::ServerSettings(ss) => {
                eprintln!("ServerSettings: buffer={}ms (continuing...)", ss.buffer_ms);
            }
            _ => eprintln!("Got {:?} (continuing...)", msg.base.msg_type),
        }
    }
    assert!(got_codec, "never received CodecHeader");
}
