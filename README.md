# snapcast-rs

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

A Rust reimplementation of [Snapcast](https://github.com/snapcast/snapcast), the multiroom audio system by [Johannes Pohl (badaix)](https://github.com/badaix), which synchronizes playback across devices with sub-millisecond precision. The library crates implement the protocol, codecs and time sync without owning any audio device, port or file. The `snapserver-rs` and `snapclient-rs` binaries wrap them as standalone replacements for TCP-based Snapcast setups. They interoperate with the original C++ Snapcast.

## Key Features

- **Dynamic Audio Pipeline**: The client automatically re-initializes the audio device when the server changes sample rate or channels.
- **Integrated Resampling**: Automatic fallback to `rubato`-based resampling if the local hardware doesn't support the server's native format.
- **Bounded Protocol Reads**: Client and server reject oversized binary-protocol payloads before allocation.
- **Per-Stream Format Ownership**: Each server stream owns its codec/sample-format encoder state.
- **Lossless f32 Decode Path**: The FLAC decoder outputs native f32 samples — no intermediate 16-bit quantization.
- **Configurable Bind Addresses**: Listeners bind loopback, IPv4, IPv6, or specific interfaces.
- **Systemd Integration**: Native `sd-notify` support on Linux for service readiness and status reporting.

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

## Server Features

| Feature  | Default | C dep     | Description |
|----------|---------|-----------|-------------|
| `flac`   | ✅      | none      | FLAC encoding (pure Rust, flacenc) |
| `opus`   | —       | libopus   | Opus encoding |
| `mdns`   | ✅      | avahi     | mDNS service advertisement (binary only) |

## Codecs

| Codec  | Default | C dep | Precision | Latency |
|--------|---------|-------|-----------|---------|
| PCM    | ✅ always | none | 16/24/32-bit | zero |
| FLAC   | ✅ default | none | 16/24-bit (decoded to f32) | 24ms (block size) |
| Opus   | optional | libopus | 16-bit | 20ms |

FLAC supports up to 24-bit, 96 kHz and 8 channels; use PCM for anything beyond that.

Codec options go after the codec name, separated by `:`, as in C++ snapserver (`codec = ...` in the config file or `--codec`):

- `flac:<0-8>`: compression level
- `opus:BITRATE:<6000-512000>,COMPLEXITY:<0-10>`: bitrate in bits/s (default 192000) and encoder complexity

## Building

Requires Rust **1.94.1+**. Install the system libraries first.

On Arch Linux:

```bash
sudo pacman -S base-devel pkgconf alsa-lib avahi
# only for the optional Opus codec:
sudo pacman -S opus
```

On Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libasound2-dev libavahi-compat-libdnssd-dev
# only for the optional Opus codec:
sudo apt install libopus-dev
```

Then build from source:

```bash
git clone https://github.com/pbtrung/snapcast-rs.git
cd snapcast-rs
cargo build --release                              # default: flac + mdns
cargo build --release -p snapserver-rs --features opus  # + Opus
```

The binaries land in `target/release/snapserver-rs` and `target/release/snapclient-rs`.

Pre-built Linux binaries for `x86_64` and `aarch64` are on the [Releases](https://github.com/pbtrung/snapcast-rs/releases) page, named `snapserver-rs-<target>` / `snapclient-rs-<target>`. They need glibc 2.39+. They are built with Opus enabled. On Arch Linux, the server needs `avahi` and `opus` (`sudo pacman -S avahi opus`) and the client needs `alsa-lib`.

Run the checks with `make check` (fmt, clippy, tests).

## Usage

```bash
# Server
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
snapserver-rs --codec flac
snapserver-rs --codec "opus:BITRATE:256000,COMPLEXITY:10"  # needs the opus feature
snapserver-rs --stream-bind-address 127.0.0.1             # bind audio listener to loopback
snapserver-rs --help

# Client
snapclient-rs tcp://192.168.1.50:1704
snapclient-rs tcp://[::1]:1704
snapclient-rs                                            # mDNS auto-discovery
snapclient-rs --help

# Feed audio
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

## License

GPL-3.0-only — same as the original Snapcast.
