# snapcast-rs

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

A Rust reimplementation of [Snapcast](https://github.com/snapcast/snapcast), the multiroom audio system by [Johannes Pohl (badaix)](https://github.com/badaix), which synchronizes playback across devices with sub-millisecond precision. The library crates implement the protocol, codecs and time sync without owning any audio device, port or file. The `snapserver-rs` and `snapclient-rs` binaries wrap them as standalone replacements for TCP-based Snapcast setups. They interoperate with the original C++ Snapcast.

## Key Features

- **Dynamic Audio Pipeline**: The client automatically re-initializes the audio device when the server changes sample rate or channels.
- **Integrated Resampling**: Optional `rubato`-based resampling (the client's `resampler` feature) when the local hardware doesn't support the server's native format.
- **Bounded Protocol Reads**: Client and server reject oversized binary-protocol payloads before allocation.
- **Per-Stream Format Ownership**: Each server stream owns its codec/sample-format encoder state.
- **Lossless f32 Decode Path**: The FLAC decoder outputs native f32 samples — no intermediate 16-bit quantization.
- **WebSocket Streaming**: Clients can stream over `ws://host:1780` (the server's `/stream` endpoint on its HTTP port) as well as plain TCP, one binary-protocol frame per WebSocket message, as in C++ Snapcast. Snapweb works against `snapserver-rs` for both control and in-browser playback (FLAC, PCM and Opus). No TLS (`wss://`) support.
- **Configurable Bind Addresses**: Listeners bind loopback, IPv4, IPv6, or specific interfaces.
- **Systemd Integration**: `snapclient-rs` reports readiness and status via `sd-notify` on Linux.

## Architecture

```text
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization
├── snapcast-client     Client library: embeddable, f32 audio output
├── snapcast-server     Server library: embeddable, f32 audio input
├── snapclient-rs       Client binary: cpal audio, software + hardware (ALSA) mixer
├── snapserver-rs       Server binary: stream readers, JSON-RPC, HTTP
└── snapcast-tests      Integration tests
```

Both libraries are pure audio engines — no device I/O, no HTTP, no config files.

## Cargo Features

Server (`snapserver-rs`; `flac` and `opus` also on the `snapcast-server` library):

| Feature  | Default | C dep     | Description |
|----------|---------|-----------|-------------|
| `flac`   | ✅      | none      | FLAC encoding (pure Rust, flacenc) |
| `opus`   | —       | none (bundled libopus, needs cmake) | Opus encoding |

Client (`snapclient-rs`; FLAC, PCM and Opus decoding are always built in, all pure Rust):

| Feature     | Default | C dep | Description |
|-------------|---------|-------|-------------|
| `websocket` | ✅      | none  | `ws://` streaming transport |
| `resampler` | —       | none  | Resample when the device can't play the stream format |

## Stream Sources

`--source` (repeatable) or `source = ...` under `[stream]` in `snapserver.conf` (default path `/etc/snapserver.conf`, set with `-c`). Each URI takes `?name=<id>` plus an optional `&sampleformat=<rate>:<bits>:<channels>`; all streams use the server-wide codec:

- `pipe:///path/to/fifo`: named pipe (the default source is `pipe:///tmp/snapfifo?name=default`)
- `file:///path/to/file.pcm`: raw PCM (or 44-byte-header WAV) file, played in real time and looped
- `process:///path/to/binary?params=...`: a child process's stdout
- `tcp://<bind-host>:<port>`: listen for TCP connections sending PCM (default port 4953)

Ports: 1704 (audio), 1705 (TCP JSON-RPC control), 1780 (HTTP/WebSocket JSON-RPC, `/stream` and Snapweb via `--doc-root`).

## Inactive Clients

The server drops dead connections and forgets long-gone clients. Both are set
in `snapserver.conf` (durations take `s`/`m`/`h`/`d`, bare numbers are
seconds, `0` disables):

```ini
[streaming_client]
# Close a session that sends nothing (clients sync time every second) or
# whose writes stall for this long. The client then shows as disconnected.
idle_timeout = 10s
# Delete clients disconnected for this long, as Server.DeleteClient does.
remove_disconnected_after = 2d
```

Library users set `ServerConfig::client_idle_timeout` and
`ServerConfig::remove_disconnected_clients_after`.

## Codecs

| Codec  | Default | C dep | Precision | Latency |
|--------|---------|-------|-----------|---------|
| PCM    | ✅ always | none | 16/24/32-bit | zero |
| FLAC   | ✅ default | none | 16/24-bit (decoded to f32) | 24ms (block size) |
| Opus   | optional | bundled libopus | 16-bit | 20ms |

FLAC supports up to 24-bit, 96 kHz and 8 channels; use PCM for anything beyond that.

Codec options go after the codec name, separated by `:`, as in C++ snapserver (`codec = ...` in the config file or `--codec`):

- `flac:<0-8>`: compression level
- `opus:BITRATE:<6000-512000>,COMPLEXITY:<0-10>`: bitrate in bits/s (default 192000) and encoder complexity

## Building

Requires Rust **1.94.1+**. Install the system libraries first (Arch Linux):

```bash
sudo pacman -S base-devel pkgconf alsa-lib
# only for the optional Opus codec (libopus is built from source):
sudo pacman -S cmake
```

Then build from source:

```bash
git clone https://github.com/pbtrung/snapcast-rs.git
cd snapcast-rs
cargo build --release                              # default: flac
cargo build --release -p snapserver-rs --features opus  # + Opus
cargo build --release -p snapclient-rs --features resampler  # + client resampling
```

The binaries land in `target/release/snapserver-rs` and `target/release/snapclient-rs`.

Pre-built Linux binaries for `x86_64` and `aarch64` are on the [Releases](https://github.com/pbtrung/snapcast-rs/releases) page, named `snapserver-rs-<target>` / `snapclient-rs-<target>`. They need glibc 2.39+. They are built with Opus enabled. On Arch Linux, the server needs `avahi` and `opus` (`sudo pacman -S avahi opus`) and the client needs `alsa-lib`.

Run the checks with `make check` (fmt, clippy, tests).

## Usage

```bash
# Server
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"   # creates the FIFO if missing (&mode=read to only open it)
snapserver-rs --codec flac
snapserver-rs --codec "opus:BITRATE:256000,COMPLEXITY:10"  # needs the opus feature
snapserver-rs --stream-bind-address 127.0.0.1             # bind audio listener to loopback
snapserver-rs --help

# Client
snapclient-rs tcp://192.168.1.50:1704
snapclient-rs tcp://[::1]:1704
snapclient-rs ws://192.168.1.50:1780                     # WebSocket (server HTTP port)
snapclient-rs --help

# Feed audio
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

## Known Limitations

- The control API's `--auth` / `[auth]` gate is not access control yet: `Server.GetToken` issues a token for any username without checking credentials.
- Server state (client names, groups, latency) is kept in memory only; it is not saved across restarts.
- `Stream.AddStream` is rejected (streams are fixed at startup), and `Stream.Control` is accepted but not acted on.
- No mDNS: the server doesn't advertise itself and the client needs a server URL.

## License

GPL-3.0-only — same as the original Snapcast.
