# GC United — a garbage collection course in Rust.
#
#   just run 1      work module 1: tests, then the benchmark if they pass
#   just list       what the modules are
#   just test-all   run every module's tests

_default:
    @just list

# What the modules are, and how far along each one is.
list:
    #!/usr/bin/env bash
    set -uo pipefail
    printf "\n  GC United\n\n"
    printf "  %-3s %-22s %s\n" "#" "module" "paradigm"
    printf "  %s\n" "$(printf '%.0s-' {1..74})"
    printf "  %-3s %-22s %s\n" 1 "gc-mod01" "bump allocation and free lists"
    printf "  %-3s %-22s %s\n" 2 "gc-mod02" "reference counting"
    printf "  %-3s %-22s %s\n" 3 "gc-mod03" "cycle collection by trial deletion"
    printf "  %-3s %-22s %s\n" 4 "gc-mod04" "mark and sweep"
    printf "  %-3s %-22s %s\n" 5 "gc-mod05" "mark-compact"
    printf "  %-3s %-22s %s\n" 6 "gc-mod06" "semispace copying"
    printf "  %-3s %-22s %s\n" 7 "gc-mod07" "generational collection"
    printf "  %-3s %-22s %s\n" 8 "gc-mod08" "incremental tri-colour marking"
    printf "\n  just run N   to work on one\n\n"

# Work module N: run its tests, then its benchmark if they all pass.
run N: (test N)
    @just bench {{N}}

# Run module N's tests.
test N:
    #!/usr/bin/env bash
    set -uo pipefail
    crate=$(printf "gc-mod%02d" {{N}})
    if ! command -v cargo-nextest >/dev/null 2>&1; then
        echo "cargo-nextest is required: cargo install cargo-nextest" >&2
        exit 127
    fi
    printf "\n  module %s — %s\n\n" {{N}} "$crate"
    cargo nextest run -p "$crate" --no-fail-fast --status-level all

# Run module N's benchmark. Only meaningful once its tests pass.
bench N:
    #!/usr/bin/env bash
    set -uo pipefail
    crate=$(printf "gc-mod%02d" {{N}})
    cargo run --release -q -p "$crate" --bin "bench-mod$(printf '%02d' {{N}})"

# Read module N's brief in your pager.
brief N:
    #!/usr/bin/env bash
    set -uo pipefail
    crate=$(printf "gc-mod%02d" {{N}})
    cargo doc -p "$crate" --no-deps --open

# Every module's tests, in order.
test-all:
    #!/usr/bin/env bash
    set -uo pipefail
    for n in 1 2 3 4 5 6 7 8; do
        crate=$(printf "gc-mod%02d" "$n")
        [ -d "crates/$crate" ] || continue
        printf "\n  === module %s ===\n" "$n"
        cargo nextest run -p "$crate" --no-fail-fast || true
    done

# Tests for the shared substrate, which is not part of the exercises.
test-core:
    cargo nextest run -p gc-core -p gc-workloads -p gc-harness --no-fail-fast

# Type-check everything without running anything.
check:
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo fmt --all

clean:
    cargo clean
