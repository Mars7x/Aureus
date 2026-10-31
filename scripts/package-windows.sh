#!/usr/bin/env bash
# Bundles the MSYS2 UCRT64 release build of Aureus into a portable folder and
# zip under dist/. Run from the repository root in an MSYS2 UCRT64 shell after
# `cargo build --release --locked`.
#
# Usage: scripts/package-windows.sh <bundle-name>
set -euo pipefail

name="${1:?usage: scripts/package-windows.sh <bundle-name>}"
prefix="${MINGW_PREFIX:?run this inside an MSYS2 UCRT64 shell}"
dist="dist/${name}"

rm -rf "${dist}" "dist/${name}.zip"
mkdir -p "${dist}/bin" "${dist}/lib" "${dist}/share/glib-2.0/schemas" "${dist}/share/icons/hicolor"

cp target/release/aureus.exe "${dist}/bin/"

# GTK finds share/ and lib/ relative to the folder holding its DLLs, so the
# bundle mirrors the MSYS2 prefix layout: bin/, lib/ and share/.
# GdkPixbuf loaders render the SVG app icon and symbolic icons.
cp -r "${prefix}/lib/gdk-pixbuf-2.0" "${dist}/lib/"
if grep -qiE '[a-z]:[\\/]' "${dist}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"; then
    echo "::warning::loaders.cache contains absolute paths; icons may not load"
fi

# Copy every MSYS2 DLL that the executable and the pixbuf loaders link against.
ldd "${dist}/bin/aureus.exe" "${dist}"/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.dll \
    | awk -v prefix="${prefix}/" 'index($3, prefix) == 1 { print $3 }' \
    | sort -u \
    | while read -r dll; do
        cp -n "${dll}" "${dist}/bin/"
    done

cp "${prefix}/share/glib-2.0/schemas/gschemas.compiled" "${dist}/share/glib-2.0/schemas/"

cp -r "${prefix}/share/icons/Adwaita" "${dist}/share/icons/"
cp "${prefix}/share/icons/hicolor/index.theme" "${dist}/share/icons/hicolor/"
mkdir -p "${dist}/share/icons/hicolor/scalable"
cp -r data/icons/hicolor/scalable/apps "${dist}/share/icons/hicolor/scalable/"
gtk4-update-icon-cache --quiet --ignore-theme-index --force "${dist}/share/icons/hicolor"

cp LICENSE "${dist}/LICENSE.txt"
cat > "${dist}/README.txt" <<'README'
Aureus for Windows

Start Aureus by running bin\aureus.exe. Keep the bin, lib and share folders
together; Aureus needs all three.

Your portfolio is stored in %LOCALAPPDATA%\io.github.Mars7x.Aureus.
README

(cd dist && zip -qr "${name}.zip" "${name}")
echo "Created dist/${name}.zip"
