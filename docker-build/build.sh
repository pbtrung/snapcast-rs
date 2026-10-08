#!/usr/bin/env bash
# Build snapcast-rs release binaries in Docker (Arch Linux, cross-compiled).
#
#   docker-build/build.sh [--targets t1,t2,...] [--bins b1,b2] [--out DIR]
#                         [--image NAME] [--no-image-build] [--pull]
#
# Defaults: all four targets, both binaries, output in <repo>/dist.
# Cargo artifacts go to <repo>/target/docker (never the host's target/release),
# the cargo registry is cached in the docker volume "snapcast-rs-cargo".
# Env overrides: BASE_IMAGE (e.g. archlinux:base-devel@sha256:...),
# CARGO_VOLUME, TARGET_DIR.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/.." && pwd)

IMAGE=snapcast-rs-build:latest
OUT=$REPO/dist
TARGETS=x86_64-unknown-linux-gnu,x86_64-unknown-linux-musl,aarch64-unknown-linux-gnu,aarch64-unknown-linux-musl
BINS=snapserver-rs,snapclient-rs
BUILD_IMAGE=1
PULL=()
CARGO_VOLUME=${CARGO_VOLUME:-snapcast-rs-cargo}
TARGET_DIR=${TARGET_DIR:-$REPO/target/docker}

usage() { sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --targets) TARGETS=$2; shift 2 ;;
        --targets=*) TARGETS=${1#*=}; shift ;;
        --bins) BINS=$2; shift 2 ;;
        --bins=*) BINS=${1#*=}; shift ;;
        --out) OUT=$2; shift 2 ;;
        --out=*) OUT=${1#*=}; shift ;;
        --image) IMAGE=$2; shift 2 ;;
        --image=*) IMAGE=${1#*=}; shift ;;
        --no-image-build) BUILD_IMAGE=0; shift ;;
        --pull) PULL=(--pull); shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

mkdir -p "$OUT" "$TARGET_DIR"
OUT=$(cd "$OUT" && pwd)
TARGET_DIR=$(cd "$TARGET_DIR" && pwd)
# Keep build output out of `git status` without touching the repo's .gitignore.
if [ "$OUT" = "$REPO/dist" ] && [ ! -e "$OUT/.gitignore" ]; then
    printf '*\n' > "$OUT/.gitignore"
fi

if [ "$BUILD_IMAGE" = 1 ]; then
    echo "==> building image $IMAGE"
    BUILD_ARGS=()
    [ -n "${BASE_IMAGE:-}" ] && BUILD_ARGS+=(--build-arg "BASE_IMAGE=$BASE_IMAGE")
    docker build "${PULL[@]}" "${BUILD_ARGS[@]}" -t "$IMAGE" "$HERE"
fi

TTY=()
[ -t 1 ] && TTY=(-t)

start=$(date +%s)
echo "==> running build in container (uid $(id -u):$(id -g))"
docker run --rm "${TTY[@]}" \
    --user "$(id -u):$(id -g)" \
    -e HOME=/home/builder \
    -v "$REPO:/src:ro" \
    -v "$TARGET_DIR:/build" \
    -v "$OUT:/out" \
    -v "$CARGO_VOLUME:/cargo" \
    "$IMAGE" --targets "$TARGETS" --bins "$BINS"
echo "==> total container time: $(( $(date +%s) - start ))s; binaries in $OUT"
