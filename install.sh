#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail

REPO="${OPENCODE_TURBO_REPO:-devnusret/opencode-turbo}"
ASSET="${OPENCODE_TURBO_ASSET:-opencode-turbo-aarch64-android}"
SOURCE_BUILD=0
LOCAL_BINARY=""

usage() {
  cat <<'EOF'
opencode-turbo installer

Usage:
  ./install.sh                 download the latest prebuilt launcher
  ./install.sh --from-source   build with cargo and install
  ./install.sh --binary PATH   install a specific launcher binary

Environment:
  OPENCODE_TURBO_REPO          GitHub repository (default: devnusret/opencode-turbo)
  OPENCODE_TURBO_ASSET         release asset name (default: opencode-turbo-aarch64-android)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --from-source) SOURCE_BUILD=1; shift ;;
    --binary) LOCAL_BINARY="${2:-}"; [ -n "$LOCAL_BINARY" ] || { echo "error: --binary needs a path" >&2; exit 2; }; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ "$(uname -m)" != "aarch64" ]; then
  echo "error: opencode-turbo supports aarch64 Termux only (found $(uname -m))" >&2
  exit 1
fi

HOME="${HOME:?HOME is not set}"
PREFIX="${PREFIX:-/data/data/com.termux/files/usr}"
PACKAGE="$PREFIX/lib/node_modules/opencode-termux"
BIN_DIR="$HOME/.local/bin"
TARGET="$BIN_DIR/opencode-turbo"

if [ ! -f "$PACKAGE/bin/opencode" ]; then
  echo "error: opencode-termux was not found at $PACKAGE" >&2
  echo "install opencode-termux first, then run this installer again" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"

TMP=""
cleanup() {
  if [ -n "$TMP" ] && [ -d "$TMP" ]; then
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

if [ -n "$LOCAL_BINARY" ]; then
  echo "installing launcher from $LOCAL_BINARY"
  cp "$LOCAL_BINARY" "$TARGET.tmp.$$"
elif [ "$SOURCE_BUILD" = "1" ]; then
  echo "building launcher from source"
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  (
    cd "$SCRIPT_DIR"
    cargo build --release
  )
  cp "$SCRIPT_DIR/target/release/opencode-turbo" "$TARGET.tmp.$$"
else
  TMP="$(mktemp -d)"
  echo "downloading $ASSET from $REPO"
  curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' \
    -o "$TMP/$ASSET" \
    "https://github.com/$REPO/releases/latest/download/$ASSET"
  curl -fsSL --retry 3 --proto '=https' --proto-redir '=https' \
    -o "$TMP/SHA256SUMS" \
    "https://github.com/$REPO/releases/latest/download/SHA256SUMS"
  (
    cd "$TMP"
    grep "  $ASSET\$" SHA256SUMS > selected.sha256
    sha256sum -c selected.sha256
  )
  cp "$TMP/$ASSET" "$TARGET.tmp.$$"
fi

chmod 0755 "$TARGET.tmp.$$"
mv -f "$TARGET.tmp.$$" "$TARGET"
ln -sf opencode-turbo "$BIN_DIR/opencode"

BASHRC="$HOME/.bashrc"
if ! grep -q '>>> opencode-turbo >>>' "$BASHRC" 2>/dev/null; then
  touch "$BASHRC"
  cat >> "$BASHRC" <<'EOF'

# >>> opencode-turbo >>>
# Prefer the native opencode launcher over the npm shell wrapper.
if [ -x "$HOME/.local/bin/opencode" ]; then
  case "$PATH" in
    "$HOME/.local/bin:"*) ;;
    *) export PATH="$HOME/.local/bin:$PATH" ;;
  esac
fi
# <<< opencode-turbo <<<
EOF
fi

"$TARGET" --turbo-repair
"$TARGET" --turbo-doctor

cat <<EOF

Installed:
  $TARGET
  $BIN_DIR/opencode -> opencode-turbo

Open a new shell (or run: source ~/.bashrc), then type:
  opencode

Useful checks:
  opencode --version
  opencode-turbo --turbo-doctor
EOF
