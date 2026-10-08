# syntax=docker/dockerfile:1
#
# Cross-compilation image for snapcast-rs release binaries.
#
# Everything runs in an x86_64 Arch Linux container; aarch64 binaries are
# cross-compiled (no qemu needed). Toolchains and the cross-built alsa-lib are
# baked into the image; `cargo build` happens at run time (see build-in-container.sh).
#
# Targets:
#   x86_64-unknown-linux-gnu    gcc (host)                 + Arch alsa-lib (shared)
#   aarch64-unknown-linux-gnu   aarch64-linux-gnu-gcc      + alsa-lib from source (shared)
#   x86_64-unknown-linux-musl   zig cc -target x86_64-linux-musl  + alsa-lib from source (static)
#   aarch64-unknown-linux-musl  zig cc -target aarch64-linux-musl + alsa-lib from source (static)
# musl binaries are linked by rust-lld against Rust's self-contained musl (crt-static).

ARG BASE_IMAGE=archlinux:base-devel
FROM ${BASE_IMAGE}

# ---------------------------------------------------------------------------
# 1. Toolchains: all from the official (signed) Arch repositories.
# ---------------------------------------------------------------------------
RUN pacman -Syu --noconfirm --needed \
        rust rust-musl rust-aarch64-gnu rust-aarch64-musl \
        aarch64-linux-gnu-gcc \
        zig \
        cmake ninja pkgconf \
        alsa-lib \
        file gnupg \
    && pacman -Scc --noconfirm \
    && rm -rf /var/cache/pacman/pkg/*

# zig needs a writable cache; at run time build-in-container.sh points these at
# the (host-owned) cargo target dir. During the image build root's /tmp is fine.
ENV ZIG_GLOBAL_CACHE_DIR=/tmp/zig-global-cache \
    ZIG_LOCAL_CACHE_DIR=/tmp/zig-local-cache

# zig cc wrappers (+ ar/ranlib) and per-target CMake toolchain files.
COPY toolchain/ /opt/toolchain/
RUN chmod 0755 /opt/toolchain/bin/* \
    && for t in x86_64-linux-musl aarch64-linux-musl; do \
         /opt/toolchain/bin/$t-cc --version >/dev/null; \
       done

# ---------------------------------------------------------------------------
# 2. alsa-lib built from source for the non-native targets.
#    Pinned version, verified against a pinned SHA-512 *and* the ALSA release
#    team's GPG signature (key bundled in this directory, fingerprint pinned).
#    --prefix=/usr so the compiled-in config path (/usr/share/alsa) matches the
#    target system; installed into per-target staging sysroots via DESTDIR.
# ---------------------------------------------------------------------------
ARG ALSA_VERSION=1.2.16.1
ARG ALSA_SHA512=2ddc5eef7fafe73ba6d8e4f79640a5aaa6e27aac2c77075fda9ee8ae61727bddfcda661e257057d8cf2f1647897d33153703f9ba913574ae7f1fbec53db3fa22
ARG ALSA_GPG_FPR=F04DF50737AC1A884C4B3D718380596DA6E59C91
COPY alsa-release-key.asc /tmp/alsa/alsa-release-key.asc
RUN set -eux; \
    cd /tmp/alsa; \
    url="https://www.alsa-project.org/files/pub/lib/alsa-lib-${ALSA_VERSION}.tar.bz2"; \
    curl -fsSLo src.tar.bz2 "$url"; \
    curl -fsSLo src.tar.bz2.sig "$url.sig"; \
    echo "${ALSA_SHA512}  src.tar.bz2" | sha512sum -c -; \
    export GNUPGHOME="$(mktemp -d)"; \
    gpg --batch --import alsa-release-key.asc; \
    gpg --batch --list-keys --with-colons "${ALSA_GPG_FPR}" | grep -q "^fpr:::::::::${ALSA_GPG_FPR}:"; \
    gpg --batch --status-fd 1 --verify src.tar.bz2.sig src.tar.bz2 | grep -q "VALIDSIG ${ALSA_GPG_FPR} "; \
    rm -rf "$GNUPGHOME"; \
    tar xjf src.tar.bz2; \
    build() { \
        triple=$1; sysroot=$2; shift 2; \
        rm -rf "build-$triple"; mkdir "build-$triple"; cd "build-$triple"; \
        ../alsa-lib-${ALSA_VERSION}/configure --host="$triple" --prefix=/usr \
            --disable-python --disable-topology --disable-alisp --without-debug \
            "$@"; \
        make -j"$(nproc)"; \
        make DESTDIR="$sysroot" install; \
        cd ..; \
    }; \
    CC=aarch64-linux-gnu-gcc AR=aarch64-linux-gnu-ar RANLIB=aarch64-linux-gnu-ranlib \
        build aarch64-linux-gnu /opt/sysroot/aarch64-unknown-linux-gnu \
        --enable-shared --disable-static; \
    for arch in x86_64 aarch64; do \
        CC=/opt/toolchain/bin/$arch-linux-musl-cc \
        AR=/opt/toolchain/bin/$arch-linux-musl-ar \
        RANLIB=/opt/toolchain/bin/$arch-linux-musl-ranlib \
        CFLAGS="-O2 -fPIE" \
            build $arch-linux-musl /opt/sysroot/$arch-unknown-linux-musl \
            --enable-static --disable-shared; \
    done; \
    rm -rf /tmp/alsa /tmp/zig-*; \
    for s in /opt/sysroot/*; do \
        test -f "$s/usr/lib/pkgconfig/alsa.pc"; \
        rm -rf "$s/usr/bin" "$s/usr/share/man"; \
    done

# ---------------------------------------------------------------------------
# 3. Run-time layout. CARGO_HOME is a named volume (registry cache); the image
#    directory is world-writable so a non-root --user can populate it.
# ---------------------------------------------------------------------------
ENV CARGO_HOME=/cargo \
    CARGO_TERM_COLOR=always
RUN mkdir -p /cargo /home/builder && chmod 1777 /cargo /home/builder

COPY build-in-container.sh /usr/local/bin/build-in-container.sh
RUN chmod 0755 /usr/local/bin/build-in-container.sh

WORKDIR /src
ENTRYPOINT ["/usr/local/bin/build-in-container.sh"]
