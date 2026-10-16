#!/bin/sh
# Luxforge's desktop integration for one person (docs/design/release-v0.1.md#deliverables): adds or
# removes the launcher entry and icon in the person's own data directory. It needs no root and
# changes no PATH, shell profile, driver or system file. It is shipped inside the extracted
# Luxforge folder and always describes the folder it is in, never the working directory.
set -eu

usage() {
    cat <<'EOF'
Usage: ./Luxforge/desktop-integration.sh install|uninstall|help

install    Adds Luxforge to your desktop's application launcher, for you only:
           luxforge.desktop in ${XDG_DATA_HOME:-~/.local/share}/applications, starting this
           folder's ./luxforge, and the Luxforge icon in ${XDG_DATA_HOME:-~/.local/share}/icons/hicolor.
           Run it again after moving the Luxforge folder. An update installed by Luxforge itself
           keeps the entry pointing at the folder.
uninstall  Removes that entry and icon. It removes only what install wrote, and refuses to touch a
           luxforge.desktop that install did not write.

Neither needs root, edits PATH or a shell profile, or installs graphics drivers. Running
./Luxforge/luxforge directly works without either.

Neither touches your photographs or Luxforge's data:
  original photographs     stay where you keep them; Luxforge never writes to them
  settings and catalog     ${XDG_CONFIG_HOME:-~/.config}/luxforge (catalog.sqlite, unless you chose another)
  catalog files            beside each catalog: CATALOG.artifacts/, its index and its log
  downloaded resources     ${XDG_DATA_HOME:-~/.local/share}/luxforge
  logs                     ${XDG_STATE_HOME:-~/.local/state}/luxforge/logs
To remove Luxforge completely, run uninstall, then delete the Luxforge folder. Delete the data
folders above yourself only if you no longer want them.
EOF
}

fail() {
    printf 'desktop-integration: %s\n' "$1" >&2
    exit 1
}

# The physical directory holding $1. Each command substitution ends in an x, so a trailing line
# break in a name survives it, for install to refuse.
dir_of() {
    dir=$(dirname -- "$1" && printf x)
    dir=${dir%?x}
    dir=$(CDPATH='' cd -- "$dir" && pwd -P && printf x) || return 1
    printf '%s' "${dir%?x}"
}

ENTRY_NAME=luxforge.desktop
MARKER='X-Luxforge-Installed-By=desktop-integration.sh'
# Every icon install may write, relative to hicolor/. Uninstall removes only these.
ICONS='256x256/apps/luxforge.png 512x512/apps/luxforge.png scalable/apps/luxforge.svg'

payload=$(dir_of "$0" && printf x) || fail 'cannot find the Luxforge folder this script is in'
payload=${payload%x}
[ -f "$payload/build.json" ] && [ -x "$payload/luxforge" ] ||
    fail "$payload is not an extracted Luxforge folder; run the script that came inside it"
template=$payload/desktop/luxforge.desktop.in
icons=$payload/desktop/icons/hicolor

case ${XDG_DATA_HOME-} in
/*) data=$XDG_DATA_HOME ;;
*)
    [ -n "${HOME-}" ] || fail 'neither XDG_DATA_HOME (absolute) nor HOME is set'
    data=$HOME/.local/share
    ;;
esac
applications=$data/applications
entry=$applications/$ENTRY_NAME
hicolor=$data/icons/hicolor

# Whether the entry is a regular file this script wrote.
owned() {
    [ -f "$entry" ] && [ ! -L "$entry" ] && grep -qxF "$MARKER" "$entry"
}

present() {
    [ -e "$1" ] || [ -L "$1" ]
}

# Replaces $2 with a copy of $1, atomically, readable by everyone and writable by its owner.
place() {
    [ ! -d "$2" ] || fail "$2 is a directory"
    mkdir -p -- "${2%/*}"
    temporary=$(mktemp "${2%/*}/.luxforge.XXXXXX") || fail "cannot write in ${2%/*}"
    if cp -- "$1" "$temporary" && chmod 644 "$temporary" && mv -f -- "$temporary" "$2"; then
        return 0
    fi
    rm -f -- "$temporary"
    fail "cannot write $2"
}

# The Exec program for the entry: always quoted, with the characters the Desktop Entry
# Specification reserves inside quotes ($, ` and ") escaped twice, once for the quoting rule and
# once for the string escape rule.
exec_value() {
    escaped=$(printf '%s\n' "$1" | LC_ALL=C sed -e 's/[$`"]/\\\\&/g' && printf x)
    printf '"%s"' "${escaped%?x}"
}

install_entry() {
    executable=$payload/luxforge
    # A desktop entry is UTF-8 text with one key per line. Luxforge's updater does not write a
    # backslash back, and GLib checks an Exec program before it reads %% as %, so a path with
    # either cannot be launched reliably.
    case $executable in
    *[[:cntrl:]]* | *\\* | *%*) fail "the folder's path has a control character, a backslash or a %; move the Luxforge folder to a plainer path: $payload" ;;
    esac
    if command -v iconv >/dev/null 2>&1; then
        printf '%s' "$executable" | iconv -f UTF-8 -t UTF-8 >/dev/null 2>&1 ||
            fail 'the folder'"'"'s path is not UTF-8; move the Luxforge folder to a UTF-8 path'
    fi
    [ -f "$template" ] && grep -qxF 'Exec=@EXEC@' "$template" &&
        grep -qxF 'TryExec=@TRYEXEC@' "$template" ||
        fail "$template is missing or damaged; extract Luxforge again"
    [ -f "$icons/256x256/apps/luxforge.png" ] ||
        fail "$icons/256x256/apps/luxforge.png is missing; extract Luxforge again"
    if present "$entry" && ! owned; then
        fail "$entry exists and was not written by this script; it was left unchanged. Move it aside to install"
    fi
    for icon in $ICONS; do
        if [ -f "$icons/$icon" ]; then
            place "$icons/$icon" "$hicolor/$icon"
        fi
    done
    exec_line=$(exec_value "$executable")
    mkdir -p -- "$applications"
    generated=$(mktemp "$applications/.luxforge.XXXXXX") || fail "cannot write in $applications"
    while IFS= read -r line || [ -n "$line" ]; do
        case $line in
        'Exec=@EXEC@') printf 'Exec=%s\n' "$exec_line" ;;
        'TryExec=@TRYEXEC@') printf 'TryExec=%s\n' "$executable" ;;
        *) printf '%s\n' "$line" ;;
        esac
    done <"$template" >"$generated" || {
        rm -f -- "$generated"
        fail "cannot write $entry"
    }
    chmod 644 "$generated" && mv -f -- "$generated" "$entry" || {
        rm -f -- "$generated"
        fail "cannot write $entry"
    }
    # Desktops that cache the icon theme notice a changed directory.
    touch -c -- "$hicolor" 2>/dev/null || true
    printf 'Installed %s\nIt starts %s\n' "$entry" "$executable"
}

uninstall_entry() {
    ours=false
    if present "$entry"; then
        owned || fail "$entry was not written by this script; nothing was removed"
        rm -f -- "$entry"
        ours=true
        printf 'Removed %s\n' "$entry"
    fi
    for icon in $ICONS; do
        target=$hicolor/$icon
        [ -f "$target" ] && [ ! -L "$target" ] || continue
        if $ours || { [ -f "$icons/$icon" ] && cmp -s -- "$icons/$icon" "$target"; }; then
            rm -f -- "$target"
            printf 'Removed %s\n' "$target"
        fi
    done
    touch -c -- "$hicolor" 2>/dev/null || true
    printf 'Your photographs, catalogs, settings, logs and this Luxforge folder were not touched.\n'
}

if [ $# -gt 1 ]; then
    usage >&2
    exit 2
fi
case ${1-} in
install) install_entry ;;
uninstall) uninstall_entry ;;
help | -h | --help) usage ;;
*)
    usage >&2
    exit 2
    ;;
esac
