#!/usr/bin/env bash
# Bundles the Homebrew release build of Aureus into an Aureus.app inside a zip
# under dist/. Run from the repository root on macOS after
# `cargo build --release --locked`, with Homebrew's gtk4, libadwaita, librsvg,
# adwaita-icon-theme, hicolor-icon-theme and dylibbundler installed.
#
# Usage: scripts/package-macos.sh <bundle-name>
set -euo pipefail

name="${1:?usage: scripts/package-macos.sh <bundle-name>}"
brew_prefix="$(brew --prefix)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
app="dist/${name}/Aureus.app"
contents="${app}/Contents"
resources="${contents}/Resources"
loaders="${resources}/lib/gdk-pixbuf-2.0/2.10.0/loaders"

rm -rf "dist/${name}" "dist/${name}.zip"
mkdir -p "${contents}/MacOS" "${contents}/Frameworks" "${resources}/bin" "${resources}/lib" "${loaders}" \
    "${resources}/share/glib-2.0/schemas" "${resources}/share/icons/hicolor/scalable"

cp target/release/aureus "${resources}/bin/aureus"

# GdkPixbuf loaders render the SVG app icon and symbolic icons.
cp -L "${brew_prefix}"/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.so "${loaders}/"

# The loader cache needs absolute paths, so the launcher rewrites this
# template for the bundle's location on every start. It must be generated
# before dylibbundler rewrites the loaders' library paths.
gdk-pixbuf-query-loaders "${loaders}"/*.so \
    | sed "s|${loaders}|@LOADERS@|g" > "${resources}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache.in"

# Copy every Homebrew dylib the executable and loaders use into
# Contents/Frameworks and point them at the bundled copies. The loaders run
# inside the aureus process, so @executable_path is Resources/bin for both.
loader_args=()
for loader in "${loaders}"/*.so; do
    loader_args+=(-x "${loader}")
done
dylibbundler --overwrite-dir --bundle-deps --no-codesign \
    -x "${resources}/bin/aureus" "${loader_args[@]}" \
    -d "${contents}/Frameworks/" -p "@executable_path/../../Frameworks/"

cp "${brew_prefix}/share/glib-2.0/schemas/gschemas.compiled" "${resources}/share/glib-2.0/schemas/"
cp -RL "${brew_prefix}/share/icons/Adwaita" "${resources}/share/icons/"
cp "${brew_prefix}/share/icons/hicolor/index.theme" "${resources}/share/icons/hicolor/"
cp -R data/icons/hicolor/scalable/apps "${resources}/share/icons/hicolor/scalable/"

cat > "${contents}/MacOS/Aureus" <<'LAUNCHER'
#!/bin/bash
# Points GTK at the files bundled in Aureus.app, then starts Aureus.
resources="$(cd "$(dirname "$0")/../Resources" && pwd)"
cache="${HOME}/Library/Caches/io.github.Mars7x.Aureus"
mkdir -p "${cache}"
loaders="${resources}/lib/gdk-pixbuf-2.0/2.10.0"
sed "s|@LOADERS@|${loaders}/loaders|g" "${loaders}/loaders.cache.in" > "${cache}/loaders.cache"
export GDK_PIXBUF_MODULE_FILE="${cache}/loaders.cache"
export GDK_PIXBUF_MODULEDIR="${loaders}/loaders"
export GSETTINGS_SCHEMA_DIR="${resources}/share/glib-2.0/schemas"
export XDG_DATA_DIRS="${resources}/share"
exec "${resources}/bin/aureus" "$@"
LAUNCHER
chmod +x "${contents}/MacOS/Aureus"

iconset="dist/${name}/Aureus.iconset"
mkdir -p "${iconset}"
for size in 16 32 128 256 512; do
    rsvg-convert -w "${size}" -h "${size}" data/icons/hicolor/scalable/apps/io.github.Mars7x.Aureus.svg \
        -o "${iconset}/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) data/icons/hicolor/scalable/apps/io.github.Mars7x.Aureus.svg \
        -o "${iconset}/icon_${size}x${size}@2x.png"
done
iconutil -c icns "${iconset}" -o "${resources}/Aureus.icns"
rm -rf "${iconset}"

cat > "${contents}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>Aureus</string>
  <key>CFBundleDisplayName</key>
  <string>Aureus</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.Mars7x.Aureus</string>
  <key>CFBundleExecutable</key>
  <string>Aureus</string>
  <key>CFBundleIconFile</key>
  <string>Aureus</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

# Bundled binaries were modified, so give everything an ad-hoc signature.
# The app is not notarized; macOS asks users to confirm the first launch.
find "${contents}/Frameworks" "${loaders}" -type f \( -name '*.dylib' -o -name '*.so' \) -exec codesign --force --sign - {} \;
codesign --force --sign - "${resources}/bin/aureus"
codesign --force --sign - "${app}"

cp LICENSE "dist/${name}/LICENSE.txt"
cat > "dist/${name}/README.txt" <<'README'
Aureus for macOS

Move Aureus.app to Applications. Aureus is not notarized, so the first time
you open it, Control-click Aureus.app and choose Open, then confirm.
README

(cd dist && ditto -c -k --keepParent "${name}" "${name}.zip")
echo "Created dist/${name}.zip"
