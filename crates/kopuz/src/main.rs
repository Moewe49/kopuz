#[cfg(target_arch = "wasm32")]
use crate::web_storage::{
    clear_web_queue_state, load_web_config, load_web_favorites, load_web_library,
    load_web_playlists, load_web_queue_state, load_web_ui_state, save_web_config,
    save_web_favorites, save_web_library, save_web_playlists, save_web_queue_state,
    save_web_ui_state,
};
use components::{
    bottombar::Bottombar, download_overlay::DownloadOverlay, fullscreen::Fullscreen,
    rightbar::Rightbar, sidebar::Sidebar, titlebar::Titlebar,
};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use dioxus::desktop::RequestAsyncResponder;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use dioxus::desktop::tao::dpi::LogicalSize;
#[cfg(all(not(target_arch = "wasm32"), target_os = "macos"))]
use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
use dioxus::desktop::tao::platform::windows::WindowExtWindows;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use dioxus::desktop::tao::window::Icon;
use dioxus::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use discord_presence::Presence;
use kopuz_route::Route;
use pages::server::download_manager::DownloadQueue;
use player::player::Player;
use queue_state::PersistedQueueState;
use reader::FavoritesStore;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
use windows::Win32::Foundation::HWND;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod pot_minter;
// Shared BgUtils minter JS (desktop wry + Android System WebView).
#[cfg(not(target_arch = "wasm32"))]
mod pot_minter_script;
// Android PoToken minter driver (headless System WebView via JNI).
#[cfg(target_os = "android")]
mod pot_minter_android;
mod queue_state;
mod web_storage;
#[cfg(target_os = "windows")]
mod windows_titlebar;

#[cfg(not(target_arch = "wasm32"))]
fn migrate_legacy_locations() {
    let Some(dirs) = directories::ProjectDirs::from("com", "temidaradev", "kopuz") else {
        return;
    };
    let new_config = dirs.config_dir().to_path_buf();
    let sentinel = new_config.join(".migrated");
    if sentinel.exists() {
        return;
    }

    let old_cache = dirs.cache_dir().to_path_buf();
    let files = [
        "library.json",
        "playlists.json",
        "favorites.json",
        "queue_state.json",
    ];
    for file in files {
        let src = old_cache.join(file);
        let dst = new_config.join(file);
        if src.exists() && !dst.exists() {
            if let Err(e) = std::fs::rename(&src, &dst) {
                tracing::warn!("Failed to migrate {file} from cache to config: {e}");
            } else {
                tracing::info!("Migrated {file} to config dir");
            }
        }
    }

    let _ = std::fs::write(&sentinel, "");
}

const FAVICON: Asset = asset!("../assets/favicon.ico");
const MAIN_CSS: Asset = asset!("../assets/main.css");
const THEME_CSS: Asset = asset!("../assets/themes.css");
const TAILWIND_CSS: Asset = asset!("../assets/tailwind.css");
const REDUCED_ANIMATIONS_CSS: Asset = asset!("../assets/reduced-animations.css");
#[cfg(target_os = "windows")]
const TOOLBAR_ICONS: Asset = asset!("../assets/toolbar_icons", AssetOptions::folder());
const QUEUE_STATE_SAVE_DEBOUNCE_MS: u64 = 1200;
const QUEUE_STATE_PROGRESS_STEP_SECS: u64 = 5;
// Coalesce rapid config mutations (listen-count bumps on every skip,
// recently-played pushes on every track change, volume scrubbing, settings
// sliders, bulk-download offline_tracks inserts) into a single disk write
// instead of serializing the whole AppConfig — HashMaps and all — every time.
const CONFIG_SAVE_DEBOUNCE_MS: u64 = 800;

#[cfg(target_os = "windows")]
#[component]
fn WindowsToolbarIconAssets() -> Element {
    rsx! {
        div {
            hidden: true,
            "data-toolbar-icons": "{TOOLBAR_ICONS}",
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[component]
fn WindowsToolbarIconAssets() -> Element {
    rsx! {}
}

#[cfg(not(target_arch = "wasm32"))]
static PRESENCE: std::sync::OnceLock<Option<Arc<Presence>>> = std::sync::OnceLock::new();

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
fn build_window_icon() -> Option<Icon> {
    let image = image::load_from_memory(include_bytes!("../assets/logo-512.png")).ok()?;
    let image = image.into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct AvailableUpdate {
    version: String,
    release_url: String,
    /// Download URL of the release asset that installs THIS platform's build
    /// (`.apk` on Android, `setup.exe`/`.msi` on Windows, `.AppImage` on Linux).
    /// `None` when the release has no matching asset — the banner then only
    /// offers the "View release" page link.
    installer_url: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_version_parts(version: &str) -> Option<Vec<u64>> {
    let core = version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    let parts: Option<Vec<u64>> = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect();
    parts.filter(|parts| !parts.is_empty())
}

#[cfg(not(target_arch = "wasm32"))]
fn is_newer_version(current: &str, candidate: &str) -> bool {
    let Some(current_parts) = parse_version_parts(current) else {
        return false;
    };
    let Some(candidate_parts) = parse_version_parts(candidate) else {
        return false;
    };

    let max_len = current_parts.len().max(candidate_parts.len());
    for idx in 0..max_len {
        let current_part = *current_parts.get(idx).unwrap_or(&0);
        let candidate_part = *candidate_parts.get(idx).unwrap_or(&0);
        match candidate_part.cmp(&current_part) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }

    false
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_available_update() -> Option<AvailableUpdate> {
    let client = reqwest::Client::builder()
        .user_agent(format!("kopuz/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    // This fork's own releases (Moewe49/kopuz), not the upstream Kopuz-org repo —
    // otherwise the in-app updater would offer upstream builds that don't carry
    // this fork's Android work.
    let release = client
        .get("https://api.github.com/repos/Moewe49/kopuz/releases/latest")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<GithubRelease>()
        .await
        .ok()?;

    if is_newer_version(env!("CARGO_PKG_VERSION"), &release.tag_name) {
        Some(AvailableUpdate {
            version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
            release_url: release.html_url,
            installer_url: pick_installer_asset(&release.assets),
        })
    } else {
        None
    }
}

/// Pick the release asset that installs the current platform's build, matched by
/// filename suffix. `None` if the release carries no asset for this OS (the
/// banner then falls back to the release-page link).
#[cfg(not(target_arch = "wasm32"))]
fn pick_installer_asset(assets: &[GithubReleaseAsset]) -> Option<String> {
    let pick = |pred: &dyn Fn(&str) -> bool| {
        assets
            .iter()
            .find(|a| pred(&a.name.to_ascii_lowercase()))
            .map(|a| a.browser_download_url.clone())
    };
    #[cfg(target_os = "android")]
    {
        pick(&|n| n.ends_with(".apk"))
    }
    #[cfg(target_os = "windows")]
    {
        // ONLY the portable .zip. It carries exactly the installed layout
        // (kopuz.exe + assets/), so the updater applies it over the install
        // directory itself: no setup wizard, no path prompts, no elevation (the
        // app installs per-user under %LOCALAPPDATA%\Programs).
        //
        // There is deliberately no installer fallback. Running the NSIS setup
        // over an existing installation destroyed a user's %APPDATA% config —
        // session, playlists, favourites — because the upgrade runs the old
        // uninstaller first. An updater that can do that to someone is worse
        // than an updater that finds nothing to do, so a release without a
        // portable zip simply offers the release page instead.
        pick(&|n| n.contains("portable") && n.ends_with(".zip"))
    }
    #[cfg(target_os = "linux")]
    {
        pick(&|n| n.ends_with(".appimage"))
    }
    #[cfg(target_os = "macos")]
    {
        pick(&|n| n.ends_with(".dmg"))
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "windows",
        target_os = "linux",
        target_os = "macos"
    )))]
    {
        let _ = pick;
        None
    }
}

/// Kick off an in-app update: download the platform installer, then apply it.
/// Android hands the APK to the package installer (FileProvider); desktop runs
/// the downloaded installer and exits so it can replace the running files. Both
/// still show the OS's own confirmation — a sideloaded / self-updating app can't
/// swap itself silently. Progress/errors surface via `on_status`.
#[cfg(not(target_arch = "wasm32"))]
fn start_update(installer_url: String, version: String, mut on_status: Signal<Option<String>>) {
    on_status.set(Some("downloading".to_string()));
    spawn(async move {
        let dest = update_dest_path(&installer_url);
        // Already downloaded this exact version? Install it straight away.
        // Android sends the user to the "install unknown apps" settings page the
        // first time, and the tap that comes back afterwards used to re-download
        // the whole APK. Keeping the staged file means the second tap installs
        // immediately.
        let ok = if staged_version(&dest).as_deref() == Some(version.as_str()) {
            true
        } else {
            let downloaded = async {
                let client = reqwest::Client::builder()
                    .user_agent(format!("kopuz/{}", env!("CARGO_PKG_VERSION")))
                    .timeout(std::time::Duration::from_secs(300))
                    .build()
                    .ok()?;
                let bytes = client
                    .get(&installer_url)
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .bytes()
                    .await
                    .ok()?;
                let dest = dest.clone();
                tokio::task::spawn_blocking(move || std::fs::write(&dest, &bytes))
                    .await
                    .ok()?
                    .ok()?;
                Some(())
            }
            .await
            .is_some();
            if downloaded {
                mark_staged_version(&dest, &version);
            }
            downloaded
        };
        if ok {
            on_status.set(Some("installing".to_string()));
            launch_update(&dest);
            on_status.set(None);
        } else {
            on_status.set(Some("failed".to_string()));
        }
    });
}

/// Sidecar recording which release the staged installer belongs to, so a
/// half-finished update can be resumed without paying for the download twice.
#[cfg(not(target_arch = "wasm32"))]
fn staged_marker_path(dest: &std::path::Path) -> std::path::PathBuf {
    dest.with_extension("staged-version")
}

#[cfg(not(target_arch = "wasm32"))]
fn staged_version(dest: &std::path::Path) -> Option<String> {
    let size = std::fs::metadata(dest).ok()?.len();
    if size == 0 {
        return None;
    }
    let marker = std::fs::read_to_string(staged_marker_path(dest)).ok()?;
    let marker = marker.trim();
    (!marker.is_empty()).then(|| marker.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn mark_staged_version(dest: &std::path::Path, version: &str) {
    let _ = std::fs::write(staged_marker_path(dest), version);
}

/// Where to stage the downloaded installer. Android needs it in the app's files
/// dir so the FileProvider can serve it to the package installer; desktop uses
/// the temp dir. The extension is preserved so the OS opens it correctly.
#[cfg(not(target_arch = "wasm32"))]
fn update_dest_path(url: &str) -> std::path::PathBuf {
    let lower = url.to_ascii_lowercase();
    let name = if lower.ends_with(".apk") {
        "kopuz-update.apk"
    } else if lower.ends_with(".zip") {
        "kopuz-update.zip"
    } else if lower.ends_with(".msi") {
        "kopuz-update.msi"
    } else if lower.ends_with(".appimage") {
        "kopuz-update.AppImage"
    } else if lower.ends_with(".dmg") {
        "kopuz-update.dmg"
    } else {
        "kopuz-update-setup.exe"
    };
    #[cfg(target_os = "android")]
    let dir = player::systemint::get_files_dir()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(not(target_os = "android"))]
    let dir = std::env::temp_dir();
    dir.join(name)
}

/// Apply a downloaded update. Platform-specific: Android → package installer;
/// Windows → run the installer and exit; Linux → replace the AppImage in place
/// and relaunch (or open it); macOS → open the disk image.
#[cfg(not(target_arch = "wasm32"))]
fn launch_update(path: &std::path::Path) {
    #[cfg(target_os = "android")]
    {
        player::systemint::install_apk(&path.to_string_lossy());
    }
    #[cfg(target_os = "windows")]
    {
        // Normal path: the portable .zip, applied over the installation right
        // here. No installer process at all — which is the point, since the
        // unsigned NSIS setup.exe is what Defender's ML heuristic flags.
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            match apply_zip_update(path) {
                Ok(exe) => {
                    let _ = std::process::Command::new(exe).spawn();
                    std::process::exit(0);
                }
                Err(e) => {
                    tracing::error!("in-place update failed: {e}");
                    return;
                }
            }
        }
        // Fallback for releases without a portable zip: hand over to the
        // installer and exit so it can replace the running binary.
        let is_msi = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("msi"))
            .unwrap_or(false);
        let spawned = if is_msi {
            std::process::Command::new("msiexec")
                .arg("/i")
                .arg(path)
                .spawn()
                .is_ok()
        } else {
            std::process::Command::new(path).spawn().is_ok()
        };
        if spawned {
            std::process::exit(0);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        // Running as an AppImage? Overwrite ourselves in place and relaunch.
        // The kernel keeps the open inode, so overwriting the file is safe.
        if let Ok(current) = std::env::var("APPIMAGE") {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::copy(path, &current).is_ok() {
                let _ = std::fs::set_permissions(
                    &current,
                    std::fs::Permissions::from_mode(0o755),
                );
                let _ = std::process::Command::new(&current).spawn();
                std::process::exit(0);
            }
        }
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Unpack the portable zip over the current installation and return the path of
/// the new executable to start.
///
/// Windows refuses to *overwrite* a running .exe but happily *renames* it, so
/// every file that is in the way is moved aside to `<name>.old` (swept on the
/// next launch, when it is no longer locked) and the new one takes its place.
/// Everything is extracted to a staging directory first — a download that dies
/// halfway must never leave a half-replaced installation behind.
///
/// Doing it in-process is deliberate: the obvious alternative, dropping a batch
/// file into %TEMP% and running it through cmd.exe, is precisely the shape
/// antivirus heuristics score as a dropper.
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
fn apply_zip_update(zip_path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| "cannot determine the install directory".to_string())?;
    apply_zip_update_into(zip_path, install_dir)
}

/// The testable core of [`apply_zip_update`], with the target directory passed
/// in rather than derived from the running process.
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
fn apply_zip_update_into(
    zip_path: &std::path::Path,
    install_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let install_dir = install_dir.to_path_buf();

    let file = std::fs::File::open(zip_path).map_err(|e| format!("open archive: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read archive: {e}"))?;

    let staging = install_dir.join(".kopuz-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| {
        format!(
            "cannot write to {} ({e}) — an install outside your user profile needs admin rights",
            install_dir.display()
        )
    })?;

    let mut staged: Vec<std::path::PathBuf> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("archive entry {i}: {e}"))?;
        // enclosed_name() rejects absolute paths and `..` traversal. Without it
        // a crafted archive could write anywhere on disk ("zip slip").
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let out = staging.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("staging dir: {e}"))?;
        }
        let mut dest = std::fs::File::create(&out)
            .map_err(|e| format!("staging {}: {e}", rel.display()))?;
        std::io::copy(&mut entry, &mut dest)
            .map_err(|e| format!("extracting {}: {e}", rel.display()))?;
        staged.push(rel);
    }

    // A zip without the binary would "succeed" into a broken install.
    let has_exe = staged.iter().any(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("kopuz.exe"))
    });
    if !has_exe {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("the archive contains no kopuz.exe".into());
    }

    for rel in &staged {
        let dest = install_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("install dir: {e}"))?;
        }
        if dest.exists() {
            let aside = dest.with_file_name(format!(
                "{}.old",
                dest.file_name().unwrap_or_default().to_string_lossy()
            ));
            let _ = std::fs::remove_file(&aside);
            std::fs::rename(&dest, &aside)
                .map_err(|e| format!("cannot move {} aside: {e}", dest.display()))?;
        }
        std::fs::rename(staging.join(rel), &dest)
            .map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(install_dir.join("kopuz.exe"))
}

/// Delete the `<name>.old` files the previous in-place update left behind. They
/// could not be removed then — the running process still held them open.
#[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
fn sweep_update_leftovers() {
    fn sweep(dir: &std::path::Path, depth: u8) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth > 0 {
                    sweep(&path, depth - 1);
                }
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("old"))
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        sweep(dir, 2);
    }
}

/// The in-app "Update" button for the banner: downloads the platform installer
/// and applies it. Shows a transient "Update…" label while it runs, and a
/// retry label after a failure. Hidden when the release has no matching asset.
#[cfg(not(target_arch = "wasm32"))]
fn update_button(
    installer_url: Option<String>,
    version: String,
    update_status: Signal<Option<String>>,
) -> Element {
    let status_now = update_status.read().clone();
    if matches!(status_now.as_deref(), Some("downloading") | Some("installing")) {
        return rsx! {
            span { class: "ml-2 text-xs opacity-80", "Update…" }
        };
    }
    let Some(url) = installer_url else {
        return rsx! {};
    };
    let label = if status_now.as_deref() == Some("failed") {
        "Erneut versuchen"
    } else {
        "Update"
    };
    rsx! {
        button {
            class: "ml-2 px-2 py-0.5 text-xs rounded bg-sky-500/30 hover:bg-sky-500/50 transition-colors",
            onclick: move |_| start_update(url.clone(), version.clone(), update_status),
            "{label}"
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_config_snapshot(config_snapshot: config::AppConfig, path: std::path::PathBuf) {
    spawn(async move {
        let result = tokio::task::spawn_blocking(move || config_snapshot.save(&path)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Failed to save config: {}", e),
            Err(e) => tracing::error!("Failed to join config save task: {}", e),
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn persist_config_snapshot(config_snapshot: config::AppConfig, _path: std::path::PathBuf) {
    save_web_config(&config_snapshot);
}

#[cfg(not(target_arch = "wasm32"))]
/// "Stay signed in automatically": pull a fresh YouTube cookie header from a
/// signed-in desktop browser and adopt it as the active session. Runs on boot
/// and periodically so a pasted session never has to be refreshed by hand.
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
async fn run_auto_refresh(mut config: Signal<config::AppConfig>) {
    let active_yt = config
        .peek()
        .server
        .as_ref()
        .map(|s| s.service == config::MusicService::YtMusic)
        .unwrap_or(false);
    if !active_yt || !config.peek().yt_auto_refresh {
        return;
    }
    match server::ytmusic::manual_cookies::extract_from_any_browser().await {
        Ok(fresh) => {
            let user_id =
                server::ytmusic::derive_user_id(&fresh).unwrap_or_else(|| "me".to_string());
            let mut cfg = config.write();
            let saved_id = cfg.server.as_ref().and_then(|s| s.id.clone());
            if let Some(srv) = cfg.server.as_mut() {
                // Only swap if it actually changed, to avoid churn.
                if srv.access_token.as_deref() != Some(fresh.as_str()) {
                    srv.access_token = Some(fresh.clone());
                    srv.user_id = Some(user_id);
                    srv.yt_manual = true;
                    eprintln!("[yt-auto] refreshed cookies from browser");
                }
            }
            if let Some(id) = saved_id
                && let Some(saved) = cfg.servers.iter_mut().find(|s| s.id == id)
            {
                saved.yt_manual = true;
                saved.yt_saved_cookies = Some(fresh);
            }
        }
        Err(e) => eprintln!("[yt-auto] browser refresh failed: {e}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Mint a fresh OAuth access token from the stored refresh token and adopt it
/// as the active YT session (`oauth:<access>`). Runs on boot and periodically
/// (access tokens live ~1h). Browser-free, so it works on every platform —
/// this is the "set up once, always signed in" path.
async fn run_oauth_refresh(mut config: Signal<config::AppConfig>) {
    let (active_yt, refresh_token, client_id, client_secret) = {
        let c = config.peek();
        (
            c.server
                .as_ref()
                .map(|s| s.service == config::MusicService::YtMusic)
                .unwrap_or(false),
            c.yt_oauth_refresh_token.clone(),
            c.yt_oauth_client_id.clone(),
            c.yt_oauth_client_secret.clone(),
        )
    };
    if !active_yt || refresh_token.is_empty() || client_id.is_empty() {
        return;
    }
    match server::ytmusic::oauth::refresh(&client_id, &client_secret, &refresh_token).await {
        Ok((access, _ttl)) => {
            let sentinel = server::ytmusic::oauth::to_sentinel(&access);
            let mut cfg = config.write();
            if let Some(srv) = cfg.server.as_mut() {
                srv.access_token = Some(sentinel.clone());
                srv.yt_manual = true;
            }
            let saved_id = cfg.server.as_ref().and_then(|s| s.id.clone());
            if let Some(id) = saved_id
                && let Some(saved) = cfg.servers.iter_mut().find(|s| s.id == id)
            {
                saved.yt_manual = true;
                saved.yt_saved_cookies = Some(sentinel);
            }
            eprintln!("[yt-oauth] refreshed access token");
        }
        Err(e) => eprintln!("[yt-oauth] token refresh failed: {e}"),
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
/// Silent "auto-login" refresh: for a YT server set up via the managed-browser
/// sign-in, drive its persistent profile HEADLESS via CDP to pull freshly
/// rotated cookies (no visible window). Runs on boot + periodically so the
/// session the user logged into once stays alive until they actually log out.
async fn run_browser_refresh(mut config: Signal<config::AppConfig>) {
    let (browser, server_id) = {
        let c = config.peek();
        let srv = c.server.as_ref();
        let is_yt = srv.map(|s| s.service == config::MusicService::YtMusic).unwrap_or(false);
        // Only browser-login servers (yt_browser set, not manual paste).
        let browser = srv.filter(|_| is_yt).and_then(|s| (!s.yt_manual).then_some(()).and(s.yt_browser));
        (browser, srv.and_then(|s| s.id.clone()).unwrap_or_default())
    };
    let Some(browser) = browser else { return };

    let profile = server::ytmusic::isolated_profile::profile_dir(&server_id);
    if !profile.is_dir() {
        return;
    }
    match server::ytmusic::cdp::fetch_cookies(
        browser,
        &profile,
        true,
        std::time::Duration::from_secs(25),
    )
    .await
    {
        Ok(fresh) if !fresh.is_empty() => {
            let user_id =
                server::ytmusic::derive_user_id(&fresh).unwrap_or_else(|| "me".to_string());
            let mut cfg = config.write();
            let saved_id = cfg.server.as_ref().and_then(|s| s.id.clone());
            if let Some(srv) = cfg.server.as_mut()
                && srv.access_token.as_deref() != Some(fresh.as_str())
            {
                srv.access_token = Some(fresh.clone());
                srv.user_id = Some(user_id);
                eprintln!("[yt-browser] refreshed cookies via headless CDP");
            }
            if let Some(id) = saved_id
                && let Some(saved) = cfg.servers.iter_mut().find(|s| s.id == id)
            {
                saved.yt_browser = Some(browser);
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("[yt-browser] headless refresh skipped: {e}"),
    }
}

async fn run_rotation(mut config: Signal<config::AppConfig>) {
    let cookies = match config.peek().server.as_ref() {
        Some(s) if s.service == config::MusicService::YtMusic => s.access_token.clone(),
        _ => return,
    };
    let Some(cookies) = cookies else { return };
    // Anonymous YT carries an empty token — nothing to keep alive.
    if cookies.is_empty() {
        return;
    }
    let started = std::time::Instant::now();
    // Logging policy: noisy in the rare cases (jar rotated, error),
    // silent on the steady-state OK-no-change tick. The keepalive
    // fires every 5 min and 99% of ticks are no-change; the original
    // per-tick OK line drowned stderr.
    match server::ytmusic::verify_session_keepalive::tick(&cookies).await {
        Ok(Some(updated)) => {
            eprintln!(
                "[yt-keepalive] verify_session OK in {:.1}s, jar updated ({}B → {}B)",
                started.elapsed().as_secs_f32(),
                cookies.len(),
                updated.len()
            );
            let mut cfg = config.write();
            let saved_id = cfg.server.as_ref().and_then(|s| s.id.clone());
            let is_manual = cfg.server.as_ref().map(|s| s.yt_manual).unwrap_or(false);
            if let Some(srv) = cfg.server.as_mut() {
                srv.access_token = Some(updated.clone());
            }
            // Manual-cookie servers persist the rotated jar to the saved
            // entry too, so a restart restores a still-valid session
            // instead of the original (now-stale) pasted cookies.
            if is_manual
                && let Some(id) = saved_id
                && let Some(saved) = cfg.servers.iter_mut().find(|s| s.id == id)
            {
                saved.yt_saved_cookies = Some(updated);
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("[yt-keepalive] verify_session failed: {e}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn persist_queue_state_snapshot(
    queue_state: Option<PersistedQueueState>,
    path: std::path::PathBuf,
) {
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(queue_state) = queue_state {
            queue_state.save(&path)
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!("Failed to save queue state: {}", e),
        Err(e) => tracing::error!("Failed to join queue state save task: {}", e),
    }
}

#[cfg(target_arch = "wasm32")]
async fn persist_queue_state_snapshot(
    queue_state: Option<PersistedQueueState>,
    _path: std::path::PathBuf,
) {
    if let Some(queue_state) = queue_state {
        save_web_queue_state(&queue_state);
    } else {
        clear_web_queue_state();
    }
}

fn is_server_queue_track(track: &reader::Track) -> bool {
    matches!(
        track
            .path
            .to_string_lossy()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "jellyfin" | "subsonic" | "custom"
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn is_restorable_queue_track(track: &reader::Track) -> bool {
    is_server_queue_track(track) || track.path.exists()
}

#[cfg(target_arch = "wasm32")]
fn is_restorable_queue_track(_track: &reader::Track) -> bool {
    true
}

fn sanitize_queue_state(state: PersistedQueueState) -> Option<PersistedQueueState> {
    if state.queue.is_empty() {
        return None;
    }

    let original_index = state
        .current_queue_index
        .min(state.queue.len().saturating_sub(1));
    let mut selected_track_survived = false;
    let survivors: Vec<(usize, reader::Track)> = state
        .queue
        .into_iter()
        .enumerate()
        .filter(|(idx, track)| {
            let keep = is_restorable_queue_track(track);
            if keep && *idx == original_index {
                selected_track_survived = true;
            }
            keep
        })
        .collect();

    if survivors.is_empty() {
        return None;
    }

    let restored_index = if selected_track_survived {
        survivors
            .iter()
            .position(|(idx, _)| *idx == original_index)
            .unwrap_or(0)
    } else {
        survivors
            .iter()
            .enumerate()
            .min_by_key(|(_, (idx, _))| (idx.abs_diff(original_index), *idx > original_index))
            .map(|(restored_idx, _)| restored_idx)
            .unwrap_or(0)
    };

    let old_queue_len = survivors
        .iter()
        .map(|(old_idx, _)| *old_idx)
        .max()
        .map_or(0, |m| m + 1);

    let mut old_to_new_index: Vec<Option<usize>> = vec![None; old_queue_len];
    for (new_idx, (old_idx, _)) in survivors.iter().enumerate() {
        old_to_new_index[*old_idx] = Some(new_idx);
    }

    let shuffle_order: Vec<usize> = state
        .shuffle_order
        .into_iter()
        .filter_map(|old_idx| old_to_new_index.get(old_idx).and_then(|&new_idx| new_idx))
        .collect();

    let queue: Vec<_> = survivors.into_iter().map(|(_, track)| track).collect();
    let progress_secs = if selected_track_survived {
        queue
            .get(restored_index)
            .map(|track| state.progress_secs.min(track.duration))
            .unwrap_or(0)
    } else {
        0
    };

    Some(PersistedQueueState {
        version: state.version,
        queue,
        current_queue_index: restored_index,
        progress_secs,
        shuffle_order,
        shuffle_enabled: state.shuffle_enabled,
    })
}

fn build_queue_state_snapshot(
    queue: &[reader::Track],
    current_queue_index: usize,
    current_song_progress: u64,
    is_playing: bool,
    shuffle_order: &[usize],
    shuffle_enabled: bool,
) -> Option<PersistedQueueState> {
    if queue.is_empty() {
        return None;
    }

    let current_idx = current_queue_index.min(queue.len() - 1);
    let progress_secs = queue
        .get(current_idx)
        .map(|track| current_song_progress.min(track.duration))
        .unwrap_or(0);
    let progress_secs = if is_playing {
        progress_secs - (progress_secs % QUEUE_STATE_PROGRESS_STEP_SECS)
    } else {
        progress_secs
    };

    Some(PersistedQueueState {
        version: 1,
        queue: queue.to_vec(),
        current_queue_index: current_idx,
        progress_secs,
        shuffle_order: shuffle_order.to_vec(),
        shuffle_enabled,
    })
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn read_titlebar_mode_from_disk() -> config::TitlebarMode {
    directories::ProjectDirs::from("com", "temidaradev", "kopuz")
        .map(|d| d.config_dir().join("config.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<config::AppConfig>(&s).ok())
        .map(|c| c.titlebar_mode)
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn thumb_cache_path(file_path: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file_path.hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir().join(format!("rusic_thumb_{:016x}.jpg", hash))
}

#[cfg(not(target_arch = "wasm32"))]
fn hq_cache_path(file_path: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "hq".hash(&mut hasher);
    file_path.hash(&mut hasher);
    let hash = hasher.finish();
    std::env::temp_dir().join(format!("rusic_hq_{:016x}.jpg", hash))
}

#[cfg(not(target_arch = "wasm32"))]
fn make_thumbnail(raw: &[u8], cache_path: &std::path::Path) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    let img = image::load_from_memory(raw).ok()?;
    const MAX: u32 = 400;
    let img = if img.width() > MAX || img.height() > MAX {
        img.thumbnail(MAX, MAX)
    } else {
        img
    };
    let mut out: Vec<u8> = Vec::new();
    img.write_with_encoder(JpegEncoder::new_with_quality(&mut out, 75))
        .ok()?;
    let _ = std::fs::write(cache_path, &out);
    Some(out)
}

// Returns Some(compressed) when the image exceeded the size/dimension limit,
// or None when the original is already small enough to serve as-is.
#[cfg(not(target_arch = "wasm32"))]
fn make_hq_image(raw: &[u8], cache_path: &std::path::Path) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    const SIZE_LIMIT: usize = 2 * 1024 * 1024; // 2 MB
    const MAX_DIM: u32 = 1920;
    const QUALITY: u8 = 85;

    if raw.len() <= SIZE_LIMIT {
        return None;
    }
    let img = image::load_from_memory(raw).ok()?;
    let img = if img.width() > MAX_DIM || img.height() > MAX_DIM {
        img.thumbnail(MAX_DIM, MAX_DIM)
    } else {
        img
    };
    let mut out: Vec<u8> = Vec::new();
    img.write_with_encoder(JpegEncoder::new_with_quality(&mut out, QUALITY))
        .ok()?;
    let _ = std::fs::write(cache_path, &out);
    Some(out)
}

fn main() {
    // Clear out what the last in-place update had to leave behind (the old
    // binary was still locked by the process that installed the new one).
    #[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
    sweep_update_leftovers();

    // Android has no file logger; instead route tracing to stderr, which the
    // mobile runtime pipes into logcat (tag RustStdoutStderr) alongside the
    // engine's eprintln lines. Without a subscriber the server-crate resolve
    // diagnostics (emitted via tracing) are silently dropped on-device.
    #[cfg(target_os = "android")]
    {
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::io::stderr),
            )
            .try_init();
    }

    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        let log_dir = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
            .map(|dirs| dirs.cache_dir().join("logs"))
            .unwrap_or_else(|| std::path::PathBuf::from("logs"));
        let _ = std::fs::create_dir_all(&log_dir);

        let file_appender = tracing_appender::rolling::daily(&log_dir, "kopuz.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(non_blocking),
            )
            .init();
        tracing::info!("Log file: {}", log_dir.display());

        migrate_legacy_locations();

        // Presence::new never fails on desktop anymore — it connects lazily
        // and reconnects on its own, so Discord starting AFTER Kopuz (or
        // restarting mid-session) now picks the status up automatically.
        let presence: Option<Arc<Presence>> = match Presence::new("1470087339639443658") {
            Ok(p) => {
                tracing::info!("Discord presence initialized (lazy connect)");
                Some(Arc::new(p))
            }
            Err(e) => {
                tracing::warn!("Discord presence unavailable: {e}");
                None
            }
        };

        PRESENCE.set(presence).ok();

        #[cfg(target_os = "macos")]
        {
            player::systemint::init();
        }

        let mut window = dioxus::desktop::WindowBuilder::new()
            .with_title("Kopuz")
            .with_resizable(true)
            .with_inner_size(LogicalSize::new(1350.0, 800.0));

        if let Some(icon) = build_window_icon() {
            window = window.with_window_icon(Some(icon));
        }

        #[cfg(target_os = "macos")]
        {
            window = window
                .with_title_hidden(true)
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true);
        }

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let initial_titlebar_mode = read_titlebar_mode_from_disk();
            window = window.with_decorations(initial_titlebar_mode == config::TitlebarMode::System);
        }

        let webview_data_dir = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
            .map(|dirs| dirs.cache_dir().join("webview"))
            .unwrap_or_else(|| std::path::PathBuf::from("./cache/webview"));
        let _ = std::fs::create_dir_all(&webview_data_dir);

        let config = dioxus::desktop::Config::new()
            .with_custom_head(
                "<style>html,body{background:#000;margin:0;padding:0}body{opacity:0}</style>"
                    .to_string(),
            )
            .with_background_color((0, 0, 0, 255))
            .with_data_directory(webview_data_dir)
            .with_window(window)
            // Anon PoToken minter: stand up the hidden music.youtube.com webview
            // once we have the event-loop target (issue #349).
            .with_custom_event_handler(|_event, _target| {
                crate::pot_minter::install_if_wanted(_target);
                crate::pot_minter::pump();
            })
            .with_asynchronous_custom_protocol(
                "artwork",
                |_id, request, responder: RequestAsyncResponder| {
                    let uri = request.uri().clone();

                    tokio::spawn(async move {
                        let query = uri.query().unwrap_or_default();
                        let file_path: String = query
                            .split('&')
                            .find_map(|kv| kv.strip_prefix("p="))
                            .map(|encoded| {
                                percent_encoding::percent_decode_str(encoded)
                                    .decode_utf8_lossy()
                                    .into_owned()
                            })
                            .unwrap_or_default();
                        let high_quality = query.split('&').any(|kv| kv == "hq=1");

                        if file_path.is_empty() {
                            responder.respond(
                                http::Response::builder()
                                    .status(400)
                                    .body(std::borrow::Cow::from(Vec::new()))
                                    .unwrap(),
                            );
                            return;
                        }

                        #[cfg(target_os = "windows")]
                        let file_path = file_path.replace('/', "\\");

                        #[cfg(not(target_os = "windows"))]
                        let file_path = if file_path.starts_with('~') {
                            if let Ok(home) = std::env::var("HOME") {
                                file_path.replacen('~', &home, 1)
                            } else {
                                file_path
                            }
                        } else {
                            file_path
                        };

                        if high_quality {
                            let hq_path = hq_cache_path(&file_path);
                            if hq_path.exists()
                                && let Ok(b) = tokio::fs::read(&hq_path).await
                            {
                                responder.respond(
                                    http::Response::builder()
                                        .header("Content-Type", "image/jpeg")
                                        .header("Access-Control-Allow-Origin", "*")
                                        .header("Cache-Control", "public, max-age=31536000")
                                        .body(std::borrow::Cow::from(b))
                                        .unwrap(),
                                );
                                return;
                            }
                            match tokio::fs::read(&file_path).await {
                                Ok(raw) => {
                                    let file_path_clone = file_path.clone();
                                    let result = tokio::task::spawn_blocking(move || {
                                        make_hq_image(&raw, &hq_path)
                                            .map(|b| (b, "image/jpeg"))
                                            .unwrap_or_else(|| {
                                                let mime = if file_path_clone.ends_with(".png") {
                                                    "image/png"
                                                } else {
                                                    "image/jpeg"
                                                };
                                                (raw, mime)
                                            })
                                    })
                                    .await;
                                    match result {
                                        Ok((bytes, mime)) => responder.respond(
                                            http::Response::builder()
                                                .header("Content-Type", mime)
                                                .header("Access-Control-Allow-Origin", "*")
                                                .header("Cache-Control", "public, max-age=31536000")
                                                .body(std::borrow::Cow::from(bytes))
                                                .unwrap(),
                                        ),
                                        Err(_) => responder.respond(
                                            http::Response::builder()
                                                .status(500)
                                                .body(std::borrow::Cow::from(Vec::new()))
                                                .unwrap(),
                                        ),
                                    }
                                }
                                Err(_) => responder.respond(
                                    http::Response::builder()
                                        .status(404)
                                        .body(std::borrow::Cow::from(Vec::new()))
                                        .unwrap(),
                                ),
                            }
                            return;
                        }

                        let thumb_path = thumb_cache_path(&file_path);

                        let (bytes, mime) = if thumb_path.exists() {
                            match tokio::fs::read(&thumb_path).await {
                                Ok(b) => (b, "image/jpeg"),
                                Err(_) => {
                                    let _ = std::fs::remove_file(&thumb_path);
                                    match tokio::fs::read(&file_path).await {
                                        Ok(b) => (
                                            b,
                                            if file_path.ends_with(".png") {
                                                "image/png"
                                            } else {
                                                "image/jpeg"
                                            },
                                        ),
                                        Err(_) => {
                                            responder.respond(
                                                http::Response::builder()
                                                    .status(404)
                                                    .body(std::borrow::Cow::from(Vec::new()))
                                                    .unwrap(),
                                            );
                                            return;
                                        }
                                    }
                                }
                            }
                        } else {
                            match tokio::fs::read(&file_path).await {
                                Ok(raw) => {
                                    let thumb_path_clone = thumb_path.clone();
                                    match tokio::task::spawn_blocking(move || match make_thumbnail(
                                        &raw,
                                        &thumb_path_clone,
                                    ) {
                                        Some(b) => Ok(b),
                                        None => Err(raw),
                                    })
                                    .await
                                    {
                                        Ok(Ok(b)) => (b, "image/jpeg"),
                                        Ok(Err(raw)) => (
                                            raw,
                                            if file_path.ends_with(".png") {
                                                "image/png"
                                            } else {
                                                "image/jpeg"
                                            },
                                        ),
                                        Err(_) => {
                                            responder.respond(
                                                http::Response::builder()
                                                    .status(500)
                                                    .body(std::borrow::Cow::from(Vec::new()))
                                                    .unwrap(),
                                            );
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("[artwork] not found {}: {}", file_path, e);
                                    responder.respond(
                                        http::Response::builder()
                                            .status(404)
                                            .body(std::borrow::Cow::from(Vec::new()))
                                            .unwrap(),
                                    );
                                    return;
                                }
                            }
                        };

                        responder.respond(
                            http::Response::builder()
                                .header("Content-Type", mime)
                                .header("Access-Control-Allow-Origin", "*")
                                .header("Cache-Control", "public, max-age=31536000")
                                .body(std::borrow::Cow::from(bytes))
                                .unwrap(),
                        );
                    });
                },
            );

        dioxus::LaunchBuilder::desktop()
            .with_cfg(config)
            .launch(App);
    }

    #[cfg(target_os = "android")]
    {
        // JNI media session + classloader cache. Player::new() also calls this (idempotent
        // OnceLock), but doing it up front means the session exists before first playback.
        player::systemint::init();

        let config = dioxus::mobile::Config::new()
            .with_background_color((0, 0, 0, 255))
            // artwork://local?p=<percent-encoded-absolute-path> — the Android WebView mostly
            // receives base64 data URLs from utils, but keep a synchronous handler for any
            // code path that still emits artwork:// URLs.
            .with_custom_protocol("artwork".to_string(), |_headers, request| {
                let query = request.uri().query().unwrap_or("");
                let raw_p = query
                    .split('&')
                    .find_map(|kv| {
                        let mut parts = kv.splitn(2, '=');
                        if parts.next() == Some("p") {
                            parts.next()
                        } else {
                            None
                        }
                    })
                    .unwrap_or("");
                let decoded = percent_encoding::percent_decode_str(raw_p).decode_utf8_lossy();

                let mime = if decoded.ends_with(".png") {
                    "image/png"
                } else {
                    "image/jpeg"
                };

                let mut decoded_path = decoded.to_string();
                if decoded_path.starts_with("/~") {
                    if let Ok(home) = std::env::var("HOME") {
                        decoded_path = decoded_path.replacen("/~", &home, 1);
                    }
                } else if decoded_path.starts_with('~') {
                    if let Ok(home) = std::env::var("HOME") {
                        decoded_path = decoded_path.replacen('~', &home, 1);
                    }
                }

                let read_result =
                    std::fs::read(std::path::Path::new(&decoded_path)).or_else(|_| {
                        if decoded_path.strip_prefix('/').is_some() {
                            std::fs::read(std::path::Path::new(&decoded_path[1..]))
                        } else {
                            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                        }
                    });

                match read_result {
                    Ok(bytes) => http::Response::builder()
                        .header("Content-Type", mime)
                        .header("Access-Control-Allow-Origin", "*")
                        .body(std::borrow::Cow::from(bytes))
                        .unwrap(),
                    Err(e) => {
                        let status = if e.kind() == std::io::ErrorKind::NotFound {
                            404
                        } else {
                            500
                        };
                        http::Response::builder()
                            .status(status)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(std::borrow::Cow::from(Vec::new()))
                            .unwrap()
                    }
                }
            });

        dioxus::LaunchBuilder::mobile().with_cfg(config).launch(App);
    }

    #[cfg(target_arch = "wasm32")]
    {
        dioxus::launch(App);
    }
}

#[component]
fn App() -> Element {
    // Native YouTube sig/n deciphering runs in this WebView's own
    // JavaScriptCore (issue #349): register a JS engine that forwards each
    // solver program to this task, which executes it via `document::eval` and
    // returns the printed result. No external JS runtime, no yt-dlp, no
    // botguard — the decipher path uses the engine already loaded for the UI.
    use_hook(|| {
        let (engine, mut rx) = server::ytmusic::decipher::webview_channel();
        if server::ytmusic::decipher::set_engine(engine).is_err() {
            eprintln!("[yt-decipher] engine already registered — webview solver not active");
        }
        spawn(async move {
            while let Some(req) = rx.recv().await {
                // Always send exactly one message back: the printed result, or
                // the JS error prefixed with a NUL marker. Without the
                // try/catch a throwing solver would never call `dioxus.send`
                // and `recv()` below would hang forever, stalling playback.
                let wrapped = format!(
                    "globalThis.print=function(s){{dioxus.send(s);}};\
                     try{{{}}}catch(e){{dioxus.send('\\u0000ERR'+(e&&e.stack?e.stack:e));}}",
                    req.program
                );
                // Bound the wait — a non-returning solver script must not stall
                // the decipher queue forever.
                let mut eval = dioxus::document::eval(&wrapped);
                let result = match tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    eval.recv::<String>(),
                )
                .await
                {
                    Ok(Ok(s)) => match s.strip_prefix('\u{0}') {
                        Some(err) => Err(format!("webview JS: {}", err.trim_start_matches("ERR"))),
                        None => Ok(s),
                    },
                    Ok(Err(e)) => Err(format!("webview eval recv: {e}")),
                    Err(_) => Err("webview decipher timed out".to_string()),
                };
                let _ = req.reply.send(result);
            }
        });
    });

    let mut library = use_signal(reader::Library::default);
    let mut current_route = use_signal(|| Route::Home);
    let mut scroll_positions: Signal<std::collections::HashMap<Route, f64>> =
        use_signal(std::collections::HashMap::new);
    // Album/artist list and detail share one Route, so detail scroll is kept in a
    // separate map keyed by `album:<id>` / `artist:<name>`. This stops a detail's
    // scroll from clobbering the list scroll the user expects back on return.
    let mut detail_scroll_positions: Signal<std::collections::HashMap<String, f64>> =
        use_signal(std::collections::HashMap::new);
    let cache_dir = use_memo(move || {
        // Android: external/ProjectDirs paths aren't writable; use the app-internal files
        // dir (getFilesDir via JNI) so saves don't fail with EACCES.
        #[cfg(target_os = "android")]
        {
            let mut path = player::systemint::get_files_dir()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("cache");
            if std::fs::create_dir_all(&path).is_err() {
                path = std::path::PathBuf::from("./cache");
                let _ = std::fs::create_dir_all(&path);
            }
            path
        }
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        {
            let path = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
                .map(|dirs| dirs.cache_dir().to_path_buf())
                .unwrap_or_else(|| std::path::Path::new("./cache").to_path_buf());
            let _ = std::fs::create_dir_all(&path);
            path
        }
        #[cfg(target_arch = "wasm32")]
        std::path::PathBuf::from("./cache")
    });
    let config_dir = use_memo(move || {
        #[cfg(target_os = "android")]
        {
            let mut path = player::systemint::get_files_dir()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("config");
            if std::fs::create_dir_all(&path).is_err() {
                path = std::path::PathBuf::from("./config");
                let _ = std::fs::create_dir_all(&path);
            }
            path
        }
        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
        {
            let path = directories::ProjectDirs::from("com", "temidaradev", "kopuz")
                .map(|dirs| dirs.config_dir().to_path_buf())
                .unwrap_or_else(|| std::path::Path::new("./config").to_path_buf());
            let _ = std::fs::create_dir_all(&path);
            path
        }
        #[cfg(target_arch = "wasm32")]
        std::path::PathBuf::from("./config")
    });
    let lib_path = use_memo(move || config_dir().join("library.json"));
    let config_path = use_memo(move || config_dir().join("config.json"));
    let mut config = use_signal(config::AppConfig::default);
    // Start the PoToken minter whenever a YouTube Music server is active — not
    // just anon. A *signed-in but non-Premium* account streams the same 251 as
    // anon and also needs a content pot for deep ranges; only true Premium
    // subscribers (itag 774) are pot-exempt, and we can't know that until a
    // track resolves. So run the minter for any YtMusic session; Premium just
    // leaves it idle. Reactive: fires when config loads or the server changes.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    use_effect(move || {
        let yt_active = config
            .read()
            .server
            .as_ref()
            .is_some_and(|s| s.service == config::MusicService::YtMusic);
        if yt_active {
            crate::pot_minter::request();
        }
    });
    // Android: same trigger, but start the headless-WebView PoToken minter driver
    // (the desktop wry minter is unavailable). Pass the YT cookies so the minter
    // WebView is signed in and skips the consent wall. Idempotent.
    #[cfg(target_os = "android")]
    use_effect(move || {
        let cookies = {
            let cfg = config.read();
            match cfg.server.as_ref() {
                Some(s) if s.service == config::MusicService::YtMusic => {
                    Some(s.access_token.clone().unwrap_or_default())
                }
                _ => None,
            }
        };
        if let Some(cookies) = cookies {
            crate::pot_minter_android::start(cookies);
        }
    });
    #[allow(unused_variables)]
    let playlist_path = use_memo(move || config_dir().join("playlists.json"));
    let mut playlist_store = use_signal(reader::PlaylistStore::default);
    #[allow(unused_variables)]
    let favorites_path = use_memo(move || config_dir().join("favorites.json"));
    let queue_state_path = use_memo(move || config_dir().join("queue_state.json"));
    let mut favorites_store = use_signal(FavoritesStore::default);
    let mut initial_load_done = use_signal(|| false);
    #[allow(unused_variables)]
    let cover_cache = use_memo(move || cache_dir().join("covers"));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = std::fs::create_dir_all(cover_cache());
    let download_queue = use_signal(DownloadQueue::default);
    let download_progress = use_signal(::server::DownloadProgress::default);
    pages::server::download_manager::register_progress_signal(download_progress);

    // Self-manage yt-dlp: download/refresh Kopuz's own copy in the background
    // on every launch (no-op when already fresh). An outdated yt-dlp is the #1
    // cause of YouTube bot-checks, so this is what makes downloads + playback
    // keep working long-term with zero user maintenance — no winget, no PATH.
    use_effect(move || {
        #[cfg(not(target_arch = "wasm32"))]
        spawn(async move {
            match ::server::deps::ensure_ytdlp_fresh().await {
                Ok(p) => tracing::info!("Managed yt-dlp ready: {}", p.display()),
                Err(e) => tracing::warn!("yt-dlp auto-update skipped (using system if present): {e}"),
            }
            // ffmpeg gives downloads opus extraction + embedded cover art. Fetch
            // a static build once on first run so the user never installs it by
            // hand — this is what replaces the old install-windows.ps1 step.
            match ::server::deps::ensure_ffmpeg().await {
                Ok(p) => tracing::info!("Managed ffmpeg ready: {}", p.display()),
                Err(e) => tracing::warn!("ffmpeg auto-setup skipped (using system if present): {e}"),
            }
        });
    });
    let mut trigger_rescan = use_signal(|| 0);
    let mut last_scan_key = use_signal(|| None::<String>);
    let mut scan_current_file = use_signal(|| Option::<String>::None);
    let current_playing = use_signal(|| 0);
    let mut player = use_signal(Player::new);
    let current_song_cover_url = use_signal(String::new);
    let current_song_title = use_signal(String::new);
    let current_song_artist = use_signal(String::new);
    let current_song_album = use_signal(String::new);
    let current_song_duration = use_signal(|| 0u64);
    let current_song_khz = use_signal(|| 0u32);
    let current_song_bitrate = use_signal(|| 0u16);
    let current_song_progress = use_signal(|| 0u64);
    let current_track_snapshot = use_signal(|| None::<reader::Track>);
    let mut volume = use_signal(|| 1.0f32);
    let mut persisted_volume = use_signal(|| 1.0f32);
    let mut configured_music_dirs = use_signal(|| config.peek().music_directory.clone());

    let is_playing = use_signal(|| false);
    let mut is_fullscreen = use_signal(|| false);
    let is_rightbar_open = use_signal(|| false);
    let rightbar_width = use_signal(|| 320usize);
    let mut palette = use_signal(|| Option::<Vec<utils::color::Color>>::None);
    let mut pending_queue_state_snapshot = use_signal(|| None::<PersistedQueueState>);
    let mut pending_queue_state_revision = use_signal(|| 0u64);
    let mut pending_config_snapshot = use_signal(|| None::<config::AppConfig>);
    let mut pending_config_revision = use_signal(|| 0u64);

    #[cfg(all(not(target_arch = "wasm32"), target_os = "macos"))]
    use_effect(move || {
        let _ = dioxus::document::eval(
            r#"(function(){
            try {
                var ctx = new (window.AudioContext||window.webkitAudioContext)({sampleRate:8000});
                var buf = ctx.createBuffer(1,1,8000);
                var src = ctx.createBufferSource();
                src.buffer = buf;
                src.loop = true;
                src.connect(ctx.destination);
                src.start(0);
                document.addEventListener('visibilitychange', function(){
                    if (ctx.state === 'suspended') ctx.resume();
                });
            } catch(e) {}
        })()"#,
        );
    });

    use_effect(move || {
        let _ = dioxus::document::eval(
            r#"(function(){
                function show(){document.body.style.transition='opacity .15s';document.body.style.opacity='1';}
                var links=document.querySelectorAll('link[rel="stylesheet"]');
                if(!links.length){show();return;}
                var loaded=0;
                function onLoad(){if(++loaded>=links.length)show();}
                links.forEach(function(l){if(l.sheet){onLoad();}else{l.addEventListener('load',onLoad);l.addEventListener('error',onLoad);}});
            })();"#,
        );
    });

    use_effect(move || {
        let _ = dioxus::document::eval(
            r#"document.addEventListener('error',function(e){
                var t=e.target;
                if(t.tagName==='IMG'&&!t.dataset.fallback&&t.src){
                    t.dataset.fallback='1';
                    t.src='data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 width=%27400%27 height=%27400%27 viewBox=%270 0 400 400%27%3E%3Crect width=%27400%27 height=%27400%27 fill=%27%231e1b2e%27/%3E%3Ccircle cx=%27200%27 cy=%27180%27 r=%2770%27 fill=%27none%27 stroke=%27%233d3466%27 stroke-width=%276%27/%3E%3Cpath d=%27M155 280 Q200 240 245 280%27 fill=%27none%27 stroke=%27%233d3466%27 stroke-width=%276%27 stroke-linecap=%27round%27/%3E%3C/svg%3E';
                }
            },true);"#,
        );
    });

    use_effect(move || {
        let url = current_song_cover_url.read().clone();
        if !url.is_empty() {
            spawn(async move {
                if let Some(colors) = utils::color::get_palette_from_url(&url).await {
                    palette.set(Some(colors));
                }
            });
        } else {
            palette.set(None);
        }
    });

    use_effect(move || {
        let next_dirs = config.read().music_directory.clone();
        if *configured_music_dirs.peek() != next_dirs {
            configured_music_dirs.set(next_dirs);
        }
    });

    #[cfg(not(target_arch = "wasm32"))]
    let presence = PRESENCE.get().cloned().flatten();
    #[cfg(not(target_arch = "wasm32"))]
    provide_context(presence.clone());

    let mut station_registry = use_signal(radio::registry::StationRegistry::new);
    provide_context(station_registry);

    let mut last_radio_registry_key = use_signal(|| None::<String>);

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }

        let registry_paths: Vec<String> = config
            .read()
            .radio_registries
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.url.clone())
            .collect();

        let key = registry_paths.join(",");
        if *last_radio_registry_key.peek() == Some(key.clone()) {
            return;
        }
        last_radio_registry_key.set(Some(key));

        spawn(async move {
            let mut new_registry = radio::registry::StationRegistry::new();
            let mut import_count = 0;

            for path in registry_paths {
                match new_registry.import_registry(&path).await {
                    Ok(_) => import_count += 1,
                    Err(e) => tracing::warn!("Failed to import registry from {}: {}", path, e),
                }
            }

            station_registry.set(new_registry);

            if import_count > 0 {
                tracing::info!("Imported {} external radio registries", import_count);
            }
        });
    });

    let mut selected_album_id = use_signal(String::new);
    let mut selected_playlist_id = use_signal(|| None::<String>);
    let mut discover_selected_playlist_id = use_signal(|| None::<String>);
    // Which route opened the playlist detail view — Discover, Artist or
    // Search — so its back button returns where the user actually was.
    let mut discover_playlist_origin = use_signal(|| Route::Discover);
    let mut discover_selected_playlist_title = use_signal(|| None::<String>);
    // YT channel id corresponding to selected_artist_name when known
    // (Discover tile / mix entry carries it). Left None when the
    // click only had a name — the YT artist page resolves it via
    // search at render time.
    let mut selected_artist_channel_id = use_signal(|| None::<String>);
    let mut selected_artist_name = use_signal(String::new);
    let fetched_artist_images: Signal<std::collections::HashMap<String, String>> =
        use_signal(std::collections::HashMap::new);
    let is_fetching_artist_images = use_signal(|| false);
    let mut search_query = use_signal(String::new);
    let mut last_server_playlist_key = use_signal(|| None::<String>);
    let mut server_playlist_key_initialized = use_signal(|| false);
    let mut queue = use_signal(Vec::<reader::Track>::new);
    let current_queue_index = use_signal(|| 0usize);

    let mut network_banner: Signal<Option<bool>> = use_signal(|| None);
    #[cfg(not(target_arch = "wasm32"))]
    let mut update_banner: Signal<Option<AvailableUpdate>> = use_signal(|| None);
    // In-app update progress: None = idle, Some("downloading"|"installing"|"failed").
    #[cfg(not(target_arch = "wasm32"))]
    let update_status: Signal<Option<String>> = use_signal(|| None);
    #[cfg(not(target_arch = "wasm32"))]
    let mut did_check_updates = use_signal(|| false);
    let mut auto_switched_to_offline = use_signal(|| false);
    let mut ctrl = hooks::use_player_controller(
        player,
        is_playing,
        queue,
        current_queue_index,
        current_song_title,
        current_song_artist,
        current_song_album,
        current_song_khz,
        current_song_bitrate,
        current_song_duration,
        current_song_progress,
        current_song_cover_url,
        current_track_snapshot,
        volume,
        library,
        config,
    );

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }

        // Server identity excludes access_token: tokens rotate without making it a
        // different account, but their rotation would otherwise wipe synced playlists.
        let current_server_key = {
            let conf = config.read();
            conf.server.as_ref().map(|server| {
                format!(
                    "{:?}|{}|{}",
                    server.service,
                    server.url,
                    server.user_id.as_deref().unwrap_or_default(),
                )
            })
        };

        if !*server_playlist_key_initialized.read() {
            last_server_playlist_key.set(current_server_key);
            server_playlist_key_initialized.set(true);
            return;
        }

        if *last_server_playlist_key.read() != current_server_key {
            last_server_playlist_key.set(current_server_key);
            selected_playlist_id.set(None);
            playlist_store.write().jellyfin_playlists.clear();
            {
                let mut lib = library.write();
                lib.jellyfin_tracks.clear();
                lib.jellyfin_albums.clear();
            }
            ctrl.reset_for_backend_switch();
        }
    });

    #[cfg(not(target_arch = "wasm32"))]
    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }

        if !config.read().auto_check_updates {
            update_banner.set(None);
            if *did_check_updates.peek() {
                did_check_updates.set(false);
            }
            return;
        }

        if *did_check_updates.read() {
            return;
        }

        did_check_updates.set(true);
        spawn(async move {
            if let Some(update) = fetch_available_update().await {
                update_banner.set(Some(update));
            }
        });
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let mut config_snapshot = config.read().clone();
        config_snapshot.volume = *volume.peek();
        pending_config_snapshot.set(Some(config_snapshot));
        pending_config_revision.with_mut(|revision| *revision += 1);
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }

        let committed_volume = *persisted_volume.read();
        let mut config_snapshot = config.peek().clone();
        config_snapshot.volume = committed_volume;
        pending_config_snapshot.set(Some(config_snapshot));
        pending_config_revision.with_mut(|revision| *revision += 1);
    });

    // Debounced config writer: mirrors the queue-state saver below. Rapid
    // config churn only bumps a revision; the actual serialize + disk write
    // happens once the dust settles, coalescing bursts into one write.
    use_future(move || async move {
        let mut flushed_revision = 0u64;
        loop {
            let pending_revision = *pending_config_revision.read();
            if pending_revision == flushed_revision {
                utils::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            }

            utils::sleep(std::time::Duration::from_millis(CONFIG_SAVE_DEBOUNCE_MS)).await;

            let latest_revision = *pending_config_revision.read();
            if latest_revision != pending_revision {
                continue;
            }

            if let Some(snapshot) = pending_config_snapshot.read().clone() {
                persist_config_snapshot(snapshot, config_path());
            }
            flushed_revision = latest_revision;
        }
    });

    // Keepalive is rearm-on-account-change, not rearm-on-every-config-
    // write. Re-running the effect on every config save would spawn
    // a fresh loop that immediately fires run_rotation, spamming
    // /verify_session a dozen times a minute on any settings churn.
    //
    // The signal stores the YT identity (a stable hash of the SAPISID
    // cookie) we currently have a loop running against. The effect
    // re-runs cheap, but only spawns a new loop when the identity
    // changes (sign-in, account switch). Sign-out clears the
    // identity and the running loop exits on its next tick.
    // "Stay signed in automatically": when enabled, re-read YouTube cookies
    // from a signed-in browser on boot and every 15 min, so the session never
    // has to be pasted again. The keepalive loop above then keeps the adopted
    // cookies fresh between refreshes.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    let mut yt_auto_refresh_started = use_signal(|| false);
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let want = config.read().yt_auto_refresh
            && config
                .read()
                .server
                .as_ref()
                .map(|s| s.service == config::MusicService::YtMusic)
                .unwrap_or(false);
        if !want || *yt_auto_refresh_started.peek() {
            return;
        }
        yt_auto_refresh_started.set(true);
        spawn(async move {
            run_auto_refresh(config).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
                if !config.peek().yt_auto_refresh {
                    yt_auto_refresh_started.set(false);
                    return;
                }
                run_auto_refresh(config).await;
            }
        });
    });

    // OAuth "always signed in": when a refresh token is stored, mint a fresh
    // access token on boot and every 45 min (tokens live ~1h). No browser, so
    // this runs on every platform including mobile.
    #[cfg(not(target_arch = "wasm32"))]
    let mut yt_oauth_started = use_signal(|| false);
    #[cfg(not(target_arch = "wasm32"))]
    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let want = !config.read().yt_oauth_refresh_token.is_empty()
            && config
                .read()
                .server
                .as_ref()
                .map(|s| s.service == config::MusicService::YtMusic)
                .unwrap_or(false);
        if !want || *yt_oauth_started.peek() {
            return;
        }
        yt_oauth_started.set(true);
        spawn(async move {
            run_oauth_refresh(config).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(45 * 60)).await;
                if config.peek().yt_oauth_refresh_token.is_empty() {
                    yt_oauth_started.set(false);
                    return;
                }
                run_oauth_refresh(config).await;
            }
        });
    });

    // Managed-browser "auto-login": refresh the session headlessly via CDP on
    // boot and every 10 min, so a browser-login YT server stays alive without
    // any visible window or re-pasting. Desktop only (needs a real browser).
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    let mut yt_browser_refresh_started = use_signal(|| false);
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let want = config
            .read()
            .server
            .as_ref()
            .map(|s| {
                s.service == config::MusicService::YtMusic && !s.yt_manual && s.yt_browser.is_some()
            })
            .unwrap_or(false);
        if !want || *yt_browser_refresh_started.peek() {
            return;
        }
        yt_browser_refresh_started.set(true);
        spawn(async move {
            run_browser_refresh(config).await;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10 * 60)).await;
                let still = config
                    .peek()
                    .server
                    .as_ref()
                    .map(|s| {
                        s.service == config::MusicService::YtMusic
                            && !s.yt_manual
                            && s.yt_browser.is_some()
                    })
                    .unwrap_or(false);
                if !still {
                    yt_browser_refresh_started.set(false);
                    return;
                }
                run_browser_refresh(config).await;
            }
        });
    });

    #[cfg(not(target_arch = "wasm32"))]
    let mut yt_keepalive_identity = use_signal(|| None::<String>);
    #[cfg(not(target_arch = "wasm32"))]
    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let yt_cookies: Option<String> = config.read().server.as_ref().and_then(|s| {
            (s.service == config::MusicService::YtMusic)
                .then(|| s.access_token.clone())
                .flatten()
                .filter(|t| !t.is_empty())
        });
        let live_identity = yt_cookies
            .as_deref()
            .and_then(server::ytmusic::derive_user_id);
        if live_identity == *yt_keepalive_identity.peek() {
            return;
        }
        // Identity changed (fresh sign-in, account switch, or
        // sign-out): the previously-running loop (if any) will read
        // the new identity on its next tick and exit. Update the
        // tracked identity; spawn a fresh loop only if we still have
        // valid auth.
        yt_keepalive_identity.set(live_identity.clone());
        let Some(my_identity) = live_identity else {
            return;
        };
        spawn(async move {
            run_rotation(config).await;
            loop {
                // 3 min, not 5: YouTube rotates __Secure-*PSIDTS roughly every
                // few minutes and tears idle sessions down near the 10-min
                // mark. Refreshing more often captures the rotated cookies
                // before they go stale, which is the main lever on how long a
                // pasted session survives.
                tokio::time::sleep(std::time::Duration::from_secs(180)).await;
                if yt_keepalive_identity
                    .peek()
                    .as_deref()
                    != Some(my_identity.as_str())
                {
                    return;
                }
                run_rotation(config).await;
            }
        });
    });

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_os = "linux", target_os = "windows")
    ))]
    use_effect(move || {
        let mode = config.read().titlebar_mode;
        let win = dioxus::desktop::use_window();
        win.set_decorations(mode == config::TitlebarMode::System);
    });

    #[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
    use_effect(move || {
        let mode = config.read().titlebar_mode;
        let win = dioxus::desktop::use_window();
        let hwnd = HWND(win.window.hwnd() as _);
        windows_titlebar::install(hwnd);
        windows_titlebar::set_custom_titlebar_enabled(mode == config::TitlebarMode::Custom);
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store_snapshot = playlist_store.read().clone();
            let path = playlist_path();
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || store_snapshot.save(&path)).await;
                if let Ok(Err(e)) = result {
                    tracing::error!("Failed to save playlists: {}", e);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let store_snapshot = playlist_store.read().clone();
            save_web_playlists(&store_snapshot);
        }
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let lib_snapshot = library.read().clone();
            let path = lib_path();
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || lib_snapshot.save(&path)).await;
                if let Ok(Err(e)) = result {
                    tracing::error!("Failed to save library: {}", e);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let lib_snapshot = library.read().clone();
            save_web_library(&lib_snapshot);
        }
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store_snapshot = favorites_store.read().clone();
            let path = favorites_path();
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || store_snapshot.save(&path)).await;
                if let Ok(Err(e)) = result {
                    tracing::error!("Failed to save favorites: {}", e);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let store_snapshot = favorites_store.read().clone();
            save_web_favorites(&store_snapshot);
        }
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }

        let queue_snapshot = queue.read().clone();
        let shuffle_order_snapshot = ctrl.shuffle_order.read().clone();
        let shuffle_enabled_snapshot = *ctrl.shuffle.read();

        let queue_state = build_queue_state_snapshot(
            &queue_snapshot,
            *current_queue_index.read(),
            *current_song_progress.read(),
            *is_playing.read(),
            &shuffle_order_snapshot,
            shuffle_enabled_snapshot,
        );

        if *pending_queue_state_snapshot.peek() != queue_state {
            pending_queue_state_snapshot.set(queue_state);
            pending_queue_state_revision.with_mut(|revision| *revision += 1);
        }
    });

    use_future(move || {
        let path = queue_state_path();
        async move {
            let mut flushed_revision = 0u64;

            loop {
                let pending_revision = *pending_queue_state_revision.read();
                if pending_revision == flushed_revision {
                    utils::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }

                utils::sleep(std::time::Duration::from_millis(
                    QUEUE_STATE_SAVE_DEBOUNCE_MS,
                ))
                .await;

                let latest_revision = *pending_queue_state_revision.read();
                if latest_revision != pending_revision {
                    continue;
                }

                let snapshot = pending_queue_state_snapshot.read().clone();
                persist_queue_state_snapshot(snapshot, path.clone()).await;
                flushed_revision = latest_revision;
            }
        }
    });

    let mut is_offline = use_signal(|| false);
    use_context_provider(|| is_offline);

    // Network connectivity monitor — only active in server mode and on non-wasm targets
    #[cfg(not(target_arch = "wasm32"))]
    use_future(move || async move {
        loop {
            if *initial_load_done.read() {
                break;
            }
            utils::sleep(std::time::Duration::from_millis(500)).await;
        }
        let mut was_reachable = true;
        let mut consecutive_failures: u8 = 0;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build();
        let Ok(client) = client else { return };
        loop {
            utils::sleep(std::time::Duration::from_secs(30)).await;

            let server_url = {
                let conf = config.read();
                if conf.active_source != config::MusicSource::Server {
                    was_reachable = true;
                    consecutive_failures = 0;
                    continue;
                }
                conf.server.as_ref().map(|s| s.url.clone())
            };

            let Some(base_url) = server_url else {
                was_reachable = true;
                consecutive_failures = 0;
                continue;
            };

            let ping_url = format!("{}/System/Ping", base_url.trim_end_matches('/'));
            let reachable = client
                .get(&ping_url)
                .send()
                .await
                .map(|r| r.status().as_u16() < 500)
                .unwrap_or(false);

            if reachable {
                consecutive_failures = 0;
            } else {
                consecutive_failures = consecutive_failures.saturating_add(1);
            }

            if !reachable && consecutive_failures >= 2 && was_reachable {
                was_reachable = false;
                is_offline.set(true);
                auto_switched_to_offline.set(true);
                config.write().active_source = config::MusicSource::Local;
                network_banner.set(Some(true));
            } else if reachable && !was_reachable {
                was_reachable = true;
                consecutive_failures = 0;
                is_offline.set(false);
                if *auto_switched_to_offline.read() {
                    auto_switched_to_offline.set(false);
                    config.write().active_source = config::MusicSource::Server;
                    network_banner.set(Some(false));
                    spawn(async move {
                        utils::sleep(std::time::Duration::from_secs(4)).await;
                        if network_banner.read().as_ref() == Some(&false) {
                            network_banner.set(None);
                        }
                    });
                }
            }
        }
    });

    use_hook(move || {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let lib_path = lib_path();
            let config_path = config_path();
            let playlist_path = playlist_path();
            let favorites_path = favorites_path();
            let queue_state_path = queue_state_path();
            let mut ctrl = ctrl;

            spawn(async move {
                let lib_path_c = lib_path.clone();
                let config_path_c = config_path.clone();
                let playlist_path_c = playlist_path.clone();
                let favorites_path_c = favorites_path.clone();
                let queue_state_path_c = queue_state_path.clone();

                let (lib_res, cfg_res, pl_res, fav_res, queue_res) = tokio::join!(
                    tokio::task::spawn_blocking(move || reader::Library::load(&lib_path_c)),
                    tokio::task::spawn_blocking(move || config::AppConfig::load(&config_path_c)),
                    tokio::task::spawn_blocking(move || reader::PlaylistStore::load(
                        &playlist_path_c
                    )),
                    tokio::task::spawn_blocking(move || FavoritesStore::load(&favorites_path_c)),
                    tokio::task::spawn_blocking(move || {
                        PersistedQueueState::load(&queue_state_path_c)
                    }),
                );

                if let Ok(Ok(loaded)) = lib_res {
                    library.set(loaded.clone());
                }
                if let Ok(loaded) = cfg_res {
                    config.set(loaded.clone());
                    // Reconcile offline_tracks with what's actually on disk:
                    //  - drop legacy opus-webm entries (delete the dead file) so
                    //    they re-download as playable m4a;
                    //  - drop entries whose file is gone, so the app stops
                    //    claiming a playlist is "downloaded" when the folder is
                    //    empty and the download button re-fetches it.
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let stale: Vec<(String, bool)> = config
                            .peek()
                            .offline_tracks
                            .iter()
                            .filter_map(|(id, p)| {
                                let is_webm = p.to_lowercase().ends_with(".webm");
                                let missing = !std::path::Path::new(p).exists();
                                (is_webm || missing).then(|| (id.clone(), is_webm))
                            })
                            .collect();
                        if !stale.is_empty() {
                            tracing::info!("Reconciling {} stale offline entries", stale.len());
                            let mut c = config.write();
                            for (id, is_webm) in stale {
                                if let Some(path) = c.offline_tracks.remove(&id)
                                    && is_webm
                                {
                                    let _ = std::fs::remove_file(&path);
                                }
                            }
                        }
                    }
                    configured_music_dirs.set(loaded.music_directory.clone());
                    volume.set(loaded.volume);
                    persisted_volume.set(loaded.volume);
                    player.write().set_volume(loaded.volume);
                    player.write().set_channel_mode(loaded.channel_mode);
                    player.write().set_equalizer(loaded.equalizer.clone());
                    i18n::set_locale(&loaded.language);
                }
                if let Ok(Ok(loaded)) = pl_res {
                    playlist_store.set(loaded);
                }
                if let Ok(Ok(loaded)) = fav_res {
                    favorites_store.set(loaded);
                }

                {
                    let cfg = config.peek();
                    let no_local_tracks = library.peek().tracks.is_empty();
                    let server_connected = cfg
                        .server
                        .as_ref()
                        .and_then(|s| s.access_token.as_ref())
                        .is_some();
                    let not_explicitly_set = !cfg.source_explicitly_set;
                    drop(cfg);
                    if no_local_tracks && server_connected && not_explicitly_set {
                        config.write().active_source = config::MusicSource::Server;
                    }
                }

                if let Ok(Ok(loaded_queue_state)) = queue_res
                    && let Some(queue_state) = sanitize_queue_state(loaded_queue_state)
                {
                    ctrl.restore_queue_state(
                        queue_state.queue,
                        queue_state.current_queue_index,
                        queue_state.progress_secs,
                        queue_state.shuffle_order,
                        queue_state.shuffle_enabled,
                    );
                }

                initial_load_done.set(true);
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut ctrl = ctrl;
            let mut loaded = load_web_config().unwrap_or_default();
            if loaded.server.is_none() {
                loaded.active_source = config::MusicSource::Server;
            }
            let loaded_volume = loaded.volume;
            let loaded_language = loaded.language.clone();
            configured_music_dirs.set(loaded.music_directory.clone());
            config.set(loaded);
            volume.set(loaded_volume);
            persisted_volume.set(loaded_volume);
            player.write().set_volume(loaded_volume);
            player.write().set_channel_mode(config.read().channel_mode);
            player
                .write()
                .set_equalizer(config.read().equalizer.clone());
            i18n::set_locale(&loaded_language);

            if let Some((
                route,
                saved_album_id,
                saved_playlist_id,
                saved_artist_name,
                saved_search_query,
            )) = load_web_ui_state()
            {
                current_route.set(route);
                selected_album_id.set(saved_album_id);
                selected_playlist_id.set(saved_playlist_id);
                selected_artist_name.set(saved_artist_name);
                search_query.set(saved_search_query);
            }

            if let Some(loaded_library) = load_web_library() {
                library.set(loaded_library);
            }
            if let Some(loaded_playlists) = load_web_playlists() {
                playlist_store.set(loaded_playlists);
            }
            if let Some(loaded_favorites) = load_web_favorites() {
                favorites_store.set(loaded_favorites);
            }
            if let Some(loaded_queue_state) = load_web_queue_state() {
                if let Some(queue_state) = sanitize_queue_state(loaded_queue_state) {
                    ctrl.restore_queue_state(
                        queue_state.queue,
                        queue_state.current_queue_index,
                        queue_state.progress_secs,
                        queue_state.shuffle_order,
                        queue_state.shuffle_enabled,
                    );
                }
            }

            initial_load_done.set(true);
        }
    });

    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let route = *current_route.read();
            let album_id = selected_album_id.read().clone();
            let playlist_id = selected_playlist_id.read().clone();
            let artist_name = selected_artist_name.read().clone();
            let query = search_query.read().clone();

            save_web_ui_state(
                route,
                &album_id,
                playlist_id.as_deref(),
                &artist_name,
                &query,
            );
        }
    });

    // Auto-rescan the local library when a download session finishes. Downloads
    // land under <music folder>/Kopuz/…, so a rescan surfaces them in the Local
    // library / Albums / Artists without the user hitting refresh manually.
    let mut last_dl_done_count = use_signal(|| 0usize);
    use_effect(move || {
        let (active, done) = {
            let q = download_queue.read();
            (q.is_active(), q.done_count())
        };
        if !active && done > 0 && done != *last_dl_done_count.peek() {
            last_dl_done_count.set(done);
            *trigger_rescan.write() += 1;
        }
    });

    use_effect(move || {
        if !*initial_load_done.read() {
            return;
        }
        let configured_dirs = configured_music_dirs.read().clone();
        let trigger = *trigger_rescan.read();
        let fetch_covers = config.peek().auto_fetch_covers;
        let fetch_strategy = config.peek().cover_fetch_strategy;
        let lastfm_key = {
            let key = config.peek().lastfm_api_key.trim().to_owned();
            (!key.is_empty()).then_some(key)
        };

        let scan_key = format!(
            "{}|{}",
            configured_dirs
                .iter()
                .map(|d| d.to_string_lossy())
                .collect::<Vec<_>>()
                .join(","),
            trigger,
        );
        if *last_scan_key.peek() == Some(scan_key.clone()) {
            return;
        }
        last_scan_key.set(Some(scan_key));

        #[cfg(not(target_arch = "wasm32"))]
        spawn(async move {
            let configured_dirs = configured_dirs;
            let scannable_dirs: Vec<PathBuf> = configured_dirs
                .iter()
                .filter(|d| d.exists())
                .cloned()
                .collect();
            let mut current_lib = library.peek().clone();

            let current_roots: std::collections::HashSet<_> =
                current_lib.root_paths.iter().cloned().collect();
            let new_roots: std::collections::HashSet<_> = configured_dirs.iter().cloned().collect();

            if current_roots != new_roots {
                current_lib.root_paths = configured_dirs.clone();
                current_lib.tracks.clear();
                current_lib.albums.clear();
                library.set(current_lib.clone());
            }

            if !configured_dirs.is_empty() {
                current_lib.local_artist_images.clear();
                scan_current_file.set(Some(String::new()));

                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                spawn(async move {
                    while let Some(file) = rx.recv().await {
                        scan_current_file.set(Some(file));
                    }
                    scan_current_file.set(None);
                });

                let progress_cb: std::sync::Arc<dyn Fn(String) + Send + Sync> =
                    std::sync::Arc::new(move |file: String| {
                        let _ = tx.send(file);
                    });
                for dir in &scannable_dirs {
                    let _ = reader::scan_directory(
                        dir.clone(),
                        cover_cache(),
                        &mut current_lib,
                        progress_cb.clone(),
                    )
                    .await;
                }

                current_lib.tracks.retain(|t| {
                    let in_configured_root = configured_dirs.iter().any(|d| t.path.starts_with(d));
                    let in_scannable_root = scannable_dirs.iter().any(|d| t.path.starts_with(d));

                    in_configured_root && (!in_scannable_root || t.path.exists())
                });

                let valid_album_ids: std::collections::HashSet<_> = current_lib
                    .tracks
                    .iter()
                    .map(|t| t.album_id.clone())
                    .collect();
                current_lib
                    .albums
                    .retain(|a| valid_album_ids.contains(&a.id));

                // Show the library immediately — before any cover fetching.
                library.set(current_lib.clone());
                let _ = current_lib.save(&lib_path());

                // Surface downloaded server-playlist folders
                // (<download dir>/<Playlist Name>/) as Local playlists, so a
                // playlist you downloaded shows up under Local → Playlists. The
                // entries are regenerated each scan (ids "dl:<folder>"), so they
                // track the folders on disk.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let dl_dir = config.peek().resolved_download_dir();
                    let dl_playlists = reader::scanner::playlists_from_download_dir(&dl_dir);
                    let mut store = playlist_store.write();
                    store.playlists.retain(|p| !p.id.starts_with("dl:"));
                    store.playlists.extend(dl_playlists);
                }

                if fetch_covers {
                    // Fetch missing covers in the background so the UI stays responsive.
                    // Passing `progress_cb` into the task keeps the scan-progress bar
                    // alive during fetching; it disappears automatically when the task ends.
                    let lib_for_fetch = current_lib;
                    spawn(async move {
                        let fetcher = reader::cover_fetcher::CoverFetcher::new(
                            cover_cache(),
                            fetch_strategy,
                            lastfm_key,
                            progress_cb,
                        );
                        let mut lib = lib_for_fetch;
                        let report = fetcher.fetch_missing_covers(&mut lib).await;
                        tracing::info!(
                            "Cover auto-fetch: {} found, {} missing, {} errors",
                            report.found,
                            report.missing,
                            report.errors,
                        );
                        let merged_lib = {
                            let mut current = library.write();
                            let mut changed = false;

                            for fetched_album in lib.albums.iter() {
                                let Some(fetched_cover) = fetched_album.cover_path.clone() else {
                                    continue;
                                };

                                let Some(current_album) =
                                    current.albums.iter_mut().find(|a| a.id == fetched_album.id)
                                else {
                                    continue;
                                };

                                if current_album.cover_path.is_none() && !current_album.manual_cover
                                {
                                    current_album.cover_path = Some(fetched_cover);
                                    changed = true;
                                }
                            }

                            changed.then(|| current.clone())
                        };

                        if let Some(merged_lib) = merged_lib {
                            let _ = merged_lib.save(&lib_path());
                        }
                    });
                } else {
                    // No cover fetching — drop the callback so the progress bar closes.
                    drop(progress_cb);
                }
            } else {
                current_lib.tracks.clear();
                current_lib.albums.clear();
                current_lib.root_paths.clear();
                library.set(current_lib.clone());
                let _ = current_lib.save(&lib_path());
            }
        });
    });

    use_effect(move || {
        let route = *current_route.read();
        // Read detail selections so this re-runs on list<->detail toggle, not just
        // on route change (album/artist list and detail are the same Route).
        let album_sel = selected_album_id.read().clone();
        let artist_sel = selected_artist_name.read().clone();
        let pos = match route {
            Route::Album if !album_sel.is_empty() => detail_scroll_positions
                .peek()
                .get(&format!("album:{album_sel}"))
                .copied()
                .unwrap_or(0.0),
            Route::Artist if !artist_sel.is_empty() => detail_scroll_positions
                .peek()
                .get(&format!("artist:{artist_sel}"))
                .copied()
                .unwrap_or(0.0),
            _ => scroll_positions.peek().get(&route).copied().unwrap_or(0.0),
        };
        let _ = dioxus::document::eval(&format!(
            "let el = document.getElementById('main-scroll-area'); if (el) el.scrollTop = {pos};"
        ));
    });

    provide_context(ctrl);
    provide_context(config);
    let discover_now_playing = use_signal(|| None::<String>);
    provide_context(pages::server::discover::DiscoverNowPlaying(
        discover_now_playing,
    ));
    let discover_prefetch_cache = use_signal(std::collections::HashMap::new);
    provide_context(pages::server::discover::DiscoverPrefetchCache(
        discover_prefetch_cache,
    ));
    provide_context(download_queue);
    provide_context(download_progress);
    let sleep_timer_deadline = use_signal(|| None::<u64>);
    provide_context(components::sleep_timer::SleepTimerState(
        sleep_timer_deadline,
    ));
    let add_to_playlist_pending = use_signal(|| None::<reader::models::Track>);
    provide_context(components::add_to_playlist::AddToPlaylistState(
        add_to_playlist_pending,
    ));
    let toast_message = use_signal(|| None::<String>);
    provide_context(components::toast::ToastState(toast_message));
    // SoundCloud "download" buttons drop a permalink here; the Downloads
    // (yt-dlp) page reads + clears it on open.
    let ytdlp_prefill_url = use_signal(|| None::<String>);
    provide_context(components::soundcloud_search::YtdlpPrefillUrl(
        ytdlp_prefill_url,
    ));
    // Unified search-bar source selector (Local / server / SoundCloud / Spotify).
    //
    // Seeded to the default and corrected below rather than read from the
    // config here: `config` is populated by an ASYNC load, so at first render it
    // is still `AppConfig::default()`. Peeking it meant the search source was
    // pinned to Local on every launch — the dropdown said "Local" while a server
    // was active, and the saved choice never came back.
    let mut search_source = use_signal(config::SearchSource::default);
    provide_context(components::search_bar::SearchSourceState(search_source));
    // Adopt the persisted choice once, as soon as the config load lands.
    let mut search_source_seeded = use_signal(|| false);
    use_effect(move || {
        if !*initial_load_done.read() || *search_source_seeded.peek() {
            return;
        }
        search_source_seeded.set(true);
        let conf = config.peek();
        search_source.set(conf.search_source.resolve(conf.server.is_some()));
    });
    // Keep the dropdown honest when the backend is switched somewhere else (the
    // sidebar's Local/Server toggle, the offline auto-switch). An explicit
    // SoundCloud/Spotify pick is left alone — those overlay the backend rather
    // than replacing it. `peek` on the source + the equality guard are what stop
    // this from re-triggering itself.
    use_effect(move || {
        let want = match config.read().active_source {
            config::MusicSource::Server => config::SearchSource::Server,
            _ => config::SearchSource::Local,
        };
        let current = *search_source.peek();
        if matches!(
            current,
            config::SearchSource::Local | config::SearchSource::Server
        ) && current != want
        {
            search_source.set(want);
        }
    });
    provide_context(scroll_positions);
    provide_context(fetched_artist_images);
    provide_context(is_fetching_artist_images);
    provide_context(components::NavigationController {
        current_route,
        selected_artist_name,
        selected_artist_channel_id,
        selected_album_id,
    });

    // Sidebar collapse state. On Android the sidebar is an overlay drawer that
    // starts collapsed and is toggled by the mobile header hamburger; the
    // Sidebar component reads this from context.
    let mut is_sidebar_collapsed = use_signal(|| cfg!(target_os = "android"));
    use_context_provider(|| components::sidebar::SidebarCollapsed(is_sidebar_collapsed));

    hooks::use_player_task(ctrl);

    // Inject CSS for all custom themes reactively
    let custom_themes_css = use_memo(move || {
        config
            .read()
            .custom_themes
            .iter()
            .map(|(id, ct)| utils::themes::custom_theme_to_css(id, &ct.vars))
            .collect::<Vec<_>>()
            .join("\n\n")
    });

    use_effect(move || {
        let css = custom_themes_css.read().clone();
        // Serialize as a JSON string literal so no CSS content can escape the JS context
        let css_json = serde_json::to_string(&css).unwrap_or_else(|_| "\"\"".to_string());
        let _ = dioxus::document::eval(&format!(
            r#"(function(){{
                let el = document.getElementById('custom-themes-style');
                if (!el) {{ el = document.createElement('style'); el.id = 'custom-themes-style'; document.head.appendChild(el); }}
                el.textContent = {css_json};
            }})()"#
        ));
    });

    let theme_class = use_memo(move || {
        if config.read().theme == "album-art" {
            "theme-default".to_string()
        } else {
            format!("theme-{}", config.read().theme)
        }
    });

    let is_rtl = i18n::is_rtl();
    let dir = if is_rtl { "rtl" } else { "ltr" };
    let content_row_class = "flex flex-1 overflow-hidden";
    #[cfg(not(target_arch = "wasm32"))]
    let update_banner_state = update_banner.read().clone();

    let background_style = use_memo(move || {
        if config.read().theme == "album-art" {
            utils::color::get_background_style(palette.read().as_deref())
        } else {
            "background-color: var(--color-black); background-image: none;".to_string()
        }
    });

    let reduce_animations = use_memo(move || config.read().reduce_animations);
    let active_source = use_memo(move || config.read().active_source);

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: THEME_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        document::Link { rel: "stylesheet", href: REDUCED_ANIMATIONS_CSS }
        WindowsToolbarIconAssets {}
        document::Script {
            "(function(){{
                ['https://fonts.bunny.net/css?family=jetbrains-mono:400,500,700,800&display=swap',
                 'https://cdnjs.cloudflare.com/ajax/libs/font-awesome/6.5.1/css/all.min.css']
                .forEach(function(href){{
                    var l=document.createElement('link');
                    l.rel='stylesheet';l.href=href;
                    document.head.appendChild(l);
                }});
            }})();"
        }

        div {
            class: "flex flex-col h-screen text-white select-none overflow-x-hidden {theme_class}",
            style: "{background_style}",
            dir: "{dir}",
            "data-platform": if cfg!(target_os = "android") { "android" } else { "desktop" },
            "data-reduce-animations": "{reduce_animations}",
            tabindex: "0",
            autofocus: true,
            onkeydown: move |evt| {
                use dioxus::prelude::Key;
                let key = evt.key();
                if key == Key::Escape {
                    is_fullscreen.set(false);
                } else if key == Key::Character(" ".into()) {
                    ctrl.toggle();
                    evt.prevent_default();
                }
            },
            if cfg!(any(target_os = "linux", target_os = "windows")) {
                div { dir: "ltr", Titlebar {} }
            }

            if active_source == config::MusicSource::Local {
                if let Some(file) = scan_current_file.read().clone() {
                    div {
                        class: "flex-shrink-0",
                        div {
                            class: "h-[2px] bg-white/5 overflow-hidden",
                            div { class: "h-full w-1/4 bg-[var(--color-primary,#6366f1)] animate-scan" }
                        }
                        div {
                            class: "px-3 py-[3px] flex items-center gap-2 bg-black/30 border-b border-white/5",
                            i { class: "fa-solid fa-compact-disc fa-spin text-[9px] text-white/30 flex-shrink-0" }
                            span {
                                class: "text-[10px] text-white/35 font-mono truncate",
                                if file.is_empty() {
                                    "Scanning library…"
                                } else {
                                    "{file}"
                                }
                            }
                        }
                    }
                }
            }

            // Only show playback errors when the active server is YouTube
            // Music — other backends (Jellyfin/Subsonic/Custom) surface
            // their own errors via the settings popup, and a lingering YT
            // error from a previous session shouldn't haunt a switched-to
            // server.
            if config
                .read()
                .server
                .as_ref()
                .map(|s| s.service == config::MusicService::YtMusic)
                .unwrap_or(false)
            {
                if let Some(msg) = ctrl.playback_error.read().clone() {
                    div {
                        class: "flex-shrink-0",
                        div {
                            class: "flex items-center justify-between gap-3 px-4 py-2 bg-rose-500/15 border-b border-rose-500/20 text-rose-200 text-sm",
                            div {
                                class: "flex items-center gap-2 whitespace-pre-line",
                                i { class: "fa-solid fa-triangle-exclamation text-xs" }
                                span { "{msg}" }
                            }
                            button {
                                class: "opacity-50 hover:opacity-100 transition-opacity p-1",
                                onclick: move |_| ctrl.playback_error.set(None),
                                i { class: "fa-solid fa-xmark text-xs" }
                            }
                        }
                    }
                }
            }

            if let Some(is_offline) = *network_banner.read() {
                div {
                    class: "flex-shrink-0",
                    div {
                        class: if is_offline {
                            "flex items-center justify-between gap-3 px-4 py-2 bg-amber-500/15 border-b border-amber-500/20 text-amber-300 text-sm"
                        } else {
                            "flex items-center justify-between gap-3 px-4 py-2 bg-emerald-500/15 border-b border-emerald-500/20 text-emerald-300 text-sm"
                        },
                        div {
                            class: "flex items-center gap-2",
                            i { class: if is_offline { "fa-solid fa-wifi-slash text-xs" } else { "fa-solid fa-wifi text-xs" } }
                            span {
                                if is_offline {
                                    "No internet connection — switched to offline mode"
                                } else {
                                    "Back online — switched to server mode"
                                }
                            }
                            if is_offline {
                                button {
                                    class: "ml-2 text-xs underline opacity-70 hover:opacity-100 transition-opacity",
                                    onclick: move |_| {
                                        config.write().active_source = config::MusicSource::Server;
                                        network_banner.set(None);
                                    },
                                    "Keep server mode"
                                }
                            }
                        }
                        button {
                            class: "opacity-50 hover:opacity-100 transition-opacity p-1",
                            onclick: move |_| network_banner.set(None),
                            i { class: "fa-solid fa-xmark text-xs" }
                        }
                    }
                }
            }

            if let Some(update) = {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    update_banner_state.clone()
                }
                #[cfg(target_arch = "wasm32")]
                {
                    None
                }
            } {
                div {
                    class: "flex-shrink-0",
                    div {
                        class: "flex items-center justify-between gap-3 px-4 py-2 bg-sky-500/15 border-b border-sky-500/20 text-sky-200 text-sm",
                        div {
                            class: "flex items-center gap-2",
                            i { class: "fa-solid fa-download text-xs" }
                            span { class: "font-medium", "{i18n::t(\"update_available\")} - " }
                            span { "{i18n::t_with(\"update_banner_message\", &[(\"version\", update.version.clone())])}" }
                            if !cfg!(target_os = "android") {
                                button {
                                    class: "ml-2 text-xs underline opacity-80 hover:opacity-100 transition-opacity",
                                    onclick: {
                                        let release_url = update.release_url.clone();
                                        move |_| {
                                            #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
                                            if let Err(e) = webbrowser::open(&release_url) {
                                                tracing::error!("Failed to open release page: {}", e);
                                            }
                                            #[cfg(target_os = "android")]
                                            let _ = &release_url;
                                        }
                                    },
                                    "{i18n::t(\"view_release\")}"
                                }
                            }
                            {
                                #[cfg(not(target_arch = "wasm32"))]
                                { update_button(update.installer_url.clone(), update.version.clone(), update_status) }
                                #[cfg(target_arch = "wasm32")]
                                { rsx! {} }
                            }
                        }
                        button {
                            class: "opacity-50 hover:opacity-100 transition-opacity p-1",
                            onclick: move |_| update_banner.set(None),
                            i { class: "fa-solid fa-xmark text-xs" }
                        }
                    }
                }
            }

            if config.read().player_bar_position == config::PlayerBarPosition::Top {
                Bottombar {
                    library: library,
                    favorites_store,
                    config,
                    current_song_cover_url: current_song_cover_url,
                    current_song_title: current_song_title,
                    current_song_artist: current_song_artist,
                    player: player,
                    is_playing: is_playing,
                    is_fullscreen: is_fullscreen,
                    current_song_duration: current_song_duration,
                    current_song_progress: current_song_progress,
                    queue: queue,
                    current_queue_index: current_queue_index,
                    volume: volume,
                    persisted_volume: persisted_volume,
                    is_rightbar_open: is_rightbar_open,
                }
            }
            // App-wide add-to-playlist picker — opened by track rows and the
            // now-playing bar's right-click menu via the AddToPlaylistState
            // context.
            components::add_to_playlist::AddToPlaylistHost {
                config: config,
                playlist_store: playlist_store,
            }
            components::toast::ToastHost {}
            div {
                class: "{content_row_class}",
                Sidebar {
                    current_route,
                    on_navigate: move |route| {
                        if route == Route::Album {
                            selected_album_id.set(String::new());
                        }
                        if route == Route::Artist {
                            selected_artist_name.set(String::new());
                            selected_artist_channel_id.set(None);
                        }
                        // Clicking "Playlists" in the sidebar should always land
                        // on the overview, not whichever playlist was last open.
                        if route == Route::Playlists {
                            selected_playlist_id.set(None);
                            discover_selected_playlist_id.set(None);
                        }
                        current_route.set(route);
                    }
                }
                div {
                    id: "main-scroll-area",
                    class: if cfg!(target_os = "android") { "flex-1 min-h-0 flex flex-col overflow-hidden relative" } else { "flex-1 overflow-y-auto" },
                    onscroll: move |evt| {
                        let pos = evt.scroll_top();
                        let route = *current_route.peek();
                        let album_sel = selected_album_id.peek().clone();
                        let artist_sel = selected_artist_name.peek().clone();
                        match route {
                            Route::Album if !album_sel.is_empty() => {
                                detail_scroll_positions
                                    .write()
                                    .insert(format!("album:{album_sel}"), pos);
                            }
                            Route::Artist if !artist_sel.is_empty() => {
                                detail_scroll_positions
                                    .write()
                                    .insert(format!("artist:{artist_sel}"), pos);
                            }
                            _ => {
                                scroll_positions.write().insert(route, pos);
                            }
                        }
                    },

                    if cfg!(target_os = "android") {
                        {
                            let is_details = match *current_route.read() {
                                Route::Album => !selected_album_id.read().is_empty(),
                                Route::Artist => !selected_artist_name.read().is_empty(),
                                Route::Playlists => selected_playlist_id.read().is_some(),
                                _ => false,
                            };
                            let page_title = match *current_route.read() {
                                Route::Home => i18n::t("home"),
                                Route::Search => i18n::t("search"),
                                Route::Library => i18n::t("library"),
                                Route::Album => if is_details { i18n::t("album") } else { i18n::t("albums") },
                                Route::Artist => if is_details { i18n::t("artist") } else { i18n::t("artists") },
                                Route::Playlists => i18n::t("playlists"),
                                Route::Favorites => i18n::t("favorites"),
                                Route::Settings => i18n::t("settings"),
                                _ => i18n::t("home"),
                            };
                            rsx! {
                                div { class: "shrink-0 z-[60] bg-black/60 backdrop-blur-2xl border-b border-white/5 pt-[env(safe-area-inset-top)] flex items-center h-[calc(env(safe-area-inset-top)_+_2.75rem)] px-3 shadow-xl",
                                    if is_details {
                                        button {
                                            class: "w-10 h-10 flex items-center justify-center rounded-xl bg-white/5 text-white active:scale-95 transition-all border border-white/10",
                                            onclick: move |_| {
                                                match *current_route.peek() {
                                                    Route::Album => selected_album_id.set(String::new()),
                                                    Route::Artist => {
                                                        selected_artist_name.set(String::new());
                                                        selected_artist_channel_id.set(None);
                                                    }
                                                    Route::Playlists => selected_playlist_id.set(None),
                                                    _ => {}
                                                }
                                            },
                                            i { class: "fa-solid fa-arrow-left text-lg" }
                                        }
                                    } else {
                                        button {
                                            class: "w-10 h-10 flex items-center justify-center rounded-xl bg-white/5 text-white active:scale-95 transition-all border border-white/10",
                                            onclick: move |_| is_sidebar_collapsed.toggle(),
                                            i { class: "fa-solid fa-bars text-lg" }
                                        }
                                    }
                                    div { class: "flex-1 flex justify-center pr-10",
                                        h2 {
                                            class: "text-[13px] font-black tracking-[0.2em] text-white/90 uppercase",
                                            style: "font-family: 'JetBrains Mono', monospace;",
                                            "{page_title}"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    div { class: if cfg!(target_os = "android") { "relative flex-1 min-h-0 overflow-y-auto" } else { "contents" },
                    match *current_route.read() {
                        Route::Home => rsx! {
                            pages::home::Home {
                                library,
                                playlist_store,
                                favorites_store,
                                on_select_album: move |id: String| {
                                    selected_album_id.set(id);
                                    current_route.set(Route::Album);
                                },
                                on_play_album: move |id: String| {
                                    selected_album_id.set(id.clone());

                                    let lib = library.peek();
                                    let is_jelly = id.starts_with("jellyfin:");
                                    let mut tracks: Vec<reader::Track> = if is_jelly {
                                        lib.jellyfin_tracks.iter().filter(|t| t.album_id == id).cloned().collect()
                                    } else {
                                        lib.tracks.iter().filter(|t| t.album_id == id).cloned().collect()
                                    };

                                    if !tracks.is_empty() {
                                        tracks.sort_by(|a, b| {
                                            let disc_cmp = a.disc_number.unwrap_or(1).cmp(&b.disc_number.unwrap_or(1));
                                            if disc_cmp == std::cmp::Ordering::Equal {
                                                a.track_number.unwrap_or(0).cmp(&b.track_number.unwrap_or(0))
                                            } else {
                                                disc_cmp
                                            }
                                        });
                                        queue.set(tracks);
                                        ctrl.play_track(0);
                                    }
                                    current_route.set(Route::Album);
                                },
                                on_select_playlist: move |id: String| {
                                    selected_playlist_id.set(Some(id));
                                    current_route.set(Route::Playlists);
                                },
                                on_search_artist: move |artist: String| {
                                    selected_artist_name.set(artist);
                                    selected_artist_channel_id.set(None);
                                    current_route.set(Route::Artist);
                                }
                            }
                        },
                        Route::Discover => rsx! {
                            pages::server::discover::DiscoverPage {
                                library: library,
                                on_select_album: move |id: String| {
                                    selected_album_id.set(id);
                                    current_route.set(Route::Album);
                                },
                                on_select_playlist: move |(id, title): (String, String)| {
                                    discover_selected_playlist_id.set(Some(id));
                                    discover_selected_playlist_title.set(Some(title));
                                    discover_playlist_origin.set(Route::Discover);
                                    current_route.set(Route::DiscoverPlaylist);
                                },
                                on_open_artist: move |(cid, name): (String, String)| {
                                    selected_artist_channel_id.set(Some(cid));
                                    selected_artist_name.set(name);
                                    current_route.set(Route::Artist);
                                },
                                on_search_artist: move |name: String| {
                                    search_query.set(name);
                                    current_route.set(Route::Search);
                                },
                            }
                        },
                        Route::DiscoverPlaylist => rsx! {
                            pages::server::discover::DiscoverPlaylistDetail {
                                selected_playlist_id: discover_selected_playlist_id,
                                selected_playlist_title: discover_selected_playlist_title,
                                on_back: move |_| {
                                    // Mirror DiscoverArtist: clear id so
                                    // re-opening the same playlist refetches.
                                    discover_selected_playlist_id.set(None);
                                    discover_selected_playlist_title.set(None);
                                    current_route.set(*discover_playlist_origin.peek());
                                },
                            }
                        },
                        Route::Search => rsx! {
                            pages::search::Search {
                                library: library,
                                config: config,
                                playlist_store: playlist_store,
                                search_query: search_query,
                                player: player,
                                is_playing: is_playing,
                                current_playing: current_playing,
                                current_song_cover_url: current_song_cover_url,
                                current_song_title: current_song_title,
                                current_song_artist: current_song_artist,
                                current_song_duration: current_song_duration,
                                current_song_progress: current_song_progress,
                                queue: queue,
                                current_queue_index: current_queue_index,
                                on_select_album: move |id: String| {
                                    selected_album_id.set(id);
                                    current_route.set(Route::Album);
                                },
                                on_select_playlist: move |(id, title): (String, String)| {
                                    discover_selected_playlist_id.set(Some(id));
                                    discover_selected_playlist_title.set(Some(title));
                                    discover_playlist_origin.set(Route::Search);
                                    current_route.set(Route::DiscoverPlaylist);
                                },
                                on_open_artist: move |(cid, name): (String, String)| {
                                    selected_artist_channel_id.set(Some(cid));
                                    selected_artist_name.set(name);
                                    current_route.set(Route::Artist);
                                },
                                on_search_artist: move |name: String| {
                                    search_query.set(name);
                                },
                            }
                        },
                        Route::Library => rsx! {
                            pages::library::LibraryPage {
                                library: library,
                                config: config,
                                playlist_store: playlist_store,
                                on_rescan: move |_| *trigger_rescan.write() += 1,
                                player: player,
                                is_playing: is_playing,
                                current_playing: current_playing,
                                current_song_cover_url: current_song_cover_url,
                                current_song_title: current_song_title,
                                current_song_artist: current_song_artist,
                                current_song_duration: current_song_duration,
                                current_song_progress: current_song_progress,
                                queue: queue,
                                current_queue_index: current_queue_index,
                            }
                        },
                        Route::Album => rsx! {
                            pages::album::Album {
                                library: library,
                                config: config,
                                album_id: selected_album_id,
                                playlist_store: playlist_store,
                                queue: queue,
                                current_queue_index: current_queue_index,
                            }
                        },
                        Route::Artist => {
                            // YT Music gets the rich YT-backed profile (banner,
                            // top songs, albums, related) ONLY when an artist
                            // is actually selected. The Artists sidebar tab /
                            // back-to-list navigation lands with both signals
                            // cleared — fall through to the library-driven
                            // grid in that case (populated on YT from followed
                            // artists + liked-song artists by the library
                            // sync). Local / Jellyfin / Subsonic keep the
                            // library-driven page in all cases.
                            let is_ytmusic = config
                                .read()
                                .server
                                .as_ref()
                                .map(|s| s.service == config::MusicService::YtMusic)
                                .unwrap_or(false);
                            let has_selection = !selected_artist_name.read().is_empty()
                                || selected_artist_channel_id.read().is_some();
                            if is_ytmusic && has_selection {
                                rsx! {
                                    pages::server::discover::DiscoverArtistPage {
                                        selected_artist_id: selected_artist_channel_id,
                                        selected_artist_name: selected_artist_name,
                                        on_back: move |_| {
                                            // Empty selection on Route::Artist renders the grid.
                                            selected_artist_name.set(String::new());
                                            selected_artist_channel_id.set(None);
                                            current_route.set(Route::Artist);
                                        },
                                        on_select_album: move |id: String| {
                                            selected_album_id.set(id);
                                            current_route.set(Route::Album);
                                        },
                                        on_select_playlist: move |(id, title): (String, String)| {
                                            discover_selected_playlist_id.set(Some(id));
                                            discover_selected_playlist_title.set(Some(title));
                                            discover_playlist_origin.set(Route::Artist);
                                            current_route.set(Route::DiscoverPlaylist);
                                        },
                                        on_open_artist: move |(cid, name): (String, String)| {
                                            selected_artist_channel_id.set(Some(cid));
                                            selected_artist_name.set(name);
                                        },
                                        on_search_artist: move |name: String| {
                                            search_query.set(name);
                                            current_route.set(Route::Search);
                                        },
                                    }
                                }
                            } else {
                                rsx! {
                                    pages::artist::Artist {
                                        library: library,
                                        config: config,
                                        artist_name: selected_artist_name,
                                        playlist_store: playlist_store,
                                        player: player,
                                        on_navigate: move |album_id| {
                                            selected_album_id.set(album_id);
                                            current_route.set(Route::Album);
                                        },
                                        is_playing: is_playing,
                                        current_playing: current_playing,
                                        current_song_cover_url: current_song_cover_url,
                                        current_song_title: current_song_title,
                                        current_song_artist: current_song_artist,
                                        current_song_duration: current_song_duration,
                                        current_song_progress: current_song_progress,
                                        queue: queue,
                                        current_queue_index: current_queue_index,
                                    }
                                }
                            }
                        },
                        Route::Favorites => rsx! {
                            pages::favorites::FavoritesPage {
                                favorites_store,
                                library,
                                config,
                                playlist_store,
                                player,
                                is_playing,
                                current_playing,
                                current_song_cover_url,
                                current_song_title,
                                current_song_artist,
                                current_song_duration,
                                current_song_progress,
                                queue,
                                current_queue_index,
                            }
                        },
                        Route::Playlists => rsx! {
                            pages::playlists::PlaylistsPage {
                                playlist_store: playlist_store,
                                library: library,
                                config: config,
                                selected_playlist_id: selected_playlist_id,
                            }
                        },
                        Route::Activity => rsx! {
                          pages::activity::Activity {
                              library: library,
                              config: config,
                          }
                        },
                        Route::Radio => rsx! {
                            pages::radio::Radio {
                                config: config,
                            }
                        },
                        #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android"), not(target_os = "ios")))]
                        Route::Ytdlp => rsx! { pages::ytdlp::YtdlpPage { config, trigger_rescan } },
                        #[cfg(target_arch = "wasm32")]
                        Route::Ytdlp => rsx! { pages::settings::Settings { config } },
                        Route::Settings => rsx! { pages::settings::Settings { config } },
                        #[cfg(not(target_os = "android"))]
                        Route::ThemeEditor => rsx! { pages::theme_editor::ThemeEditorPage { config } },
                    }
                    }
                }
                Rightbar {
                    library: library,
                    is_rightbar_open: is_rightbar_open,
                    width: rightbar_width,
                    current_song_duration: current_song_duration,
                    current_song_progress: current_song_progress,
                    queue: queue,
                    current_queue_index: current_queue_index,
                    current_song_title: current_song_title,
                    current_song_artist: current_song_artist,
                    current_song_album: current_song_album,
                }
            }
            Fullscreen {
                library: library,
                player: player,
                is_playing: is_playing,
                is_fullscreen: is_fullscreen,
                current_song_duration: current_song_duration,
                current_song_progress: current_song_progress,
                queue: queue,
                current_song_album: current_song_album,
                current_queue_index: current_queue_index,
                current_song_title: current_song_title,
                current_song_bitrate: current_song_bitrate,
                current_song_artist: current_song_artist,
                current_song_cover_url: current_song_cover_url,
                volume: volume,
                persisted_volume: persisted_volume,
                palette: palette,
            }
            DownloadOverlay { queue: download_queue }
            // First-run welcome: shown once on a fresh config, then never again.
            if *initial_load_done.read() && !config.read().onboarded {
                components::onboarding::OnboardingModal {
                    on_close: move |_| { config.write().onboarded = true; },
                    on_open_settings: move |_| {
                        config.write().onboarded = true;
                        current_route.set(Route::Settings);
                    },
                }
            }
            if config.read().player_bar_position == config::PlayerBarPosition::Bottom {
                Bottombar {
                    library: library,
                    favorites_store,
                    config,
                    current_song_cover_url: current_song_cover_url,
                    current_song_title: current_song_title,
                    current_song_artist: current_song_artist,
                    player: player,
                    is_playing: is_playing,
                    is_fullscreen: is_fullscreen,
                    current_song_duration: current_song_duration,
                    current_song_progress: current_song_progress,
                    queue: queue,
                    current_queue_index: current_queue_index,
                    volume: volume,
                    persisted_volume: persisted_volume,
                    is_rightbar_open: is_rightbar_open,
                }
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), target_os = "windows"))]
mod update_tests {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Unique scratch directory per test — these touch the real filesystem.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kopuz-update-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            zip.start_file(*name, opts).expect("start entry");
            zip.write_all(body).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }

    fn install_with(dir: &Path, files: &[(&str, &[u8])]) {
        for (rel, body) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    #[test]
    fn replaces_the_install_and_parks_the_previous_files() {
        let root = scratch("replace");
        let install = root.join("install");
        install_with(
            &install,
            &[("kopuz.exe", b"OLD-BINARY"), ("assets/app.css", b"old-css")],
        );
        let zip = root.join("update.zip");
        write_zip(
            &zip,
            &[("kopuz.exe", b"NEW-BINARY"), ("assets/app.css", b"new-css")],
        );

        let exe = apply_zip_update_into(&zip, &install).expect("update should apply");

        assert_eq!(exe, install.join("kopuz.exe"));
        assert_eq!(std::fs::read(install.join("kopuz.exe")).unwrap(), b"NEW-BINARY");
        assert_eq!(
            std::fs::read(install.join("assets/app.css")).unwrap(),
            b"new-css"
        );
        // The running binary can't be deleted yet, so it must be parked, not lost.
        assert_eq!(
            std::fs::read(install.join("kopuz.exe.old")).unwrap(),
            b"OLD-BINARY",
            "the previous binary must be recoverable until the next launch",
        );
        assert!(
            !install.join(".kopuz-update").exists(),
            "staging directory must be cleaned up",
        );
    }

    #[test]
    fn sweep_removes_the_parked_files() {
        let root = scratch("sweep");
        install_with(
            &root,
            &[
                ("kopuz.exe.old", b"stale"),
                ("assets/app.css.old", b"stale"),
                ("kopuz.exe", b"live"),
            ],
        );
        // Same walk the startup sweep performs, rooted at the scratch dir.
        fn sweep(dir: &Path, depth: u8) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if depth > 0 {
                        sweep(&path, depth - 1);
                    }
                } else if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("old"))
                {
                    std::fs::remove_file(&path).unwrap();
                }
            }
        }
        sweep(&root, 2);

        assert!(!root.join("kopuz.exe.old").exists());
        assert!(!root.join("assets/app.css.old").exists());
        assert!(root.join("kopuz.exe").exists(), "live files stay");
    }

    #[test]
    fn refuses_an_archive_without_the_binary_and_leaves_the_install_alone() {
        let root = scratch("no-exe");
        let install = root.join("install");
        install_with(&install, &[("kopuz.exe", b"OLD-BINARY")]);
        let zip = root.join("update.zip");
        write_zip(&zip, &[("readme.txt", b"nothing useful here")]);

        let err = apply_zip_update_into(&zip, &install).expect_err("must be rejected");

        assert!(err.contains("kopuz.exe"), "unexpected error: {err}");
        assert_eq!(
            std::fs::read(install.join("kopuz.exe")).unwrap(),
            b"OLD-BINARY",
            "a bad archive must not touch the installation",
        );
        assert!(!install.join(".kopuz-update").exists());
    }

    #[test]
    fn zip_slip_entries_cannot_escape_the_install_directory() {
        let root = scratch("slip");
        let install = root.join("install");
        install_with(&install, &[("kopuz.exe", b"OLD-BINARY")]);
        let zip = root.join("update.zip");
        write_zip(
            &zip,
            &[
                ("../../pwned.txt", b"escaped"),
                ("kopuz.exe", b"NEW-BINARY"),
            ],
        );

        apply_zip_update_into(&zip, &install).expect("the valid entry should still apply");

        assert!(
            !root.join("pwned.txt").exists() && !root.parent().unwrap().join("pwned.txt").exists(),
            "a traversal entry must never be written outside the install dir",
        );
        assert_eq!(std::fs::read(install.join("kopuz.exe")).unwrap(), b"NEW-BINARY");
    }
}
