#!/bin/sh
# Build and install Camera Studio for the current user (no root needed).
set -e
SRC="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID=io.github.hsthe29.CameraStudio
OLD_APP_IDS="io.github.sytheho.CameraStudio"   # earlier builds used this id
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"

if [ ! -x "$CARGO" ]; then
    echo "Rust is not installed. Install it with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
for pc in gtk4 libadwaita-1 gstreamer-1.0 gstreamer-video-1.0 gstreamer-app-1.0; do
    pkg-config --exists "$pc" 2>/dev/null || {
        echo "Missing development files ($pc). Install them with:"
        echo "  sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-gtk4"
        exit 1
    }
done

echo "Building (release)…"
(cd "$SRC" && "$CARGO" build --release --quiet)

install -Dm755 "$SRC/target/release/camera-studio" "$PREFIX/bin/camera-studio"
mkdir -p "$PREFIX/share/camera-studio"
rm -rf "$PREFIX/share/camera-studio/icons"
cp -r "$SRC/data/icons" "$PREFIX/share/camera-studio/icons"
install -Dm644 "$SRC/data/icons/hicolor/scalable/apps/$APP_ID.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
for old in $OLD_APP_IDS; do
    rm -f "$PREFIX/share/applications/$old.desktop" "$PREFIX/share/icons/hicolor/scalable/apps/$old.svg"
done
mkdir -p "$PREFIX/share/applications"
sed "s|@EXEC@|$PREFIX/bin/camera-studio|" "$SRC/data/$APP_ID.desktop.in" > "$PREFIX/share/applications/$APP_ID.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true

if command -v vainfo >/dev/null && vainfo 2>/dev/null | grep -q VAProfileJPEGBaseline; then
    echo "GPU: hardware JPEG decoding available (VA-API) ✓"
else
    echo "GPU: no VA-API — the app still works but decodes on the CPU (hint: sudo apt install intel-media-va-driver-non-free)"
fi
echo "Installed Camera Studio $("$PREFIX/bin/camera-studio" --version | cut -d' ' -f3) to $PREFIX/bin. Open it from the app menu or run: camera-studio"
