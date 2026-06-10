# Kopuz — Windows dependency installer.
#
# Installs the runtime tools Kopuz uses for YouTube Music playback fallback
# and SoundCloud/yt-dlp downloads:
#   - yt-dlp  (stream resolution bot-check fallback + SoundCloud search/download)
#   - ffmpeg  (audio remux/convert for downloads)
#
# Run once after installing Kopuz:
#   powershell -ExecutionPolicy Bypass -File install-windows.ps1
#
# Re-running is safe — winget skips already-installed packages.

$ErrorActionPreference = "Stop"

function Has($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

if (-not (Has "winget")) {
    Write-Host "winget not found. Install 'App Installer' from the Microsoft Store, then re-run." -ForegroundColor Yellow
    exit 1
}

Write-Host "Installing Kopuz dependencies (yt-dlp, ffmpeg)..." -ForegroundColor Cyan

if (Has "yt-dlp") {
    Write-Host "  yt-dlp already installed." -ForegroundColor Green
} else {
    winget install --id yt-dlp.yt-dlp -e --accept-source-agreements --accept-package-agreements
}

if (Has "ffmpeg") {
    Write-Host "  ffmpeg already installed." -ForegroundColor Green
} else {
    winget install --id Gyan.FFmpeg -e --accept-source-agreements --accept-package-agreements
}

Write-Host ""
Write-Host "Done. Restart Kopuz so it picks up the new PATH." -ForegroundColor Green
