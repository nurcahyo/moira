#!/usr/bin/env bash
# Graduated disk reclaim for cargo target directories.
#
# **Why this is in the repo.** It used to live in a session-local scratchpad, and
# `plans/reports/HANDOFF.md` told every future agent to run it — from a path that would not exist
# for them. A tool referenced by durable instructions has to be as durable as the instructions.
#
# **Why graduated rather than `cargo clean`.** Measured on this project:
#
#     debug/deps         ~11 GB   compiled dependencies — 405 crates, minutes to rebuild
#     debug/incremental   ~7 GB   pure cache — free to delete, costs one non-incremental build
#
# So the cheapest ~40% of the space is also the safest. `cargo clean` deletes both and forces a full
# rebuild to recover the half that was never the problem.
#
# **Trigger on FREE DISK, not on directory size.** A 20 GB target is harmless on an empty disk and
# fatal on a full one. What breaks a build is running out of room.
#
# Usage:  scripts/reclaim.sh [min_free_gb]      (default 60, matching the ledger threshold)

set -euo pipefail

MIN_FREE_GB="${1:-60}"
TARGET_ROOT="${CARGO_TARGET_ROOT:-$HOME/.cargo-targets}"

free_gb() { df -g . | awk 'NR==2 {print $4}'; }
size_of() { du -sh "$1" 2>/dev/null | cut -f1 || echo "0"; }

# Deleting artifacts under a live build corrupts it. `pgrep` rather than `ps aux | grep`: the latter
# gives false negatives in this sandbox, and `ps -eo` returns nothing at all — an agent once nearly
# concluded a three-hour gate run had died and started dropping its databases underneath it.
if pgrep -x cargo >/dev/null 2>&1 || pgrep -f cargo-mutants >/dev/null 2>&1; then
    echo "REFUSING: cargo or cargo-mutants is running. Deleting artifacts under a live build corrupts it."
    exit 1
fi

echo "free: $(free_gb) GB   (want >= ${MIN_FREE_GB})"
[ -d "$TARGET_ROOT" ] && du -sh "$TARGET_ROOT"/* 2>/dev/null || true

# Level 1 — incremental caches. Best space-per-unit-of-pain by a wide margin: the next build of the
# local crate is non-incremental, which is ONE crate, not 405.
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
    for dir in "$TARGET_ROOT"/*/debug/incremental "$TARGET_ROOT"/*/release/incremental ./target/debug/incremental; do
        [ -d "$dir" ] || continue
        echo "L1: dropping $(size_of "$dir") from $dir"
        rm -rf "$dir"
    done
    echo "    free now: $(free_gb) GB"
fi

# Level 2 — target directories of branches already merged. These are the routine waste: an agent
# finishes, its branch merges, and its ~11 GB sits there indefinitely.
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
    echo "L2: agent target dirs still present — delete any whose branch has merged:"
    [ -d "$TARGET_ROOT" ] && du -sh "$TARGET_ROOT"/* 2>/dev/null || true
    echo "    (not automatic: only you know which are still in flight)"
fi

# Level 3 — the main target dir. Full rebuild is ~2m21s with `debug = 1`, so this is an annoyance
# rather than the disaster it was before the profile change.
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
    echo "L3: cleaning ./target ($(size_of ./target)) — every dependency rebuilds (~2m21s)"
    cargo clean
    echo "    free now: $(free_gb) GB"
fi

echo "done. free: $(free_gb) GB"
