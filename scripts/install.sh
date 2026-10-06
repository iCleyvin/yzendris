#!/bin/sh
# Yzendris installer for Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/iCleyvin/yzendris/main/scripts/install.sh | sh
#
# Downloads the latest release for this machine, checks it against the
# published SHA-256, and runs `yzendris install`, which sets everything up for
# the current user. It asks for administrator rights once, to add a udev rule
# (and open the firewall on a host). Nothing else runs as root.
#
# Works on Ubuntu/Debian, Fedora and its atomic variants (Bazzite, Silverblue,
# Kinoite), Arch-based systems (CachyOS, Omarchy, EndeavourOS), openSUSE — any
# distribution with systemd and glibc 2.35 or newer.
set -eu

REPO="iCleyvin/yzendris"
case "$(uname -m)" in
    x86_64 | amd64) ARCH=x86_64 ;;
    *) echo "No prebuilt package for $(uname -m) yet. Build from source: https://github.com/$REPO#build-from-source" >&2; exit 1 ;;
esac
[ "$(uname -s)" = Linux ] || { echo "This installer is for Linux. On Windows use scripts/install.ps1." >&2; exit 1; }
command -v curl >/dev/null || { echo "curl is required." >&2; exit 1; }

NAME="yzendris-linux-$ARCH"
BASE="https://github.com/$REPO/releases/latest/download"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading $NAME…"
curl -fsSL "$BASE/$NAME.tar.gz" -o "$TMP/$NAME.tar.gz"
curl -fsSL "$BASE/$NAME.tar.gz.sha256" -o "$TMP/$NAME.tar.gz.sha256"
(cd "$TMP" && sha256sum -c "$NAME.tar.gz.sha256" >/dev/null) || { echo "Checksum mismatch — download corrupted. Aborting." >&2; exit 1; }
tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"

# `sh` reading this script from a pipe has no terminal on stdin; give the
# installer one so it can ask for the administrator password.
if [ -r /dev/tty ]; then
    "$TMP/$NAME/yzendris" install "$@" </dev/tty
else
    "$TMP/$NAME/yzendris" install "$@"
fi
