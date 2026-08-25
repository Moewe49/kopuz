#[cfg(not(target_arch = "wasm32"))]
pub mod candidates;
pub mod cover_fetcher;
#[cfg(not(target_arch = "wasm32"))]
pub mod metadata;
pub mod models;
pub mod rediscover;
#[cfg(not(target_arch = "wasm32"))]
pub mod scanner;
pub mod share;
pub mod styles;
pub mod taste;
#[cfg(not(target_arch = "wasm32"))]
pub mod utils;
pub mod vectors;

#[cfg(not(target_arch = "wasm32"))]
pub use metadata::{read, read_cover, write_tags};
pub use models::{
    Album, CoverChange, FavoritesStore, Library, PlaylistFolder, PlaylistStore, Track, TrackEdits,
};
#[cfg(not(target_arch = "wasm32"))]
pub use scanner::scan_directory;
