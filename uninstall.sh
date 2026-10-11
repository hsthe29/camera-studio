#!/bin/sh
# Remove what install.sh added. Photos, videos and settings are kept.
PREFIX="${PREFIX:-$HOME/.local}"
rm -f "$PREFIX/bin/camera-studio"
for id in io.github.hsthe29.CameraStudio io.github.sytheho.CameraStudio; do
    rm -f "$PREFIX/share/applications/$id.desktop" "$PREFIX/share/icons/hicolor/scalable/apps/$id.svg"
done
rm -rf "$PREFIX/share/camera-studio"
# Forget the default-app entries install.sh may have added (others untouched).
MIMEAPPS="${XDG_CONFIG_HOME:-$HOME/.config}/mimeapps.list"
if [ -f "$MIMEAPPS" ]; then
    sed -i -e 's/io\.github\.hsthe29\.CameraStudio\.desktop;\{0,1\}//g' -e '/^[A-Za-z0-9.+_/-]*=$/d' "$MIMEAPPS"
fi
command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
echo "Camera Studio removed (photos, videos and the settings in ~/.config/camera-studio are kept)."
