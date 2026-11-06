#!/bin/sh
# Build ymw and install it somewhere on PATH.
#   ./install.sh              -> ~/.local/bin
#   YMW_INSTALL_DIR=/usr/local/bin ./install.sh
set -eu

cd "$(dirname "$0")"
DEST="${YMW_INSTALL_DIR:-${YMP_INSTALL_DIR:-$HOME/.local/bin}}"

cargo build --release

mkdir -p "$DEST"
# Write to a fresh inode and rename over the old one. cp/truncate-in-place
# inherits the destination's com.apple.provenance, and macOS then kills the
# binary on launch (exit 137 / "Killed: 9").
cat target/release/ymw > "$DEST/.ymw.new"
chmod +x "$DEST/.ymw.new"
mv -f "$DEST/.ymw.new" "$DEST/ymw"

# macOS attaches com.apple.provenance to freshly copied binaries in some
# directories, and the kernel then kills them on launch (exit 137 / "Killed: 9").
if command -v xattr >/dev/null 2>&1; then
    xattr -c "$DEST/ymw" 2>/dev/null || true
fi

printf 'installed %s -> %s\n' "$("$DEST/ymw" -v)" "$DEST/ymw"

case ":$PATH:" in
    *":$DEST:"*) ;;
    *) printf 'note: %s is not on PATH, add it to your shell profile\n' "$DEST" ;;
esac
