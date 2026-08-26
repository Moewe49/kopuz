//! Reading the mix set that is actually on this machine's disk.
//!
//! Adding `relay_version` to `MixSet` changed the shape of a file that already
//! exists on every installation. `#[serde(default)]` is supposed to make that
//! a non-event — but "supposed to" is exactly the phrasing that preceded the
//! last two stale-data incidents in this project, both of which were found by
//! a person noticing wrong output rather than by a test.
//!
//! Skipped when there is no such file, so it is inert in CI and on a fresh
//! checkout. Point it somewhere explicitly with `KOPUZ_MIXES_FILE`.

use server::mixes::{MixAction, MixSet, decide};

fn on_disk() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("KOPUZ_MIXES_FILE") {
        return Some(std::path::PathBuf::from(explicit));
    }
    let path = directories::ProjectDirs::from("com", "temidaradev", "kopuz")?
        .config_dir()
        .join("mixes.json");
    path.exists().then_some(path)
}

/// A file written by the previous build carries no `relay_version`. It must
/// still load, and must load as "built here" rather than as something fetched.
#[test]
fn a_mix_set_written_before_this_change_still_loads() {
    let Some(path) = on_disk() else {
        eprintln!("no mixes.json on this machine — skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read mixes.json");
    let set: MixSet = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("the real {} no longer parses: {e}", path.display()));

    assert!(!set.mixes.is_empty(), "the file on disk has no mixes in it");
    eprintln!(
        "{} mixes, feature version {}, relay version {}, generated {}",
        set.mixes.len(),
        set.feature_version,
        set.relay_version,
        set.generated
    );
    // `relay_version` is 0 on a set that has never been published and the
    // version it was published as afterwards — both legitimate. It is not
    // asserted to be either, because which one depends on whether this machine
    // has a relay configured and has run since; what matters is that the field
    // round-trips, which the parse above already proved.

    // The device that wrote this has vectors, so it authors: it rebuilds
    // locally and never reads from the relay, whatever the relay holds.
    assert_eq!(
        decide(&set, set.generated + 60, set.feature_version, true),
        MixAction::Keep,
        "fresh, and this device authors — nothing to do"
    );

    // The same file, read by a phone: no vectors, so it asks rather than
    // rebuilding, quoting back whatever relay version the file carries. This is
    // the exact case the whole design turns on.
    assert_eq!(
        decide(&set, set.generated + 60, 0, true),
        MixAction::Fetch {
            have: set.relay_version
        },
        "a device with no vectors must ask the relay, quoting what it holds"
    );
}

/// The full trip the app will make: load from disk, hand the bytes over,
/// receive them back, stamp the version, and still have the same mixes.
#[test]
fn the_real_file_survives_being_published_and_read_back() {
    let Some(path) = on_disk() else {
        eprintln!("no mixes.json on this machine — skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read mixes.json");
    let original: MixSet = serde_json::from_str(&text).expect("parse");

    // What the desktop sends is the bytes it wrote; what the phone receives is
    // those bytes plus a version. Modelled here without the socket, which the
    // relay crate's own end-to-end test already covers.
    let mut received: MixSet = serde_json::from_slice(text.as_bytes()).expect("parse as received");
    received.relay_version = 7;

    assert_eq!(
        received.mixes, original.mixes,
        "the mixes changed in transit"
    );
    assert_eq!(received.generated, original.generated);
    assert_eq!(received.feature_version, original.feature_version);

    // Written to disk on the receiving device, then read again on next launch.
    let round = serde_json::to_string(&received).expect("re-encode");
    let reloaded: MixSet = serde_json::from_str(&round).expect("reload");
    assert_eq!(
        reloaded.relay_version, 7,
        "the version must survive being written down, or the phone re-fetches \
         fifty kilobytes on every single launch"
    );
    assert_eq!(
        decide(&reloaded, reloaded.generated + 10_000_000, 0, true),
        MixAction::Fetch { have: 7 },
        "and must be quoted back, however old the set has become"
    );
}
