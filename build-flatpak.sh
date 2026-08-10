#!/usr/bin/env bash
set -euo pipefail

APP_ID="io.github.Mars7x.Aureus"
MANIFEST="flatpak/${APP_ID}.yml"
BUILD_DIR="build-dir"

flatpak-builder \
  --force-clean \
  --user \
  --install-deps-from=flathub \
  "$BUILD_DIR" \
  "$MANIFEST"

echo
echo "Built ${APP_ID} in ${BUILD_DIR}"
