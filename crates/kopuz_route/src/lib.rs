#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Route {
    Home,
    Discover,
    DiscoverPlaylist,
    /// The tracklist behind a generated mix tile. Separate from
    /// `DiscoverPlaylist` because a mix has no YouTube playlist id to resolve
    /// against — it already carries its tracks.
    MixDetail,
    Search,
    Library,
    Album,
    Artist,
    Playlists,
    Favorites,
    Activity,
    Radio,
    // yt-dlp downloads are desktop/web only — excluded on Android and iOS
    // (rfd's file dialogs don't build on iOS; mobile has no Downloads page).
    #[cfg(all(not(target_os = "android"), not(target_os = "ios")))]
    Ytdlp,
    Settings,
    #[cfg(not(target_os = "android"))]
    ThemeEditor,
}
