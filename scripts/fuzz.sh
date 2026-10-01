#!/bin/sh
# Run every cargo-fuzz target for N seconds each (default 60) on the
# nightly pinned in fuzz/rust-toolchain.toml. The 24-hour campaign the
# plan asks for per target is `scripts/fuzz.sh 86400`, run on a machine
# of its own; a crash lands in fuzz/artifacts/<target>/ and is a bug.
set -eu

seconds=${1:-60}
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root/fuzz"
# The seeds in fuzz/corpus/<target>/ stay as they are in the tree: new
# inputs go to a working corpus under fuzz/target/, which libFuzzer reads
# back on the next run and git never sees.
for target in $(cargo fuzz list); do
  printf '== %s (%s s)\n' "$target" "$seconds"
  mkdir -p "target/corpus/$target"
  cargo fuzz run "$target" "target/corpus/$target" "corpus/$target" -- \
    -max_total_time="$seconds" -rss_limit_mb=2048 -timeout=20
done
printf 'fuzz: every target ran %s s without a crash\n' "$seconds"
