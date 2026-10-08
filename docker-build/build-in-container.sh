#!/usr/bin/env bash
# Runs inside the snapcast-rs build image (see Dockerfile / build.sh).
#
#   build-in-container.sh [--targets t1,t2,...] [--bins snapserver-rs,snapclient-rs]
#
# Expects:
#   /src    the workspace (may be read-only)
#   /build  CARGO_TARGET_DIR (writable, persistent between runs)
#   /out    output directory for <bin>-<target> + SHA256SUMS
#   /cargo  CARGO_HOME (registry cache volume)
set -euo pipefail

ALL_TARGETS="x86_64-unknown-linux-gnu,x86_64-unknown-linux-musl,aarch64-unknown-linux-gnu,aarch64-unknown-linux-musl"
TARGETS=$ALL_TARGETS
BINS="snapserver-rs,snapclient-rs"
SRC=${SRC:-/src}
OUT=${OUT:-/out}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/build}

while [ $# -gt 0 ]; do
    case "$1" in
        --targets) TARGETS=$2; shift 2 ;;
        --targets=*) TARGETS=${1#*=}; shift ;;
        --bins) BINS=$2; shift 2 ;;
        --bins=*) BINS=${1#*=}; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
IFS=, read -r -a TARGETS <<<"$TARGETS"
IFS=, read -r -a BINS <<<"$BINS"

# zig and anything else wanting a cache must not write to / as a non-root user.
export HOME=${HOME:-/home/builder}
[ -w "$HOME" ] || export HOME=/tmp
export ZIG_GLOBAL_CACHE_DIR=$CARGO_TARGET_DIR/zig-cache/global
export ZIG_LOCAL_CACHE_DIR=$CARGO_TARGET_DIR/zig-cache/local
mkdir -p "$OUT" "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR"

log() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }

# Per-target environment. Everything is scoped by target name (CC_<triple>,
# CARGO_TARGET_<TRIPLE>_*), so build scripts compiled for the host keep using
# the native gcc.
target_env() {
    local t=$1 u=${1//-/_} U
    U=$(tr '[:lower:]' '[:upper:]' <<<"$u")
    local sysroot=/opt/sysroot/$t
    case "$t" in
        x86_64-unknown-linux-gnu)
            # Native: system gcc, Arch's alsa-lib (/usr/lib/pkgconfig/alsa.pc).
            ;;
        aarch64-unknown-linux-gnu)
            export "CARGO_TARGET_${U}_LINKER=aarch64-linux-gnu-gcc"
            # Link through rust-lld (as rustc already does by default on
            # x86_64-unknown-linux-gnu). Unlike GNU ld, lld marks the
            # GLIBC_2.39 version need as WEAK when it is only referenced by
            # weak symbols (std's optional pidfd_spawnp/pidfd_getpid), so the
            # binary still loads on glibc 2.34..2.38.
            export "CARGO_TARGET_${U}_RUSTFLAGS=-C link-arg=-B$RUST_GCC_LD -C link-arg=-fuse-ld=lld"
            export "CC_${u}=aarch64-linux-gnu-gcc" "CXX_${u}=aarch64-linux-gnu-g++"
            export "AR_${u}=aarch64-linux-gnu-ar"
            export "CMAKE_TOOLCHAIN_FILE_${u}=/opt/toolchain/cmake/$t.cmake"
            export "PKG_CONFIG_ALLOW_CROSS_${u}=1"
            export "PKG_CONFIG_SYSROOT_DIR_${u}=$sysroot"
            export "PKG_CONFIG_LIBDIR_${u}=$sysroot/usr/lib/pkgconfig"
            ;;
        x86_64-unknown-linux-musl|aarch64-unknown-linux-musl)
            local zt=${t%%-*}-linux-musl
            # Link with rust-lld against Rust's self-contained musl (crt1.o,
            # libc.a, libunwind.a shipped by rust-musl / rust-aarch64-musl).
            export "CARGO_TARGET_${U}_LINKER=rust-lld"
            export "CARGO_TARGET_${U}_RUSTFLAGS=-C target-feature=+crt-static -C link-self-contained=yes"
            export "CC_${u}=/opt/toolchain/bin/$zt-cc" "CXX_${u}=/opt/toolchain/bin/$zt-c++"
            export "AR_${u}=/opt/toolchain/bin/$zt-ar"
            export "CMAKE_TOOLCHAIN_FILE_${u}=/opt/toolchain/cmake/$t.cmake"
            export "PKG_CONFIG_ALLOW_CROSS_${u}=1"
            export "PKG_CONFIG_ALL_STATIC_${u}=1" PKG_CONFIG_ALL_STATIC=1
            export "PKG_CONFIG_SYSROOT_DIR_${u}=$sysroot"
            export "PKG_CONFIG_LIBDIR_${u}=$sysroot/usr/lib/pkgconfig"
            ;;
        *) echo "unsupported target: $t" >&2; exit 2 ;;
    esac
}

cargo_args_for() {
    case "$1" in
        snapserver-rs) echo "-p snapserver-rs --features opus" ;;
        snapclient-rs) echo "-p snapclient-rs" ;;
        *) echo "unknown binary: $1" >&2; exit 2 ;;
    esac
}

log "toolchain"
rustc --version
cargo --version
RUST_GCC_LD="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/gcc-ld"
test -x "$RUST_GCC_LD/ld.lld" || { echo "rust-lld wrapper not found in $RUST_GCC_LD" >&2; exit 1; }
for t in "${TARGETS[@]}"; do
    test -d "$(rustc --print sysroot)/lib/rustlib/$t" || { echo "rust std for $t not installed" >&2; exit 1; }
done

start=$(date +%s)
cd "$SRC"
for t in "${TARGETS[@]}"; do
    for b in "${BINS[@]}"; do
        log "build $b for $t"
        # shellcheck disable=SC2046
        ( target_env "$t"; cargo build --locked --release --target "$t" $(cargo_args_for "$b") )
        install -m 0755 "$CARGO_TARGET_DIR/$t/release/$b" "$OUT/$b-$t"
    done
done
build_secs=$(( $(date +%s) - start ))

# ---------------------------------------------------------------------------
# Verification
# ---------------------------------------------------------------------------
log "verify"
fail=0
for t in "${TARGETS[@]}"; do
    for b in "${BINS[@]}"; do
        f="$OUT/$b-$t"
        echo
        echo "--- $b-$t ($(du -h "$f" | cut -f1))"
        file -b "$f" | sed 's/^/  file:   /'
        needed=$(readelf -d "$f" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | tr '\n' ' ')
        echo "  NEEDED: ${needed:-<none>}"
        case "$t" in
            *-gnu)
                glibc=$(readelf -V "$f" | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -n1)
                # Version needs flagged WEAK only back weak symbols: the loader
                # tolerates their absence, so they do not raise the minimum.
                glibc_req=$(readelf -V "$f" | awk '$2=="Name:" && $3 ~ /^GLIBC_/ && $5=="none" {print $3}' | sort -uV | tail -n1)
                echo "  max glibc symbol version: ${glibc:-?} (minimum glibc, non-weak: ${glibc_req:-?})"
                if [ "$b" = snapclient-rs ] && [[ "$needed" != *libasound.so.2* ]]; then
                    echo "  ERROR: client is not linked against libasound.so.2"; fail=1
                fi
                if [[ "$needed" == *libopus* ]]; then
                    echo "  ERROR: libopus should be bundled (static)"; fail=1
                fi
                ;;
            *-musl)
                if [ -n "$needed" ] || readelf -l "$f" | grep -q INTERP; then
                    echo "  ERROR: musl binary is not fully static"; fail=1
                else
                    echo "  static: yes (no PT_INTERP, no NEEDED)"
                fi
                ;;
        esac
        if [[ "$t" == x86_64-* ]]; then
            if v=$("$f" --version 2>&1); then
                echo "  --version: $v"
            else
                echo "  ERROR: --version failed: $v"; fail=1
            fi
        fi
    done
done

log "SHA256SUMS"
(
    cd "$OUT"
    files=()
    for f in snapserver-rs-* snapclient-rs-*; do [ -f "$f" ] && files+=("$f"); done
    sha256sum "${files[@]}" > SHA256SUMS
    cat SHA256SUMS
)

log "done in ${build_secs}s (build), output in $OUT"
exit $fail
