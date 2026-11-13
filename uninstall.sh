#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail

HOME="${HOME:?HOME is not set}"
PREFIX="${PREFIX:-/data/data/com.termux/files/usr}"
PACKAGE="$PREFIX/lib/node_modules/opencode-termux"
BACKUP="$HOME/.local/share/opencode-turbo/opencode-wrapper.orig"
BIN_DIR="$HOME/.local/bin"
TARGET="$BIN_DIR/opencode-turbo"
LINK="$BIN_DIR/opencode"
BASHRC="$HOME/.bashrc"

if [ -f "$PACKAGE/bin/opencode" ] && grep -q 'opencode-turbo shim' "$PACKAGE/bin/opencode"; then
  if [ -f "$BACKUP" ]; then
    cp -p "$BACKUP" "$PACKAGE/bin/opencode"
    echo "restored the original opencode-termux wrapper"
  else
    echo "warning: shim is present but the backup is missing: $BACKUP" >&2
    echo "reinstall opencode-termux if opencode stops working" >&2
  fi
fi

rm -f "$TARGET"
if [ -L "$LINK" ] && [ "$(readlink "$LINK")" = "opencode-turbo" ]; then
  rm -f "$LINK"
fi

if [ -f "$BASHRC" ] && grep -q '>>> opencode-turbo >>>' "$BASHRC"; then
  MODE="$(stat -c %a "$BASHRC" 2>/dev/null || echo 600)"
  TMP="$BASHRC.opencode-turbo.$$"
  awk '
    /# >>> opencode-turbo >>>/ { skip = 1; next }
    /# <<< opencode-turbo <<</ { skip = 0; next }
    !skip
  ' "$BASHRC" > "$TMP"
  chmod "$MODE" "$TMP"
  mv -f "$TMP" "$BASHRC"
  echo "removed the opencode-turbo PATH block from ~/.bashrc"
fi

rm -rf "$HOME/.cache/opencode-turbo"
rm -rf "$HOME/.local/share/opencode-turbo"

echo "opencode-turbo removed. The opencode runtime and sessions were not touched."
