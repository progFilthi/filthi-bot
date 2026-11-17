#!/bin/zsh
# Install MQL5 sources into the MT5 terminal tree and compile them with
# MetaEditor under the bundled Wine.
#
# MetaEditor's /compile argument parsing under Wine is unreliable with paths
# containing spaces, so we compile in a no-space temp dir (C:\temp) and copy
# the resulting .ex5 into MQL5/Experts (or Scripts for .mq5 script files).
set -euo pipefail

MT5_PREFIX="$HOME/Library/Application Support/net.metaquotes.wine.metatrader5"
TERMINAL_DIR="$MT5_PREFIX/drive_c/Program Files/MetaTrader 5"
WINE="/Applications/MetaTrader 5.app/Contents/SharedSupport/wine/bin/wine"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
export WINEPREFIX="$MT5_PREFIX"
export WINEDEBUG="-all"

if [[ ! -d "$TERMINAL_DIR" ]]; then
  echo "MT5 terminal dir not found: $TERMINAL_DIR" >&2
  exit 1
fi

TMP_WIN='C:\temp'
TMP_DIR="$MT5_PREFIX/drive_c/temp"
mkdir -p "$TMP_DIR"

compile_one() {
  local src="$1"      # repo path
  local dst_dir="$2"  # terminal-relative MQL5 target dir (Experts|Scripts)
  local base="$(basename "$src")"
  cp "$src" "$TMP_DIR/$base"
  echo "== compiling $base =="
  "$WINE" 'C:\Program Files\MetaTrader 5\metaeditor64.exe' \
    /compile:"$TMP_WIN\\$base" \
    /log:"$TMP_WIN\\${base:r}_compile.log" || true
  if [[ -f "$TMP_DIR/${base:r}_compile.log" ]]; then
    iconv -f UTF-16LE -t UTF-8 "$TMP_DIR/${base:r}_compile.log" 2>/dev/null |
      grep -E "error|warning|result" | head -10 || echo "(no errors/warnings in log)"
  fi
  if [[ -f "$TMP_DIR/${base:r}.ex5" ]]; then
    mkdir -p "$TERMINAL_DIR/MQL5/$dst_dir"
    cp "$TMP_DIR/${base:r}.ex5" "$TERMINAL_DIR/MQL5/$dst_dir/${base:r}.ex5"
    echo "== installed $dst_dir/${base:r}.ex5"
  else
    echo "!! compilation failed for $base (no .ex5 produced)" >&2
    return 1
  fi
}

FAILED=0
compile_one "$REPO/mql5/Bridge.mq5" "Experts" || FAILED=1
if [[ -f "$REPO/mql5/ExportHistory.mq5" ]]; then
  cp "$REPO/mql5/ExportHistory.mq5" "$TERMINAL_DIR/MQL5/Scripts/ExportHistory.mq5" 2>/dev/null || true
fi

exit $FAILED
