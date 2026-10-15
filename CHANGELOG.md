# Changelog

Releases of this fork (0.999.x, from 0.999.1 on) are listed with their
changes on [GitHub Releases](https://github.com/pbtrung/snapcast-rs/releases).
The entries below are upstream
[metaneutrons/snapcast-rs](https://github.com/metaneutrons/snapcast-rs)
releases up to 0.18.0, which this fork is based on.

## [0.18.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.17.2...v0.18.0) (2026-09-12)


### ⚠ BREAKING CHANGES

* raise MSRV to Rust 1.94.1

### Build System

* raise MSRV to Rust 1.94.1 ([d5df235](https://github.com/metaneutrons/snapcast-rs/commit/d5df235758fc0825a8fd1a9065ccd6428fb36f08))

### Code Refactoring

* modernize fixed-size audio buffer handling and sender flushing without changing codec behavior ([ee009cc](https://github.com/metaneutrons/snapcast-rs/commit/ee009cc983a551452d9b125b7f9869954eda1a5c))

## [0.17.2](https://github.com/metaneutrons/snapcast-rs/compare/v0.17.1...v0.17.2) (2026-07-02)


### Bug Fixes

* harden audio-path input handling ([#63](https://github.com/metaneutrons/snapcast-rs/issues/63)) ([3e61f21](https://github.com/metaneutrons/snapcast-rs/commit/3e61f211fbf19ccb249d968268729af9c01fc1ae))

## [0.17.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.17.0...v0.17.1) (2026-07-01)


### Bug Fixes

* **ci:** cross-compile release binaries correctly and stop clobbering assets ([#47](https://github.com/metaneutrons/snapcast-rs/issues/47)) ([ee18721](https://github.com/metaneutrons/snapcast-rs/commit/ee18721800487e3fa00064c4f11dc838af1d4663))


### Code Refactoring

* **server:** pure-Rust FLAC encoder via flacenc (drop libflac-sys) ([9c5836c](https://github.com/metaneutrons/snapcast-rs/commit/9c5836cf6cac4b0f2177b7b42df5ba7899eb5788))

## [0.17.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.16.1...v0.17.0) (2026-06-28)


### ⚠ BREAKING CHANGES

* **server:** SnapServer::run(&mut self) is replaced by serve(&mut self, listener: tokio::net::TcpListener) — the library no longer binds a port. ServerConfig drops stream_bind_address/stream_port; the embedder binds the listener and hands it to serve().
* **server:** the snapcast-server library no longer reads or writes files. ServerConfig.state_file (Option<PathBuf>) is replaced by ServerConfig.initial_state (Option<ServerState>); ServerState::load/save are removed. Embedders load the initial snapshot themselves and persist the new ServerEvent::StateChanged(ServerState) events.
* **proto:** ProtoError is now #[non_exhaustive], so downstream match expressions must include a wildcard arm. This lets future variants be added without a breaking change (the enum already gained Json/PayloadTooLarge and lost UnknownMessageType this cycle). Landing it now, in the same breaking release, prevents a churn of one-off breaks later.

### Bug Fixes

* **proto:** bound length-prefixed reads against DEFAULT_MAX_PAYLOAD_SIZE ([#25](https://github.com/metaneutrons/snapcast-rs/issues/25)) ([0e30962](https://github.com/metaneutrons/snapcast-rs/commit/0e30962094d57f52918a353f8779ca15ec5e9946))
* **proto:** consolidate the monotonic clock into snapcast-proto (SSOT) ([#26](https://github.com/metaneutrons/snapcast-rs/issues/26)) ([f6af7e2](https://github.com/metaneutrons/snapcast-rs/commit/f6af7e2c0595206b7bc2f8a244e8188b63d4befe))
* **snapclient:** reuse the PCM buffer in the cpal callback (no RT allocation) ([#28](https://github.com/metaneutrons/snapcast-rs/issues/28)) ([fc2c2b6](https://github.com/metaneutrons/snapcast-rs/commit/fc2c2b66764a15ff242ee27eb0a86dadd7db3292))
* **snapserver:** harden control-API auth and close the WebSocket bypass ([#27](https://github.com/metaneutrons/snapcast-rs/issues/27)) ([b3ce4e8](https://github.com/metaneutrons/snapcast-rs/commit/b3ce4e870c7083218e29af45fc02acd53ab829f7))


### Code Refactoring

* **proto:** mark ProtoError #[non_exhaustive] ([#38](https://github.com/metaneutrons/snapcast-rs/issues/38)) ([952a5b7](https://github.com/metaneutrons/snapcast-rs/commit/952a5b70f0a1662c301c849d30e23efcfd6ada34))
* **server:** inject the TCP listener instead of binding in the library ([#40](https://github.com/metaneutrons/snapcast-rs/issues/40)) ([d766a37](https://github.com/metaneutrons/snapcast-rs/commit/d766a37e2d6ad37c3a3169ae0bb0ef1a1bea0451))
* **server:** remove file persistence from the library; emit StateChanged ([#39](https://github.com/metaneutrons/snapcast-rs/issues/39)) ([9d5493f](https://github.com/metaneutrons/snapcast-rs/commit/9d5493f73d0a702f7ec76a8ed75c2b4c9f377cab))

## [0.16.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.16.0...v0.16.1) (2026-05-31)


### Bug Fixes

* **client:** send ClientInfo to server on volume/mute changes ([2bd0d12](https://github.com/metaneutrons/snapcast-rs/commit/2bd0d12b910dc74e89d0763845f2a0aa98aba4dc))

## [0.16.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.15.0...v0.16.0) (2026-05-23)


### ⚠ BREAKING CHANGES

* **client:** remove mdns from snapcast-client library

### Code Refactoring

* **client:** remove mdns from snapcast-client library ([4c58619](https://github.com/metaneutrons/snapcast-rs/commit/4c586191763f98aafb8049a4c3ae804abd9bb4f9))

## [0.15.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.14.0...v0.15.0) (2026-05-23)


### ⚠ BREAKING CHANGES

* **mdns:** remove mdns module from library entirely
* **mdns:** add TXT record support, document breaking change

### Features

* **mdns:** add TXT record support, document breaking change ([0f0fa65](https://github.com/metaneutrons/snapcast-rs/commit/0f0fa65368b19e8f0772398986f4707750a35661))


### Code Refactoring

* **mdns:** remove mdns module from library entirely ([1ca09fe](https://github.com/metaneutrons/snapcast-rs/commit/1ca09fe330cac7619d59087fcf97a9f85d4b5015))

## [0.14.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.13.0...v0.14.0) (2026-05-23)


### ⚠ BREAKING CHANGES

* ClientConnected carries Hello struct instead of flat fields
* **server:** add_stream() API — multi-stream, library owns encoding

### Features

* --no-mdns and --mdns-name CLI flags for snapserver-rs ([30f7c35](https://github.com/metaneutrons/snapcast-rs/commit/30f7c3527d17429556329545381405fa76fa6a50))
* add mdns_enabled config for runtime mDNS control ([0dfbea9](https://github.com/metaneutrons/snapcast-rs/commit/0dfbea9553a23c307da7c9c97001c7be9554b567))
* add structured logging to f32lz4 encoder/decoder ([c7ab264](https://github.com/metaneutrons/snapcast-rs/commit/c7ab26436d4d5b2ac229b27a162261e422ac377c))
* **binary:** platform-aware default audio backend ([4e704a6](https://github.com/metaneutrons/snapcast-rs/commit/4e704a629f3fb0c7dbd07c876ce4b244f515eafb))
* **binary:** wire JSON-RPC control + HTTP servers in snapserver-rs ([f1b130c](https://github.com/metaneutrons/snapcast-rs/commit/f1b130cc3e34a6fffc798e431c8218e2e254ee57))
* **client-bin:** IPv6 bracket notation and reject WS audio scheme ([e20f0ea](https://github.com/metaneutrons/snapcast-rs/commit/e20f0ea994ea63e74edaa98ed4eab61b6b595b55))
* **client:** 100% functional equivalency with C++ snapclient ([187f226](https://github.com/metaneutrons/snapcast-rs/commit/187f226cbe89657c6f0dc790707207d85d47d9eb))
* **client:** add --daemon and --user CLI options (Unix) ([466893c](https://github.com/metaneutrons/snapcast-rs/commit/466893c68783dfe649626fe6e9160e73dd726a59))
* **client:** audio pump — library outputs f32 via audio_rx channel ([0678848](https://github.com/metaneutrons/snapcast-rs/commit/067884873c7425dd1ba1fec52e1954493cd99f30))
* ClientConnected carries Hello struct instead of flat fields ([cc4fe01](https://github.com/metaneutrons/snapcast-rs/commit/cc4fe01db2c8f4e216c2d49748571cc95e4afd8c))
* **client:** cpal cross-platform audio backend — Windows support ([3c931c0](https://github.com/metaneutrons/snapcast-rs/commit/3c931c09a88395d075a94c91dd6e8afa49ebf65e))
* **client:** enable audio frame reporting and update integration tests ([44aa5a2](https://github.com/metaneutrons/snapcast-rs/commit/44aa5a26cee499890d62e3afdec8c020f052c446))
* **client:** f32 decode path for FLAC and Vorbis, SampleEncoding enum ([6bdf229](https://github.com/metaneutrons/snapcast-rs/commit/6bdf229afe880454820a1ab7bcd98ab2d3fcabe6))
* **client:** graceful shutdown on Ctrl-C ([5b5bee5](https://github.com/metaneutrons/snapcast-rs/commit/5b5bee5794323fb114fd0f740a69fbbe2afc1a66))
* **client:** implement ALSA and PulseAudio player backends ([940687f](https://github.com/metaneutrons/snapcast-rs/commit/940687f74465643d58800b197364629412a24caf))
* **client:** implement ClientSettings and CLI argument parsing ([78ad4c3](https://github.com/metaneutrons/snapcast-rs/commit/78ad4c3b881c86283a8faf50fcc2b1ccc9f9e1d4))
* **client:** implement Controller (main orchestrator) ([9167bd0](https://github.com/metaneutrons/snapcast-rs/commit/9167bd0dcb86910e0cdde86818aec0265fa65e1e))
* **client:** implement CoreAudio player backend ([d331cb3](https://github.com/metaneutrons/snapcast-rs/commit/d331cb34ab4ad1ec4c6a4c3d62c17d12f9065ee0))
* **client:** implement Decoder trait and PCM decoder ([d00bb20](https://github.com/metaneutrons/snapcast-rs/commit/d00bb20df7d9ffc49c037defb29e83d4d30125f3))
* **client:** implement DoubleBuffer and TimeProvider ([b4a4836](https://github.com/metaneutrons/snapcast-rs/commit/b4a48367c1515e4de804de57702c732f1d2c3128))
* **client:** implement FLAC decoder with symphonia ([b5c8440](https://github.com/metaneutrons/snapcast-rs/commit/b5c8440c6084f72463d6c265e038433d96a0c61e))
* **client:** implement mDNS discovery, WebSocket, and WSS connections ([09a598b](https://github.com/metaneutrons/snapcast-rs/commit/09a598b50fb76d7b14b2ce266a6598e38ab63459))
* **client:** implement Player trait and software volume ([667785d](https://github.com/metaneutrons/snapcast-rs/commit/667785d3c44ad3a91fe4714e9e6093999447eece))
* **client:** implement proper logging with --logsink and --logfilter ([7c4ece3](https://github.com/metaneutrons/snapcast-rs/commit/7c4ece3e022d5c081c7259d949b0a709a071e1c3))
* **client:** implement resampler and README ([7925d90](https://github.com/metaneutrons/snapcast-rs/commit/7925d90e7ae4fddb6abef2819f8870f665be0a39))
* **client:** implement Stream (time-synchronized PCM buffer) ([5cb3c21](https://github.com/metaneutrons/snapcast-rs/commit/5cb3c213bf01d4128a19768cb2017f0ea3b0b392))
* **client:** implement TCP connection with frame I/O ([d2645f2](https://github.com/metaneutrons/snapcast-rs/commit/d2645f27544183c6ddae3de0fff058a3520a5fd4))
* **client:** implement Vorbis and Opus decoders ([8d2f33c](https://github.com/metaneutrons/snapcast-rs/commit/8d2f33ce504dbe79b0a4bf5889ea255602af5640))
* **client:** implement Vorbis decoder with symphonia ([9bd3378](https://github.com/metaneutrons/snapcast-rs/commit/9bd33785a196e97d859cd2230b77188c085af22a))
* **client:** replace audiopus (C FFI) with opus-decoder (pure Rust) ([8852e40](https://github.com/metaneutrons/snapcast-rs/commit/8852e40dc097a9733a05cf9d26450789a9db8b91))
* **client:** resampler outputs f32 directly, no i16 quantization ([3f91ea4](https://github.com/metaneutrons/snapcast-rs/commit/3f91ea4bef2fa5f3114a6a68d71ef8522b327f7a))
* custom binary protocol messages (feature-gated: custom-protocol) ([c8ea9a5](https://github.com/metaneutrons/snapcast-rs/commit/c8ea9a507b4c3b8a23d80efc68d3b1e3714e00ad))
* default codecs are now PCM + f32lz4 (pure Rust, zero C deps) ([264e12a](https://github.com/metaneutrons/snapcast-rs/commit/264e12a88cd2669c635eaf2251e84a6599197949))
* eliminate C dependencies with pure Rust FLAC + feature-gated codecs ([f2bf4bd](https://github.com/metaneutrons/snapcast-rs/commit/f2bf4bd9272ca4d8830c0d3462c629418268ac22))
* **encoder:** add PCM→f32 conversion warning to vorbis encoder ([07bf6e1](https://github.com/metaneutrons/snapcast-rs/commit/07bf6e1d0ccdf7de7674144b7a7d236c3ea597aa))
* **encoder:** one-time warnings on input/codec format mismatch ([f42c3c4](https://github.com/metaneutrons/snapcast-rs/commit/f42c3c4cc89d739414c1f636cac77d1fdb0666a6))
* encryption support in binaries with default PSK ([272585c](https://github.com/metaneutrons/snapcast-rs/commit/272585c3e9e78a92d1ec5685b647e70d3c0ebd5d))
* enterprise encryption wiring — EncoderConfig, key injection, integration tests ([36a9f1f](https://github.com/metaneutrons/snapcast-rs/commit/36a9f1fc55a5ca66048d8a33f79eb155298ba05c))
* f32lz4 codec — lossless compressed f32 transmission (feature-gated) ([1863cb7](https://github.com/metaneutrons/snapcast-rs/commit/1863cb78902d55654e5a616049dfe558f1bb94a7))
* feature-gate mdns on both server and client (default on) ([151a16f](https://github.com/metaneutrons/snapcast-rs/commit/151a16f708c7140a31c4a19598082632b2407cf5))
* FLAC as default codec, f32lz4 + encryption optional ([902d6e1](https://github.com/metaneutrons/snapcast-rs/commit/902d6e19f2a1e31ba25de06a87cd37d031de3ba1))
* full C++ API parity — AddStream, RemoveStream, StreamControl, notifications ([f780a91](https://github.com/metaneutrons/snapcast-rs/commit/f780a9101b456736afa67e70359b703358685da7))
* **mdns:** add mdns-os backend via astro-dnssd (default) ([d24eb23](https://github.com/metaneutrons/snapcast-rs/commit/d24eb23f6cb429c136d21c02ac9f654bad63beb8))
* optional ChaCha20-Poly1305 encryption for f32lz4 (--features encryption) ([1ce5fcf](https://github.com/metaneutrons/snapcast-rs/commit/1ce5fcf967c2ba48467b29c4d743a3dd661abffb))
* production-quality structured logging across all crates ([fb2c72c](https://github.com/metaneutrons/snapcast-rs/commit/fb2c72c00d0bba3218ba4721cec3ccb88020b442))
* **proto:** extract Timeval to types module, add SampleFormat ([fc9c835](https://github.com/metaneutrons/snapcast-rs/commit/fc9c835c60f84a6d5d56bb1057a1db475947451b))
* **proto:** implement all message types ([15979d2](https://github.com/metaneutrons/snapcast-rs/commit/15979d2b4252500d6b8b206763297bfcaa8b4878))
* **proto:** implement message factory and full frame serialization ([92ce52e](https://github.com/metaneutrons/snapcast-rs/commit/92ce52ef888ed4d11f89fd64edb2eed312ec5c0d))
* **proto:** wire custom binary protocol messages end-to-end ([8947cbe](https://github.com/metaneutrons/snapcast-rs/commit/8947cbe439a63ee4199d1bde9c92550f999086dc))
* **server-bin:** configurable bind addresses, IPv6 stream URIs ([3d3666b](https://github.com/metaneutrons/snapcast-rs/commit/3d3666bf80b2d9f0e823a03ac091a32e2dc4bc23))
* **server:** 100% pure Rust by default — no C libraries needed ([1ed1823](https://github.com/metaneutrons/snapcast-rs/commit/1ed18236aa90717660cebebcbe3736a0f32ad9c3))
* **server:** add ClientFilter trait for connection-level filtering ([6cd60fc](https://github.com/metaneutrons/snapcast-rs/commit/6cd60fca9db9df762621cdc998f0c25e4b626f86))
* **server:** add F32AudioSender for embedded audio sources ([958d380](https://github.com/metaneutrons/snapcast-rs/commit/958d380f2be6d407436de3eb6625f0b277094dc4))
* **server:** add state persistence and SetStreamMeta command ([0fad761](https://github.com/metaneutrons/snapcast-rs/commit/0fad761bca1ea40985be2eae9ed6ac747cd94b87))
* **server:** add_stream() API — multi-stream, library owns encoding ([8a51bd8](https://github.com/metaneutrons/snapcast-rs/commit/8a51bd8ec6d085f6cdec01814e8a9b754d74592e))
* **server:** audio_tx input channel — embedding apps can push f32 audio ([9c74568](https://github.com/metaneutrons/snapcast-rs/commit/9c74568d516a62e9dbaa3904aa55c7ed31b13b98))
* **server:** auth middleware on HTTP and TCP control ([6de075b](https://github.com/metaneutrons/snapcast-rs/commit/6de075bc15c6e29af3bf44db146b6c0a3f681660))
* **server:** auto-register clients in state and create groups on connect ([086022f](https://github.com/metaneutrons/snapcast-rs/commit/086022f3f58ab08db90756a21f4850710da07b1e))
* **server:** client session management + wired SnapServer::run() ([b270d2e](https://github.com/metaneutrons/snapcast-rs/commit/b270d2ec6fc5268082ee4219befec3ae7a539d85))
* **server:** config file + HTTP/WebSocket/Snapweb — all deferred items done ([bc3d1ea](https://github.com/metaneutrons/snapcast-rs/commit/bc3d1ea1a9006ecb4bdd6dc6cb6431f1d85d89d3))
* **server:** default codec follows compiled features ([431aa13](https://github.com/metaneutrons/snapcast-rs/commit/431aa138bf8c86fddb943b28ead376239cb5ce1b))
* **server:** emit GroupNameChanged, GroupClientsChanged, ServerUpdated events ([3e86590](https://github.com/metaneutrons/snapcast-rs/commit/3e86590b0a1328f2b8504533aa8df980dda0890f))
* **server:** encoders — PCM, FLAC, Opus, Vorbis ([7af68d2](https://github.com/metaneutrons/snapcast-rs/commit/7af68d243d07330c58e2966de9d75e76c83d9d96))
* **server:** group/stream/client state model with JSON persistence ([6bd32ef](https://github.com/metaneutrons/snapcast-rs/commit/6bd32ef289b23360bfdfda0187b2a68f67be3826))
* **server:** include MAC address in ClientConnected event ([737f1b2](https://github.com/metaneutrons/snapcast-rs/commit/737f1b2f2a8b48d4cbb4e6ff035542d7318a9727))
* **server:** JSON-RPC control API — all method handlers ([10a6f9a](https://github.com/metaneutrons/snapcast-rs/commit/10a6f9a3d48d04968e2441229e29ff85c2d6851e))
* **server:** JSON-RPC control transport + extension point wired ([db1a0e4](https://github.com/metaneutrons/snapcast-rs/commit/db1a0e4dfe56b2aab4bb364240fd81902a3039eb))
* **server:** JSON-RPC method/notification registry with proper response flow ([063c916](https://github.com/metaneutrons/snapcast-rs/commit/063c91651bb5040f093d454ae48f0a32fa7a3267))
* **server:** librespot + airplay stream reader specializations ([304d665](https://github.com/metaneutrons/snapcast-rs/commit/304d665aaaa2292e798db2cea3d7ccbb26fdf6a8))
* **server:** mDNS service advertisement + auth/config deferred ([1def82a](https://github.com/metaneutrons/snapcast-rs/commit/1def82aced99e703f0acfa2499fe8e2aa811a343))
* **server:** per-stream codec/format config via add_stream_with_config() ([0d57ca2](https://github.com/metaneutrons/snapcast-rs/commit/0d57ca25fae03aec1f9f4fac01e59c87aecca04d))
* **server:** re-export DEFAULT_ENCRYPTION_PSK for embedders ([edfb9f8](https://github.com/metaneutrons/snapcast-rs/commit/edfb9f86b4075807daec426d4f3e96a9d6ad5a13))
* **server:** realtime pacing for F32 embedded sources ([b6caee3](https://github.com/metaneutrons/snapcast-rs/commit/b6caee365cde44df8328c38e5b4ab8405cdec250))
* **server:** redesign AudioFrame with AudioData enum (F32/Pcm) ([21fcc97](https://github.com/metaneutrons/snapcast-rs/commit/21fcc97fb7da94224a7057b17dcd75db75ae69d9))
* **server:** replace all stubs with real implementations ([3717946](https://github.com/metaneutrons/snapcast-rs/commit/37179463c6cd95a84eb40121e843769fa98ea511))
* **server:** scaffold snapcast-server library + snapserver-rs binary ([2117537](https://github.com/metaneutrons/snapcast-rs/commit/2117537f936e612b4f0e92438cb36d9e1ca09baa))
* **server:** stream manager — routes PCM through encoders to clients ([a5eba1f](https://github.com/metaneutrons/snapcast-rs/commit/a5eba1fbba96b0b94b139f525b919b5ebaed4a3b))
* **server:** stream readers — pipe, file, process, TCP ([773986a](https://github.com/metaneutrons/snapcast-rs/commit/773986aceae3318038989cc9858853ffba00d095))
* **server:** streaming client auth — AuthValidator trait + StaticAuthValidator ([4e2a52c](https://github.com/metaneutrons/snapcast-rs/commit/4e2a52c85599045c65c6182e7b093b51d3167b0c))
* **server:** typed ServerCommand API — no JSON-RPC detour for embedding ([fd17723](https://github.com/metaneutrons/snapcast-rs/commit/fd1772346511a2539fb4830ca12e144893458fd1))
* **server:** typed ServerEvent notifications for all state changes ([2f74ea5](https://github.com/metaneutrons/snapcast-rs/commit/2f74ea5917b50cf6dcdad403f2fe9145a7d540aa))
* **server:** typed ServerStatus API — no more JSON in library boundary ([53decec](https://github.com/metaneutrons/snapcast-rs/commit/53decec0468285ff757e0be56a9707e9796b9c22))
* **server:** warn once on input/codec format mismatch ([1e53bf7](https://github.com/metaneutrons/snapcast-rs/commit/1e53bf7846eb9639df55f1dd8ebb3b9b04f413e0))
* **session:** per-stream chunk routing + mute filtering ([40b82e2](https://github.com/metaneutrons/snapcast-rs/commit/40b82e2af4e83c82f8cb29a3a90067fb12a722fb))
* **snapclient-rs:** implement volume mixer (software + hardware ALSA) ([9d7eeaf](https://github.com/metaneutrons/snapcast-rs/commit/9d7eeaf773ff6f5fd26078695921fc9d9fc03849))
* **snapclient:** add systemd notification support and status reporting ([77e69df](https://github.com/metaneutrons/snapcast-rs/commit/77e69df39199e546752dbe54447f19eac94504f4))
* **snapclient:** implement dynamic format handling and resampler integration ([6d354b7](https://github.com/metaneutrons/snapcast-rs/commit/6d354b72fdc1e9051c2862b10032e5de10948133))
* **status:** full C++ wire-format compatible types ([981bfcf](https://github.com/metaneutrons/snapcast-rs/commit/981bfcf0fa0ba9a51e072affb71af21c074f6edb))
* **status:** typed StreamStatus enum with From&lt;&str&gt; conversion ([cee98fe](https://github.com/metaneutrons/snapcast-rs/commit/cee98fefb1a2c7f77aff5c18c5dcce58036df7a7))


### Bug Fixes

* add readme path to all published crates for crates.io ([e889ac2](https://github.com/metaneutrons/snapcast-rs/commit/e889ac2e8e74aac50a33ce0b370dc263dac5af88))
* add version to workspace internal deps (required for crates.io publish) ([db98108](https://github.com/metaneutrons/snapcast-rs/commit/db981085ac436e19200bac07f99d62129bb992ad))
* align workspace dep versions with 0.5.0 release ([1452d9c](https://github.com/metaneutrons/snapcast-rs/commit/1452d9c3aef1a3c164b3f65666448d4c4ad9922d))
* **alsa:** use CStr for HintIter (alsa crate 0.9 API) ([3f41265](https://github.com/metaneutrons/snapcast-rs/commit/3f4126559b2ef69377eee106c3fc6ed06baf99aa))
* **audio:** cpal callback pulls from Stream directly — no ring buffer ([fc2aefb](https://github.com/metaneutrons/snapcast-rs/commit/fc2aefb489ce1095b95ff771d28e9d87a5eeaee6))
* **binary:** forward codec features (flac, opus, vorbis, f32lz4) to server library ([2344723](https://github.com/metaneutrons/snapcast-rs/commit/2344723822e9a949cdc9268836d97896b1d0ad91))
* **binary:** forward f32lz4 feature to client library, enable by default ([943fe27](https://github.com/metaneutrons/snapcast-rs/commit/943fe270e67731055d06b41e8a764f02bb4b339c))
* **binary:** forward feature flags from snapclient-rs to snapcast-client ([36c6fd0](https://github.com/metaneutrons/snapcast-rs/commit/36c6fd051e33abdb845b499e2a6a95363bf223a0))
* **binary:** remove duplicate disconnect log — controller already logs it ([b4a885c](https://github.com/metaneutrons/snapcast-rs/commit/b4a885c0e19dd7f53648517e1defada2994796a4))
* **binary:** restore native CoreAudio + ALSA backends from v0.1.0 ([37d0345](https://github.com/metaneutrons/snapcast-rs/commit/37d034581e95fffbefc5dbddb632f7fea738923b))
* **binary:** spawn Ctrl-C handler before setup — always exits cleanly ([30cfb1d](https://github.com/metaneutrons/snapcast-rs/commit/30cfb1ddc94a03b6471dbc2ac7f60b1e5c22049c))
* **ci:** aarch64 cross-compile needs arm64 multiarch deps ([f7cba80](https://github.com/metaneutrons/snapcast-rs/commit/f7cba80fc2964f0015253ac68a61e90345ace1a8))
* **ci:** configure release-please for workspace — manifest mode ([8b70fad](https://github.com/metaneutrons/snapcast-rs/commit/8b70fad836e748cb06ac419e82284b6ca4236c5b))
* **ci:** correct ARM runner name to ubuntu-24.04-arm ([e731b23](https://github.com/metaneutrons/snapcast-rs/commit/e731b23675f6a67453bf375b18332217f2f0eae2))
* **ci:** correct release-please manifest format ([55a8459](https://github.com/metaneutrons/snapcast-rs/commit/55a8459c8a01d8c601e1bd57ac7e88de042399c0))
* **ci:** install libasound2-dev for Linux builds (cpal needs ALSA) ([67367e3](https://github.com/metaneutrons/snapcast-rs/commit/67367e3e3e2645cf93553923d4d088e4fd809d3e))
* **ci:** native ARM runner for aarch64-linux — no cross-compile hacks ([bcf24ed](https://github.com/metaneutrons/snapcast-rs/commit/bcf24eda2034f7db8541d77c1b36eafca7dcbfab))
* **ci:** release workflow needs libasound2-dev for Linux builds ([df26332](https://github.com/metaneutrons/snapcast-rs/commit/df263323be5d5218e8b0f9a1e60b0ca583ab7ba8))
* **ci:** release workflow triggers on snapcast-rs-v* tags ([5c5ef63](https://github.com/metaneutrons/snapcast-rs/commit/5c5ef6383ad7fe0d3e46a046710ad53ef8b5167d))
* **ci:** release-please simple mode with workspace toml path ([d56313d](https://github.com/metaneutrons/snapcast-rs/commit/d56313d49638510aa7e1063a44eb5d9fafd10d77))
* **ci:** release-please tags as v0.x.0 (no component prefix) ([7166b3b](https://github.com/metaneutrons/snapcast-rs/commit/7166b3b10189347f13ff1c5312025fbd3355ef3f))
* **ci:** remove empty env block in release workflow ([1749f66](https://github.com/metaneutrons/snapcast-rs/commit/1749f664a46e7c302f424bf311cf349efceee4c5))
* **ci:** remove sccache config from repo — local dev only ([6e461bf](https://github.com/metaneutrons/snapcast-rs/commit/6e461bfe70dde82ec3c117658b6741b8d5f3f574))
* **ci:** remove unnecessary i64 casts in time.rs (clippy on Linux) ([fedfac5](https://github.com/metaneutrons/snapcast-rs/commit/fedfac5871df818861d9116ad36a9eff69d5f309))
* **ci:** skip client + FLAC for aarch64-linux cross-compile ([a5b4242](https://github.com/metaneutrons/snapcast-rs/commit/a5b4242b3e82255d24f9a41ced61985aa5978f8f))
* **ci:** tolerate already-published crates, use CARGO_REGISTRY_TOKEN env ([a5f9466](https://github.com/metaneutrons/snapcast-rs/commit/a5f9466be57f65f9d0c07ebf7edf24c843e354c1))
* **ci:** use ubuntu-latest-arm runner ([65d182d](https://github.com/metaneutrons/snapcast-rs/commit/65d182d5ccc79a252eb351caecc2c466e8e35e30))
* clean Ctrl-C exit — process::exit(0) after mDNS shutdown ([c48b33c](https://github.com/metaneutrons/snapcast-rs/commit/c48b33ccc0fc691bf6bdfd788401db596a85eb0f))
* **client-bin,server-bin:** reject ambiguous unbracketed IPv6 with port ([1b1c844](https://github.com/metaneutrons/snapcast-rs/commit/1b1c844a615c03c2420b15e46dc417ba68718de3))
* **client:** align CLI options with C++ snapclient ([45f1dda](https://github.com/metaneutrons/snapcast-rs/commit/45f1dda0deaa59c13390e9851a717c3b14f5f0f0))
* **client:** clean Stream with working hard_sync, no drift correction ([cd18914](https://github.com/metaneutrons/snapcast-rs/commit/cd189145455e4be4c8391bd3f5b0ac9e09d3ba62))
* **client:** compile_error when no audio backend instead of runtime bail ([666a61a](https://github.com/metaneutrons/snapcast-rs/commit/666a61a4c4b767071f8109bcb97d17f10e3f9e36))
* **client:** confirm audio pipeline works — Stream hard_sync is the bug ([e16a8e6](https://github.com/metaneutrons/snapcast-rs/commit/e16a8e6821e0096992aa6e406ae12b4851364ec5))
* **client:** correct ALSA/Pulse constructor args for Linux build ([485636b](https://github.com/metaneutrons/snapcast-rs/commit/485636bacf0e793193e282891cecdb3c9459a85f))
* **client:** don't clear shared stream on cleanup, handle None safely ([bf84ca6](https://github.com/metaneutrons/snapcast-rs/commit/bf84ca62fef0b2a85da9872d439ccaf82029b5d8))
* **client:** fix audio output issues across all player backends ([a43fc3f](https://github.com/metaneutrons/snapcast-rs/commit/a43fc3f0cf2a409930217af476d1e625e03aafd4))
* **client:** fix FLAC decoder — use SampleBuffer&lt;i16&gt; not &lt;i32&gt; ([51728c4](https://github.com/metaneutrons/snapcast-rs/commit/51728c4222fd9e38736efe517db60435681672f8))
* **client:** fix FLAC/Vorbis decoder output size (capacity vs frames) ([3e10e18](https://github.com/metaneutrons/snapcast-rs/commit/3e10e18abb839eac7092a16dfa701c68bb31351a))
* **client:** full drift correction matching C++ behavior ([d88e564](https://github.com/metaneutrons/snapcast-rs/commit/d88e5643e97c211a700c2c4c6dbefffab2e85b76))
* **client:** graceful shutdown — Stop command exits run() instead of reconnecting ([8883097](https://github.com/metaneutrons/snapcast-rs/commit/88830972b4720901e749efcb00ba72c150ad108c))
* **client:** multi-backend player selection via cfg features ([ef1f194](https://github.com/metaneutrons/snapcast-rs/commit/ef1f194c726bc8edee0f4de85032008dfb452fad))
* **client:** process WireChunks during time sync instead of dropping ([dd8f377](https://github.com/metaneutrons/snapcast-rs/commit/dd8f3772bcfc88c20f1af68f914d300075bd5c53))
* **client:** proper CoreAudio DAC delay from device properties ([4f25891](https://github.com/metaneutrons/snapcast-rs/commit/4f25891bd0ba0bf12448ba53b3b4ee7765ed7bf2))
* **client:** proper drift correction with adjusted chunk timestamps ([781ac9e](https://github.com/metaneutrons/snapcast-rs/commit/781ac9ee19f1cc3538a26ad043a6c11cf3f9f32c))
* **client:** quiet reconnect — warn once, then debug, exponential backoff ([b0675d6](https://github.com/metaneutrons/snapcast-rs/commit/b0675d61a1d93b259960fae1b147bb4d2ddf263b))
* **client:** read stream format dynamically in cpal callback ([9c0b9e9](https://github.com/metaneutrons/snapcast-rs/commit/9c0b9e95dc29653aac4af435d8b8360fde80a19f))
* **client:** reinitialize shared Stream in-place — binary player sees audio ([55c1382](https://github.com/metaneutrons/snapcast-rs/commit/55c1382b1a9caa0b5e9a608355284dac1ba1b10c))
* **client:** remove audio_pump remnants, cpal reads Stream directly ([dba9e47](https://github.com/metaneutrons/snapcast-rs/commit/dba9e4733a9f67c5b87fd9d356ad0cec8eabb16b))
* **client:** remove double-counted buffer period in DAC delay ([ebf2049](https://github.com/metaneutrons/snapcast-rs/commit/ebf204967a5cdaf96d62f13ba20ee095e801663f))
* **client:** replace send_request with send+recv to fix deadlock ([b819c52](https://github.com/metaneutrons/snapcast-rs/commit/b819c5288f2e1d2ccdea8e251c9bae049c673d36))
* **client:** resolve breaking change in rustls-native-certs v0.8 ([bdbfa74](https://github.com/metaneutrons/snapcast-rs/commit/bdbfa74745532b046baee7157e0637084d840191))
* **client:** restore transport support and improve handshake robustness ([29a11b8](https://github.com/metaneutrons/snapcast-rs/commit/29a11b855250aa010dbeab4cf70cb041dc5db5fb))
* **client:** set CoreAudio sample rate to match stream format ([d152e1f](https://github.com/metaneutrons/snapcast-rs/commit/d152e1f19a4e4c1517260813931868766fae09fe))
* **client:** single reader architecture — audio_pump is the only Stream reader ([1ae87b4](https://github.com/metaneutrons/snapcast-rs/commit/1ae87b4595350892c35ee5dc34a6b237bd7f84ea))
* **client:** use CLOCK_MONOTONIC for time sync (matching C++ steady_clock) ([0f6a962](https://github.com/metaneutrons/snapcast-rs/commit/0f6a9628f22bf6cb2877ec3f71daa604b54878b4))
* **client:** use mach_continuous_time on macOS for time sync ([55e7132](https://github.com/metaneutrons/snapcast-rs/commit/55e7132e8a91bbfe2ee308faf23191817019a419))
* **client:** use real MAC address instead of dummy zeros ([0dfe866](https://github.com/metaneutrons/snapcast-rs/commit/0dfe866ffb4194ec6cb6061b2686e4549be5996d))
* **client:** working Stream with hard_sync + soft sync ([a57b508](https://github.com/metaneutrons/snapcast-rs/commit/a57b5087602c920c83bb668adcb2b11e81cef35b))
* **control:** return 'Method not found' for unhandled JSON-RPC on TCP ([df79ef0](https://github.com/metaneutrons/snapcast-rs/commit/df79ef059f9714ac4d158e7fa0fae9c9bfde8697))
* correct Time response latency + handle broadcast lag ([556f41c](https://github.com/metaneutrons/snapcast-rs/commit/556f41c40d18abc3fe92b0707ecd340492209798))
* **cpal:** use actual DAC delay from cpal timestamps instead of hardcoded 5ms ([3677040](https://github.com/metaneutrons/snapcast-rs/commit/367704028a23d8a599ed2e423e22df2a523c9457))
* cross-platform compilation (Linux, Windows, macOS) ([6416aea](https://github.com/metaneutrons/snapcast-rs/commit/6416aea344f91777468f4de7ba3dce54abe6565d))
* Ctrl-C exits within 1 second on both client and server ([cbde047](https://github.com/metaneutrons/snapcast-rs/commit/cbde0471ed501182cfd460e4d60619ee6cdfaff1))
* custom-protocol session_loop — drain outbound before select ([b839f6c](https://github.com/metaneutrons/snapcast-rs/commit/b839f6c40d835fd72ff610625371b21b7bdbe54c))
* **f32lz4:** zero-conversion path for f32 audio input ([3ca3eb2](https://github.com/metaneutrons/snapcast-rs/commit/3ca3eb28a9a26652c9ca74ecc7747d5923d1931f))
* **flac:** generate header by encoding silent frames (flac-codec requires data) ([a1e3be3](https://github.com/metaneutrons/snapcast-rs/commit/a1e3be3f879ee4a24c301657cafc09e9c13aaca2))
* **flac:** raw libflac-sys FFI with write callback — header/frame separation ([56080cb](https://github.com/metaneutrons/snapcast-rs/commit/56080cb398097c8ef7d62c723f647496d4c66487))
* **flac:** use same encoder instance for header and frames ([8c8a544](https://github.com/metaneutrons/snapcast-rs/commit/8c8a544d66c47aa52779f3caadaf65cb71f212a8))
* force exit via OS thread — tokio sleep can't fire when runtime is blocked ([d2a4012](https://github.com/metaneutrons/snapcast-rs/commit/d2a4012a3c3cbb7a55541ec34b043bfecec650c3))
* gate server ChunkDecryptor to #[cfg(test)] only ([30ef650](https://github.com/metaneutrons/snapcast-rs/commit/30ef650cc7258c2a0699b9e6956dd4c994d9c621))
* generate UUID-formatted group IDs matching C++ snapserver ([cf6c4a1](https://github.com/metaneutrons/snapcast-rs/commit/cf6c4a132aa0588718972f705aaeb9a7f933fb31))
* handle ClientInfo (type 7) from streaming clients ([a840032](https://github.com/metaneutrons/snapcast-rs/commit/a84003290cea74ab6bff7df57b4e447c89e4ea24))
* HIGH audit — crash/security fixes ([a9b6f5c](https://github.com/metaneutrons/snapcast-rs/commit/a9b6f5c6df2fe60b79702f48d67dc8ade0717c7e))
* **http:** add debug log before TcpListener::bind for diagnostics ([882ca13](https://github.com/metaneutrons/snapcast-rs/commit/882ca1307c8474a926d418bba8095c60acb2ba84))
* mDNS hostname .local. suffix + FLAC encoder header with None total_bytes ([63e8a65](https://github.com/metaneutrons/snapcast-rs/commit/63e8a6582fa0ed67190ed217bd22be671c74ea2a))
* **mdns:** clean shutdown (no unregister spam) + hostname fix ([385ae53](https://github.com/metaneutrons/snapcast-rs/commit/385ae5352d4a1cf7a3d989773e8214ff7e54de77))
* **mdns:** drop mDNS before aborting tasks — clean Ctrl-C shutdown ([5fb6a90](https://github.com/metaneutrons/snapcast-rs/commit/5fb6a907043270d59148d47d433e79e28633ee8e))
* **mdns:** enable all network interfaces explicitly ([2d7824d](https://github.com/metaneutrons/snapcast-rs/commit/2d7824d9ba0c8739cf5572cfe70c473c23ee6055))
* **mdns:** graceful shutdown — unregister before stopping daemon ([6d385ea](https://github.com/metaneutrons/snapcast-rs/commit/6d385eaa462f355135220b7a39ff1660ac02a992))
* **mdns:** just shutdown daemon on drop — no unregister attempt ([fadff3e](https://github.com/metaneutrons/snapcast-rs/commit/fadff3e5192bf112adc2884f7ccf85f767ad0f2e))
* **mixer:** quadratic curve instead of cubic ([6a74c26](https://github.com/metaneutrons/snapcast-rs/commit/6a74c2645c7fb9701cced21835c8037072776e54))
* never disconnect on unknown message types ([d18169b](https://github.com/metaneutrons/snapcast-rs/commit/d18169bc3388b21b7349edaa8a1bbe06740d8389))
* **player:** detect format from Stream, handle 32-bit f32lz4 correctly ([2fea112](https://github.com/metaneutrons/snapcast-rs/commit/2fea1123ecedfcf3173a6d2fd6c82d0c747e74d6))
* **player:** start cpal immediately — don't wait for AudioFrame gate ([6ae5c44](https://github.com/metaneutrons/snapcast-rs/commit/6ae5c44aa5fb4e732ea52e263ccdfee41c5779cc))
* remove all allow(dead_code) — zero exemptions, zero TODOs ([587b06a](https://github.com/metaneutrons/snapcast-rs/commit/587b06a97cf8bb504f4cde2a56a7d30455416df3))
* remove all blanket #![allow] and dead code from binary modules ([c57478d](https://github.com/metaneutrons/snapcast-rs/commit/c57478d809175f9928eb093416a92c5b0495e826))
* remove duplicate lz4_flex feature in client Cargo.toml ([72faaf8](https://github.com/metaneutrons/snapcast-rs/commit/72faaf8789f18ac786d63521fdb425a8508b4766))
* remove last allow(dead_code) — unused ManagedStream.name field removed ([7a42595](https://github.com/metaneutrons/snapcast-rs/commit/7a42595ac2a43a80b66cd493b727ba5ae449f688))
* remove unwrap/expect from library code ([8d779e7](https://github.com/metaneutrons/snapcast-rs/commit/8d779e7e8e039bffa7a272a673fe4e5c23d42c01))
* rename --no-mdns to --mdns-disable ([fa2f3d0](https://github.com/metaneutrons/snapcast-rs/commit/fa2f3d02d49a487f00489ed76abe13c0b7930df9))
* **server:** add f32lz4 to CLI help, replace panic with error propagation ([da71da2](https://github.com/metaneutrons/snapcast-rs/commit/da71da2446f96895cbf1298464baf95bede4a191))
* **server:** add flush(), fix Instant underflow in encoder gap detection ([0ef253c](https://github.com/metaneutrons/snapcast-rs/commit/0ef253cd3e7856af04e01ae14e8db56f9a966505))
* **server:** deduplicate streams on startup from persisted state ([fd0ccf3](https://github.com/metaneutrons/snapcast-rs/commit/fd0ccf34ab553d831b25de4830e99f99f85776ac))
* **server:** deep audit — wire all channels, fix all TODOs ([ca0eccd](https://github.com/metaneutrons/snapcast-rs/commit/ca0eccd70f9292175d3a941b6964cadb9964368e))
* **server:** default codec PCM until FLAC streaming encoder is fixed ([cedca6c](https://github.com/metaneutrons/snapcast-rs/commit/cedca6c8e260d5bc6f292d279cfb3cd7c47a5a2d))
* **server:** eliminate all TODOs — settings push + stream control fully routed ([7d4dd82](https://github.com/metaneutrons/snapcast-rs/commit/7d4dd8277602d975887d9a737cb2f9e0ebfd8b18))
* **server:** enable automatic IP address discovery in mDNS ServiceInfo ([7507911](https://github.com/metaneutrons/snapcast-rs/commit/7507911dde6c046cc2b2e7682f6083949aad1916))
* **server:** enterprise-grade fixes for F32AudioSender and encoder pacing ([1bb7ae0](https://github.com/metaneutrons/snapcast-rs/commit/1bb7ae01ca5f44135cae3f8ab475dcc8113d7e53))
* **server:** generate codec header from config when no streams exist yet ([c3dae89](https://github.com/metaneutrons/snapcast-rs/commit/c3dae89f1343ad60ec6dae0e615f30008f703ae3))
* **server:** match chunk size to FLAC block size (1152 frames) ([d451d47](https://github.com/metaneutrons/snapcast-rs/commit/d451d472947bcdcfaa5ef4e023e1487a4926e1ed))
* **server:** only create internal stream when no external streams configured ([c921065](https://github.com/metaneutrons/snapcast-rs/commit/c9210652e356a7fc9e7f4411f33143dff1d11f66))
* **server:** persistent streaming FLAC encoder — keeps state across chunks ([44e96d1](https://github.com/metaneutrons/snapcast-rs/commit/44e96d1cb71821526046d39a77f6042b5c0bd259))
* **server:** push volume/latency changes to streaming clients ([02e4b87](https://github.com/metaneutrons/snapcast-rs/commit/02e4b87c10101a816ca80483635d8ecab7089f2b))
* **server:** remove GroupClientsChanged, match C++ snapserver behavior ([09eccc1](https://github.com/metaneutrons/snapcast-rs/commit/09eccc1da0a23f333cfd23cbcaece7c055ab1be0))
* **server:** reuse encoder instance for codec header and stream encoding ([87614bc](https://github.com/metaneutrons/snapcast-rs/commit/87614bcf8bae67820d6e6759ecdc554e0f8c2208))
* **server:** route audio_tx through StreamManager encoder ([981c5b9](https://github.com/metaneutrons/snapcast-rs/commit/981c5b949217a046f9eb1e68a1e7411516399673))
* **server:** sample-count-based chunk timestamps instead of wall clock ([cb7d601](https://github.com/metaneutrons/snapcast-rs/commit/cb7d6011fe5fc743619fdcf68235658ac6d50333))
* **server:** switch FLAC encoder to flac-bound (libflac streaming API) ([4e222dd](https://github.com/metaneutrons/snapcast-rs/commit/4e222ddd39acbe97dd285ea2ec0aed0b62691891))
* **server:** use first-buffered-chunk timestamp for encoded output ([6c88495](https://github.com/metaneutrons/snapcast-rs/commit/6c88495f71e2d99c5db6d9867cd8f59b03d395c1))
* **server:** use monotonic clock for time sync (mach_continuous_time/CLOCK_MONOTONIC) ([0c676de](https://github.com/metaneutrons/snapcast-rs/commit/0c676de1d6672144079786f48c9f4560ba3561ff))
* set refersTo on ServerSettings to match Hello request id ([0b564d7](https://github.com/metaneutrons/snapcast-rs/commit/0b564d70004dac2698a210aa98050543fbcd0019))
* **snapclient-rs:** lifetime error in validate_alsa_control ([e23dfb9](https://github.com/metaneutrons/snapcast-rs/commit/e23dfb9cc5c7df3f6cbf981f6a8e34b0fcf83dba))
* **snapclient-rs:** perceptual volume curve for hardware mixer ([4423c1c](https://github.com/metaneutrons/snapcast-rs/commit/4423c1c2218a2c60d78344fa3093b1ba54fdf4a3))
* **snapclient:** correct custom message field access ([797656f](https://github.com/metaneutrons/snapcast-rs/commit/797656ffab3942de716aaf5c76e8ac9b52a5d97c))
* **snapclient:** gate sd-notify and systemd support for Linux only ([f637aa5](https://github.com/metaneutrons/snapcast-rs/commit/f637aa51037cf7162ddb9e64868a17f086a4415d))
* **snapclient:** resolve lint and formatting errors ([b39e79d](https://github.com/metaneutrons/snapcast-rs/commit/b39e79d01409462bae052a3f00cde95b89dafd6c))
* Stream.OnProperties + Stream.OnUpdate notification broadcasting ([9195b0e](https://github.com/metaneutrons/snapcast-rs/commit/9195b0e0c36746be4209c53529b991cc467d4d0f))
* **test:** encryption tests must explicitly set codec to f32lz4 ([37333bd](https://github.com/metaneutrons/snapcast-rs/commit/37333bd390437f49ef8ab6d69922ec78222d90a2))
* **tests:** fix encryption test destructuring and missing stream/field ([f8d31fd](https://github.com/metaneutrons/snapcast-rs/commit/f8d31fd50c97aec35cd84aa14f3c1a8749a2f7c3))
* three audit findings ([deb61b8](https://github.com/metaneutrons/snapcast-rs/commit/deb61b84843da9e18681cb6d2f426bc869be6ff0))
* three enterprise-grade audit findings ([36d61b0](https://github.com/metaneutrons/snapcast-rs/commit/36d61b0ce4becb3989db35c3a3424cf9b6a9e77e))
* three more audit findings ([d48d326](https://github.com/metaneutrons/snapcast-rs/commit/d48d326c6ed0387e8cf56d6e534154ab89e20f93))
* workspace snapcast-client dep default-features=false for cross-platform ([6c94c2a](https://github.com/metaneutrons/snapcast-rs/commit/6c94c2a99ebf21b7f6a517b9d119540ca8b04d62))

## [0.13.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.12.2...v0.13.0) (2026-05-23)


### Features

* **mdns:** add mdns-os backend via astro-dnssd (default) ([d24eb23](https://github.com/metaneutrons/snapcast-rs/commit/d24eb23f6cb429c136d21c02ac9f654bad63beb8))

## [0.12.2](https://github.com/metaneutrons/snapcast-rs/compare/v0.12.1...v0.12.2) (2026-05-23)


### Bug Fixes

* **mdns:** enable all network interfaces explicitly ([2d7824d](https://github.com/metaneutrons/snapcast-rs/commit/2d7824d9ba0c8739cf5572cfe70c473c23ee6055))

## [0.12.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.12.0...v0.12.1) (2026-05-22)


### Bug Fixes

* **server:** enable automatic IP address discovery in mDNS ServiceInfo ([7507911](https://github.com/metaneutrons/snapcast-rs/commit/7507911dde6c046cc2b2e7682f6083949aad1916))

## [0.12.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.11.0...v0.12.0) (2026-05-19)


### Features

* **client-bin:** IPv6 bracket notation and reject WS audio scheme ([e20f0ea](https://github.com/metaneutrons/snapcast-rs/commit/e20f0ea994ea63e74edaa98ed4eab61b6b595b55))
* **client:** f32 decode path for FLAC and Vorbis, SampleEncoding enum ([6bdf229](https://github.com/metaneutrons/snapcast-rs/commit/6bdf229afe880454820a1ab7bcd98ab2d3fcabe6))
* **client:** resampler outputs f32 directly, no i16 quantization ([3f91ea4](https://github.com/metaneutrons/snapcast-rs/commit/3f91ea4bef2fa5f3114a6a68d71ef8522b327f7a))
* **server-bin:** configurable bind addresses, IPv6 stream URIs ([3d3666b](https://github.com/metaneutrons/snapcast-rs/commit/3d3666bf80b2d9f0e823a03ac091a32e2dc4bc23))


### Bug Fixes

* **client-bin,server-bin:** reject ambiguous unbracketed IPv6 with port ([1b1c844](https://github.com/metaneutrons/snapcast-rs/commit/1b1c844a615c03c2420b15e46dc417ba68718de3))

## [0.11.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.10.1...v0.11.0) (2026-05-15)


### Features

* **client:** enable audio frame reporting and update integration tests ([44aa5a2](https://github.com/metaneutrons/snapcast-rs/commit/44aa5a26cee499890d62e3afdec8c020f052c446))


### Bug Fixes

* **client:** restore transport support and improve handshake robustness ([29a11b8](https://github.com/metaneutrons/snapcast-rs/commit/29a11b855250aa010dbeab4cf70cb041dc5db5fb))
* **snapclient:** correct custom message field access ([797656f](https://github.com/metaneutrons/snapcast-rs/commit/797656ffab3942de716aaf5c76e8ac9b52a5d97c))

## [0.10.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.10.0...v0.10.1) (2026-05-15)


### Bug Fixes

* **snapclient:** gate sd-notify and systemd support for Linux only ([f637aa5](https://github.com/metaneutrons/snapcast-rs/commit/f637aa51037cf7162ddb9e64868a17f086a4415d))

## [0.10.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.9.1...v0.10.0) (2026-05-15)


### Features

* **snapclient:** add systemd notification support and status reporting ([77e69df](https://github.com/metaneutrons/snapcast-rs/commit/77e69df39199e546752dbe54447f19eac94504f4))
* **snapclient:** implement dynamic format handling and resampler integration ([6d354b7](https://github.com/metaneutrons/snapcast-rs/commit/6d354b72fdc1e9051c2862b10032e5de10948133))


### Bug Fixes

* **client:** resolve breaking change in rustls-native-certs v0.8 ([bdbfa74](https://github.com/metaneutrons/snapcast-rs/commit/bdbfa74745532b046baee7157e0637084d840191))
* **snapclient:** resolve lint and formatting errors ([b39e79d](https://github.com/metaneutrons/snapcast-rs/commit/b39e79d01409462bae052a3f00cde95b89dafd6c))

## [0.9.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.9.0...v0.9.1) (2026-05-11)


### Bug Fixes

* **mixer:** quadratic curve instead of cubic ([6a74c26](https://github.com/metaneutrons/snapcast-rs/commit/6a74c2645c7fb9701cced21835c8037072776e54))
* **server:** deduplicate streams on startup from persisted state ([fd0ccf3](https://github.com/metaneutrons/snapcast-rs/commit/fd0ccf34ab553d831b25de4830e99f99f85776ac))
* **snapclient-rs:** perceptual volume curve for hardware mixer ([4423c1c](https://github.com/metaneutrons/snapcast-rs/commit/4423c1c2218a2c60d78344fa3093b1ba54fdf4a3))

## [0.9.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.8.0...v0.9.0) (2026-05-02)


### Features

* **snapclient-rs:** implement volume mixer (software + hardware ALSA) ([9d7eeaf](https://github.com/metaneutrons/snapcast-rs/commit/9d7eeaf773ff6f5fd26078695921fc9d9fc03849))


### Bug Fixes

* **snapclient-rs:** lifetime error in validate_alsa_control ([e23dfb9](https://github.com/metaneutrons/snapcast-rs/commit/e23dfb9cc5c7df3f6cbf981f6a8e34b0fcf83dba))

## [0.8.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.7.0...v0.8.0) (2026-05-01)


### Features

* **server:** add ClientFilter trait for connection-level filtering ([6cd60fc](https://github.com/metaneutrons/snapcast-rs/commit/6cd60fca9db9df762621cdc998f0c25e4b626f86))

## [0.7.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.6.0...v0.7.0) (2026-04-15)


### ⚠ BREAKING CHANGES

* ClientConnected carries Hello struct instead of flat fields

### Features

* ClientConnected carries Hello struct instead of flat fields ([cc4fe01](https://github.com/metaneutrons/snapcast-rs/commit/cc4fe01db2c8f4e216c2d49748571cc95e4afd8c))

## [0.6.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.5.0...v0.6.0) (2026-04-12)


### Features

* --no-mdns and --mdns-name CLI flags for snapserver-rs ([30f7c35](https://github.com/metaneutrons/snapcast-rs/commit/30f7c3527d17429556329545381405fa76fa6a50))
* add mdns_enabled config for runtime mDNS control ([0dfbea9](https://github.com/metaneutrons/snapcast-rs/commit/0dfbea9553a23c307da7c9c97001c7be9554b567))
* full C++ API parity — AddStream, RemoveStream, StreamControl, notifications ([f780a91](https://github.com/metaneutrons/snapcast-rs/commit/f780a9101b456736afa67e70359b703358685da7))
* **server:** add F32AudioSender for embedded audio sources ([958d380](https://github.com/metaneutrons/snapcast-rs/commit/958d380f2be6d407436de3eb6625f0b277094dc4))
* **server:** add state persistence and SetStreamMeta command ([0fad761](https://github.com/metaneutrons/snapcast-rs/commit/0fad761bca1ea40985be2eae9ed6ac747cd94b87))
* **server:** auto-register clients in state and create groups on connect ([086022f](https://github.com/metaneutrons/snapcast-rs/commit/086022f3f58ab08db90756a21f4850710da07b1e))
* **server:** realtime pacing for F32 embedded sources ([b6caee3](https://github.com/metaneutrons/snapcast-rs/commit/b6caee365cde44df8328c38e5b4ab8405cdec250))
* **server:** typed ServerStatus API — no more JSON in library boundary ([53decec](https://github.com/metaneutrons/snapcast-rs/commit/53decec0468285ff757e0be56a9707e9796b9c22))
* **session:** per-stream chunk routing + mute filtering ([40b82e2](https://github.com/metaneutrons/snapcast-rs/commit/40b82e2af4e83c82f8cb29a3a90067fb12a722fb))
* **status:** full C++ wire-format compatible types ([981bfcf](https://github.com/metaneutrons/snapcast-rs/commit/981bfcf0fa0ba9a51e072affb71af21c074f6edb))
* **status:** typed StreamStatus enum with From&lt;&str&gt; conversion ([cee98fe](https://github.com/metaneutrons/snapcast-rs/commit/cee98fefb1a2c7f77aff5c18c5dcce58036df7a7))


### Bug Fixes

* correct Time response latency + handle broadcast lag ([556f41c](https://github.com/metaneutrons/snapcast-rs/commit/556f41c40d18abc3fe92b0707ecd340492209798))
* custom-protocol session_loop — drain outbound before select ([b839f6c](https://github.com/metaneutrons/snapcast-rs/commit/b839f6c40d835fd72ff610625371b21b7bdbe54c))
* gate server ChunkDecryptor to #[cfg(test)] only ([30ef650](https://github.com/metaneutrons/snapcast-rs/commit/30ef650cc7258c2a0699b9e6956dd4c994d9c621))
* generate UUID-formatted group IDs matching C++ snapserver ([cf6c4a1](https://github.com/metaneutrons/snapcast-rs/commit/cf6c4a132aa0588718972f705aaeb9a7f933fb31))
* handle ClientInfo (type 7) from streaming clients ([a840032](https://github.com/metaneutrons/snapcast-rs/commit/a84003290cea74ab6bff7df57b4e447c89e4ea24))
* never disconnect on unknown message types ([d18169b](https://github.com/metaneutrons/snapcast-rs/commit/d18169bc3388b21b7349edaa8a1bbe06740d8389))
* rename --no-mdns to --mdns-disable ([fa2f3d0](https://github.com/metaneutrons/snapcast-rs/commit/fa2f3d02d49a487f00489ed76abe13c0b7930df9))
* **server:** add flush(), fix Instant underflow in encoder gap detection ([0ef253c](https://github.com/metaneutrons/snapcast-rs/commit/0ef253cd3e7856af04e01ae14e8db56f9a966505))
* **server:** enterprise-grade fixes for F32AudioSender and encoder pacing ([1bb7ae0](https://github.com/metaneutrons/snapcast-rs/commit/1bb7ae01ca5f44135cae3f8ab475dcc8113d7e53))
* set refersTo on ServerSettings to match Hello request id ([0b564d7](https://github.com/metaneutrons/snapcast-rs/commit/0b564d70004dac2698a210aa98050543fbcd0019))
* Stream.OnProperties + Stream.OnUpdate notification broadcasting ([9195b0e](https://github.com/metaneutrons/snapcast-rs/commit/9195b0e0c36746be4209c53529b991cc467d4d0f))
* **tests:** fix encryption test destructuring and missing stream/field ([f8d31fd](https://github.com/metaneutrons/snapcast-rs/commit/f8d31fd50c97aec35cd84aa14f3c1a8749a2f7c3))
* three audit findings ([deb61b8](https://github.com/metaneutrons/snapcast-rs/commit/deb61b84843da9e18681cb6d2f426bc869be6ff0))
* three enterprise-grade audit findings ([36d61b0](https://github.com/metaneutrons/snapcast-rs/commit/36d61b0ce4becb3989db35c3a3424cf9b6a9e77e))
* three more audit findings ([d48d326](https://github.com/metaneutrons/snapcast-rs/commit/d48d326c6ed0387e8cf56d6e534154ab89e20f93))

## [0.5.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.4.0...v0.5.0) (2026-04-10)


### Features

* **server:** re-export DEFAULT_ENCRYPTION_PSK for embedders ([edfb9f8](https://github.com/metaneutrons/snapcast-rs/commit/edfb9f86b4075807daec426d4f3e96a9d6ad5a13))

## [0.4.0](https://github.com/metaneutrons/snapcast-rs/compare/v0.3.1...v0.4.0) (2026-04-10)


### ⚠ BREAKING CHANGES

* **server:** add_stream() API — multi-stream, library owns encoding

### Features

* **encoder:** add PCM→f32 conversion warning to vorbis encoder ([07bf6e1](https://github.com/metaneutrons/snapcast-rs/commit/07bf6e1d0ccdf7de7674144b7a7d236c3ea597aa))
* **encoder:** one-time warnings on input/codec format mismatch ([f42c3c4](https://github.com/metaneutrons/snapcast-rs/commit/f42c3c4cc89d739414c1f636cac77d1fdb0666a6))
* encryption support in binaries with default PSK ([272585c](https://github.com/metaneutrons/snapcast-rs/commit/272585c3e9e78a92d1ec5685b647e70d3c0ebd5d))
* **server:** add_stream() API — multi-stream, library owns encoding ([8a51bd8](https://github.com/metaneutrons/snapcast-rs/commit/8a51bd8ec6d085f6cdec01814e8a9b754d74592e))
* **server:** emit GroupNameChanged, GroupClientsChanged, ServerUpdated events ([3e86590](https://github.com/metaneutrons/snapcast-rs/commit/3e86590b0a1328f2b8504533aa8df980dda0890f))
* **server:** include MAC address in ClientConnected event ([737f1b2](https://github.com/metaneutrons/snapcast-rs/commit/737f1b2f2a8b48d4cbb4e6ff035542d7318a9727))
* **server:** per-stream codec/format config via add_stream_with_config() ([0d57ca2](https://github.com/metaneutrons/snapcast-rs/commit/0d57ca25fae03aec1f9f4fac01e59c87aecca04d))
* **server:** redesign AudioFrame with AudioData enum (F32/Pcm) ([21fcc97](https://github.com/metaneutrons/snapcast-rs/commit/21fcc97fb7da94224a7057b17dcd75db75ae69d9))
* **server:** warn once on input/codec format mismatch ([1e53bf7](https://github.com/metaneutrons/snapcast-rs/commit/1e53bf7846eb9639df55f1dd8ebb3b9b04f413e0))


### Bug Fixes

* add readme path to all published crates for crates.io ([e889ac2](https://github.com/metaneutrons/snapcast-rs/commit/e889ac2e8e74aac50a33ce0b370dc263dac5af88))
* **client:** read stream format dynamically in cpal callback ([9c0b9e9](https://github.com/metaneutrons/snapcast-rs/commit/9c0b9e95dc29653aac4af435d8b8360fde80a19f))
* **server:** only create internal stream when no external streams configured ([c921065](https://github.com/metaneutrons/snapcast-rs/commit/c9210652e356a7fc9e7f4411f33143dff1d11f66))
* **server:** remove GroupClientsChanged, match C++ snapserver behavior ([09eccc1](https://github.com/metaneutrons/snapcast-rs/commit/09eccc1da0a23f333cfd23cbcaece7c055ab1be0))
* **server:** reuse encoder instance for codec header and stream encoding ([87614bc](https://github.com/metaneutrons/snapcast-rs/commit/87614bcf8bae67820d6e6759ecdc554e0f8c2208))
* **server:** route audio_tx through StreamManager encoder ([981c5b9](https://github.com/metaneutrons/snapcast-rs/commit/981c5b949217a046f9eb1e68a1e7411516399673))

## [0.3.1](https://github.com/metaneutrons/snapcast-rs/compare/v0.3.0...v0.3.1) (2026-04-10)


### Bug Fixes

* **ci:** tolerate already-published crates, use CARGO_REGISTRY_TOKEN env ([a5f9466](https://github.com/metaneutrons/snapcast-rs/commit/a5f9466be57f65f9d0c07ebf7edf24c843e354c1))

## [0.3.0](https://github.com/metaneutrons/snapcast-rs/compare/snapcast-rs-v0.2.0...snapcast-rs-v0.3.0) (2026-04-10)


### Features

* add structured logging to f32lz4 encoder/decoder ([c7ab264](https://github.com/metaneutrons/snapcast-rs/commit/c7ab26436d4d5b2ac229b27a162261e422ac377c))
* **binary:** platform-aware default audio backend ([4e704a6](https://github.com/metaneutrons/snapcast-rs/commit/4e704a629f3fb0c7dbd07c876ce4b244f515eafb))
* **binary:** wire JSON-RPC control + HTTP servers in snapserver-rs ([f1b130c](https://github.com/metaneutrons/snapcast-rs/commit/f1b130cc3e34a6fffc798e431c8218e2e254ee57))
* **client:** 100% functional equivalency with C++ snapclient ([187f226](https://github.com/metaneutrons/snapcast-rs/commit/187f226cbe89657c6f0dc790707207d85d47d9eb))
* **client:** add --daemon and --user CLI options (Unix) ([466893c](https://github.com/metaneutrons/snapcast-rs/commit/466893c68783dfe649626fe6e9160e73dd726a59))
* **client:** audio pump — library outputs f32 via audio_rx channel ([0678848](https://github.com/metaneutrons/snapcast-rs/commit/067884873c7425dd1ba1fec52e1954493cd99f30))
* **client:** cpal cross-platform audio backend — Windows support ([3c931c0](https://github.com/metaneutrons/snapcast-rs/commit/3c931c09a88395d075a94c91dd6e8afa49ebf65e))
* **client:** graceful shutdown on Ctrl-C ([5b5bee5](https://github.com/metaneutrons/snapcast-rs/commit/5b5bee5794323fb114fd0f740a69fbbe2afc1a66))
* **client:** implement ALSA and PulseAudio player backends ([940687f](https://github.com/metaneutrons/snapcast-rs/commit/940687f74465643d58800b197364629412a24caf))
* **client:** implement ClientSettings and CLI argument parsing ([78ad4c3](https://github.com/metaneutrons/snapcast-rs/commit/78ad4c3b881c86283a8faf50fcc2b1ccc9f9e1d4))
* **client:** implement Controller (main orchestrator) ([9167bd0](https://github.com/metaneutrons/snapcast-rs/commit/9167bd0dcb86910e0cdde86818aec0265fa65e1e))
* **client:** implement CoreAudio player backend ([d331cb3](https://github.com/metaneutrons/snapcast-rs/commit/d331cb34ab4ad1ec4c6a4c3d62c17d12f9065ee0))
* **client:** implement Decoder trait and PCM decoder ([d00bb20](https://github.com/metaneutrons/snapcast-rs/commit/d00bb20df7d9ffc49c037defb29e83d4d30125f3))
* **client:** implement DoubleBuffer and TimeProvider ([b4a4836](https://github.com/metaneutrons/snapcast-rs/commit/b4a48367c1515e4de804de57702c732f1d2c3128))
* **client:** implement FLAC decoder with symphonia ([b5c8440](https://github.com/metaneutrons/snapcast-rs/commit/b5c8440c6084f72463d6c265e038433d96a0c61e))
* **client:** implement mDNS discovery, WebSocket, and WSS connections ([09a598b](https://github.com/metaneutrons/snapcast-rs/commit/09a598b50fb76d7b14b2ce266a6598e38ab63459))
* **client:** implement Player trait and software volume ([667785d](https://github.com/metaneutrons/snapcast-rs/commit/667785d3c44ad3a91fe4714e9e6093999447eece))
* **client:** implement proper logging with --logsink and --logfilter ([7c4ece3](https://github.com/metaneutrons/snapcast-rs/commit/7c4ece3e022d5c081c7259d949b0a709a071e1c3))
* **client:** implement resampler and README ([7925d90](https://github.com/metaneutrons/snapcast-rs/commit/7925d90e7ae4fddb6abef2819f8870f665be0a39))
* **client:** implement Stream (time-synchronized PCM buffer) ([5cb3c21](https://github.com/metaneutrons/snapcast-rs/commit/5cb3c213bf01d4128a19768cb2017f0ea3b0b392))
* **client:** implement TCP connection with frame I/O ([d2645f2](https://github.com/metaneutrons/snapcast-rs/commit/d2645f27544183c6ddae3de0fff058a3520a5fd4))
* **client:** implement Vorbis and Opus decoders ([8d2f33c](https://github.com/metaneutrons/snapcast-rs/commit/8d2f33ce504dbe79b0a4bf5889ea255602af5640))
* **client:** implement Vorbis decoder with symphonia ([9bd3378](https://github.com/metaneutrons/snapcast-rs/commit/9bd33785a196e97d859cd2230b77188c085af22a))
* **client:** replace audiopus (C FFI) with opus-decoder (pure Rust) ([8852e40](https://github.com/metaneutrons/snapcast-rs/commit/8852e40dc097a9733a05cf9d26450789a9db8b91))
* custom binary protocol messages (feature-gated: custom-protocol) ([c8ea9a5](https://github.com/metaneutrons/snapcast-rs/commit/c8ea9a507b4c3b8a23d80efc68d3b1e3714e00ad))
* default codecs are now PCM + f32lz4 (pure Rust, zero C deps) ([264e12a](https://github.com/metaneutrons/snapcast-rs/commit/264e12a88cd2669c635eaf2251e84a6599197949))
* eliminate C dependencies with pure Rust FLAC + feature-gated codecs ([f2bf4bd](https://github.com/metaneutrons/snapcast-rs/commit/f2bf4bd9272ca4d8830c0d3462c629418268ac22))
* enterprise encryption wiring — EncoderConfig, key injection, integration tests ([36a9f1f](https://github.com/metaneutrons/snapcast-rs/commit/36a9f1fc55a5ca66048d8a33f79eb155298ba05c))
* f32lz4 codec — lossless compressed f32 transmission (feature-gated) ([1863cb7](https://github.com/metaneutrons/snapcast-rs/commit/1863cb78902d55654e5a616049dfe558f1bb94a7))
* feature-gate mdns on both server and client (default on) ([151a16f](https://github.com/metaneutrons/snapcast-rs/commit/151a16f708c7140a31c4a19598082632b2407cf5))
* FLAC as default codec, f32lz4 + encryption optional ([902d6e1](https://github.com/metaneutrons/snapcast-rs/commit/902d6e19f2a1e31ba25de06a87cd37d031de3ba1))
* optional ChaCha20-Poly1305 encryption for f32lz4 (--features encryption) ([1ce5fcf](https://github.com/metaneutrons/snapcast-rs/commit/1ce5fcf967c2ba48467b29c4d743a3dd661abffb))
* production-quality structured logging across all crates ([fb2c72c](https://github.com/metaneutrons/snapcast-rs/commit/fb2c72c00d0bba3218ba4721cec3ccb88020b442))
* **proto:** extract Timeval to types module, add SampleFormat ([fc9c835](https://github.com/metaneutrons/snapcast-rs/commit/fc9c835c60f84a6d5d56bb1057a1db475947451b))
* **proto:** implement all message types ([15979d2](https://github.com/metaneutrons/snapcast-rs/commit/15979d2b4252500d6b8b206763297bfcaa8b4878))
* **proto:** implement message factory and full frame serialization ([92ce52e](https://github.com/metaneutrons/snapcast-rs/commit/92ce52ef888ed4d11f89fd64edb2eed312ec5c0d))
* **proto:** wire custom binary protocol messages end-to-end ([8947cbe](https://github.com/metaneutrons/snapcast-rs/commit/8947cbe439a63ee4199d1bde9c92550f999086dc))
* **server:** 100% pure Rust by default — no C libraries needed ([1ed1823](https://github.com/metaneutrons/snapcast-rs/commit/1ed18236aa90717660cebebcbe3736a0f32ad9c3))
* **server:** audio_tx input channel — embedding apps can push f32 audio ([9c74568](https://github.com/metaneutrons/snapcast-rs/commit/9c74568d516a62e9dbaa3904aa55c7ed31b13b98))
* **server:** auth middleware on HTTP and TCP control ([6de075b](https://github.com/metaneutrons/snapcast-rs/commit/6de075bc15c6e29af3bf44db146b6c0a3f681660))
* **server:** client session management + wired SnapServer::run() ([b270d2e](https://github.com/metaneutrons/snapcast-rs/commit/b270d2ec6fc5268082ee4219befec3ae7a539d85))
* **server:** config file + HTTP/WebSocket/Snapweb — all deferred items done ([bc3d1ea](https://github.com/metaneutrons/snapcast-rs/commit/bc3d1ea1a9006ecb4bdd6dc6cb6431f1d85d89d3))
* **server:** default codec follows compiled features ([431aa13](https://github.com/metaneutrons/snapcast-rs/commit/431aa138bf8c86fddb943b28ead376239cb5ce1b))
* **server:** encoders — PCM, FLAC, Opus, Vorbis ([7af68d2](https://github.com/metaneutrons/snapcast-rs/commit/7af68d243d07330c58e2966de9d75e76c83d9d96))
* **server:** group/stream/client state model with JSON persistence ([6bd32ef](https://github.com/metaneutrons/snapcast-rs/commit/6bd32ef289b23360bfdfda0187b2a68f67be3826))
* **server:** JSON-RPC control API — all method handlers ([10a6f9a](https://github.com/metaneutrons/snapcast-rs/commit/10a6f9a3d48d04968e2441229e29ff85c2d6851e))
* **server:** JSON-RPC control transport + extension point wired ([db1a0e4](https://github.com/metaneutrons/snapcast-rs/commit/db1a0e4dfe56b2aab4bb364240fd81902a3039eb))
* **server:** JSON-RPC method/notification registry with proper response flow ([063c916](https://github.com/metaneutrons/snapcast-rs/commit/063c91651bb5040f093d454ae48f0a32fa7a3267))
* **server:** librespot + airplay stream reader specializations ([304d665](https://github.com/metaneutrons/snapcast-rs/commit/304d665aaaa2292e798db2cea3d7ccbb26fdf6a8))
* **server:** mDNS service advertisement + auth/config deferred ([1def82a](https://github.com/metaneutrons/snapcast-rs/commit/1def82aced99e703f0acfa2499fe8e2aa811a343))
* **server:** replace all stubs with real implementations ([3717946](https://github.com/metaneutrons/snapcast-rs/commit/37179463c6cd95a84eb40121e843769fa98ea511))
* **server:** scaffold snapcast-server library + snapserver-rs binary ([2117537](https://github.com/metaneutrons/snapcast-rs/commit/2117537f936e612b4f0e92438cb36d9e1ca09baa))
* **server:** stream manager — routes PCM through encoders to clients ([a5eba1f](https://github.com/metaneutrons/snapcast-rs/commit/a5eba1fbba96b0b94b139f525b919b5ebaed4a3b))
* **server:** stream readers — pipe, file, process, TCP ([773986a](https://github.com/metaneutrons/snapcast-rs/commit/773986aceae3318038989cc9858853ffba00d095))
* **server:** streaming client auth — AuthValidator trait + StaticAuthValidator ([4e2a52c](https://github.com/metaneutrons/snapcast-rs/commit/4e2a52c85599045c65c6182e7b093b51d3167b0c))
* **server:** typed ServerCommand API — no JSON-RPC detour for embedding ([fd17723](https://github.com/metaneutrons/snapcast-rs/commit/fd1772346511a2539fb4830ca12e144893458fd1))
* **server:** typed ServerEvent notifications for all state changes ([2f74ea5](https://github.com/metaneutrons/snapcast-rs/commit/2f74ea5917b50cf6dcdad403f2fe9145a7d540aa))


### Bug Fixes

* **alsa:** use CStr for HintIter (alsa crate 0.9 API) ([3f41265](https://github.com/metaneutrons/snapcast-rs/commit/3f4126559b2ef69377eee106c3fc6ed06baf99aa))
* **audio:** cpal callback pulls from Stream directly — no ring buffer ([fc2aefb](https://github.com/metaneutrons/snapcast-rs/commit/fc2aefb489ce1095b95ff771d28e9d87a5eeaee6))
* **binary:** forward codec features (flac, opus, vorbis, f32lz4) to server library ([2344723](https://github.com/metaneutrons/snapcast-rs/commit/2344723822e9a949cdc9268836d97896b1d0ad91))
* **binary:** forward f32lz4 feature to client library, enable by default ([943fe27](https://github.com/metaneutrons/snapcast-rs/commit/943fe270e67731055d06b41e8a764f02bb4b339c))
* **binary:** forward feature flags from snapclient-rs to snapcast-client ([36c6fd0](https://github.com/metaneutrons/snapcast-rs/commit/36c6fd051e33abdb845b499e2a6a95363bf223a0))
* **binary:** remove duplicate disconnect log — controller already logs it ([b4a885c](https://github.com/metaneutrons/snapcast-rs/commit/b4a885c0e19dd7f53648517e1defada2994796a4))
* **binary:** restore native CoreAudio + ALSA backends from v0.1.0 ([37d0345](https://github.com/metaneutrons/snapcast-rs/commit/37d034581e95fffbefc5dbddb632f7fea738923b))
* **binary:** spawn Ctrl-C handler before setup — always exits cleanly ([30cfb1d](https://github.com/metaneutrons/snapcast-rs/commit/30cfb1ddc94a03b6471dbc2ac7f60b1e5c22049c))
* **ci:** configure release-please for workspace — manifest mode ([8b70fad](https://github.com/metaneutrons/snapcast-rs/commit/8b70fad836e748cb06ac419e82284b6ca4236c5b))
* **ci:** correct release-please manifest format ([55a8459](https://github.com/metaneutrons/snapcast-rs/commit/55a8459c8a01d8c601e1bd57ac7e88de042399c0))
* **ci:** install libasound2-dev for Linux builds (cpal needs ALSA) ([67367e3](https://github.com/metaneutrons/snapcast-rs/commit/67367e3e3e2645cf93553923d4d088e4fd809d3e))
* **ci:** release-please simple mode with workspace toml path ([d56313d](https://github.com/metaneutrons/snapcast-rs/commit/d56313d49638510aa7e1063a44eb5d9fafd10d77))
* **ci:** remove sccache config from repo — local dev only ([6e461bf](https://github.com/metaneutrons/snapcast-rs/commit/6e461bfe70dde82ec3c117658b6741b8d5f3f574))
* **ci:** remove unnecessary i64 casts in time.rs (clippy on Linux) ([fedfac5](https://github.com/metaneutrons/snapcast-rs/commit/fedfac5871df818861d9116ad36a9eff69d5f309))
* clean Ctrl-C exit — process::exit(0) after mDNS shutdown ([c48b33c](https://github.com/metaneutrons/snapcast-rs/commit/c48b33ccc0fc691bf6bdfd788401db596a85eb0f))
* **client:** align CLI options with C++ snapclient ([45f1dda](https://github.com/metaneutrons/snapcast-rs/commit/45f1dda0deaa59c13390e9851a717c3b14f5f0f0))
* **client:** clean Stream with working hard_sync, no drift correction ([cd18914](https://github.com/metaneutrons/snapcast-rs/commit/cd189145455e4be4c8391bd3f5b0ac9e09d3ba62))
* **client:** compile_error when no audio backend instead of runtime bail ([666a61a](https://github.com/metaneutrons/snapcast-rs/commit/666a61a4c4b767071f8109bcb97d17f10e3f9e36))
* **client:** confirm audio pipeline works — Stream hard_sync is the bug ([e16a8e6](https://github.com/metaneutrons/snapcast-rs/commit/e16a8e6821e0096992aa6e406ae12b4851364ec5))
* **client:** correct ALSA/Pulse constructor args for Linux build ([485636b](https://github.com/metaneutrons/snapcast-rs/commit/485636bacf0e793193e282891cecdb3c9459a85f))
* **client:** don't clear shared stream on cleanup, handle None safely ([bf84ca6](https://github.com/metaneutrons/snapcast-rs/commit/bf84ca62fef0b2a85da9872d439ccaf82029b5d8))
* **client:** fix audio output issues across all player backends ([a43fc3f](https://github.com/metaneutrons/snapcast-rs/commit/a43fc3f0cf2a409930217af476d1e625e03aafd4))
* **client:** fix FLAC decoder — use SampleBuffer&lt;i16&gt; not &lt;i32&gt; ([51728c4](https://github.com/metaneutrons/snapcast-rs/commit/51728c4222fd9e38736efe517db60435681672f8))
* **client:** fix FLAC/Vorbis decoder output size (capacity vs frames) ([3e10e18](https://github.com/metaneutrons/snapcast-rs/commit/3e10e18abb839eac7092a16dfa701c68bb31351a))
* **client:** full drift correction matching C++ behavior ([d88e564](https://github.com/metaneutrons/snapcast-rs/commit/d88e5643e97c211a700c2c4c6dbefffab2e85b76))
* **client:** graceful shutdown — Stop command exits run() instead of reconnecting ([8883097](https://github.com/metaneutrons/snapcast-rs/commit/88830972b4720901e749efcb00ba72c150ad108c))
* **client:** multi-backend player selection via cfg features ([ef1f194](https://github.com/metaneutrons/snapcast-rs/commit/ef1f194c726bc8edee0f4de85032008dfb452fad))
* **client:** process WireChunks during time sync instead of dropping ([dd8f377](https://github.com/metaneutrons/snapcast-rs/commit/dd8f3772bcfc88c20f1af68f914d300075bd5c53))
* **client:** proper CoreAudio DAC delay from device properties ([4f25891](https://github.com/metaneutrons/snapcast-rs/commit/4f25891bd0ba0bf12448ba53b3b4ee7765ed7bf2))
* **client:** proper drift correction with adjusted chunk timestamps ([781ac9e](https://github.com/metaneutrons/snapcast-rs/commit/781ac9ee19f1cc3538a26ad043a6c11cf3f9f32c))
* **client:** quiet reconnect — warn once, then debug, exponential backoff ([b0675d6](https://github.com/metaneutrons/snapcast-rs/commit/b0675d61a1d93b259960fae1b147bb4d2ddf263b))
* **client:** reinitialize shared Stream in-place — binary player sees audio ([55c1382](https://github.com/metaneutrons/snapcast-rs/commit/55c1382b1a9caa0b5e9a608355284dac1ba1b10c))
* **client:** remove audio_pump remnants, cpal reads Stream directly ([dba9e47](https://github.com/metaneutrons/snapcast-rs/commit/dba9e4733a9f67c5b87fd9d356ad0cec8eabb16b))
* **client:** remove double-counted buffer period in DAC delay ([ebf2049](https://github.com/metaneutrons/snapcast-rs/commit/ebf204967a5cdaf96d62f13ba20ee095e801663f))
* **client:** replace send_request with send+recv to fix deadlock ([b819c52](https://github.com/metaneutrons/snapcast-rs/commit/b819c5288f2e1d2ccdea8e251c9bae049c673d36))
* **client:** set CoreAudio sample rate to match stream format ([d152e1f](https://github.com/metaneutrons/snapcast-rs/commit/d152e1f19a4e4c1517260813931868766fae09fe))
* **client:** single reader architecture — audio_pump is the only Stream reader ([1ae87b4](https://github.com/metaneutrons/snapcast-rs/commit/1ae87b4595350892c35ee5dc34a6b237bd7f84ea))
* **client:** use CLOCK_MONOTONIC for time sync (matching C++ steady_clock) ([0f6a962](https://github.com/metaneutrons/snapcast-rs/commit/0f6a9628f22bf6cb2877ec3f71daa604b54878b4))
* **client:** use mach_continuous_time on macOS for time sync ([55e7132](https://github.com/metaneutrons/snapcast-rs/commit/55e7132e8a91bbfe2ee308faf23191817019a419))
* **client:** use real MAC address instead of dummy zeros ([0dfe866](https://github.com/metaneutrons/snapcast-rs/commit/0dfe866ffb4194ec6cb6061b2686e4549be5996d))
* **client:** working Stream with hard_sync + soft sync ([a57b508](https://github.com/metaneutrons/snapcast-rs/commit/a57b5087602c920c83bb668adcb2b11e81cef35b))
* **control:** return 'Method not found' for unhandled JSON-RPC on TCP ([df79ef0](https://github.com/metaneutrons/snapcast-rs/commit/df79ef059f9714ac4d158e7fa0fae9c9bfde8697))
* **cpal:** use actual DAC delay from cpal timestamps instead of hardcoded 5ms ([3677040](https://github.com/metaneutrons/snapcast-rs/commit/367704028a23d8a599ed2e423e22df2a523c9457))
* cross-platform compilation (Linux, Windows, macOS) ([6416aea](https://github.com/metaneutrons/snapcast-rs/commit/6416aea344f91777468f4de7ba3dce54abe6565d))
* Ctrl-C exits within 1 second on both client and server ([cbde047](https://github.com/metaneutrons/snapcast-rs/commit/cbde0471ed501182cfd460e4d60619ee6cdfaff1))
* **f32lz4:** zero-conversion path for f32 audio input ([3ca3eb2](https://github.com/metaneutrons/snapcast-rs/commit/3ca3eb28a9a26652c9ca74ecc7747d5923d1931f))
* **flac:** generate header by encoding silent frames (flac-codec requires data) ([a1e3be3](https://github.com/metaneutrons/snapcast-rs/commit/a1e3be3f879ee4a24c301657cafc09e9c13aaca2))
* **flac:** raw libflac-sys FFI with write callback — header/frame separation ([56080cb](https://github.com/metaneutrons/snapcast-rs/commit/56080cb398097c8ef7d62c723f647496d4c66487))
* **flac:** use same encoder instance for header and frames ([8c8a544](https://github.com/metaneutrons/snapcast-rs/commit/8c8a544d66c47aa52779f3caadaf65cb71f212a8))
* force exit via OS thread — tokio sleep can't fire when runtime is blocked ([d2a4012](https://github.com/metaneutrons/snapcast-rs/commit/d2a4012a3c3cbb7a55541ec34b043bfecec650c3))
* HIGH audit — crash/security fixes ([a9b6f5c](https://github.com/metaneutrons/snapcast-rs/commit/a9b6f5c6df2fe60b79702f48d67dc8ade0717c7e))
* **http:** add debug log before TcpListener::bind for diagnostics ([882ca13](https://github.com/metaneutrons/snapcast-rs/commit/882ca1307c8474a926d418bba8095c60acb2ba84))
* mDNS hostname .local. suffix + FLAC encoder header with None total_bytes ([63e8a65](https://github.com/metaneutrons/snapcast-rs/commit/63e8a6582fa0ed67190ed217bd22be671c74ea2a))
* **mdns:** clean shutdown (no unregister spam) + hostname fix ([385ae53](https://github.com/metaneutrons/snapcast-rs/commit/385ae5352d4a1cf7a3d989773e8214ff7e54de77))
* **mdns:** drop mDNS before aborting tasks — clean Ctrl-C shutdown ([5fb6a90](https://github.com/metaneutrons/snapcast-rs/commit/5fb6a907043270d59148d47d433e79e28633ee8e))
* **mdns:** graceful shutdown — unregister before stopping daemon ([6d385ea](https://github.com/metaneutrons/snapcast-rs/commit/6d385eaa462f355135220b7a39ff1660ac02a992))
* **mdns:** just shutdown daemon on drop — no unregister attempt ([fadff3e](https://github.com/metaneutrons/snapcast-rs/commit/fadff3e5192bf112adc2884f7ccf85f767ad0f2e))
* **player:** detect format from Stream, handle 32-bit f32lz4 correctly ([2fea112](https://github.com/metaneutrons/snapcast-rs/commit/2fea1123ecedfcf3173a6d2fd6c82d0c747e74d6))
* **player:** start cpal immediately — don't wait for AudioFrame gate ([6ae5c44](https://github.com/metaneutrons/snapcast-rs/commit/6ae5c44aa5fb4e732ea52e263ccdfee41c5779cc))
* remove all allow(dead_code) — zero exemptions, zero TODOs ([587b06a](https://github.com/metaneutrons/snapcast-rs/commit/587b06a97cf8bb504f4cde2a56a7d30455416df3))
* remove all blanket #![allow] and dead code from binary modules ([c57478d](https://github.com/metaneutrons/snapcast-rs/commit/c57478d809175f9928eb093416a92c5b0495e826))
* remove duplicate lz4_flex feature in client Cargo.toml ([72faaf8](https://github.com/metaneutrons/snapcast-rs/commit/72faaf8789f18ac786d63521fdb425a8508b4766))
* remove last allow(dead_code) — unused ManagedStream.name field removed ([7a42595](https://github.com/metaneutrons/snapcast-rs/commit/7a42595ac2a43a80b66cd493b727ba5ae449f688))
* remove unwrap/expect from library code ([8d779e7](https://github.com/metaneutrons/snapcast-rs/commit/8d779e7e8e039bffa7a272a673fe4e5c23d42c01))
* **server:** add f32lz4 to CLI help, replace panic with error propagation ([da71da2](https://github.com/metaneutrons/snapcast-rs/commit/da71da2446f96895cbf1298464baf95bede4a191))
* **server:** deep audit — wire all channels, fix all TODOs ([ca0eccd](https://github.com/metaneutrons/snapcast-rs/commit/ca0eccd70f9292175d3a941b6964cadb9964368e))
* **server:** default codec PCM until FLAC streaming encoder is fixed ([cedca6c](https://github.com/metaneutrons/snapcast-rs/commit/cedca6c8e260d5bc6f292d279cfb3cd7c47a5a2d))
* **server:** eliminate all TODOs — settings push + stream control fully routed ([7d4dd82](https://github.com/metaneutrons/snapcast-rs/commit/7d4dd8277602d975887d9a737cb2f9e0ebfd8b18))
* **server:** generate codec header from config when no streams exist yet ([c3dae89](https://github.com/metaneutrons/snapcast-rs/commit/c3dae89f1343ad60ec6dae0e615f30008f703ae3))
* **server:** match chunk size to FLAC block size (1152 frames) ([d451d47](https://github.com/metaneutrons/snapcast-rs/commit/d451d472947bcdcfaa5ef4e023e1487a4926e1ed))
* **server:** persistent streaming FLAC encoder — keeps state across chunks ([44e96d1](https://github.com/metaneutrons/snapcast-rs/commit/44e96d1cb71821526046d39a77f6042b5c0bd259))
* **server:** push volume/latency changes to streaming clients ([02e4b87](https://github.com/metaneutrons/snapcast-rs/commit/02e4b87c10101a816ca80483635d8ecab7089f2b))
* **server:** sample-count-based chunk timestamps instead of wall clock ([cb7d601](https://github.com/metaneutrons/snapcast-rs/commit/cb7d6011fe5fc743619fdcf68235658ac6d50333))
* **server:** switch FLAC encoder to flac-bound (libflac streaming API) ([4e222dd](https://github.com/metaneutrons/snapcast-rs/commit/4e222ddd39acbe97dd285ea2ec0aed0b62691891))
* **server:** use first-buffered-chunk timestamp for encoded output ([6c88495](https://github.com/metaneutrons/snapcast-rs/commit/6c88495f71e2d99c5db6d9867cd8f59b03d395c1))
* **server:** use monotonic clock for time sync (mach_continuous_time/CLOCK_MONOTONIC) ([0c676de](https://github.com/metaneutrons/snapcast-rs/commit/0c676de1d6672144079786f48c9f4560ba3561ff))
* **test:** encryption tests must explicitly set codec to f32lz4 ([37333bd](https://github.com/metaneutrons/snapcast-rs/commit/37333bd390437f49ef8ab6d69922ec78222d90a2))
* workspace snapcast-client dep default-features=false for cross-platform ([6c94c2a](https://github.com/metaneutrons/snapcast-rs/commit/6c94c2a99ebf21b7f6a517b9d119540ca8b04d62))
