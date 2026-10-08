# Docker release build

Builds the release binaries for four Linux targets in one x86_64 Arch Linux
container. The two aarch64 targets are cross-compiled, so qemu/binfmt is not
needed.

| binary | x86_64-unknown-linux-gnu | x86_64-unknown-linux-musl | aarch64-unknown-linux-gnu | aarch64-unknown-linux-musl |
|---|---|---|---|---|
| `snapserver-rs` (`--features opus`) | yes | yes | yes | yes |
| `snapclient-rs` (default features) | yes | yes | yes | yes |

Each output file is named `<bin>-<target>`. A `SHA256SUMS` file is written
next to them.

## Prerequisites

- Docker. Tested with Docker 29 on an x86_64 host. The build does not need BuildKit features beyond the defaults.
- Network access during the first image build (Arch packages, the alsa-lib tarball) and during the first cargo build (crates.io). After that the registry is cached.
- About 5 GB of disk for the image. `target/docker/` grows to a few GB.

## Usage

```sh
docker-build/build.sh                       # all 4 targets x 2 binaries -> dist/
docker-build/build.sh --targets x86_64-unknown-linux-musl,aarch64-unknown-linux-musl
docker-build/build.sh --bins snapclient-rs --out /tmp/snap
docker-build/build.sh --no-image-build      # reuse the existing snapcast-rs-build image
docker-build/build.sh --pull                # refresh the Arch base image (rolling release)
```

What `build.sh` does:

1. Runs `docker build -t snapcast-rs-build:latest docker-build/`. The build context is only `docker-build/`, so the repo is never sent to the daemon and no root `.dockerignore` is needed.
2. Runs the container as **your uid:gid**, with these mounts:
   - `<repo>` → `/src`, **read-only**.
   - `<repo>/target/docker` → `/build`. This is `CARGO_TARGET_DIR`. The host's `target/{debug,release}` is never touched.
   - the output dir (default `<repo>/dist`) → `/out`.
   - the named volume `snapcast-rs-cargo` → `/cargo`. This is `CARGO_HOME`, so the registry and git cache persist between runs.
3. Inside the container, `build-in-container.sh` runs these commands for each target:
   ```
   cargo build --locked --release --target <t> -p snapserver-rs --features opus
   cargo build --locked --release --target <t> -p snapclient-rs
   ```
   It then copies the binaries to `/out`, verifies them (see below) and writes `SHA256SUMS`.

Every file written to the repo is owned by you. `dist/` gets its own `.gitignore` containing `*`, so the output never shows up in `git status`.

Environment overrides for `build.sh`:

- `BASE_IMAGE`: for example `archlinux:base-devel@sha256:…`, to pin the base image.
- `CARGO_VOLUME`: the name of the registry volume.
- `TARGET_DIR`: the cargo target dir on the host.

To drop the registry cache, run `docker volume rm snapcast-rs-cargo`.

A full build from scratch on a 2026 desktop CPU took about 2.5 min for the image and about 7.5 min for all 8 cargo builds.

## What each binary links against

Each verification step prints `file`, the `NEEDED` libraries (`readelf -d`) and the glibc symbol versions (`readelf -V`). For the musl binaries it also checks that there is no `PT_INTERP` and no `NEEDED`. It runs `--version` for the x86_64 binaries. The step fails the build if a check fails.

| binary | linkage | NEEDED | glibc |
|---|---|---|---|
| snapserver-rs-x86_64-unknown-linux-gnu | dynamic, PIE | libgcc_s.so.1 libm.so.6 libc.so.6 | needs ≥ 2.34 (2.39 weak, see below) |
| snapclient-rs-x86_64-unknown-linux-gnu | dynamic, PIE | **libasound.so.2** libgcc_s.so.1 libm.so.6 libc.so.6 | needs ≥ 2.34 |
| snapserver-rs-aarch64-unknown-linux-gnu | dynamic, PIE | libgcc_s.so.1 libm.so.6 libc.so.6 ld-linux-aarch64.so.1 | needs ≥ 2.34 (2.39 weak) |
| snapclient-rs-aarch64-unknown-linux-gnu | dynamic, PIE | **libasound.so.2** libgcc_s.so.1 libm.so.6 libc.so.6 | needs ≥ 2.34 |
| snapserver-rs-x86_64-unknown-linux-musl | static-pie | none | none |
| snapclient-rs-x86_64-unknown-linux-musl | static-pie | none (libasound statically linked) | none |
| snapserver-rs-aarch64-unknown-linux-musl | static | none | none |
| snapclient-rs-aarch64-unknown-linux-musl | static | none (libasound statically linked) | none |

libopus 1.6.1 is always compiled from source by `opusic-sys` (cmake) and linked statically. No binary depends on `libopus.so`.

### Minimum glibc (gnu builds)

The gnu binaries need **glibc ≥ 2.34**. Examples: Debian 12, Ubuntu 22.04, RHEL/Alma/Rocky 9, Fedora 35, Raspberry Pi OS bookworm. They are built against Arch's glibc (2.44 at the time of writing), but Rust's std only references symbols up to GLIBC_2.34, with one exception.

The exception is that the server also references `pidfd_spawnp` and `pidfd_getpid` at version GLIBC_2.39. std uses them optionally for `Command`, as **weak** symbols. The linker is `rust-lld` on both architectures. lld marks the GLIBC_2.39 version need as `WEAK`, so the loader does not refuse to start on an older glibc. The verify step prints the result as `minimum glibc, non-weak`.

On glibc 2.34–2.38 the dynamic loader prints this harmless line on stderr at every start:

```
snapserver-rs: /lib/x86_64-linux-gnu/libc.so.6: weak version `GLIBC_2.39' not found (required by snapserver-rs)
```

This was tested with the x86_64 gnu server and client in an `ubuntu:22.04` container (glibc 2.35). Both run. For glibc < 2.34, for example Debian 11 or Ubuntu 20.04, use the musl builds.

GNU ld (`aarch64-linux-gnu-gcc` without `-fuse-ld=lld`) does *not* set the WEAK flag. A server linked with GNU ld would hard-require glibc 2.39. That is why the aarch64 gnu target links through rust-lld.

## musl caveats (client)

The musl client contains a **static libasound**, and a static musl binary cannot `dlopen()`. So ALSA's external plugins cannot load. That includes the PipeWire plugin (`libasound_module_pcm_pipewire.so`), the PulseAudio plugin, the JACK plugin and others. The built-in PCM types do work: `hw`, `plughw`, `plug`, `dmix`, `dsnoop`, `softvol`, `asym`, `route` and so on.

The client always opens the ALSA **`default`** device. What `default` does depends on the target system's ALSA config:

- **Plain ALSA systems**, for example headless Raspberry Pi OS Lite or a minimal Debian/Alpine without pipewire-alsa or pulseaudio-alsa: `default` comes from `/usr/share/alsa/alsa.conf` and is `plug`/`dmix` on card 0. The musl client works.
- **Desktop systems with pipewire-alsa or pulseaudio-alsa installed**: these install `/etc/alsa/conf.d/99-pipewire-default.conf` (or the pulse equivalent), which sets `pcm.!default` to the PipeWire/Pulse plugin. The musl client then fails to open the device and retries every second. Tested on an Arch host with PipeWire:
  ```
  ALSA lib .../src/dlmisc.c:342:(snd_dlobj_cache_get0) [error.core] Cannot open shared library libasound_module_pcm_pipewire.so (Dynamic loading not supported)
  ERROR snapclient_rs::player: Audio output failed, retrying in 1s error=ALSA function 'snd_pcm_open' failed with error 'No such device or address (6)'
  ```
  It does **not** fall back to `hw:`. To work around it, override `default` in `~/.asoundrc` (or `/etc/asound.conf`), for example:
  ```
  pcm.!default { type plug slave.pcm "dmix" }
  ctl.!default { type hw card 0 }
  ```
  This was tested and works. Audio then bypasses PipeWire. The device has to be free, or PipeWire has to have suspended it.

**Recommendation:** use the **gnu** client on desktops with PipeWire or PulseAudio. It loads the system's libasound and its plugins. Use the musl client on headless or embedded boxes, or on systems with an old or unusual libc.

The static libasound is alsa-lib 1.2.16.1, built with `--prefix=/usr`. It reads the target's `/usr/share/alsa/alsa.conf`, `/etc/asound.conf` and `~/.asoundrc`. Configs from older alsa-lib releases are normally fine.

The server has no ALSA or plugin dependency, so the musl server has none of these caveats.

## How the toolchain is set up

All compilers and Rust targets come from the official, signed Arch repositories. Nothing is installed with rustup. The only download outside pacman is the verified alsa-lib tarball.

| piece | source |
|---|---|
| Rust + std for 4 targets | Arch `rust` (x86_64-gnu), `rust-musl`, `rust-aarch64-gnu`, `rust-aarch64-musl` |
| x86_64 gnu C compiler | Arch `gcc` (base-devel) |
| aarch64 gnu C compiler/sysroot | Arch `aarch64-linux-gnu-gcc` (pulls in `aarch64-linux-gnu-glibc`, binutils) |
| musl C compilers (both arches) | Arch `zig`, used as `zig cc -target <arch>-linux-musl` |
| linker, gnu x86_64 | rustc default (gcc driver + rust-lld) |
| linker, gnu aarch64 | `aarch64-linux-gnu-gcc` + `-B<rustlib>/gcc-ld -fuse-ld=lld` (rust-lld) |
| linker, musl | `rust-lld` directly, `-C target-feature=+crt-static -C link-self-contained=yes` (Rust's bundled musl `crt1.o`/`libc.a`/`libunwind.a`) |
| cmake/ninja/pkgconf | Arch packages |
| alsa-lib, x86_64 gnu | Arch `alsa-lib` (shared) |
| alsa-lib, other 3 targets | built from source in the image (see below) |

**Why zig for musl.** There is no aarch64 musl cross compiler in the Arch repos. The x86_64 `musl-gcc` wrapper from the `musl` package only covers x86_64, and it pairs Arch's musl headers with whatever Rust bundles. zig is an official, signed Arch package. It ships musl headers and a clang for every target, so one tool covers both arches and C/C++. This avoids downloading and trusting a third-party musl-cross tarball. zig is used only to *compile* C (libopus via cmake, alsa-lib via autotools). The final link is done by rust-lld against Rust's own self-contained musl, so the binary contains exactly one libc.

`toolchain/bin/zig-cc-wrapper` is reached through the symlinks `<arch>-linux-musl-{cc,c++,ar,ranlib}`. It does three things:

- It removes the clang-style `--target=<rust triple>` that the `cc` crate adds, because zig gets `-target` from the wrapper name.
- It adds `-fno-sanitize=all`. zig enables UBSan by default when no `-O` flag is given, and the zig ubsan runtime is not linked by rustc.
- It maps `ar`/`ranlib` to `zig ar`/`zig ranlib`.

**C dependencies of the Rust crates.**

- `opusic-sys` (server, `--features opus`) builds libopus with the `cmake` crate. Per target, `build-in-container.sh` exports `CC_<triple>`, `CXX_<triple>`, `AR_<triple>` and `CMAKE_TOOLCHAIN_FILE_<triple>`. The toolchain files are in `toolchain/cmake/`. They set the system name and processor, the compilers, `CMAKE_AR`/`CMAKE_RANLIB` and the find-root paths. The native x86_64 gnu target uses the system gcc and needs no toolchain file.
- `alsa-sys` (client) uses pkg-config. For cross targets the script exports these variables, scoped by target:
  - `PKG_CONFIG_ALLOW_CROSS_<triple>=1`
  - `PKG_CONFIG_SYSROOT_DIR_<triple>=/opt/sysroot/<target>`
  - `PKG_CONFIG_LIBDIR_<triple>=/opt/sysroot/<target>/usr/lib/pkgconfig`

  musl targets also get `PKG_CONFIG_ALL_STATIC=1`. Their sysroots contain only `libasound.a`.

All of these variables are scoped by target triple. Host build scripts and proc-macros still build with the native toolchain.

**alsa-lib from source** (Dockerfile step 2):

- Version `ALSA_VERSION=1.2.16.1`.
- The tarball is checked against a pinned `ALSA_SHA512`, which is the value Arch's PKGBUILD uses.
- Its detached `.sig` is checked against the ALSA Release Team key (`alsa-release-key.asc`, bundled here). The build requires a `VALIDSIG` for the pinned fingerprint `F04DF50737AC1A884C4B3D718380596DA6E59C91`.
- Configure options: `--prefix=/usr --disable-python --disable-topology --disable-alisp --without-debug`. The install goes to `DESTDIR=/opt/sysroot/<rust-target>`.
- aarch64 gnu: built shared (`--enable-shared --disable-static`) with `aarch64-linux-gnu-gcc`. It is used only for linking. At runtime the target's own `libasound.so.2` is used.
- x86_64/aarch64 musl: built static (`--enable-static --disable-shared`) with zig cc and `CFLAGS="-O2 -fPIE"`. PIE is needed for the x86_64 static-pie link.

## Bumping pinned versions

- **Rust, compilers, zig, cmake:** these follow the Arch repos. Rebuild with `build.sh --pull` to get the current packages. For a reproducible image, pin the base image by digest, for example `BASE_IMAGE=archlinux:base-devel@sha256:<digest> docker-build/build.sh`. Note that `pacman -Syu` in the Dockerfile still upgrades to the repo state at build time. Full reproducibility needs an Arch Linux Archive snapshot mirror, for example a `Server = https://archive.archlinux.org/repos/YYYY/MM/DD/$repo/os/$arch` line in `/etc/pacman.d/mirrorlist` before the `pacman` step. The workspace's `rust-version` (currently 1.94.1) must be ≤ Arch's `rust`.
- **alsa-lib:** edit `ALSA_VERSION` and `ALSA_SHA512` in the Dockerfile. Take the sha512 from Arch's PKGBUILD (`https://gitlab.archlinux.org/archlinux/packaging/packages/alsa-lib/-/raw/main/PKGBUILD`) or compute it yourself after checking the signature. The GPG check fails if the release is signed by a different key. In that case, review the new key, replace `alsa-release-key.asc` (for example from `https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x<FPR>`) and update `ALSA_GPG_FPR`.
- **libopus:** comes from the `opusic-sys` crate version in `Cargo.lock`. Bump it with `cargo update -p opusic-sys`. Nothing in `docker-build/` needs to change.
- **Adding a target:** add a `case` branch in `target_env()` in `build-in-container.sh`, install the Arch `rust-<target>` package, and, if the client needs it, an alsa-lib build in the Dockerfile plus a cmake toolchain file.

## Files

| file | purpose |
|---|---|
| `build.sh` | host entry point: builds the image, runs the container |
| `Dockerfile` | Arch image: toolchains, zig wrappers, cross-built alsa-lib sysroots |
| `build-in-container.sh` | container entry point: cargo builds, verification, SHA256SUMS |
| `toolchain/bin/zig-cc-wrapper` (+ symlinks) | musl C compiler/ar/ranlib wrappers around zig |
| `toolchain/cmake/*.cmake` | CMake toolchain files for the cross targets (used by opusic-sys) |
| `alsa-release-key.asc` | ALSA Release Team public key, used to verify the alsa-lib tarball |
