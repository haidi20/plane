#!/bin/sh
# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.
#
# Fast Rust gate for commits that touch apps/api-rs.
#
# Called by `.husky/pre-commit` after `pnpm lint-staged`; a commit that touches
# no Rust file exits immediately. Pass `--all` to run it by hand:
#
#     sh apps/api-rs/scripts/rust-precommit.sh --all
#
# Environment:
#   RUST_PRECOMMIT_FAST=1  skip clippy (it is still required by `just check`)
#
# Steps run cheapest-first and stop at the first failure, so a syntax error
# never reaches `cargo check` and a type error never reaches clippy.

set -eu

cd "$(git rev-parse --show-toplevel)"

force=0
if [ "${1:-}" = "--all" ]; then
  force=1
fi

count=0
if [ "$force" -eq 1 ]; then
  count=1
else
  staged=$(git diff --cached --name-only --diff-filter=ACMR)
  if [ -n "$staged" ]; then
    matching=$(printf '%s\n' "$staged" | grep -E '^(apps/api-rs/.*\.(rs|sql)|apps/api-rs/(Cargo\.(toml|lock)|rust-toolchain\.toml|rustfmt\.toml|clippy\.toml)|apps/api-rs/\.cargo/.*)$' || true)
    if [ -n "$matching" ]; then
      count=$(printf '%s\n' "$matching" | wc -l | tr -d ' ')
    fi
  fi
fi

if [ "$count" -eq 0 ]; then
  exit 0
fi

echo "rust-precommit: ${count} staged Rust input(s) under apps/api-rs"
cd apps/api-rs

run_step() {
  label=$1
  shift
  started=$(date +%s)
  if "$@"; then
    elapsed=$(( $(date +%s) - started ))
    echo "  ok   ${label} (${elapsed}s)"
  else
    echo "  FAIL ${label}" >&2
    echo "  fix:  cd apps/api-rs && $*" >&2
    exit 1
  fi
}

run_step "cargo fmt --all -- --check" cargo fmt --all -- --check
run_step "cargo check -q --all-targets" cargo check -q --all-targets
if [ "${RUST_PRECOMMIT_FAST:-0}" != "1" ]; then
  run_step "cargo clippy -q --all-targets -- -D warnings" cargo clippy -q --all-targets -- -D warnings
fi
echo "rust-precommit: ok"
