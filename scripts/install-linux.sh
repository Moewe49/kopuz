#!/usr/bin/env bash
# Kopuz — Linux dependency installer.
#
# Installs the runtime tools Kopuz uses for YouTube Music playback fallback
# and SoundCloud/yt-dlp downloads:
#   - yt-dlp  (stream resolution bot-check fallback + SoundCloud search/download)
#   - ffmpeg  (audio remux/convert for downloads)
#
# Also installs the WebKitGTK runtime the AppImage needs if it's missing.
#
# Usage:  chmod +x install-linux.sh && ./install-linux.sh
set -e

have() { command -v "$1" >/dev/null 2>&1; }

install_pkgs() {
    if have apt-get; then
        sudo apt-get update
        sudo apt-get install -y "$@"
    elif have dnf; then
        sudo dnf install -y "$@"
    elif have pacman; then
        sudo pacman -S --needed --noconfirm "$@"
    elif have zypper; then
        sudo zypper install -y "$@"
    else
        echo "No supported package manager found (apt/dnf/pacman/zypper)."
        echo "Install these manually: $*"
        return 1
    fi
}

echo "Installing Kopuz dependencies..."

# yt-dlp + ffmpeg. yt-dlp is in every major distro repo now; fall back to pipx.
if have yt-dlp; then
    echo "  yt-dlp already installed."
else
    install_pkgs yt-dlp || {
        echo "  Falling back to pipx for yt-dlp..."
        install_pkgs pipx || true
        pipx install yt-dlp || pip install --user yt-dlp
    }
fi

if have ffmpeg; then
    echo "  ffmpeg already installed."
else
    install_pkgs ffmpeg
fi

# WebKitGTK runtime for the AppImage (webkit2gtk-4.1 + gtk3).
if have apt-get; then
    install_pkgs libwebkit2gtk-4.1-0 libgtk-3-0 || true
elif have dnf; then
    install_pkgs webkit2gtk4.1 gtk3 || true
elif have pacman; then
    install_pkgs webkit2gtk-4.1 gtk3 || true
fi

echo ""
echo "Done. Make the AppImage executable and run it:"
echo "  chmod +x Kopuz-x86_64.AppImage && ./Kopuz-x86_64.AppImage"
