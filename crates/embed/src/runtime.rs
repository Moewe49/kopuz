//! Where to get the ONNX Runtime, and why it is not in the binary.
//!
//! # The measurement behind this
//!
//! Linking ONNX Runtime statically — which is what `ort` does by default —
//! added **17.6 MB** to a binary: the same example measured 3.8 MB without it
//! and 21.4 MB with. The app itself is 26.7 MB, so it would have grown by two
//! thirds, and the in-app updater downloads the whole archive on every
//! release. Turning off DirectML and every other execution provider changed
//! nothing; that size is the CPU runtime itself.
//!
//! Against that: the model is already a runtime download, because its licence
//! forbids bundling it, and recommendations are opt-in anyway. So the runtime
//! rides along with the model. Someone who never turns the feature on pays
//! nothing, and no build — local or CI — needs to fetch a native library.
//!
//! # Version
//!
//! `ort` 2.0.0-rc.13 targets ONNX Runtime 1.28. Microsoft's official release
//! for that version was verified to load and to produce **byte-identical**
//! vectors to the statically linked build.

/// The ONNX Runtime release these bindings expect.
pub const VERSION: &str = "1.28.0";

/// Asset name for the current platform, or `None` where Microsoft publishes
/// nothing for it.
///
/// The gap worth knowing about is **Intel macOS**: release 1.28.0 ships
/// `osx-arm64` and no x64 build at all, so recommendations cannot run there
/// without sourcing a runtime elsewhere. Better to say so than to hand out a
/// URL that 404s.
pub fn asset_name() -> Option<String> {
    let name = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        format!("onnxruntime-win-x64-{VERSION}.zip")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        format!("onnxruntime-win-arm64-{VERSION}.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        format!("onnxruntime-linux-x64-{VERSION}.tgz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        format!("onnxruntime-linux-aarch64-{VERSION}.tgz")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        format!("onnxruntime-osx-arm64-{VERSION}.tgz")
    } else {
        return None;
    };
    Some(name)
}

/// Full download URL for the current platform.
pub fn download_url() -> Option<String> {
    asset_name().map(|name| {
        format!("https://github.com/microsoft/onnxruntime/releases/download/v{VERSION}/{name}")
    })
}

/// File name of the shared library inside that archive.
pub fn library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every asset name here was read off the actual release listing, not
    /// guessed from a pattern — `osx-universal2` looked obvious and returns
    /// 404. If this ever drifts, the download fails at the worst moment: when
    /// a listener has just switched the feature on.
    #[test]
    fn the_url_is_built_from_the_verified_asset_names() {
        let Some(url) = download_url() else {
            // Only the platforms Microsoft does not publish for, which the
            // module documents.
            return;
        };
        assert!(
            url.starts_with("https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/")
        );
        assert!(url.ends_with(".zip") || url.ends_with(".tgz"), "{url}");
        assert!(url.contains(VERSION));
    }

    #[test]
    fn the_library_name_matches_the_platform() {
        let name = library_name();
        if cfg!(target_os = "windows") {
            assert_eq!(name, "onnxruntime.dll");
        } else {
            assert!(name.starts_with("libonnxruntime."));
        }
    }
}
