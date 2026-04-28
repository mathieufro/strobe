#!/usr/bin/env bash
# install-local.sh
#
# Build the current source tree and install it to ~/.strobe/bin/strobe with
# the same entitlements `install.sh` uses. Use this whenever you `cargo build`
# and want the new binary to actually replace the running one — a plain
# `cp target/release/strobe ~/.strobe/bin/` is NOT enough on macOS:
#
#   * The cp inherits cargo's default linker-signed adhoc signature, which
#     lacks the Frida entitlements (`get-task-allow`,
#     `cs.disable-library-validation`).
#   * Without those entitlements, dyld can stall during library validation
#     and the process becomes a permanent UE-state zombie that not even
#     SIGKILL can reap. (See main.rs for the full story.)
#
# This script rebuilds (release), copies, and re-signs in one shot.

set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${STROBE_HOME:-$HOME/.strobe}/bin"
DEST="$BIN_DIR/strobe"

echo "> Building strobe (release)..."
cargo build --manifest-path "$SRC_DIR/Cargo.toml" --release --bin strobe

mkdir -p "$BIN_DIR"
cp "$SRC_DIR/target/release/strobe" "$DEST"
chmod +x "$DEST"

if [ "$(uname)" = "Darwin" ]; then
    ent_file="$(mktemp)"
    cat > "$ent_file" <<'ENTEOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>com.apple.security.get-task-allow</key><true/>
<key>com.apple.security.cs.disable-library-validation</key><true/>
</dict></plist>
ENTEOF
    codesign -f -s - --entitlements "$ent_file" "$DEST"
    rm -f "$ent_file"
    echo "  Signed with Frida entitlements."
fi

echo "  Installed: $DEST"
echo
echo "Tip: also restart any running daemon so it picks up the new binary:"
echo "  pkill -f 'strobe daemon'  # daemon is auto-respawned on next MCP launch"
