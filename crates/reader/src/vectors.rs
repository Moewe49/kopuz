//! Storing one vector per track, and finding the nearest ones.
//!
//! # Why this is a file and not a database
//!
//! A style vector is 400 floats — 1600 bytes. Measured on 5000 tracks, which
//! is a large personal library: 8.05 MB on disk, and a full scan answers a
//! query in 1.2 ms. An index would be optimising something nobody waits for.
//!
//! The alternative was SQLite with a vector extension. That would mean pulling
//! a native library into a crate the Android build compiles — a build that
//! cannot be compiled or tested on this machine — to save a millisecond. The
//! trade is not worth it. `full_scan_is_fast_enough_to_need_no_index` records
//! the measurement and re-runs it on demand, so the decision can be revisited
//! against numbers rather than opinion if a library ever grows tenfold.
//!
//! # Format
//!
//! A small binary file rather than JSON: 1600 bytes of floats become roughly
//! 5 kB of decimal text, and the parse cost shows up on every launch.
//!
//! ```text
//! magic   "KPZV"        4 bytes
//! version u8            1 byte
//! dim     u16 LE        2 bytes
//! count   u32 LE        4 bytes
//! entries count times:
//!   id_len u16 LE, id bytes (UTF-8), dim * f32 LE
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

const MAGIC: &[u8; 4] = b"KPZV";

/// Bumped whenever the vectors *mean* something different — a change to the
/// audio preprocessing, the model, or the pooling. Not just when the file
/// layout changes.
///
/// This exists because of a real failure: a magnitude spectrogram was used
/// where the model expects power, every vector in the store was quietly wrong,
/// and nothing noticed until a listener said the mixes contained the wrong
/// songs. Vectors carry no evidence of how they were made, so the version is
/// the only thing that can invalidate them — and an outdated store is
/// discarded and rebuilt rather than reported as an error, because the data is
/// regenerable and a broken recommendation is worse than a slow one.
///
/// v1: magnitude spectrogram (wrong).
/// v2: power spectrogram, matching Essentia at cosine 0.999919.
pub const FEATURE_VERSION: u8 = 2;
const VERSION: u8 = FEATURE_VERSION;

/// One vector per track id, all of the same width.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorStore {
    dim: usize,
    entries: HashMap<String, Vec<f32>>,
}

#[derive(Debug, PartialEq)]
pub enum StoreError {
    /// Not a vector file, or a version this build does not know.
    Format(&'static str),
    /// A store written by an older feature version. Its contents are stale
    /// rather than corrupt, so the caller starts again instead of failing.
    Outdated { found: u8 },
    /// The file ends in the middle of a record.
    Truncated,
    /// A vector of a different width than the rest of the store.
    WrongWidth { expected: usize, got: usize },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Format(m) => write!(f, "not a vector store: {m}"),
            StoreError::Outdated { found } => write!(
                f,
                "vector store was written by feature version {found}, this build needs {VERSION}"
            ),
            StoreError::Truncated => write!(f, "vector store ends mid-record"),
            StoreError::WrongWidth { expected, got } => {
                write!(f, "vector has {got} dimensions, store has {expected}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl VectorStore {
    /// An empty store of a fixed width.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            entries: HashMap::new(),
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&[f32]> {
        self.entries.get(id).map(|v| v.as_slice())
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|s| s.as_str())
    }

    /// Add or replace one track's vector. The first insert into an empty store
    /// fixes its width.
    pub fn insert(&mut self, id: impl Into<String>, vector: Vec<f32>) -> Result<(), StoreError> {
        if self.entries.is_empty() && self.dim == 0 {
            self.dim = vector.len();
        }
        if vector.len() != self.dim {
            return Err(StoreError::WrongWidth {
                expected: self.dim,
                got: vector.len(),
            });
        }
        self.entries.insert(id.into(), vector);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }

    /// The `n` closest tracks to `query`, best first.
    ///
    /// `skip` is what the listener has already heard: passing it here rather
    /// than filtering afterwards means a request for ten suggestions returns
    /// ten, instead of ten minus however many were already known.
    pub fn nearest(
        &self,
        query: &[f32],
        n: usize,
        skip: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, f32)> {
        if query.len() != self.dim || n == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(String, f32)> = self
            .entries
            .iter()
            .filter(|(id, _)| !skip(id))
            .map(|(id, v)| {
                let s = v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>();
                (id.clone(), s)
            })
            .collect();
        // Ties broken by id, because a HashMap has no order and a suggestion
        // list that reshuffles between launches looks broken.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(n);
        scored
    }

    /// Every vector, in a stable order, for feeding into clustering.
    pub fn matrix(&self) -> (Vec<String>, Vec<Vec<f32>>) {
        let mut ids: Vec<&String> = self.entries.keys().collect();
        ids.sort();
        let vectors = ids.iter().map(|id| self.entries[*id].clone()).collect();
        (ids.into_iter().cloned().collect(), vectors)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.entries.len() * (self.dim * 4 + 16));
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(self.dim as u16).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        // Sorted, so the same store always produces the same bytes and a sync
        // or a backup does not see spurious changes.
        let mut ids: Vec<&String> = self.entries.keys().collect();
        ids.sort();
        for id in ids {
            out.extend_from_slice(&(id.len() as u16).to_le_bytes());
            out.extend_from_slice(id.as_bytes());
            for x in &self.entries[id] {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, StoreError> {
        if data.len() < 11 || &data[..4] != MAGIC {
            return Err(StoreError::Format("bad magic"));
        }
        if data[4] < VERSION {
            return Err(StoreError::Outdated { found: data[4] });
        }
        if data[4] != VERSION {
            return Err(StoreError::Format("unknown version"));
        }
        let dim = u16::from_le_bytes([data[5], data[6]]) as usize;
        let count = u32::from_le_bytes([data[7], data[8], data[9], data[10]]) as usize;

        let mut store = Self::new(dim);
        let mut at = 11;
        for _ in 0..count {
            if at + 2 > data.len() {
                return Err(StoreError::Truncated);
            }
            let id_len = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
            at += 2;
            if at + id_len + dim * 4 > data.len() {
                return Err(StoreError::Truncated);
            }
            let id = String::from_utf8(data[at..at + id_len].to_vec())
                .map_err(|_| StoreError::Format("id is not UTF-8"))?;
            at += id_len;
            let vector = data[at..at + dim * 4]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            at += dim * 4;
            store.entries.insert(id, vector);
        }
        Ok(store)
    }

    /// Read a store, treating a missing file as an empty one — the normal
    /// state before anything has been analysed.
    pub fn load(path: &Path, dim: usize) -> Result<Self, StoreError> {
        match std::fs::read(path) {
            Ok(bytes) => match Self::from_bytes(&bytes) {
                // Stale, not broken: start again rather than refuse. The work
                // to rebuild is real but it is work the machine can do on its
                // own, and serving recommendations from vectors that mean
                // something else is worse than serving none.
                Err(StoreError::Outdated { found }) => {
                    tracing::info!(
                        "vector store is from feature version {found}, rebuilding for {VERSION}"
                    );
                    Ok(Self::new(dim))
                }
                other => other,
            },
            Err(_) => Ok(Self::new(dim)),
        }
    }

    /// Write via a temporary file and rename, so a crash or a full disk during
    /// the write leaves the previous store intact rather than a half-written
    /// one that fails to parse on the next launch.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&self.to_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_of(n: usize, dim: usize) -> VectorStore {
        let mut s = VectorStore::new(dim);
        for i in 0..n {
            let mut v = vec![0.1f32; dim];
            v[i % dim] = 1.0;
            s.insert(format!("track{i}"), v).unwrap();
        }
        s
    }

    #[test]
    fn a_store_survives_a_round_trip_unchanged() {
        let s = store_of(20, 8);
        let back = VectorStore::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(s, back);
        assert_eq!(back.len(), 20);
        assert_eq!(back.dim(), 8);
    }

    /// Same content must give the same bytes, or every backup and sync sees a
    /// change that is not one.
    #[test]
    fn serialisation_does_not_depend_on_hashmap_order() {
        let a = store_of(30, 6);
        let mut b = VectorStore::new(6);
        // Insert in the opposite order.
        for i in (0..30).rev() {
            let mut v = vec![0.1f32; 6];
            v[i % 6] = 1.0;
            b.insert(format!("track{i}"), v).unwrap();
        }
        assert_eq!(a.to_bytes(), b.to_bytes());
    }

    /// A truncated or foreign file must be reported, not silently read as
    /// garbage vectors that would quietly poison every recommendation.
    #[test]
    fn a_damaged_file_is_rejected_rather_than_misread() {
        let good = store_of(5, 4).to_bytes();
        assert_eq!(
            VectorStore::from_bytes(b"not a store at all").unwrap_err(),
            StoreError::Format("bad magic")
        );
        assert_eq!(
            VectorStore::from_bytes(&good[..good.len() - 3]).unwrap_err(),
            StoreError::Truncated
        );
        let mut wrong_version = good.clone();
        wrong_version[4] = 99;
        assert_eq!(
            VectorStore::from_bytes(&wrong_version).unwrap_err(),
            StoreError::Format("unknown version")
        );
        // An older feature version is stale rather than damaged.
        let mut older = good.clone();
        older[4] = 1;
        assert_eq!(
            VectorStore::from_bytes(&older).unwrap_err(),
            StoreError::Outdated { found: 1 }
        );
        // An empty file is damaged, not empty — an empty store still has a
        // header.
        assert!(VectorStore::from_bytes(&[]).is_err());
    }

    #[test]
    fn a_vector_of_the_wrong_width_is_refused() {
        let mut s = VectorStore::new(4);
        assert!(s.insert("a", vec![1.0; 4]).is_ok());
        assert_eq!(
            s.insert("b", vec![1.0; 5]).unwrap_err(),
            StoreError::WrongWidth {
                expected: 4,
                got: 5
            }
        );
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn nearest_returns_the_closest_first() {
        let mut s = VectorStore::new(3);
        s.insert("same", vec![1.0, 0.0, 0.0]).unwrap();
        s.insert("near", vec![0.9, 0.436, 0.0]).unwrap();
        s.insert("far", vec![0.0, 0.0, 1.0]).unwrap();
        let hits = s.nearest(&[1.0, 0.0, 0.0], 3, &|_| false);
        assert_eq!(
            hits.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["same", "near", "far"]
        );
        assert!(hits[0].1 > hits[1].1 && hits[1].1 > hits[2].1);
    }

    /// Filtering during the scan, not after: asking for two suggestions must
    /// return two, not two minus the ones already heard.
    #[test]
    fn skipping_known_tracks_still_fills_the_request() {
        let s = store_of(10, 4);
        let hits = s.nearest(&[1.0, 0.1, 0.1, 0.1], 3, &|id| {
            id == "track0" || id == "track4"
        });
        assert_eq!(hits.len(), 3);
        assert!(!hits.iter().any(|(id, _)| id == "track0" || id == "track4"));
    }

    /// A HashMap has no order, so equal scores must be broken deterministically
    /// or the same query gives a different list on the next launch.
    #[test]
    fn equal_scores_are_broken_the_same_way_every_time() {
        let mut s = VectorStore::new(2);
        for id in ["c", "a", "b", "d"] {
            s.insert(id, vec![1.0, 0.0]).unwrap();
        }
        let first = s.nearest(&[1.0, 0.0], 4, &|_| false);
        assert_eq!(
            first.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        for _ in 0..5 {
            assert_eq!(s.nearest(&[1.0, 0.0], 4, &|_| false), first);
        }
    }

    #[test]
    fn a_query_of_the_wrong_width_returns_nothing_rather_than_nonsense() {
        let s = store_of(5, 4);
        assert!(s.nearest(&[1.0, 0.0], 3, &|_| false).is_empty());
        assert!(s.nearest(&[1.0; 4], 0, &|_| false).is_empty());
    }

    #[test]
    fn the_matrix_pairs_ids_with_vectors_in_a_stable_order() {
        let s = store_of(12, 5);
        let (ids, vectors) = s.matrix();
        assert_eq!(ids.len(), vectors.len());
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        for (id, v) in ids.iter().zip(&vectors) {
            assert_eq!(s.get(id).unwrap(), v.as_slice());
        }
    }

    /// Vectors carry no evidence of how they were computed, so a preprocessing
    /// change leaves a store full of confident nonsense. Loading must throw it
    /// away by itself — nobody will remember to.
    #[test]
    fn a_store_from_an_older_feature_version_is_rebuilt_not_refused() {
        let dir = std::env::temp_dir().join("kopuz-vectors-version-test");
        let path = dir.join("vectors.bin");
        let _ = std::fs::create_dir_all(&dir);
        let mut bytes = store_of(5, 4).to_bytes();
        bytes[4] = 1; // written by the version with the wrong spectrogram
        std::fs::write(&path, &bytes).unwrap();

        let loaded = VectorStore::load(&path, 4).expect("stale is not an error");
        assert!(loaded.is_empty(), "stale vectors were kept");
        assert_eq!(loaded.dim(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_store() {
        let path = std::env::temp_dir().join("kopuz-no-such-vectors.bin");
        let _ = std::fs::remove_file(&path);
        let s = VectorStore::load(&path, 400).unwrap();
        assert!(s.is_empty());
        assert_eq!(s.dim(), 400);
    }

    /// The measurement behind the decision not to use a database. Run with:
    ///   cargo test -p reader --lib vectors --release -- --ignored --nocapture
    #[test]
    #[ignore = "measures the full-scan cost that justifies having no index"]
    fn full_scan_is_fast_enough_to_need_no_index() {
        const N: usize = 5_000;
        const DIM: usize = 400;
        let mut s = VectorStore::new(DIM);
        for i in 0..N {
            let mut v = vec![0f32; DIM];
            for (j, x) in v.iter_mut().enumerate() {
                *x = (((i * 31 + j * 17) % 97) as f32 / 97.0) - 0.5;
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in v.iter_mut() {
                *x /= norm;
            }
            s.insert(format!("track{i}"), v).unwrap();
        }
        let query = s.get("track0").unwrap().to_vec();

        let started = std::time::Instant::now();
        let rounds = 20;
        for _ in 0..rounds {
            std::hint::black_box(s.nearest(&query, 25, &|_| false));
        }
        let per_query = started.elapsed() / rounds;

        let bytes = s.to_bytes().len();
        println!(
            "{N} tracks x {DIM} dims: {:.2} MB on disk, {:?} per query",
            bytes as f64 / 1e6,
            per_query
        );
    }

    #[test]
    fn saving_and_loading_preserves_the_store() {
        let dir = std::env::temp_dir().join("kopuz-vectors-test");
        let path = dir.join("vectors.bin");
        let _ = std::fs::remove_file(&path);
        let s = store_of(15, 7);
        s.save(&path).unwrap();
        assert_eq!(VectorStore::load(&path, 7).unwrap(), s);
        // The temporary file must not survive a successful write.
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
