#!/bin/sh
# Sets Helix up for kotlin-analyzer:
#
# 1. installs the server (editors/install-server.sh) unless it's on the PATH
#    already;
# 2. adds languages.toml to Helix's, unless that one configures Kotlin, Java
#    or kotlin-analyzer already — then it says what to merge by hand;
# 3. installs editors/gradle/gradle_task.py and adds config.toml's Gradle
#    keys (Space G) to Helix's config.toml, unless it has Space G keys.
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
        -h | --help) sed -n '2,11p' "$0"; exit 0 ;;
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
else
    mkdir -p "$(dirname "$configuration")"
    {
        [ -s "$configuration" ] && printf '\n'
        printf '%s\n' "$snippet_text"
    } >> "$configuration"
    echo "Added kotlin-analyzer to $configuration"
fi

gradle_task="${XDG_DATA_HOME:-$HOME/.local/share}/kotlin-analyzer/gradle_task.py"
mkdir -p "$(dirname "$gradle_task")"
cp "$here/../gradle/gradle_task.py" "$gradle_task"
keys="${XDG_CONFIG_HOME:-$HOME/.config}/helix/config.toml"
keys_text=$(sed "s|GRADLE_TASK|$gradle_task|" "$here/config.toml")
if [ -f "$keys" ] && grep -q 'keys\.normal\.space\.G' "$keys"; then
    echo "$keys already binds Space G; merge the Gradle keys by hand:"
    printf '%s\n' "$keys_text"
else
    {
        [ -s "$keys" ] && printf '\n'
        printf '%s\n' "$keys_text"
    } >> "$keys"
    echo "Added the Gradle keys (Space G b/c/t) to $keys"
fi
hx --health kotlin 2>/dev/null | sed -n '1,3p' || true
