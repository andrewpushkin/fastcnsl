#!/bin/sh
# Prints the build version: <major>.<minor> from Cargo.toml, then the
# number of commits on the current branch (main in CI), e.g. 0.1.47.
# Used by the Makefile, the packaging scripts and GitHub Actions.
cd "$(dirname "$0")/.." || exit 1
base=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\..*"/\1/p' Cargo.toml | head -n 1)
count=$(git rev-list --count HEAD 2>/dev/null || echo 0)
echo "$base.$count"
