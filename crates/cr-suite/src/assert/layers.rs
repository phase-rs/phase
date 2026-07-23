//! Continuous-effect / layer assertions (CR 613) — documentation stubs.
//!
//! CR 613 defines the layer system used to compute an object's characteristics
//! from continuous effects (copy → control → text → type → color → ability →
//! P/T). The engine applies layers before exposing `obj.power`,
//! `obj.toughness`, and `obj.keywords`, so the *existing* creature/keyword
//! assertions already observe post-layer values. A dedicated layer-ordering
//! assertion family (e.g. "this P/T came from layer 7c, not 7b") is deferred
//! until a fixture needs to discriminate layer interactions.

/// CR 613 layer-system predicate vocabulary for future assertions.
pub const LAYER_NOTES: &[&str] = &[
    "post_layer_power_toughness: assert the derived P/T after all CR 613 layers.",
    "post_layer_keywords: assert the derived keyword set after CR 613 layer 6.",
    "post_layer_types: assert the derived type line after CR 613 layer 4.",
    "post_layer_controller: assert control-changing effects from CR 613 layer 2.",
];
