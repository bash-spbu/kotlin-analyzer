#!/bin/sh
# Runs the plugin in a clean, headless Neovim against a fresh Gradle-shaped
# workspace, with no kotlin-analyzer on the PATH: the release it downloads
# is served from the locally built binary (KOTLIN_ANALYZER_BINARY, default
# target/release/kotlin-analyzer).
set -eu

plugin=$(cd "$(dirname "$0")/.." && pwd)
repository=$(cd "$plugin/../.." && pwd)
binary="${KOTLIN_ANALYZER_BINARY:-$repository/target/release/kotlin-analyzer}"
version=$(sed -n 's/^return "\(.*\)"$/\1/p' "$plugin/lua/kotlin_analyzer/version.lua")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) triple=aarch64-apple-darwin ;;
    Darwin-x86_64) triple=x86_64-apple-darwin ;;
    Linux-aarch64) triple=aarch64-unknown-linux-gnu ;;
    Linux-x86_64) triple=x86_64-unknown-linux-gnu ;;
    *) echo "run.sh: no release for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
asset="kotlin-analyzer-$triple.gz"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}
for kind in good tampered; do
    mkdir -p "$scratch/$kind/v$version"
    gzip -c "$binary" > "$scratch/$kind/v$version/$asset"
    (cd "$scratch/$kind/v$version" && sha256 "$asset" > "$asset.sha256")
done
printf '%064d  %s\n' 0 "$asset" > "$scratch/tampered/v$version/$asset.sha256"

workspace="$scratch/build"
mkdir -p "$workspace/app/src/main/kotlin/app"
printf 'rootProject.name = "demo"\ninclude("app")\n' > "$workspace/settings.gradle.kts"
: > "$workspace/app/build.gradle.kts"
printf 'package app\n\nclass Box(val size: Int)\n' > "$workspace/app/src/main/kotlin/app/Box.kt"
printf 'package app\n\nfun use(): Int = Box(3).size\n' > "$workspace/app/src/main/kotlin/app/Use.kt"
printf 'package app\n\nfun broken( {\n' > "$workspace/app/src/main/kotlin/app/Broken.kt"

neovim=$(command -v nvim)
run() {
    kind=$1
    shift
    echo "$kind:"
    env -i HOME="$scratch/home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
        XDG_DATA_HOME="$scratch/data-$kind" XDG_STATE_HOME="$scratch/state" \
        XDG_CONFIG_HOME="$scratch/config" \
        KOTLIN_ANALYZER_DOWNLOADS="file://$scratch/$kind" \
        "$neovim" --clean --headless --cmd "set runtimepath^=$plugin" \
        -l "$plugin/test/checks.lua" "$@"
}
run good downloads "$workspace"
run tampered tampered
