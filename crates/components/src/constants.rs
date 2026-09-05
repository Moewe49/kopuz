// Desktop: zero-min flexible columns, same as Android. The old fixed mins
// (200/150/150) summed with the gaps to a ~760px floor, so any content area
// narrower than that — a portrait secondary monitor, the queue panel open —
// overflowed into a horizontal scrollbar and the columns looked stretched. The
// fr ratios still split a wide row exactly as before; only a narrow one now
// shrinks and truncates instead of forcing a sideways scroll.
#[cfg(not(target_os = "android"))]
pub const COLUMNS_MODERN: &str = "40px minmax(0, 2fr) minmax(0, 1fr) minmax(0, 1fr) 64px 40px";
#[cfg(not(target_os = "android"))]
pub const COLUMNS_NORMAL: &str = "20px minmax(0, 2fr) minmax(0, 1fr) minmax(0, 1fr) 64px 40px";
#[cfg(not(target_os = "android"))]
pub const COLUMNS_MODERN_ALBUM: &str = "40px minmax(0, 2fr) minmax(0, 1fr) 64px 40px";
#[cfg(not(target_os = "android"))]
pub const COLUMNS_NORMAL_ALBUM: &str = "20px minmax(0, 2fr) minmax(0, 1fr) 64px 40px";

// Android: zero column mins so the grid never exceeds the viewport width (no
// horizontal scroll). Secondary text columns collapse first; title keeps the
// largest fr share.
#[cfg(target_os = "android")]
pub const COLUMNS_MODERN: &str = "28px minmax(0, 2fr) minmax(0, 1fr) minmax(0, 1fr) 48px 28px";
#[cfg(target_os = "android")]
pub const COLUMNS_NORMAL: &str = "18px minmax(0, 2fr) minmax(0, 1fr) minmax(0, 1fr) 48px 28px";
#[cfg(target_os = "android")]
pub const COLUMNS_MODERN_ALBUM: &str = "28px minmax(0, 2fr) minmax(0, 1fr) 48px 28px";
#[cfg(target_os = "android")]
pub const COLUMNS_NORMAL_ALBUM: &str = "18px minmax(0, 2fr) minmax(0, 1fr) 48px 28px";
