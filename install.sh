#!/bin/sh
# Build and install Camera Studio for the current user (no root needed).
#   PREFIX=/some/dir ./install.sh     install somewhere else than ~/.local
set -e
SRC="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
APP_ID=io.github.hsthe29.CameraStudio
OLD_APP_IDS="io.github.sytheho.CameraStudio"   # earlier builds used this id
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
MIN_RUST=1.85   # edition 2024

# version_ge A B: A >= B for dotted version numbers.
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; }
have_element() { gst-inspect-1.0 "$1" >/dev/null 2>&1; }
# gst-inspect only sees $PREFIX plugins when GST_PLUGIN_PATH points there.
PLUGIN_DIR="$PREFIX/lib/gstreamer-1.0"

# --- toolchain -----------------------------------------------------------------
if [ ! -x "$CARGO" ]; then
    echo "Rust is not installed. Install it with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
RUST_VERSION="$("$CARGO" --version | cut -d' ' -f2)"
version_ge "$RUST_VERSION" "$MIN_RUST" || {
    echo "Rust $MIN_RUST or newer is required (found $RUST_VERSION). Update with: rustup update stable"
    exit 1
}
command -v pkg-config >/dev/null || {
    echo "pkg-config is missing. Install the build dependencies with:"
    echo "  sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev"
    exit 1
}

# --- libraries (development files) ------------------------------------------------
MISSING=""
for pc in gtk4 libadwaita-1 gstreamer-1.0 gstreamer-video-1.0 gstreamer-app-1.0; do
    pkg-config --exists "$pc" 2>/dev/null || MISSING="$MISSING $pc"
done
if [ -n "$MISSING" ]; then
    echo "Missing development files for:$MISSING"
    echo "Install them with:"
    echo "  sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev"
    exit 1
fi
OLD=""
version_ge "$(pkg-config --modversion gtk4)" 4.14 || OLD="$OLD GTK 4.14 (found $(pkg-config --modversion gtk4))"
version_ge "$(pkg-config --modversion libadwaita-1)" 1.5 || OLD="$OLD libadwaita 1.5 (found $(pkg-config --modversion libadwaita-1))"
version_ge "$(pkg-config --modversion gstreamer-1.0)" 1.24 || OLD="$OLD GStreamer 1.24 (found $(pkg-config --modversion gstreamer-1.0))"
if [ -n "$OLD" ]; then
    echo "Too old, need at least:$OLD"
    echo "Camera Studio supports Ubuntu 24.04 and newer."
    exit 1
fi

# GTK 4.20 + libadwaita 1.8 (Ubuntu 26.04+) give the full interface; older
# versions (Ubuntu 24.04) build with simpler stand-in widgets (src/compat.rs).
FEATURES=""
if version_ge "$(pkg-config --modversion gtk4)" 4.20 && version_ge "$(pkg-config --modversion libadwaita-1)" 1.8; then
    echo "GTK $(pkg-config --modversion gtk4), libadwaita $(pkg-config --modversion libadwaita-1): full interface."
else
    echo "GTK $(pkg-config --modversion gtk4), libadwaita $(pkg-config --modversion libadwaita-1): compatibility interface."
    FEATURES="--no-default-features"
fi

# --- runtime plugins ------------------------------------------------------------------
# The viewfinder needs gtk4paintablesink; without it the app starts without a picture.
if ! have_element gtk4paintablesink; then
    if GST_PLUGIN_PATH="$PLUGIN_DIR${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}" have_element gtk4paintablesink; then
        echo "Note: gtk4paintablesink is in $PLUGIN_DIR but GStreamer does not look there."
        echo "  Add to ~/.profile and log in again:  export GST_PLUGIN_PATH=$PLUGIN_DIR"
    else
        echo "Warning: the GStreamer plugin gtk4paintablesink was not found (no picture without it)."
        echo "  If your distro packages it:  sudo apt install gstreamer1.0-gtk4"
        echo "  Otherwise (e.g. Ubuntu 24.04) build gst-plugin-gtk4 from source, see README.md"
    fi
fi
have_element glsinkbin || echo "Warning: glsinkbin not found: sudo apt install gstreamer1.0-plugins-bad gstreamer1.0-gl"
have_element pulsesrc || echo "Note: no audio source plugin (video without sound): sudo apt install gstreamer1.0-pulseaudio"

echo "Building (release)…"
(cd "$SRC" && "$CARGO" build --release --quiet $FEATURES)

# --- install -----------------------------------------------------------------------------
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
# Become the default video player only for formats nothing else handles yet;
# existing choices (e.g. a browser for WebM) are left alone.
if command -v xdg-mime >/dev/null; then
    for mime in $(sed -n 's/^MimeType=//p' "$PREFIX/share/applications/$APP_ID.desktop" | tr ';' ' '); do
        if [ -z "$(xdg-mime query default "$mime" 2>/dev/null)" ]; then
            xdg-mime default "$APP_ID.desktop" "$mime" && echo "Default app for $mime: Camera Studio"
        fi
    done
fi
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true

# --- GPU acceleration (optional) -------------------------------------------------------
GPU_HINT=""
command -v vainfo >/dev/null || GPU_HINT="$GPU_HINT vainfo"
have_element vajpegdec || GPU_HINT="$GPU_HINT gstreamer1.0-plugins-bad"
[ -e /usr/lib/x86_64-linux-gnu/dri/iHD_drv_video.so ] || [ -e /usr/lib/dri/iHD_drv_video.so ] \
    || GPU_HINT="$GPU_HINT intel-media-va-driver-non-free"
if [ -z "$GPU_HINT" ] && vainfo 2>/dev/null | grep -q VAProfileJPEGBaseline; then
    echo "GPU: hardware JPEG decoding available (VA-API) ✓"
elif [ -n "$GPU_HINT" ]; then
    echo "GPU: VA-API not ready, the app still works but uses the CPU. For Intel graphics:"
    echo "  sudo apt install$GPU_HINT"
else
    echo "GPU: no VA-API JPEG decoding on this machine, the app uses the CPU."
fi

echo "Installed Camera Studio $("$PREFIX/bin/camera-studio" --version | cut -d' ' -f3) to $PREFIX/bin."
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not in your PATH; open it from the app menu or run $PREFIX/bin/camera-studio" ;;
esac
echo "Open it from the app menu or run: camera-studio"
