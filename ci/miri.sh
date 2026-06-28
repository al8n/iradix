#!/bin/bash
set -e

rustup toolchain install nightly --component miri
rustup override set nightly
cargo miri setup

export MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-disable-isolation -Zmiri-symbolic-alignment-check"
# Keep proptest cheap under Miri (it interprets every case).
export PROPTEST_CASES="${PROPTEST_CASES:-16}"

# `alloc` is mandatory, so force it into every --each-feature combination.
cargo hack miri test --each-feature --features alloc

