//! Typed evidence probe over one audit unit's lowered definitions.
//!
//! The swallow audit asks, per source unit: *does this unit's Oracle text raise a
//! semantic expectation that this unit's parse does not represent?* This module owns
//! the **evidence half** of that question — "is a carrier for semantic S present in
//! the parse?" — and answers it with a **typed** `match`, not a substring scan.
//!
//! # Why this exists: a string marker is not checked by anything
//!
//! The audit previously answered every tree-global evidence question by scanning the
//! serialized AST text for representation markers — a raw substring search for, say,
//! the marker `ConditionMet`. Nothing checks a string. A marker naming a type that does not exist compiles,
//! runs, and silently never matches; a marker that is a *substring of a longer type
//! name* compiles, runs, and silently matches the **wrong rule's fact**. Both were
//! present, and the second is the dangerous one — it fails CLOSED, suppressing true
//! positives:
//!
//! | marker (as written) | what it actually matched | pool hits |
//! |---|---|---|
//! | `ConditionMet` | `TriggerCondition::SolveConditionMet` — a CR 719 Case **solve** condition | 15/15 |
//! | `AsLongAs` | `Duration::ForAsLongAs` | — |
//! | `ManaValue` | `CastPermissionConstraint::ManaValue` (a cast gate, not a quantity) | — |
//! | `Causative`, `HaveCausative`, `HaveItVerb`, `ConditionalEffect`, `ConditionalStatic` | nothing — they name no type in the engine | 0 |
//!
//! There is no `ConditionMet` variant anywhere in the engine. So a `Condition_If`
//! expectation ("is this card's `if` represented?") was being discharged by the
//! presence of an unrelated rule's fact, on every card carrying a Case. The compiler
//! could not object, because to the compiler it was a string.
//!
//! Typed evidence removes the channel: `matches!(c, AbilityCondition::QuantityCheck { .. })`
//! either names a real variant or does not compile.
//!
//! # How total reach is achieved without hand-enumerating the type closure
//!
//! The facts these detectors need are **tree-global existence** facts ("is there a
//! dynamic quantity *anywhere* under this unit's definitions?"). Their typed carriers
//! span a 187-type forward closure (187 types transitively reach `QuantityExpr`;
//! `Effect` alone has 224 variants), so a hand-written total walk would be thousands
//! of arms — every one of which is a silent reach gap if forgotten, and a missed
//! carrier is a **false positive wave**, not a compile error.
//!
//! Instead the probe walks the `serde_json::Value` the definitions already serialize
//! to and attempts `T::deserialize` on each node. Reach is therefore **serde-derived**
//! — the compiler maintains it — while the predicate stays typed Rust. Adding a new
//! `Effect` variant that carries a `QuantityExpr` extends the probe's reach for free.
//!
//! # Soundness: what a node deserializing as `T` does and does NOT prove
//!
//! Ruling (B) was accepted on the premise that "a false deserialize is structurally
//! impossible". That is **not literally true**, and the module is built to the weaker,
//! accurate statement. Two distinct hazards:
//!
//! **(1) Externally tagged enums.** `Duration` and `StaticMode` are NOT `#[serde(tag =
//! "type")]`. Their unit variants serialize as **bare strings** (`"UntilEndOfTurn"`,
//! `"Permanent"`). An unanchored `any::<Duration>` would therefore match *any string
//! anywhere* that happens to equal a variant name — and `"Permanent"` is a common
//! string in this tree (card kinds, filters). These two types MUST be probed with
//! [`UnitEvidence::any_at`], which anchors on the JSON key the carrier field actually
//! uses. See [`DURATION_KEYS`] / [`STATIC_MODE_KEYS`].
//!
//! **(2) Shared variant names between internally tagged enums.** Computed over the
//! probed set, the colliding pairs are:
//!
//! ```text
//! Effect                 ∩ AbilityCondition = {CastFromZone}
//! Effect                 ∩ StaticMode       = {RevealHand}
//! QuantityExpr           ∩ QuantityRef      = {Power}
//! QuantityRef            ∩ AbilityCondition = {PreviousEffectAmount}
//! AbilityCondition       ∩ StaticCondition  = {And, Not, Or, SourceMatchesFilter, IsMonarch, …17}
//! StaticCondition        ∩ ActivationRestriction = {DuringYourTurn, SourceIsHarnessed}
//! ContinuousModification ∩ StaticMode       = {AssignNoCombatDamage}
//! ```
//!
//! A node bearing a shared name can deserialize as *either* enum. This is only a
//! defect if a predicate's ANSWER differs across the collision. Every predicate in
//! `swallow_check.rs` is written to be insensitive to the list above:
//!   * no predicate matches `CastFromZone`, `RevealHand`, `PreviousEffectAmount`,
//!     `AssignNoCombatDamage`, `DuringYourTurn` or `SourceIsHarnessed`;
//!   * `QuantityExpr::Power` and `QuantityRef::Power` are BOTH dynamic quantities, so
//!     the collision cannot change a `DynamicQty` answer;
//!   * the `AbilityCondition`/`StaticCondition` 17-way overlap is asked only as
//!     "*a* condition slot is populated", a question whose answer is the same for both.
//!
//! The collision that motivated this module — `ConditionMet` vs `SolveConditionMet` —
//! is NOT in that table, because the names differ. That is precisely the property a
//! type has and a substring does not.
//!
//! # The `description` channel (cond-1)
//!
//! `description` fields carry raw Oracle prose. Deserializing prose as a semantic
//! carrier is the same defect one layer down, so [`UnitEvidence`] **never descends
//! into a `description` value**. The walk is the single choke point, so no probe —
//! present or future — can read prose as evidence.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::parser::oracle::ParsedAbilities;

/// The JSON key whose value is raw Oracle prose, never semantic evidence.
const DESCRIPTION_KEY: &str = "description";

/// Every JSON key at which a `Duration`-typed field is serialized.
///
/// `Duration` is externally tagged, so its unit variants are bare strings and it MUST
/// be probed key-anchored (see module docs, hazard 1). Completeness is a grep over the
/// type definitions, not a guess — the carrier set is closed:
///
/// ```text
/// $ rg ': (Option<)?Duration[,>]' crates/engine/src/types/
///   AbilityDefinition.duration                        <- the definition's own slot
///   Effect::BecomeCopy.duration                       ┐
///   Effect::GainActivatedAbilitiesOfTarget.duration   │
///   Effect::GenericEffect.duration                    ├ the 6 Effect carriers
///   Effect::ForceAttack.duration                      │
///   Effect::CastFromZone.duration                     │
///   Effect::PreventDamage.prevention_duration         ┘ <- NOT named `duration`
///   CastingPermission::ExileWithAltCost.duration      ┐ CastingPermission
///   CastingPermission::PlayFromExile.duration         ┘
/// ```
///
/// `Effect::PreventDamage.prevention_duration` is why this is a key SET rather than the
/// single key `"duration"` — and it is a fact the old substring marker got wrong in the
/// other direction: `"prevention_duration":"UntilEndOfTurn"` does NOT contain the
/// substring `"duration":"UntilEndOfTurn"` (the character before `duration` is `_`, not
/// `"`), so the marker was blind to every damage-prevention shield's duration.
const DURATION_KEYS: &[&str] = &["duration", "prevention_duration"];

/// Every JSON key at which a `StaticMode`-typed field is serialized. Externally tagged,
/// same anchoring requirement as [`DURATION_KEYS`].
///
/// ```text
/// $ rg ': (Option<)?StaticMode[,>]' crates/engine/src/types/
///   StaticDefinition.mode
///   ContinuousModification::GrantStatic.mode
/// ```
///
/// The key `"mode"` also carries `ReplacementMode` (on `ReplacementDefinition`), which is
/// internally tagged. The two cannot be confused: a `ReplacementMode` value is the object
/// `{"type":"Optional",…}` and fails `StaticMode::deserialize`, while a `StaticMode` unit
/// variant is the bare string `"CantTap"` and fails `ReplacementMode::deserialize`.
const STATIC_MODE_KEYS: &[&str] = &["mode"];

/// One audit unit's lowered definitions, as a walkable tree with the prose removed.
///
/// Built once per unit and shared by every detector — the same one-serialization-per-unit
/// cost the `ast_json` haystack it replaces already paid.
pub(super) struct UnitEvidence {
    root: Value,
}

impl UnitEvidence {
    /// Serialize this unit's scoped definitions into a probe tree.
    ///
    /// Serialization of a definition tree cannot fail in practice (no maps with
    /// non-string keys, no non-finite floats), and a failure here must not take the
    /// parse down — an empty tree yields NO evidence, so every expectation this unit
    /// raises is reported as swallowed. That is the conservative-red direction: the
    /// audit over-reports rather than silently going green.
    pub(super) fn of(scoped: &ParsedAbilities) -> Self {
        Self {
            root: serde_json::to_value(scoped).unwrap_or(Value::Null),
        }
    }

    /// Visit nodes depth-first, short-circuiting on the first `true`.
    ///
    /// `key` is the object key the node is stored under (`None` at the root). Array
    /// elements inherit their array's key, so a `Vec<Duration>` field would still be
    /// anchored by its field name.
    ///
    /// **The `description` skip lives here and only here.** Prose is not evidence; the
    /// walk is the choke point that makes that true for every probe built on it.
    fn visit(
        node: &Value,
        key: Option<&str>,
        f: &mut impl FnMut(&Value, Option<&str>) -> bool,
    ) -> bool {
        if f(node, key) {
            return true;
        }
        match node {
            Value::Object(map) => map
                .iter()
                .filter(|(k, _)| k.as_str() != DESCRIPTION_KEY)
                .any(|(k, v)| Self::visit(v, Some(k), f)),
            Value::Array(items) => items.iter().any(|v| Self::visit(v, key, f)),
            _ => false,
        }
    }

    /// Does any node in the tree deserialize as `T` and satisfy `pred`?
    ///
    /// For **internally tagged** types only (`#[serde(tag = "type")]`): `Effect`,
    /// `QuantityExpr`, `QuantityRef`, `AbilityCondition`, `StaticCondition`,
    /// `ContinuousModification`, `ReplacementMode`, `ActivationRestriction`. The tag must
    /// name a real variant of `T` and the variant's fields must typecheck, which is what
    /// makes the match discriminating.
    ///
    /// Do NOT use for `Duration` or `StaticMode` — see [`Self::any_at`].
    pub(super) fn any<T: DeserializeOwned>(&self, pred: impl Fn(&T) -> bool) -> bool {
        Self::visit(&self.root, None, &mut |node, _| {
            T::deserialize(node.clone()).is_ok_and(|value| pred(&value))
        })
    }

    /// Does any node stored at one of `keys` deserialize as `T` and satisfy `pred`?
    ///
    /// Key anchoring is **mandatory** for externally tagged enums, whose unit variants are
    /// bare strings that would otherwise match unrelated prose-free strings elsewhere in
    /// the tree (`Duration::Permanent` vs. any `"Permanent"` string). Anchoring restores
    /// the discrimination the tag would have provided.
    pub(super) fn any_at<T: DeserializeOwned>(
        &self,
        keys: &[&str],
        pred: impl Fn(&T) -> bool,
    ) -> bool {
        Self::visit(&self.root, None, &mut |node, key| {
            key.is_some_and(|k| keys.contains(&k))
                && T::deserialize(node.clone()).is_ok_and(|value| pred(&value))
        })
    }

    /// Is an optional slot named `key` populated anywhere?
    ///
    /// For facts that are purely about **slot presence** — the parser filled in an
    /// `Option` field — where the slot's *value* carries no further discrimination the
    /// detector needs (`modal`, `dynamic_max_choices`, `repeat_for`, `source_rider`, …).
    /// Every such field is `skip_serializing_if = "Option::is_none"`, so key presence IS
    /// population; the `!is_null` guard covers any field that is not.
    ///
    /// This is a structural JSON-key test, not a substring scan: it cannot match inside a
    /// string, cannot match a longer key that contains it, and never sees prose. What it
    /// does not get is a compiler check on the key spelling — so prefer [`Self::any`] /
    /// [`Self::any_at`] whenever the fact has a typed carrier, and use this only when the
    /// fact genuinely IS "the slot is filled".
    pub(super) fn has_slot(&self, key: &str) -> bool {
        Self::visit(&self.root, None, &mut |node, _| match node {
            Value::Object(map) => map.get(key).is_some_and(|v| !v.is_null()),
            _ => false,
        })
    }

    /// Build a probe straight from a JSON fixture.
    ///
    /// Test-only, and deliberately so: production evidence must come from a real
    /// `ParsedAbilities` via [`Self::of`]. This exists for the handful of detector unit
    /// tests that pin a gate against a minimal hand-written AST shape.
    #[cfg(test)]
    pub(super) fn from_json_for_test(json: &str) -> Self {
        Self {
            root: serde_json::from_str(json).expect("test fixture must be valid JSON"),
        }
    }

    /// Does any `Duration` carrier satisfy `pred`? Key-anchored per [`DURATION_KEYS`].
    pub(super) fn any_duration(
        &self,
        pred: impl Fn(&crate::types::ability::Duration) -> bool,
    ) -> bool {
        self.any_at(DURATION_KEYS, pred)
    }

    /// Does any `StaticMode` carrier satisfy `pred`? Key-anchored per [`STATIC_MODE_KEYS`].
    pub(super) fn any_static_mode(
        &self,
        pred: impl Fn(&crate::types::statics::StaticMode) -> bool,
    ) -> bool {
        self.any_at(STATIC_MODE_KEYS, pred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Duration, Effect, PlayerScope, QuantityExpr, QuantityRef,
        TargetFilter,
    };

    fn parsed_with(def: AbilityDefinition) -> ParsedAbilities {
        ParsedAbilities {
            abilities: vec![def],
            triggers: Vec::new(),
            statics: Vec::new(),
            replacements: Vec::new(),
            extracted_keywords: Vec::new(),
            modal: None,
            additional_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            solve_condition: None,
            strive_cost: None,
            parse_warnings: Vec::new(),
        }
    }

    /// A def whose *prose* names a duration, but which carries NO duration carrier.
    fn prose_only_duration() -> ParsedAbilities {
        parsed_with(AbilityDefinition::new(
            AbilityKind::Spell,
            // `Effect::Unimplemented`'s fragment/description channel is raw Oracle text.
            Effect::unimplemented("x", "target creature gets +1/+1 until end of turn"),
        ))
    }

    /// COND-1, DIRECTION 1 — the skip is APPLIED.
    ///
    /// This test is the reason the skip is not vacuous. `Duration` is externally tagged,
    /// so `Duration::UntilEndOfTurn` serializes as the bare string `"UntilEndOfTurn"` —
    /// which means a *description string* containing exactly that text would deserialize
    /// as a `Duration` and be accepted as evidence. Delete the `description` filter in
    /// `visit` and this assertion goes RED while direction 2 below stays green: an
    /// unapplied skip is invisible to the positive test alone, which is why BOTH are
    /// mandatory.
    #[test]
    fn description_prose_is_never_evidence() {
        let mut parsed = prose_only_duration();
        // Plant the exact serialized form of `Duration::UntilEndOfTurn` inside prose.
        parsed.abilities[0].description = Some("UntilEndOfTurn".to_string());
        let evidence = UnitEvidence::of(&parsed);

        assert!(
            !evidence.any::<Duration>(|_| true),
            "a Duration variant name planted in a `description` string must NOT be read \
             as a Duration carrier — the walk must not descend into prose"
        );
    }

    /// COND-1, DIRECTION 2 — the walk still SEES real carriers.
    ///
    /// Without this, "skip everything" would pass direction 1 trivially.
    #[test]
    fn a_real_duration_node_is_evidence() {
        let def = AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("x", "y"))
            .duration(Duration::UntilEndOfTurn);
        let evidence = UnitEvidence::of(&parsed_with(def));

        assert!(
            evidence.any_duration(|d| matches!(d, Duration::UntilEndOfTurn)),
            "a real `AbilityDefinition.duration` carrier must be visible to the probe"
        );
    }

    /// HAZARD 1 — why `Duration` must be key-anchored.
    ///
    /// `Duration::Permanent` serializes as the bare string `"Permanent"`, and that string
    /// occurs all over a definition tree in positions that are not durations at all. An
    /// unanchored `any::<Duration>` therefore over-reads; `any_duration` (key-anchored)
    /// does not. This pins the anchoring as load-bearing rather than decorative.
    #[test]
    fn duration_probe_is_key_anchored_not_string_matched() {
        // A `Token` effect naming the "Permanent" *type*, with no duration anywhere.
        let parsed = parsed_with(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::unimplemented("x", "y"),
        ));
        let mut root = serde_json::to_value(&parsed).unwrap();
        // Splice a non-duration field whose value is the bare string a unit variant uses.
        root["abilities"][0]["kind_label"] = Value::String("Permanent".into());
        let evidence = UnitEvidence { root };

        assert!(
            evidence.any::<Duration>(|d| matches!(d, Duration::Permanent)),
            "unanchored: the bare string IS accepted — this is the hazard being guarded"
        );
        assert!(
            !evidence.any_duration(|d| matches!(d, Duration::Permanent)),
            "key-anchored: a `Permanent` string outside a duration slot is NOT a duration"
        );
    }

    /// The typed probe reaches a carrier nested arbitrarily deep — the property that
    /// replaces hand-enumerating the 187-type closure.
    #[test]
    fn typed_probe_reaches_nested_carriers() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                target: TargetFilter::Controller,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
            },
        );
        let evidence = UnitEvidence::of(&parsed_with(def));

        assert!(
            evidence.any::<QuantityRef>(|_| true),
            "a QuantityRef nested inside Effect::DrawCards.count must be reachable"
        );
        assert!(
            evidence.any::<QuantityExpr>(|q| !matches!(q, QuantityExpr::Fixed { .. })),
            "the wrapping QuantityExpr::Ref is a dynamic carrier"
        );
    }

    /// A `Fixed` quantity is NOT dynamic evidence. Guards the `QuantityExpr` custom
    /// `Deserialize`, which also accepts a **bare integer** as `Fixed` — so every integer
    /// in the tree deserializes as a `QuantityExpr`. A `DynamicQty` predicate that forgot
    /// to exclude `Fixed` would be satisfied by literally any number on the card.
    #[test]
    fn fixed_quantities_are_not_dynamic_evidence() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 2 },
            },
        );
        let evidence = UnitEvidence::of(&parsed_with(def));

        assert!(
            !evidence.any::<QuantityRef>(|_| true),
            "a Fixed count carries no QuantityRef"
        );
        assert!(
            !evidence.any::<QuantityExpr>(|q| !matches!(q, QuantityExpr::Fixed { .. })),
            "Fixed is a constant, not a dynamic quantity — and a bare integer must not \
             be mistaken for one"
        );
    }

    /// `has_slot` is a structural key test: it must not match a longer key that merely
    /// CONTAINS it, which is exactly the failure mode of the substring channel.
    #[test]
    fn has_slot_does_not_match_a_containing_key() {
        let parsed = prose_only_duration();
        let mut root = serde_json::to_value(&parsed).unwrap();
        root["abilities"][0]["prevention_duration"] = Value::String("UntilEndOfTurn".into());
        let evidence = UnitEvidence { root };

        assert!(evidence.has_slot("prevention_duration"));
        assert!(
            !evidence.has_slot("duration"),
            "`duration` must not be found inside the longer key `prevention_duration` — \
             the substring marker `\"duration\":\"…\"` had this bug in reverse"
        );
    }

    /// The `PlayerScope`-parameterized duration variants are objects, not bare strings,
    /// and must still be reachable through the key anchor.
    #[test]
    fn data_carrying_duration_variants_are_reachable() {
        let def = AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("x", "y"))
            .duration(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            });
        let evidence = UnitEvidence::of(&parsed_with(def));

        assert!(evidence.any_duration(|d| matches!(d, Duration::UntilNextTurnOf { .. })));
        assert!(!evidence.any_duration(|d| matches!(d, Duration::UntilEndOfTurn)));
    }
}
