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

## Control API Authentication

Off by default: anyone who can reach ports 1705/1780 can control the server.
To require a login on the JSON-RPC control API (TCP 1705, WebSocket and HTTP
`POST` on `/jsonrpc`), enable it with a JWT signing secret and at least one
user (the server refuses to start otherwise):

```ini
[auth]
enabled = true
secret = <a long random string>
# Repeatable. The name ends at the first ':', so passwords may contain ':'.
user = alice:correct horse battery staple
user = bob:s3cret
```

or `--auth --auth-secret <secret> --auth-user alice:<password>` (repeatable
`--auth-user` replaces the config file's users; command-line passwords show in
the process list).

A TCP or WebSocket connection starts unauthenticated: only
`Server.Authenticate`, `Server.GetToken` and `Server.GetRPCVersion` are
answered, anything else gets the error `{"code": 401, "message": "Unauthorized"}`,
and no notifications are sent to it.

- `Server.Authenticate` with `{"scheme": "Basic", "param": base64("name:password")}`,
  `{"scheme": "Plain", "param": "name:password"}` or
  `{"scheme": "Bearer", "param": "<token>"}` (scheme case-insensitive; the
  older `{"token": "<token>"}` is a Bearer token) answers `"ok"` and
  authenticates the connection. Wrong credentials of any kind get the 401
  error above; an unknown scheme gets `-32602`.
- `Server.GetToken` with `{"username", "password"}` answers
  `{"token": "<JWT>"}`, valid for 24 hours, for use as a Bearer token. Wrong
  credentials get the 401 error.
- HTTP `POST /jsonrpc` needs an `Authorization: Bearer <token>` or
  `Authorization: Basic <base64(name:password)>` header on every request;
  without a valid one the answer is HTTP 401 with the 401 error as body.

With authentication disabled every connection counts as authenticated:
`Server.Authenticate` answers `"ok"` without checking, and `Server.GetToken`
still checks the configured users and fails with `-32603` when no secret is
set. Audio streaming clients (TCP 1704 and the `/stream` WebSocket) are not
covered by this login.

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

Pre-built Linux binaries are on the [Releases](https://github.com/pbtrung/snapcast-rs/releases) page, named `snapserver-rs-<target>` / `snapclient-rs-<target>` for `x86_64` and `aarch64`, each as `-unknown-linux-gnu` and `-unknown-linux-musl`. The server is built with Opus (libopus linked in statically). The gnu builds need glibc 2.34+, and the gnu client needs `alsa-lib`. The musl builds are fully static, but the static client can't load ALSA plugins such as PipeWire's, so use the gnu client on PipeWire/PulseAudio desktops. The binaries are built in an Arch Linux container with `docker-build/build.sh` (see [docker-build/README.md](docker-build/README.md)).

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

- Control API passwords are stored in plain text in the config file, and there is no TLS: credentials and tokens cross the network unencrypted. Audio streaming clients connect without a login.
- Server state (client names, groups, latency) is kept in memory only; it is not saved across restarts.
- `Stream.AddStream` is rejected (streams are fixed at startup), and `Stream.Control` is accepted but not acted on.
- No mDNS: the server doesn't advertise itself and the client needs a server URL.

## License

GPL-3.0-only — same as the original Snapcast.
