#!/usr/bin/env bash
# Builds and packages the standalone code graph indexer Collapse pins (SPEC.md F19, C11):
#
#   bash graph-indexer.sh <version> <output-folder>
#
# Run it from the repository root on Linux x86-64. It writes
# `velocity-graph-<version>-x86_64-unknown-linux-gnu.tar.gz` and that file's `.sha256` to
# the output folder. release.yml and graph-indexer.yml both run this one script, so a
# release carries the same archive layout whichever of them attached it (#518).
set -euo pipefail

usage="usage: graph-indexer.sh <version> <output-folder>"
version="${1:?$usage}"
output="${2:?$usage}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Version $version is not <semver>" >&2
  exit 2
fi

target=x86_64-unknown-linux-gnu
name="velocity-graph-${version}-${target}"
# The binary reports this version in `velocity-graph version`, for Collapse's pin check.
VELOCITY_GRAPH_VERSION="$version" cargo build --release --locked --target "$target" -p velocity-graph --bin velocity-graph

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$output" "$stage/$name"
cp "${CARGO_TARGET_DIR:-target}/$target/release/velocity-graph" LICENSE "$stage/$name/"

# An archive whose binary does not start, or reports another version, is never packaged.
reported="$("$stage/$name/velocity-graph" version)"
echo "$reported"
if ! grep -qF "\"indexer\":\"$version\"" <<<"${reported//[[:space:]]/}"; then
  echo "velocity-graph does not report version $version" >&2
  exit 1
fi

tar -czf "$output/$name.tar.gz" -C "$stage" "$name"
(cd "$output" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "Packaged $output/$name.tar.gz"
