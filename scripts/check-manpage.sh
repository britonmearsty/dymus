#!/usr/bin/env sh
# Validate the manual emitted by a built binary, without installing it.
set -eu

binary="${1:-./target/release/dymus}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

"$binary" man > "$tmp_dir/dymus.1"
if ! groff -ww -man -Tutf8 "$tmp_dir/dymus.1" > "$tmp_dir/rendered" 2> "$tmp_dir/diagnostics"; then
    cat "$tmp_dir/diagnostics" >&2
    echo "Man page rendering failed." >&2
    exit 1
fi
if [ -s "$tmp_dir/diagnostics" ]; then
    cat "$tmp_dir/diagnostics" >&2
    echo "Man page rendering produced diagnostics." >&2
    exit 1
fi
if ! grep -q '^\.TH DYMUS 1 ' "$tmp_dir/dymus.1" || [ ! -s "$tmp_dir/rendered" ]; then
    echo "Expected a nonempty section-1 Dymus manual." >&2
    exit 1
fi
printf '%s\n' 'Man page renders without diagnostics.'
