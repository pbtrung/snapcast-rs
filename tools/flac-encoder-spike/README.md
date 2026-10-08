# flac-encoder-spike

A standalone verification tool, not part of the snapcast-rs workspace (see
the `[workspace]` marker in `Cargo.toml` — it deliberately roots its own,
separate Cargo workspace so it's invisible to the parent build).

It answers one question: when fed PCM in the same 1152-frame blocks
snapserver-rs's FLAC encoder actually uses, with the header emitted once up
front (matching a live, never-ending stream), do candidate pure-Rust FLAC
encoders produce bitstreams that actually decode correctly?

Verifies two ways: the real C reference decoder (`flac -t`, needs
`brew install flac` / `apt install flac`) and `symphonia` (the same decoder
`snapcast-client` uses in production).

```bash
cd tools/flac-encoder-spike
cargo run --release
```

Full write-up, results, and the resulting decision:
[`docs/flac-encoder-evaluation.md`](../../docs/flac-encoder-evaluation.md).
