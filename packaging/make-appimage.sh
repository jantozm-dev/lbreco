#!/usr/bin/env bash
# Build a self-contained AppImage from an already compiled release binary.
#
#   packaging/make-appimage.sh target/release/lbreco dist
#
# Run inside a Debian trixie (or newer) build container where the GTK 4.18 /
# libadwaita 1.6 development packages are installed.

set -euo pipefail

BIN="${1:-target/release/lbreco}"
OUT="${2:-dist}"
APP_ID="io.github.lbreco.LBreco"
APPDIR="${OUT}/AppDir"

if [[ ! -x "${BIN}" ]]; then
  echo "missing release binary: ${BIN}" >&2
  exit 1
fi

rm -rf "${APPDIR}"
mkdir -p \
  "${APPDIR}/usr/bin" \
  "${APPDIR}/usr/lib" \
  "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
  "${APPDIR}/usr/share/icons/hicolor/64x64/apps" \
  "${APPDIR}/usr/share/icons/hicolor/128x128/apps" \
  "${APPDIR}/usr/share/icons/hicolor/256x256/apps" \
  "${APPDIR}/usr/share/metainfo" \
  "${APPDIR}/usr/share/glib-2.0/schemas"

install -Dm755 "${BIN}" "${APPDIR}/usr/bin/lbreco"
install -Dm644 "data/${APP_ID}.desktop" \
  "${APPDIR}/usr/share/applications/${APP_ID}.desktop"
install -Dm644 "data/${APP_ID}.svg" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
for size in 64 128 256; do
  install -Dm644 "data/png/${size}.png" \
    "${APPDIR}/usr/share/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
done
install -Dm644 "data/${APP_ID}.metainfo.xml" \
  "${APPDIR}/usr/share/metainfo/${APP_ID}.metainfo.xml"
cp "data/png/256.png" "${APPDIR}/${APP_ID}.png"
cp "data/png/256.png" "${APPDIR}/.DirIcon"

# Bundle the libraries the binary needs, except the glibc/toolchain set that
# every host provides. The AppImage stays usable on older distributions.
while IFS= read -r lib; do
  case "${lib}" in
    */libc.so*|*/libm.so*|*/libpthread.so*|*/libdl.so*|*/librt.so*|*/ld-linux*|*/libgcc_s.so*|*/libstdc++.so*)
      continue
      ;;
    # WebKit spawns its own helper processes from the host installation;
    # bundling only the client library would mix versions and break playback.
    */libwebkit2gtk*|*/libwebkitgtk*|*/libjavascriptcoregtk*)
      continue
      ;;
  esac
  cp -L "${lib}" "${APPDIR}/usr/lib/" 2>/dev/null || true
done < <(ldd "${BIN}" | awk '/=> \// {print $3}')

# GTK data: modules, pixbuf loaders and GSettings schemas.
cp -a /usr/lib/x86_64-linux-gnu/gtk-4.0 "${APPDIR}/usr/lib/" 2>/dev/null || true
cp -a /usr/lib/x86_64-linux-gnu/gdk-pixbuf-2.0 "${APPDIR}/usr/lib/" 2>/dev/null || true
cp -a /usr/share/glib-2.0/schemas/*.gschema.xml \
  "${APPDIR}/usr/share/glib-2.0/schemas/" 2>/dev/null || true
glib-compile-schemas "${APPDIR}/usr/share/glib-2.0/schemas" 2>/dev/null || true

LOADER_DIR="${APPDIR}/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders"
if command -v gdk-pixbuf-query-loaders >/dev/null 2>&1 && [[ -d "${LOADER_DIR}" ]]; then
  GDK_PIXBUF_MODULEDIR="${LOADER_DIR}" \
    gdk-pixbuf-query-loaders > "${APPDIR}/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache" \
    2>/dev/null || true
fi

cat > "${APPDIR}/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export GSETTINGS_SCHEMA_DIR="$HERE/usr/share/glib-2.0/schemas"
export GDK_PIXBUF_MODULE_FILE="$HERE/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
export GTK_DATA_PREFIX="$HERE/usr"
exec "$HERE/usr/bin/lbreco" "$@"
RUN
chmod +x "${APPDIR}/AppRun"

TOOL="${APPIMAGETOOL:-/tmp/appimagetool-x86_64.AppImage}"
if [[ ! -x "${TOOL}" ]]; then
  wget -q -O "${TOOL}" \
    https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "${TOOL}"
fi

ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "${TOOL}" --no-appstream \
  "${APPDIR}" "${OUT}/lbreco-$(date +%Y%m%d)-x86_64.AppImage"
