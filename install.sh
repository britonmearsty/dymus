#!/usr/bin/env sh
# Install the latest Dymus release to ~/.local/bin, or set BIN_DIR to choose another location.
set -eu

repo="britonmearsty/dymus"
bin_dir="${BIN_DIR:-$HOME/.local/bin}"
version="${DYMUS_VERSION:-latest}"
man_dir="${MAN_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/man}"

if [ "$(uname -s)" != "Linux" ]; then
    echo "Dymus releases currently support Linux only." >&2
    exit 1
fi

case "$(uname -m)" in
    x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
    *)
        echo "No prebuilt Dymus release is available for $(uname -m). Build with Cargo instead." >&2
        exit 1
        ;;
esac

asset="dymus-${target}.tar.gz"
if [ "$version" = "latest" ]; then
    base_url="https://github.com/${repo}/releases/latest/download"
else
    base_url="https://github.com/${repo}/releases/download/${version}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

echo "Downloading Dymus ${version} for ${target}…"
curl --fail --location --silent --show-error "${base_url}/${asset}" -o "${tmp_dir}/${asset}"
curl --fail --location --silent --show-error "${base_url}/${asset}.sha256" -o "${tmp_dir}/${asset}.sha256"
(cd "$tmp_dir" && sha256sum --check "${asset}.sha256")

mkdir -p "$bin_dir"
tar -xzf "${tmp_dir}/${asset}" -C "$tmp_dir"
install -m 755 "${tmp_dir}/dymus" "${bin_dir}/dymus"
echo "Installed dymus to ${bin_dir}/dymus"
# Older releases contain only the binary.
if [ -f "${tmp_dir}/dymus.1" ]; then
    mkdir -p "${man_dir}/man1"
    install -m 644 "${tmp_dir}/dymus.1" "${man_dir}/man1/dymus.1"
    echo "Installed manual to ${man_dir}/man1/dymus.1 (man dymus)"
fi
case ":$PATH:" in
    *":${bin_dir}:"*) ;;
    *) echo "Add ${bin_dir} to your PATH to run dymus." ;;
esac
