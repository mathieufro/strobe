#!/bin/bash
set -euo pipefail

# Build and launch the test app, capture screenshot, then kill it
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$SCRIPT_DIR/../ui-test-app"
GOLDEN_DIR="$SCRIPT_DIR"

# Build test app
cd "$APP_DIR" && bash build.sh

# Launch app in background
"$APP_DIR/build/UITestApp" &
APP_PID=$!
sleep 2  # Wait for window to render

# Capture screenshot using screencapture
screencapture -l$(osascript -e "tell app \"System Events\" to id of first window of (processes whose unix id is $APP_PID)") "$GOLDEN_DIR/test_app.png"

# Kill app
kill $APP_PID 2>/dev/null || true

echo "Captured golden screenshot: $GOLDEN_DIR/test_app.png"
