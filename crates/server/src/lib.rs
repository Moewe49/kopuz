pub mod deps;
pub mod download_queue;
pub mod jellyfin;
pub mod provider;
pub mod soundcloud;
pub mod spotify;
pub mod subsonic;
pub mod ytmusic;

pub use download_queue::{DownloadItem, DownloadProgress, DownloadQueue, DownloadStatus};
