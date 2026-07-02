#!/bin/bash
set -ex

export ASAN_OPTIONS="detect_odr_violation=0 detect_leaks=0"

# The deep-chain recursion-depth stress tests spawn giant-stack helper threads,
# which sanitizer runtimes cannot reliably host (their shadow mappings do not
# cover such stacks); the tests skip themselves when this is set. They prove
# walk iterativeness, not thread safety, so the sanitizers lose no coverage.
export IRADIX_SANITIZER=1

TARGET="x86_64-unknown-linux-gnu"

# Run address sanitizer
RUSTFLAGS="-Z sanitizer=address" \
cargo test --tests --target "$TARGET" --all-features

# Run leak sanitizer
RUSTFLAGS="-Z sanitizer=leak" \
cargo test --tests --target "$TARGET" --all-features

# Run memory sanitizer (requires -Zbuild-std for instrumented std)
RUSTFLAGS="-Z sanitizer=memory" \
cargo -Zbuild-std test --tests --target "$TARGET" --all-features

# Run thread sanitizer (requires -Zbuild-std for instrumented std)
RUSTFLAGS="-Z sanitizer=thread" \
cargo -Zbuild-std test --tests --target "$TARGET" --all-features
