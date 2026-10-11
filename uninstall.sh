#!/bin/sh
# Remove what install.sh added. Photos, videos and settings are kept.
PREFIX="${PREFIX:-$HOME/.local}"
rm -f "$PREFIX/bin/camera-studio"
for id in io.github.hsthe29.CameraStudio io.github.sytheho.CameraStudio; do
    rm -f "$PREFIX/share/applications/$id.desktop" "$PREFIX/share/icons/hicolor/scalable/apps/$id.svg"
done
rm -rf "$PREFIX/share/camera-studio"
echo "Camera Studio removed (photos, videos and the settings in ~/.config/camera-studio are kept)."
