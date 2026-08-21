#!/usr/bin/env bash
#
# The supervisor looks for the program beside its own binary and then on PATH,
# so /usr/bin is where an installed AgentMaster has to land for a packaged
# supervisor to find it.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

DEB_PKG="svora-agent-master"
CARGO_PKG="agent-master"
BIN_NAME="svora-am"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
OUT_DIR="target/debian"

ARCH="$(dpkg --print-architecture)"
DEB="$OUT_DIR/${DEB_PKG}_${VERSION}_${ARCH}.deb"
STAGE="$OUT_DIR/${DEB_PKG}_${VERSION}_${ARCH}"

echo "==> Building release binary (version $VERSION, arch $ARCH)"
cargo build --release -p "$CARGO_PKG"

echo "==> Assembling deb tree at $STAGE"
rm -rf "$STAGE" "$DEB"
mkdir -p "$STAGE/DEBIAN"
mkdir -p "$STAGE/usr/bin"
mkdir -p "$STAGE/usr/share/doc/$DEB_PKG"

install -m 0755 "target/release/$BIN_NAME" "$STAGE/usr/bin/$BIN_NAME"

INSTALLED_KIB="$(du -ks "$STAGE/usr" | cut -f1)"

sed -e "s/@VERSION@/$VERSION/" \
    -e "s/@ARCH@/$ARCH/" \
    -e "s/@SIZE@/$INSTALLED_KIB/" \
    packaging/debian/control.in > "$STAGE/DEBIAN/control"

cat > "$STAGE/usr/share/doc/$DEB_PKG/copyright" <<'COPYRIGHT'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: agent-master
Source: https://github.com/sibatabot/agent-master

Files: *
Copyright: bitia
License: MIT
COPYRIGHT

echo "==> Building $DEB"
# `fakeroot` so files inside the .deb are owned by root:root regardless of
# the building user; dpkg-deb otherwise records the builder's uid/gid.
fakeroot dpkg-deb --build "$STAGE" "$DEB"

echo "==> Built $DEB"
echo "    install with:  sudo apt install ./$DEB"
