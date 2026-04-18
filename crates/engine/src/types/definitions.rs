//! Typed wrapper for collections of ability definitions.
//!
//! `Definitions<T>` is a newtype around `Vec<T>` used for the three
//! ability-definition collections that live on `GameObject`:
//! `StaticDefinition`, `TriggerDefinition`, and `ReplacementDefinition`.
//!
//! # Why a newtype with no public iteration?
//!
//! Iterating these collections directly silently drops the gating rules that
//! must always apply:
//!
//! - **CR 702.26b** — phased-out permanents' abilities don't function.
//! - **CR 114.4** — only emblems function in the command zone.
//! - **CR 604.1 / CR 613.1** — a static ability only applies while its
//!   `condition` evaluates true.
//!
//! Rather than expose `iter()` / `IntoIterator` (which would let callers skip
//! the gates), the wrapper exposes read-only `len`/`get`/`first`/`last` for
//! incidental access and `Index<usize>` for positional lookup. The canonical
//! way to iterate is the `game::functioning_abilities` module, which applies
//! the correct CR gates for each definition kind.
//!
//! Mutation APIs (`push`, `clear`, `retain`, plus the internal
//! `iter_all`) are crate-visible so construction and layer recomputation
//! can rebuild the collection; external read paths must use the helpers.

use serde::{Deserialize, Serialize};

/// Collection of ability definitions on a `GameObject`.
///
/// External iteration is deliberately not exposed — callers must go through
/// `crate::game::functioning_abilities` so the CR 702.26b / CR 114.4 /
/// CR 604.1 gates are applied consistently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Definitions<T>(pub(crate) Vec<T>);

// Manual Default impl — the derive would require T: Default, but an empty
// Definitions<T> is sensible for any T.
impl<T> Default for Definitions<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Definitions<T> {
    /// Number of definitions (including any that do not currently function).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there are no definitions at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Positional read access. Callers that need gated iteration should use
    /// `game::functioning_abilities` instead.
    pub fn get(&self, i: usize) -> Option<&T> {
        self.0.get(i)
    }

    /// Positional mutable access — crate-visible so effects that need to
    /// mutate a specific existing definition (regeneration shield consumption,
    /// prevention-amount updates) can do so without bypassing the gated reads.
    pub(crate) fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        self.0.get_mut(i)
    }

    /// First definition, if any. Does not apply CR gating.
    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }

    /// Last definition, if any. Does not apply CR gating.
    pub fn last(&self) -> Option<&T> {
        self.0.last()
    }

    /// Append a new definition. Public because mutation is a legitimate cross-
    /// crate operation (test fixtures, card construction, copy effects). The
    /// single-authority invariant is guarded by the absence of public
    /// iteration, not by restricting writes.
    pub fn push(&mut self, item: T) {
        self.0.push(item);
    }

    /// Remove every definition.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Keep only definitions matching `f`.
    pub fn retain<F: FnMut(&T) -> bool>(&mut self, f: F) {
        self.0.retain(f);
    }

    /// Iterate all definitions without applying any CR gate. Crate-visible —
    /// callers at the engine boundary must use `functioning_abilities` helpers.
    pub(crate) fn iter_all(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    /// Borrow the underlying slice without applying any CR gate. Exposed so
    /// engine-internal helpers and consumer crates (phase-ai, coverage) that
    /// take `&[T]` parameters can bridge from a `Definitions<T>` without
    /// reallocating. Runtime game paths must go through `functioning_abilities`
    /// instead — this is a classification-side escape hatch.
    pub fn as_slice(&self) -> &[T] {
        self.0.as_slice()
    }

    /// Public iteration over every definition regardless of functioning
    /// status. Classification/reporting only — runtime game logic in the
    /// engine crate uses `functioning_abilities` helpers.
    pub fn iter_unchecked(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<T> std::ops::Index<usize> for Definitions<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}

impl<T> std::ops::IndexMut<usize> for Definitions<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.0[i]
    }
}

impl<T> From<Vec<T>> for Definitions<T> {
    fn from(v: Vec<T>) -> Self {
        Self(v)
    }
}

impl<T> FromIterator<T> for Definitions<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(Vec::from_iter(iter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_and_is_empty() {
        let mut d: Definitions<i32> = Definitions::default();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        d.push(1);
        d.push(2);
        assert!(!d.is_empty());
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn index_and_positional_access() {
        let d: Definitions<i32> = Definitions::from(vec![10, 20, 30]);
        assert_eq!(d[0], 10);
        assert_eq!(d.get(1), Some(&20));
        assert_eq!(d.first(), Some(&10));
        assert_eq!(d.last(), Some(&30));
        assert_eq!(d.get(99), None);
    }

    #[test]
    fn serde_is_transparent() {
        let d: Definitions<i32> = Definitions::from(vec![1, 2, 3]);
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "[1,2,3]");
        let back: Definitions<i32> = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn retain_filters_in_place() {
        let mut d: Definitions<i32> = Definitions::from(vec![1, 2, 3, 4, 5]);
        d.retain(|x| x % 2 == 0);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], 2);
        assert_eq!(d[1], 4);
    }
}
