#!/bin/sh
# Downloads the kotlin-analyzer server from the project's GitHub releases,
# checks its SHA-256 and installs it as <directory>/kotlin-analyzer.
#
#   install-server.sh [--version <x.y.z>] [--directory <dir>]
#
# Defaults: the latest release, into ~/.local/bin. Supported: macOS and Linux
# on arm64 and x86_64. KOTLIN_ANALYZER_DOWNLOADS overrides where releases are
# downloaded from: <base>/v<version>/<asset>, or <base>/latest/<asset> (the
# editor tests use it).
set -eu

repository_downloads="https://github.com/bash-spbu/kotlin-analyzer/releases"
version=latest
directory="$HOME/.local/bin"

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="$2"; shift 2 ;;
        --directory) directory="$2"; shift 2 ;;
        -h | --help) sed -n '2,10p' "$0"; exit 0 ;;
        *) echo "install-server.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

case "$(uname -s)" in
    Darwin) system=apple-darwin ;;
    Linux) system=unknown-linux-gnu ;;
    *) echo "install-server.sh: no release for $(uname -s); build it with cargo install" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) architecture=aarch64 ;;
    x86_64 | amd64) architecture=x86_64 ;;
    *) echo "install-server.sh: no release for $(uname -m)" >&2; exit 1 ;;
esac
asset="kotlin-analyzer-$architecture-$system.gz"

if [ -n "${KOTLIN_ANALYZER_DOWNLOADS:-}" ] && [ "$version" = latest ]; then
    release="$KOTLIN_ANALYZER_DOWNLOADS/latest"
elif [ -n "${KOTLIN_ANALYZER_DOWNLOADS:-}" ]; then
    release="$KOTLIN_ANALYZER_DOWNLOADS/v$version"
elif [ "$version" = latest ]; then
    release="$repository_downloads/latest/download"
else
    release="$repository_downloads/download/v$version"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "Downloading $release/$asset"
curl --fail --silent --show-error --location --output "$work/$asset" "$release/$asset"
curl --fail --silent --show-error --location --output "$work/$asset.sha256" "$release/$asset.sha256"

expected=$(cut -d ' ' -f 1 "$work/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$asset" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$work/$asset" | cut -d ' ' -f 1)
fi
if [ "$expected" != "$actual" ]; then
    echo "install-server.sh: $asset has SHA-256 $actual, the release says $expected" >&2
    exit 1
fi

mkdir -p "$directory"
gzip -dc "$work/$asset" > "$work/kotlin-analyzer"
chmod +x "$work/kotlin-analyzer"
mv "$work/kotlin-analyzer" "$directory/kotlin-analyzer"
"$directory/kotlin-analyzer" --version
case ":$PATH:" in
    *":$directory:"*) ;;
    *) echo "Note: $directory isn't on the PATH; add it, or point the editor at $directory/kotlin-analyzer." ;;
esac
