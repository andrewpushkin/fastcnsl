#!/bin/sh
# Prints the version: <major>.<minor> from Cargo.toml, then the number of
# commits on the current branch (main in CI), e.g. 0.1.47. Used by the
# Makefile, the packaging scripts, GitHub Actions and the changelog skills.
#
#   get_version.sh          the version of the current commit
#   get_version.sh --next   the version once one more commit is made. The
#                           changelog commit that writes this number down is
#                           itself a commit, so that is the number to write.
cd "$(dirname "$0")/.." || exit 1

extra=0
case "${1:-}" in
    "") ;;
    --next) extra=1 ;;
    *)
        echo "usage: get_version.sh [--next]" >&2
        exit 2
        ;;
esac

base=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\..*"/\1/p' Cargo.toml | head -n 1)
count=$(git rev-list --count HEAD 2>/dev/null || echo 0)
echo "$base.$((count + extra))"
