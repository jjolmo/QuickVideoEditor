#!/bin/sh
# Builds and installs Quick Video Editor into $1 (a DESTDIR), for distribution packages.
set -eu
destdir=$(realpath -m "$1")

if ! command -v cargo >/dev/null || ! cargo --version >/dev/null 2>&1; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    . "$HOME/.cargo/env"
fi

meson setup build-release --prefix=/usr --buildtype=release
meson compile -C build-release
DESTDIR="$destdir" meson install -C build-release
