#!/usr/bin/env bash
set -euo pipefail

# Build and install lbreco for the current user.
# System packages (Arch): gtk4 libadwaita webkitgtk-6.0 rust — or use rustup.
#
# Usage: ./install.sh
#        ./install.sh --uninstall

APP_ID="io.github.lbreco.LBreco"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_ROOT="${HOME}/.local/share/icons/hicolor"

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "${BIN_DIR}/lbreco" "${APP_DIR}/${APP_ID}.desktop"
  rm -f "${ICON_ROOT}/scalable/apps/${APP_ID}.svg"
  for size in 64 128 256; do
    rm -f "${ICON_ROOT}/${size}x${size}/apps/${APP_ID}.png"
  done
  echo "lbreco removed from ~/.local"
  exit 0
fi

cargo build --release
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
install -Dm755 "${TARGET_DIR}/release/lbreco" "${BIN_DIR}/lbreco"
install -Dm644 "data/${APP_ID}.desktop" "${APP_DIR}/${APP_ID}.desktop"
install -Dm644 "data/${APP_ID}.svg" \
  "${ICON_ROOT}/scalable/apps/${APP_ID}.svg"
for size in 64 128 256; do
  install -Dm644 "data/png/${size}.png" \
    "${ICON_ROOT}/${size}x${size}/apps/${APP_ID}.png"
done

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "${ICON_ROOT}" >/dev/null 2>&1 || true
fi

echo "Installed lbreco to ${BIN_DIR}/lbreco"
echo "Launch it from your app menu, or run: lbreco"
