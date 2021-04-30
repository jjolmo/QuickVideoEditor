#!/bin/sh

export MESON_BUILD_ROOT="$1"
export MESON_SOURCE_ROOT="$2"
export CARGO_TARGET_DIR="$MESON_BUILD_ROOT"/target
export CARGO_HOME="$MESON_BUILD_ROOT"/cargo-home
export OUTPUT="$3"
export APP_ID_SUFFIX="$4"
export APP_BIN="$5"

if [[ $APP_ID_SUFFIX = "Devel" ]]
then
    echo "DEBUG MODE"
    cargo build --manifest-path \
        "$MESON_SOURCE_ROOT"/Cargo.toml && \
        cp "$CARGO_TARGET_DIR"/debug/"$APP_BIN" "$OUTPUT"
else
    echo "RELEASE MODE"
    cargo build --manifest-path \
        "$MESON_SOURCE_ROOT"/Cargo.toml --release && \
        cp "$CARGO_TARGET_DIR"/release/"$APP_BIN" "$OUTPUT"
fi

