#!/bin/sh
# Remove what install.sh added. Photos, videos and settings are kept unless
# you pass --purge (which deletes ~/.config/camera-studio and the thumbnail cache).
#   PREFIX=/some/dir ./uninstall.sh   if you installed with a different PREFIX
PREFIX="${PREFIX:-$HOME/.local}"
case "$1" in
    ""|--purge) ;;
    *) echo "Usage: $0 [--purge]"; exit 1 ;;
esac

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
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true

if [ "$1" = "--purge" ]; then
    rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/camera-studio" "${XDG_CACHE_HOME:-$HOME/.cache}/camera-studio"
    echo "Camera Studio removed, including its settings and cache (photos and videos are kept)."
else
    echo "Camera Studio removed (photos, videos and the settings in ~/.config/camera-studio are kept; use --purge to delete the settings too)."
fi
echo "The GStreamer plugin gtk4paintablesink (if you built it yourself) was left in place."
