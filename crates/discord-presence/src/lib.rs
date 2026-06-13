pub mod cover_art;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use discord_rich_presence::{
    DiscordIpc, DiscordIpcClient,
    activity::{self, Assets, Timestamps},
};
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::sync::Mutex;
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Self-healing Discord Rich Presence. The connection is established
/// lazily and re-established automatically:
///
/// - If Discord isn't running when the app starts, the first activity
///   update after Discord launches connects.
/// - If Discord closes or restarts mid-session, the broken pipe drops the
///   client and the next update reconnects.
///
/// (The old design connected exactly once in `new()` — start Kopuz before
/// Discord and presence stayed dead for the whole session.)
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
#[derive(Debug)]
pub struct Presence {
    client_id: String,
    client: Mutex<Option<DiscordIpcClient>>,
    last_attempt: Mutex<Option<Instant>>,
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
const RECONNECT_THROTTLE: Duration = Duration::from_secs(5);

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Presence {
    /// Never fails on desktop — the actual connect happens lazily on the
    /// first activity update (and is retried while Discord is closed).
    pub fn new(client_id: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let presence = Self {
            client_id: client_id.to_string(),
            client: Mutex::new(None),
            last_attempt: Mutex::new(None),
        };
        // Best-effort eager connect so presence shows immediately when
        // Discord is already running.
        presence.ensure_connected();
        Ok(presence)
    }

    /// (Re)connect if needed. Attempts are throttled so a closed Discord
    /// doesn't get hammered every player tick.
    fn ensure_connected(&self) -> bool {
        let mut guard = self.client.lock().unwrap();
        if guard.is_some() {
            return true;
        }
        {
            let mut last = self.last_attempt.lock().unwrap();
            if let Some(t) = *last
                && t.elapsed() < RECONNECT_THROTTLE
            {
                return false;
            }
            *last = Some(Instant::now());
        }
        let mut client = DiscordIpcClient::new(&self.client_id);
        match client.connect() {
            Ok(()) => {
                eprintln!("[discord] connected (app id {})", self.client_id);
                *guard = Some(client);
                true
            }
            Err(e) => {
                eprintln!("[discord] connect failed (is Discord running?): {e}");
                false
            }
        }
    }

    /// Run `f` against a live client; on error assume the pipe died
    /// (Discord closed/restarted), drop the client so the next call
    /// reconnects, and surface the error.
    fn with_client(
        &self,
        f: impl FnOnce(&mut DiscordIpcClient) -> Result<(), Box<dyn std::error::Error>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.ensure_connected() {
            return Err("Discord is not running".into());
        }
        let mut guard = self.client.lock().unwrap();
        let Some(client) = guard.as_mut() else {
            return Err("Discord is not running".into());
        };
        match f(client) {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("[discord] set_activity failed (pipe likely closed): {e}");
                let _ = client.close();
                *guard = None;
                Err(e)
            }
        }
    }

    pub fn disconnect(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(mut client) = self.client.lock().unwrap().take() {
            client.close()?;
        }
        Ok(())
    }

    pub fn set_now_playing(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        elapsed_secs: u64,
        duration_secs: u64,
        cover_url: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

        let start_time = now - elapsed_secs as i64;
        let end_time = start_time + duration_secs as i64;

        let timestamps = if duration_secs == u64::MAX {
            Timestamps::new().start(start_time)
        } else {
            Timestamps::new().start(start_time).end(end_time)
        };

        let state = format!("{artist}");

        let mut activity = activity::Activity::new()
            .details(title)
            .state(&state)
            .status_display_type(activity::StatusDisplayType::State)
            .timestamps(timestamps)
            .activity_type(activity::ActivityType::Listening);

        if let Some(url) = cover_url {
            let assets = Assets::new().large_image(url).large_text(album);
            activity = activity.assets(assets);
        }

        self.with_client(|c| Ok(c.set_activity(activity)?))
    }

    pub fn set_paused(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        cover_url: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state = format!("{artist} • Paused");
        let mut activity = activity::Activity::new()
            .details(title)
            .state(&state)
            .status_display_type(activity::StatusDisplayType::State)
            .activity_type(activity::ActivityType::Listening);

        if let Some(url) = cover_url {
            let assets = Assets::new().large_image(url).large_text(album);
            activity = activity.assets(assets);
        }

        self.with_client(|c| Ok(c.set_activity(activity)?))
    }

    pub fn clear_activity(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Nothing to clear if we're not even connected — and we must NOT
        // trigger a reconnect attempt just to clear.
        let mut guard = self.client.lock().unwrap();
        let Some(client) = guard.as_mut() else {
            return Ok(());
        };
        if let Err(e) = client.clear_activity() {
            let _ = client.close();
            *guard = None;
            return Err(Box::new(e));
        }
        Ok(())
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
impl Drop for Presence {
    fn drop(&mut self) {
        let mut guard = match self.client.lock() {
            Ok(c) => c,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(client) = guard.as_mut() {
            let _ = client.close();
        }
    }
}

// Android has no Discord IPC; this no-op stub keeps the `Presence` API surface so the
// shared player-task code compiles unchanged. The app never constructs it on Android
// (`Presence::new` errors), so the context stays `None` and every call site is skipped.
#[cfg(target_os = "android")]
#[derive(Debug)]
pub struct Presence;

#[cfg(target_os = "android")]
impl Presence {
    pub fn new(_client_id: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Err("Discord presence is not available on Android".into())
    }

    pub fn disconnect(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn set_now_playing(
        &self,
        _title: &str,
        _artist: &str,
        _album: &str,
        _elapsed_secs: u64,
        _duration_secs: u64,
        _cover_url: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn set_paused(
        &self,
        _title: &str,
        _artist: &str,
        _album: &str,
        _cover_url: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn clear_activity(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
