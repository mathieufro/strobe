#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"

mkdir -p "$BUILD_DIR"

# Compile as a macOS app bundle
swiftc \
    -parse-as-library \
    -o "$BUILD_DIR/UITestApp" \
    -framework SwiftUI \
    -framework AppKit \
    "$SCRIPT_DIR/UITestApp.swift" \
    2>&1

echo "Built: $BUILD_DIR/UITestApp"
