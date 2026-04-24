//! Extension helpers for `im` persistent containers.
//!
//! `im::Vector` omits a few slice-shaped conveniences (`.shuffle`, `Index<Range>`)
//! because it is an RRB tree, not a contiguous slice. These helpers round-trip
//! through `Vec` where the algorithmic cost is unavoidable (e.g., Fisher-Yates
//! shuffle needs random access by index).

use rand::seq::SliceRandom;
use rand::Rng;

/// Shuffle an `im::Vector` in place. Collects to `Vec`, shuffles, rebuilds.
/// Cost is O(n) temp allocation + O(n) shuffle + O(n) rebuild.
///
/// CR 701.20: Shuffling a library randomizes the order of cards in that zone.
pub fn shuffle_vector<T: Clone, R: Rng + ?Sized>(v: &mut im::Vector<T>, rng: &mut R) {
    let mut tmp: Vec<T> = v.iter().cloned().collect();
    tmp.shuffle(rng);
    *v = im::Vector::from(tmp);
}
