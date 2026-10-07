#!/bin/sh
# Run every cargo-fuzz target for N seconds each (default 60) on the
# nightly pinned in fuzz/rust-toolchain.toml. The 24-hour campaign the
# plan asks for per target is `scripts/fuzz.sh 86400`, run on a machine
# of its own; a crash lands in fuzz/artifacts/<target>/ and is a bug.
set -eu

seconds=${1:-60}
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root/fuzz"
# cargo-fuzz builds for the triple it was itself compiled for unless told
# otherwise; a prebuilt musl cargo-fuzz (CI's) would then ask for
# x86_64-unknown-linux-musl, where no sanitizer runs against the static
# libc. The host triple of the pinned nightly is what every run means.
host=$(rustc -vV | sed -n 's/^host: //p')
# The seeds in fuzz/corpus/<target>/ stay as they are in the tree: new
# inputs go to a working corpus under fuzz/target/, which libFuzzer reads
# back on the next run and git never sees.
for target in $(cargo fuzz list); do
  printf '== %s (%s s)\n' "$target" "$seconds"
  mkdir -p "target/corpus/$target"
  # libFuzzer exits on a corpus directory that is not there, and git
  # does not carry an empty one: a target without seeds runs from its
  # working corpus alone.
  seeds=
  [ -d "corpus/$target" ] && seeds="corpus/$target"
  # shellcheck disable=SC2086 # $seeds is one word or none
  cargo fuzz run --target "$host" "$target" "target/corpus/$target" $seeds -- \
    -max_total_time="$seconds" -rss_limit_mb=2048 -timeout=20
done
printf 'fuzz: every target ran %s s without a crash\n' "$seconds"
