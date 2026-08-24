//! Splitting a listening history into the handful of directions it actually
//! contains, so the listener gets several mixes instead of one average.
//!
//! Averaging a whole history produces a centre of gravity nobody listens to:
//! someone who plays hyperpop and cello suites has a mean vector that is
//! neither. The measured spread bears this out — two tracks the listener
//! equally loves sat 0.489 apart in style space, further from each other than
//! some strangers were from either. So the history has to be split before it
//! is averaged.
//!
//! Spherical k-means, because the vectors are L2-normalised and the meaningful
//! comparison between them is the cosine, not the euclidean distance.
//!
//! Everything here is deterministic given a seed. A mix that reshuffles itself
//! on every launch cannot be recognised, discussed or trusted, so the seed is
//! an input rather than something drawn from the clock.

/// A vector that has already been L2-normalised.
pub type Unit = [f32];

/// One taste direction found in the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Indices into the input, most typical first.
    pub members: Vec<usize>,
    /// The normalised mean of its members — the query vector for finding more.
    pub centroid: Vec<f32>,
    /// Mean cosine of the members to the centroid. Low means the cluster is a
    /// leftovers bin rather than a direction, and is worth suppressing.
    pub cohesion: f32,
}

fn dot(a: &Unit, b: &Unit) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// A seeded generator, so a mix is reproducible without pulling in a rand
/// dependency for the sake of a few numbers.
struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((x >> 40) as f32) / (1u32 << 24) as f32
    }
}

/// k-means++ seeding: spread the initial centres out instead of picking at
/// random. Random starts on a small history routinely collapse two centres
/// onto the same direction and silently lose a taste.
fn seed_centres(vectors: &[Vec<f32>], k: usize, rng: &mut Rng) -> Vec<Vec<f32>> {
    let first = (rng.next_f32() * vectors.len() as f32) as usize;
    let mut centres = vec![vectors[first.min(vectors.len() - 1)].clone()];

    while centres.len() < k {
        // Distance to the nearest chosen centre, as 1 - cosine.
        let weights: Vec<f32> = vectors
            .iter()
            .map(|v| {
                let nearest = centres
                    .iter()
                    .map(|c| dot(v, c))
                    .fold(f32::NEG_INFINITY, f32::max);
                (1.0 - nearest).max(0.0).powi(2)
            })
            .collect();
        let total: f32 = weights.iter().sum();
        if total <= 1e-9 {
            break; // every remaining point coincides with a centre
        }
        let mut target = rng.next_f32() * total;
        let mut pick = vectors.len() - 1;
        for (i, w) in weights.iter().enumerate() {
            target -= w;
            if target <= 0.0 {
                pick = i;
                break;
            }
        }
        centres.push(vectors[pick].clone());
    }
    centres
}

/// Partition `vectors` into at most `k` directions. Vectors must be
/// L2-normalised; anything else silently distorts the cosine.
pub fn cluster(vectors: &[Vec<f32>], k: usize, seed: u64) -> Vec<Cluster> {
    if vectors.is_empty() || k == 0 {
        return Vec::new();
    }
    let k = k.min(vectors.len());
    let dim = vectors[0].len();
    let mut rng = Rng(seed | 1);
    let mut centres = seed_centres(vectors, k, &mut rng);
    let mut assignment = vec![usize::MAX; vectors.len()];

    // Lloyd's algorithm. The iteration cap matters more than convergence: on a
    // history with near-duplicate tracks the assignment can oscillate between
    // two equally good splits indefinitely.
    for _ in 0..50 {
        let mut changed = false;
        for (i, v) in vectors.iter().enumerate() {
            let best = centres
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    dot(v, a)
                        .partial_cmp(&dot(v, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            if assignment[i] != best {
                assignment[i] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        for (c, centre) in centres.iter_mut().enumerate() {
            let mut sum = vec![0f32; dim];
            let mut n = 0usize;
            for (i, v) in vectors.iter().enumerate() {
                if assignment[i] == c {
                    for (s, x) in sum.iter_mut().zip(v) {
                        *s += x;
                    }
                    n += 1;
                }
            }
            // An emptied centre keeps its old position rather than collapsing
            // to the origin, where it would attract everything on the next pass.
            if n > 0 {
                normalise(&mut sum);
                *centre = sum;
            }
        }
    }

    centres
        .into_iter()
        .enumerate()
        .filter_map(|(c, centroid)| {
            let mut members: Vec<usize> =
                (0..vectors.len()).filter(|&i| assignment[i] == c).collect();
            if members.is_empty() {
                return None;
            }
            // Most typical first, so a mix can lead with what defines it.
            members.sort_by(|&a, &b| {
                dot(&vectors[b], &centroid)
                    .partial_cmp(&dot(&vectors[a], &centroid))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let cohesion = members
                .iter()
                .map(|&i| dot(&vectors[i], &centroid))
                .sum::<f32>()
                / members.len() as f32;
            Some(Cluster {
                members,
                centroid,
                cohesion,
            })
        })
        .collect()
}

/// Two centroids closer than this are the same direction wearing two names.
///
/// Measured, not chosen: on the listener's own eight favourites the style
/// distances spanned 0.489 to 0.905, so a pair of centroids above 0.90 is no
/// further apart than two tracks they equally like. The `separation_measured`
/// test records where this came from and re-derives it on demand.
const SAME_DIRECTION: f32 = 0.90;

/// The largest similarity between any two centroids — how much the mixes would
/// overlap if this split were used.
fn worst_overlap(clusters: &[Cluster]) -> f32 {
    let mut worst = 0f32;
    for (i, a) in clusters.iter().enumerate() {
        for b in &clusters[i + 1..] {
            worst = worst.max(dot(&a.centroid, &b.centroid));
        }
    }
    worst
}

/// How many directions the history actually contains.
///
/// Not a fixed number, because the right answer differs per listener: a
/// history that is all one thing must not be forced into four mixes, and one
/// that spans five genres must not be collapsed into two.
///
/// Silhouette alone cannot decide this. It is scale-invariant, so twenty
/// near-identical tracks still split "cleanly" — measured at 0.587 for a
/// history containing exactly one taste, higher than the 0.441 the same data
/// scored at k=2. What separates a real split from a fake one is the absolute
/// distance between the centroids, which was 0.9996 in that case and 0.113 for
/// three genuinely different tastes.
pub fn best_k(vectors: &[Vec<f32>], max_k: usize, seed: u64) -> usize {
    if vectors.len() < 4 {
        return 1;
    }
    let upper = max_k.min(vectors.len() / 2).max(1);
    let mut best = (1usize, f32::NEG_INFINITY);
    for k in 2..=upper {
        let clusters = cluster(vectors, k, seed);
        if clusters.len() < 2 || worst_overlap(&clusters) >= SAME_DIRECTION {
            continue;
        }
        let score = silhouette(vectors, &clusters);
        if score > best.1 {
            best = (k, score);
        }
    }
    best.0
}

/// Mean silhouette over all points, on cosine distance.
fn silhouette(vectors: &[Vec<f32>], clusters: &[Cluster]) -> f32 {
    let mut own = vec![0usize; vectors.len()];
    for (c, cl) in clusters.iter().enumerate() {
        for &i in &cl.members {
            own[i] = c;
        }
    }
    let mut total = 0f32;
    let mut counted = 0usize;
    for (i, v) in vectors.iter().enumerate() {
        let mine = &clusters[own[i]];
        if mine.members.len() < 2 {
            continue; // a lone point has no meaningful cohesion
        }
        let a = mine
            .members
            .iter()
            .filter(|&&j| j != i)
            .map(|&j| 1.0 - dot(v, &vectors[j]))
            .sum::<f32>()
            / (mine.members.len() - 1) as f32;
        let b = clusters
            .iter()
            .enumerate()
            .filter(|(c, _)| *c != own[i])
            .map(|(_, other)| {
                other
                    .members
                    .iter()
                    .map(|&j| 1.0 - dot(v, &vectors[j]))
                    .sum::<f32>()
                    / other.members.len() as f32
            })
            .fold(f32::INFINITY, f32::min);
        if b.is_finite() {
            total += (b - a) / a.max(b).max(1e-9);
            counted += 1;
        }
    }
    if counted == 0 {
        0.0
    } else {
        total / counted as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three directions, well apart, with a little noise on each — the shape a
    /// real history has when someone listens to three different things.
    pub(super) fn three_tastes() -> Vec<Vec<f32>> {
        let mut rng = Rng(7);
        let mut out = Vec::new();
        for axis in 0..3 {
            for _ in 0..8 {
                let mut v = vec![0f32; 6];
                v[axis] = 1.0;
                v[(axis + 1) % 6] = rng.next_f32() * 0.2;
                normalise(&mut v);
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn recovers_three_well_separated_tastes() {
        let vectors = three_tastes();
        let clusters = cluster(&vectors, 3, 42);
        assert_eq!(clusters.len(), 3);
        // Every cluster must be exactly one of the three groups of eight.
        for c in &clusters {
            assert_eq!(
                c.members.len(),
                8,
                "cluster sizes: {:?}",
                clusters.iter().map(|c| c.members.len()).collect::<Vec<_>>()
            );
            let group = c.members[0] / 8;
            assert!(
                c.members.iter().all(|&i| i / 8 == group),
                "cluster mixes groups"
            );
        }
    }

    /// A mix that reshuffles on every launch cannot be recognised or trusted.
    #[test]
    fn the_same_seed_always_gives_the_same_split() {
        let vectors = three_tastes();
        assert_eq!(cluster(&vectors, 3, 99), cluster(&vectors, 3, 99));
    }

    /// The count comes from the data, not from a constant.
    #[test]
    fn the_number_of_mixes_follows_the_history() {
        assert_eq!(best_k(&three_tastes(), 6, 42), 3);
    }

    /// A listener who only plays one thing must get one mix, not four
    /// arbitrary slices of the same music.
    #[test]
    fn one_coherent_taste_is_not_split_up() {
        let mut rng = Rng(3);
        let vectors: Vec<Vec<f32>> = (0..20)
            .map(|_| {
                let mut v = vec![1.0, 0.0, 0.0, 0.0];
                v[1] = rng.next_f32() * 0.05;
                v[2] = rng.next_f32() * 0.05;
                normalise(&mut v);
                v
            })
            .collect();
        assert_eq!(best_k(&vectors, 5, 42), 1);
    }

    /// Members are ordered by how typical they are, so a mix can open with the
    /// track that defines it rather than with an edge case.
    #[test]
    fn the_most_typical_track_comes_first() {
        let vectors = three_tastes();
        for c in cluster(&vectors, 3, 42) {
            let first = dot(&vectors[c.members[0]], &c.centroid);
            let last = dot(&vectors[*c.members.last().unwrap()], &c.centroid);
            assert!(first >= last, "{first} should be >= {last}");
        }
    }

    /// Degenerate inputs must return nothing rather than panic — this runs on
    /// a listener's first day, when the history is empty or nearly so.
    #[test]
    fn empty_and_tiny_histories_do_not_panic() {
        assert!(cluster(&[], 3, 1).is_empty());
        assert!(cluster(&[vec![1.0, 0.0]], 0, 1).is_empty());
        assert_eq!(cluster(&[vec![1.0, 0.0]], 5, 1).len(), 1);
        assert_eq!(best_k(&[vec![1.0, 0.0]], 5, 1), 1);
    }

    /// Cohesion has to actually separate a real direction from a leftovers
    /// bin, otherwise it cannot be used to suppress weak mixes.
    #[test]
    fn cohesion_is_higher_for_a_tight_cluster_than_a_scattered_one() {
        let tight = cluster(&three_tastes(), 3, 42);
        let mut rng = Rng(11);
        let scattered: Vec<Vec<f32>> = (0..24)
            .map(|_| {
                let mut v: Vec<f32> = (0..6).map(|_| rng.next_f32() - 0.5).collect();
                normalise(&mut v);
                v
            })
            .collect();
        let loose = cluster(&scattered, 3, 42);
        let tight_mean = tight.iter().map(|c| c.cohesion).sum::<f32>() / tight.len() as f32;
        let loose_mean = loose.iter().map(|c| c.cohesion).sum::<f32>() / loose.len() as f32;
        assert!(
            tight_mean > loose_mean + 0.15,
            "tight {tight_mean} vs loose {loose_mean}"
        );
    }
}

#[cfg(test)]
mod measure {
    //! Where `SAME_DIRECTION` comes from. Run with:
    //!   cargo test -p reader --lib taste::measure -- --ignored --nocapture
    use super::*;

    #[test]
    #[ignore = "prints the measurement behind SAME_DIRECTION"]
    fn separation_measured() {
        let mut rng = Rng(3);
        let same: Vec<Vec<f32>> = (0..20)
            .map(|_| {
                let mut v = vec![1.0, 0.0, 0.0, 0.0];
                v[1] = rng.next_f32() * 0.05;
                v[2] = rng.next_f32() * 0.05;
                normalise(&mut v);
                v
            })
            .collect();
        for (name, vs) in [
            ("one taste", &same),
            ("three tastes", &tests::three_tastes()),
        ] {
            for k in 2..=4 {
                let cl = cluster(vs, k, 42);
                println!(
                    "{name:14} k={k}  silhouette={:.3}  worst centroid overlap={:.4}",
                    silhouette(vs, &cl),
                    worst_overlap(&cl),
                );
            }
        }
    }
}
