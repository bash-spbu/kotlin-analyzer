#!/bin/sh
# Sets Helix up for kotlin-analyzer:
#
# 1. installs the server (editors/install-server.sh) unless it's on the PATH
#    already;
# 2. adds languages.toml to Helix's, unless that one configures Kotlin, Java
#    or kotlin-analyzer already — then it says what to merge by hand.
#
#   install.sh [--no-java] [--directory <dir for the binary>]
set -eu

here=$(cd "$(dirname "$0")" && pwd)
java=yes
server_arguments=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-java) java=no; shift ;;
        --directory) server_arguments="--directory $2"; shift 2 ;;
        -h | --help) sed -n '2,9p' "$0"; exit 0 ;;
        *) echo "install.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

if command -v kotlin-analyzer >/dev/null 2>&1; then
    echo "Using $(command -v kotlin-analyzer) ($(kotlin-analyzer --version))"
else
    # shellcheck disable=SC2086
    "$here/../install-server.sh" $server_arguments
fi

configuration="${XDG_CONFIG_HOME:-$HOME/.config}/helix/languages.toml"
snippet="$here/languages.toml"
if [ "$java" = no ]; then
    # Everything up to the Java section.
    snippet_text=$(sed '/^name = "java"/,$d' "$snippet" | sed '$d')
else
    snippet_text=$(cat "$snippet")
fi

already_configured=no
if [ -f "$configuration" ] && grep -Eq 'kotlin-analyzer|^name *= *"(kotlin|java)"' "$configuration"; then
    already_configured=yes
fi
if [ "$already_configured" = yes ]; then
    echo "$configuration already configures Kotlin, Java or kotlin-analyzer;"
    echo "merge $snippet into it by hand."
    exit 0
fi

mkdir -p "$(dirname "$configuration")"
{
    [ -s "$configuration" ] && printf '\n'
    printf '%s\n' "$snippet_text"
} >> "$configuration"
echo "Added kotlin-analyzer to $configuration"
hx --health kotlin 2>/dev/null | sed -n '1,3p' || true
