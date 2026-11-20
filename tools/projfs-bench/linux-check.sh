#!/usr/bin/env bash
# Linux before/after check for the #454 ProjFS prototype.
#
# Clones this repository into a Linux filesystem, checks out the base commit
# and the prototype's head side by side, and runs the same build, tests and
# guards on both with the same toolchain. The claim it supports is narrow:
# on Linux the prototype changes no result. Usage (from WSL or any Linux):
#
#   bash tools/projfs-bench/linux-check.sh <repo> <base> <head> <work> [toolchain]
#
# With a toolchain (for example 1.85.1, the repository's) every cargo command
# runs on it; it must be installed with rustfmt and clippy.
#
# It prints one line per step and side: `<side> <step> exit=<code> <summary>`.
set -u
repo=$1 base=$2 head=$3 work=$4 toolchain=${5:-}
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# rustup selects this toolchain for every cargo and rustc below, also under `timeout`.
[ -n "$toolchain" ] && export RUSTUP_TOOLCHAIN=$toolchain
rm -rf "$work" && mkdir -p "$work"
git clone -q "$repo" "$work/head" && git -C "$work/head" checkout -q "$head" || exit 2
git -C "$work/head" worktree add -q "$work/base" "$base" || exit 2
echo "toolchain: $(cargo --version) / $(rustc --version) / $(uname -sr)"

step() { # side name command...
  local side=$1 name=$2; shift 2
  local log="$work/$side-$name.log"
  (cd "$work/$side" && "$@") >"$log" 2>&1
  local code=$?
  local summary
  summary=$(grep -E "^test result|^(PASS|FAIL):|^Ran |^OK|^FAILED|Finished|^error(\[|:)| warning: .*generated" "$log" |
    sed -E 's/finished in [0-9.]+s//; s/in [0-9.]+s//' | sort | uniq -c | tr '\n' ';' | cut -c1-600)
  echo "$side $name exit=$code $summary"
}

for side in base head; do
  step "$side" build cargo build --locked --workspace
  step "$side" test-build cargo test --locked --workspace --exclude layerfs-node --no-run
  for package in layerfs-persistence layerfs-overlay layerfs-store-service layerfs-workspace layerfs-project; do
    step "$side" "test-$package" timeout 120 cargo test --locked -p "$package"
  done
  step "$side" guard python3 -B tools/check_product_boundary.py --skip-sdk
  step "$side" guard-tests python3 -B -m unittest discover -s tools -p 'test_*.py'
  step "$side" fmt cargo fmt --all -- --check
  step "$side" clippy cargo clippy --locked --workspace --exclude layerfs-node --all-targets -- -D warnings
done
step head test-layerfs-projfs timeout 120 cargo test --locked -p layerfs-projfs
step head clippy-projfs cargo clippy --locked -p layerfs-projfs --all-targets -- -D warnings
echo "done"
