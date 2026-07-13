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
//! **(2) Shared variant names between internally tagged enums.** An internally tagged
//! enum is often assumed to be self-identifying, so that anchoring is needed only for the
//! externally tagged types in hazard (1). **That assumption is false**, for two compounding
//! reasons:
//!
//!   * an internally-tagged **unit** variant matches on the `type` field ALONE, and serde
//!     **ignores unknown fields** by default — so a node carrying extra fields still
//!     deserializes cleanly, with those fields silently dropped;
//!   * a tag only discriminates if the variant NAME is unique across every tagged enum in
//!     the tree. It is not.
//!
//! For `QuantityRef` alone — 84 variants, against the 161 `tag = "type"` enums in `types/`
//! (the true population; an earlier count of 91 was short, which is how the `FilterProp`
//! collision below went unlisted until it suppressed Siren's Call) — 10 names are shared:
//!
//! ```text
//! QuantityRef ∩ AbilityCondition         = {PreviousEffectAmount}
//! QuantityRef ∩ FilterProp               = {AttackedThisTurn, EnteredThisTurn}
//! QuantityRef ∩ TriggerCondition         = {AttackedThisTurn, CounterAddedThisTurn}
//! QuantityRef ∩ ParsedCondition          = {BattlefieldEntriesThisTurn}
//! QuantityRef ∩ ChooseFromZoneConstraint = {DistinctCardTypes}
//! QuantityRef ∩ ManaCost                 = {SelfManaValue}
//! QuantityRef ∩ ManaProduction           = {DistinctColorsAmongPermanents}
//! QuantityRef ∩ SolveCondition           = {ObjectCount}
//! QuantityRef ∩ QuantityExpr             = {Power}
//! ```
//!
//! and elsewhere in the probed set:
//!
//! ```text
//! Effect                 ∩ AbilityCondition      = {CastFromZone}
//! Effect                 ∩ StaticMode            = {RevealHand}
//! AbilityCondition       ∩ StaticCondition       = {And, Not, Or, SourceMatchesFilter, …17}
//! StaticCondition        ∩ ActivationRestriction = {DuringYourTurn, SourceIsHarnessed}
//! ContinuousModification ∩ StaticMode            = {AssignNoCombatDamage}
//! ```
//!
//! A node bearing a shared name can deserialize as *either* enum. That is only a defect if
//! a predicate's ANSWER differs across the collision — so the safety argument has to be made
//! per predicate, and it is:
//!   * `QuantityExpr::Power` and `QuantityRef::Power` are BOTH dynamic quantities, so that
//!     collision cannot change a `DynamicQty` answer;
//!   * the `AbilityCondition`/`StaticCondition` 17-way overlap is asked only as "*a*
//!     condition slot is populated", a question whose answer is the same for both;
//!   * no predicate matches `CastFromZone`, `RevealHand`, `AssignNoCombatDamage`,
//!     `DuringYourTurn` or `SourceIsHarnessed`.
//!
//! **This argument was once made for the quantity probes too, and it was WRONG.** The
//! `PreviousEffectAmount` collision above was known and listed, under the claim "no predicate
//! matches it". But `detect_dynamic_qty` probed `any::<QuantityRef>(|_| true)` — and `|_| true`
//! is the ONE predicate that cannot be insensitive to a collision, because it matches every
//! variant by construction. The consequence was measured over the full 35,396-face pool:
//! Boing!'s `AbilityCondition::PreviousEffectAmount` (under key `condition`) and Siren's Call's
//! `FilterProp::AttackedThisTurn` (under key `properties[].prop`, a TARGET FILTER) were both
//! read as dynamic quantities, silently suppressing two true swallowed-clause warnings. The
//! `FilterProp` collision was not even in the old table — the table had been computed against
//! too small a set of enums.
//!
//! So the rule is now structural rather than argued: **`QuantityRef` and `QuantityExpr` are
//! probed KEY-ANCHORED** ([`QUANTITY_KEYS`]), exactly like the externally tagged types. A value
//! reached through a quantity key IS a quantity by construction, so no collision is reachable.
//! Prefer anchoring to a per-predicate soundness argument wherever a key set exists: the
//! argument has to be re-derived every time a predicate or an enum changes, and nothing checks
//! it. **Anchoring fails LOUD** (a missing key over-reports, and the full-pool delta shows it);
//! **an unanchored probe fails QUIET** (it under-reports, and nothing shows it).
//!
//! # The collision map, and the per-site verdicts (task #51)
//!
//! POPULATION FIRST, because the earlier version of this table was wrong by omission: it was
//! computed against **91** enums and therefore reported 13 colliding predicates. Regenerated
//! against the true population — **162** tagged enums in `types/` (93 internally tagged
//! `tag = "type"`, 69 adjacently tagged `tag = "type", content = "data"`), 1,475 variant names,
//! of which **200 are shared by more than one enum** — the same probes yield **19** colliding
//! (site, variant) pairs across 10 sites. All 13 previously listed reproduce; 6 were missed.
//! A collision table is a claim about a population: if the population is short, the table's
//! ABSENCES are worthless. Regenerate it, do not trust it.
//!
//! The generator is ~40 lines of python over `types/*.rs`: parse every `tag = "type"` enum's
//! variants, record unit-vs-struct AND internal-vs-adjacent, invert to `name -> [owning enums]`,
//! keep the names owned by more than one enum, then intersect with the variants each
//! `evidence.any::<T>()` predicate actually matches. Re-run it whenever a probe or an enum
//! changes: these numbers are a measurement, not a constant (they moved by one enum between
//! the audit and the rebase, without changing any verdict).
//!
//! Two mechanisms, and they are NOT the same:
//!   * **internally tagged** — a UNIT variant matches on the `type` field alone and serde drops
//!     unknown sibling fields, so any node bearing that tag deserializes. Widest surface.
//!   * **adjacently tagged** — the payload sits under `data`, so a struct variant will not
//!     deserialize without it. Narrower, but NOT safe: two adjacently tagged enums sharing a
//!     variant name AND a payload shape are freely interchangeable (see
//!     [`ACTIVATION_RESTRICTION_KEYS`]).
//!
//! **A struct variant is not automatically safe.** `Effect::CastFromZone` has nine fields and
//! every one carries `#[serde(default)]`, so serde matches it on the tag alone — it is a unit
//! variant in effect. Always check for a REQUIRED field before believing "struct, so it can't
//! collide".
//!
//! ## ANCHORED — the collision changes the detector's ANSWER
//!
//! ```text
//! any::<Effect>                CastFromZone       also: AbilityCondition, ReplacementCondition
//! any::<ActivationRestriction> RequiresCondition  also: CastingRestriction (field-identical)
//! ```
//!
//! Both are now key-anchored ([`EFFECT_KEYS`], [`ACTIVATION_RESTRICTION_KEYS`]) and both are
//! pinned by a differential guard test that was observed RED before it was made green.
//!
//! MEASURED over the full 35,396-face pool: the collision surface is POPULATED — 36
//! `CastFromZone` nodes sit under non-effect keys (`condition` ×25, `inner` ×9, `conditions`
//! ×2) and 34 `RequiresCondition` nodes sit under `casting_restrictions`. Every one of those 70
//! nodes was being accepted by the unanchored probe as the WRONG enum. The swallow delta is
//! nevertheless **0 gains / 0 losses**, because both are currently MASKED by an upstream text
//! gate, not by any property of the probe:
//!   * 32 of the 35 `CastFromZone` cards contain no "this turn" at all, so the detector never
//!     reaches the probe; the other 3 are dissolved by an earlier leg either way;
//!   * the activation gate demands that EVERY "this turn" occurrence sit on an "activate only"
//!     line, and a card carrying a *casting* restriction ("cast only if ...") does not say
//!     "activate only" — so the gate rejects it before the probe runs.
//!
//! That is luck, not safety: it holds only while no printed card pairs a cast-zone CONDITION
//! with a dropped "this turn", or an "activate only ... this turn" with a casting restriction.
//! Anchoring makes the answer correct by construction and costs nothing (byte-identical export).
//!
//! ## ANSWER-EQUIVALENT — left unanchored, with the reason each is collision-INVARIANT
//!
//! 8 sites / 17 pairs remain probed unanchored. Each is safe for a stated, CHECKABLE reason —
//! not a vibe, and not "it's a specific variant so it must be fine" (that claim is false, see
//! above). Anchoring them would be WRONG: it would risk removing evidence that is accidentally
//! load-bearing (exactly what anchoring the quantity probes did to `ManaProduction`, which then
//! had to be restored as a typed leg).
//!
//! **(a) The turn-scope disjunction** — sites probing `ParsedCondition`, `StaticCondition`,
//! `AbilityCondition`, `TriggerCondition`, `FilterProp` inside the `Duration_ThisTurn` detector
//! (14 of the 17 pairs). The detector asks ONE existential question — *does this unit carry any
//! turn-scoped carrier?* — as a disjunction across seven enums. **Every variant it matches has
//! `ThisTurn` in its name**, and a collision is by definition NAME IDENTITY, so whichever enum a
//! colliding node is read as, it still bears a `ThisTurn` name and is still a turn-scoped
//! carrier. The disjunction is CLOSED under its own collision set, so the answer cannot change.
//! Checkable in one grep: every variant named by those five `matches!` arms contains `ThisTurn`.
//!
//! **(b) `any::<ManaCost>` / `SelfManaValue`** (also `QuantityRef::SelfManaValue`). The question
//! is "is there a value derived from the card's OWN mana cost rather than a fixed amount?"
//! (CR 106.1). Both readings are exactly that — a self-referential dynamic value — so the
//! answer is identical under either.
//!
//! **(c) `any::<AbilityCondition>` / `any::<StaticCondition>` on `Not`.** The two are already
//! OR'd together at the call site, so those two readings are answer-identical BY CONSTRUCTION.
//! `Not` is also a variant of `FilterProp` and `TargetFilter`, but those are
//! `Not { prop }` / `Not { filter }` — the field NAME differs from `Not { condition }`, so
//! neither can deserialize as a condition. The collision is structurally impossible, not merely
//! unlikely.
//!
//! The collision that motivated this module — `ConditionMet` vs `SolveConditionMet` —
//! is NOT in any table above, because the names differ. That is precisely the property a
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

/// Every JSON key at which a `QuantityExpr`- or `QuantityRef`-typed field is serialized.
///
/// ```text
/// $ rg '(\w+): (Option<)?(Box<)?(Vec<)?(QuantityExpr|QuantityRef)\b' crates/engine/src/types/
/// ```
///
/// Anchoring these is **mandatory**, and for a different reason than [`DURATION_KEYS`].
/// `QuantityRef` is INTERNALLY tagged, so the naive argument is that its tag makes it
/// self-identifying and anchoring is unnecessary. That argument is false twice over:
///
///  1. An internally-tagged **unit** variant matches on the `type` field ALONE, and serde
///     ignores unknown fields by default. So `AbilityCondition::PreviousEffectAmount`,
///     serialized as `{"type":"PreviousEffectAmount","comparator":"LE","rhs":…}`,
///     deserializes CLEANLY as `QuantityRef::PreviousEffectAmount` — the extra fields are
///     silently dropped.
///  2. A tag is only discriminating if the variant NAME is unique across every tagged enum
///     in the tree. It is not. 10 of `QuantityRef`'s 84 variant names are shared with
///     another internally-tagged enum reachable from a parsed unit:
///
///     ```text
///     QuantityRef ∩ AbilityCondition         PreviousEffectAmount
///     QuantityRef ∩ FilterProp               AttackedThisTurn, EnteredThisTurn
///     QuantityRef ∩ TriggerCondition         AttackedThisTurn, CounterAddedThisTurn
///     QuantityRef ∩ ParsedCondition          BattlefieldEntriesThisTurn
///     QuantityRef ∩ ChooseFromZoneConstraint DistinctCardTypes
///     QuantityRef ∩ ManaCost                 SelfManaValue
///     QuantityRef ∩ ManaProduction           DistinctColorsAmongPermanents
///     QuantityRef ∩ SolveCondition           ObjectCount
///     QuantityRef ∩ QuantityExpr             Power
///     ```
///
/// Both failures were MEASURED, not hypothesized. Unanchored, `detect_dynamic_qty` accepted
/// Boing!'s `AbilityCondition::PreviousEffectAmount` (under key `condition`) and Siren's
/// Call's `FilterProp::AttackedThisTurn` (under key `properties[].prop`) as proof of a
/// dynamic quantity, and SUPPRESSED both cards' real swallowed-clause warnings — Boing!
/// lowers "scry a number of cards equal to the result" to `Scry { count: Fixed(1) }`, so the
/// dynamic quantity is genuinely dropped and the warning is a true positive.
///
/// Note the shape of the fix: a value reached through one of these keys IS a quantity by
/// construction, so no collision is possible there. The failure directions are asymmetric —
/// a key MISSING from this list makes a detector over-report (conservative-RED, visible in
/// the full-pool delta), whereas an unanchored probe under-reports SILENTLY. Anchoring fails
/// loud; not anchoring fails quiet.
const QUANTITY_KEYS: &[&str] = &[
    "amount",
    "amount_dynamic",
    "attr",
    "back",
    "count",
    "depth",
    "dynamic_count",
    "dynamic_max_choices",
    "exponent",
    "expr",
    "exprs",
    "inner",
    "keep_count_expr",
    "left",
    "lhs",
    "life_payment",
    "mana_value_limit",
    "max",
    "max_ticket_cost",
    "min",
    "mv_bound",
    "qty",
    "quantity",
    "repeat_for",
    "rhs",
    "right",
    "scale",
    "threshold",
    "total_power_cap",
    "value",
];

/// Every JSON key at which an `Effect`-typed field is serialized.
///
/// ```text
/// $ rg '(\w+): (Option<)?(Box<)?(Vec<)?Effect\b' crates/engine/src/types/   # comments stripped
///   AbilityDefinition.effect                  Box<Effect>   <- the definition's own slot
///   Effect::<nested>.effect                   Box<Effect>
///   ReplacementDefinition.replacement_effect  Box<Effect>   <- NOT named `effect`
/// ```
///
/// There is no `Vec<Effect>` anywhere: an effect CHAIN nests through `AbilityDefinition`, whose
/// own `effect` field re-anchors the inner `Effect`. So the set is closed at two keys.
///
/// Anchoring `Effect` is **mandatory**, and the reason is sharper than for [`QUANTITY_KEYS`].
/// `Effect::CastFromZone`'s NINE fields ALL carry `#[serde(default)]`, so the variant
/// deserializes from the `type` tag ALONE — to serde it is a UNIT variant despite being written
/// as a struct variant, which is the widest possible match surface. Both
/// `AbilityCondition::CastFromZone { zone }` (CR 601.2a: "if you cast this spell from your
/// graveyard") and `ReplacementCondition::CastFromZone { zone }` therefore deserialize CLEANLY
/// as an `Effect::CastFromZone`, with `zone` silently dropped as an unknown field.
///
/// MEASURED, not assumed: unanchored, the `Duration_ThisTurn` detector read those CONDITION
/// nodes as proof that a one-shot cast-permission EFFECT owns the card's "this turn" (CR 601.2
/// — a cast permission carries no duration slot), and suppressed the swallow warning on cards
/// whose "this turn" is genuinely dropped. Same defect shape as Boing! (#50).
///
/// A struct variant is only safe unanchored if some field is REQUIRED. Check for
/// `#[serde(default)]` on every field before trusting "it's a struct variant, so it can't
/// collide".
const EFFECT_KEYS: &[&str] = &["effect", "replacement_effect"];

/// Every JSON key at which an `ActivationRestriction`-typed field is serialized.
///
/// ```text
/// $ rg '(\w+): (Option<)?(Box<)?(Vec<)?ActivationRestriction\b' crates/engine/src/types/
///   activation_restrictions   Vec<ActivationRestriction>          (×2)
///   restrictions              Vec<ActivationRestriction>
///   cap                       Option<ActivationRestriction>
///   once_per_turn             Option<Box<ActivationRestriction>>  (keywords.rs)
/// ```
///
/// `ActivationRestriction::RequiresCondition { condition: Option<ParsedCondition> }` and
/// `CastingRestriction::RequiresCondition { condition: Option<ParsedCondition> }` are
/// FIELD-IDENTICAL, so each deserializes cleanly as the other — the tag discriminates nothing.
/// These are different rules: CR 602.5 (a player can't begin to activate a prohibited ability)
/// vs CR 601.2 (casting). `CastingRestriction` is carried at exactly one key —
/// `ParsedAbilities.casting_restrictions` — which is DISJOINT from the set below, so anchoring
/// eliminates the collision by construction rather than by argument.
const ACTIVATION_RESTRICTION_KEYS: &[&str] = &[
    "activation_restrictions",
    "restrictions",
    "cap",
    "once_per_turn",
];

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
            T::deserialize(node).is_ok_and(|value| pred(&value))
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
                && T::deserialize(node).is_ok_and(|value| pred(&value))
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

    /// Does any `QuantityRef` carrier satisfy `pred`? Key-anchored per [`QUANTITY_KEYS`].
    ///
    /// Never probe `QuantityRef` unanchored: its tag is not discriminating, because 10 of its
    /// variant names are shared with other internally-tagged enums. See [`QUANTITY_KEYS`].
    pub(super) fn any_quantity_ref(
        &self,
        pred: impl Fn(&crate::types::ability::QuantityRef) -> bool,
    ) -> bool {
        self.any_at(QUANTITY_KEYS, pred)
    }

    /// Does any `QuantityExpr` carrier satisfy `pred`? Key-anchored per [`QUANTITY_KEYS`].
    ///
    /// Anchoring matters doubly here: `QuantityExpr`'s hand-written `Deserialize` also accepts
    /// a BARE INTEGER as `Fixed` (the legacy on-disk form), so an unanchored probe would parse
    /// any number anywhere on the card as a quantity.
    pub(super) fn any_quantity_expr(
        &self,
        pred: impl Fn(&crate::types::ability::QuantityExpr) -> bool,
    ) -> bool {
        self.any_at(QUANTITY_KEYS, pred)
    }

    /// Does any `Effect` carrier satisfy `pred`? Key-anchored per [`EFFECT_KEYS`].
    ///
    /// Never probe `Effect` unanchored. `Effect::CastFromZone` has nine fields and every one is
    /// `#[serde(default)]`, so it matches on the `type` tag alone — and `CastFromZone` is also a
    /// variant name of `AbilityCondition` and `ReplacementCondition`. See [`EFFECT_KEYS`].
    pub(super) fn any_effect(&self, pred: impl Fn(&crate::types::ability::Effect) -> bool) -> bool {
        self.any_at(EFFECT_KEYS, pred)
    }

    /// Does any `ActivationRestriction` carrier satisfy `pred`? Key-anchored per
    /// [`ACTIVATION_RESTRICTION_KEYS`].
    ///
    /// Never probe `ActivationRestriction` unanchored: its `RequiresCondition` variant is
    /// field-identical to `CastingRestriction::RequiresCondition`, so the two deserialize as
    /// each other (CR 602.5 activation vs CR 601.2 casting — different rules).
    pub(super) fn any_activation_restriction(
        &self,
        pred: impl Fn(&crate::types::ability::ActivationRestriction) -> bool,
    ) -> bool {
        self.any_at(ACTIVATION_RESTRICTION_KEYS, pred)
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
        AbilityDefinition, AbilityKind, ActivationRestriction, Duration, Effect, PlayerScope,
        QuantityExpr, QuantityRef, TargetFilter,
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

    /// An INTERNALLY tagged enum still needs key-anchoring, because a tag only discriminates
    /// if the variant NAME is unique across every tagged enum in the tree — and it is not.
    ///
    /// This is the Boing! defect, reduced to the building block. `AbilityCondition` and
    /// `QuantityRef` both have a `PreviousEffectAmount` variant; the condition's node carries
    /// extra fields, which serde silently DROPS when it deserializes the node as the unit
    /// variant `QuantityRef::PreviousEffectAmount`. Unanchored, a *condition* was therefore
    /// read as proof the unit carries a *dynamic quantity*, suppressing a real swallow.
    #[test]
    fn quantity_probe_is_key_anchored_not_tag_matched() {
        // The verbatim shape the parser lowers Boing!'s "if the result is 3 or less" to:
        // an AbilityCondition, sitting under the key `condition`.
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"abilities":[{"condition":{"type":"PreviousEffectAmount","comparator":"LE","rhs":{"type":"Fixed","value":3}}}]}"#,
        );

        assert!(
            evidence.any::<QuantityRef>(|_| true),
            "unanchored: the AbilityCondition IS accepted as a QuantityRef — the hazard"
        );
        assert!(
            !evidence.any_quantity_ref(|_| true),
            "key-anchored: a condition under `condition` is NOT a dynamic quantity"
        );
    }

    /// The anchored probe still sees a genuine quantity — the fix must not blind the detector.
    /// This is Collective Restraint's Domain count, which legitimately dissolves its warning.
    #[test]
    fn a_quantity_under_a_quantity_key_is_still_evidence() {
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"statics":[{"condition":{"scaling":{"data":{"quantity":{"type":"BasicLandTypeCount","controller":"You"}}}}}]}"#,
        );

        assert!(
            evidence.any_quantity_ref(|q| matches!(q, QuantityRef::BasicLandTypeCount { .. })),
            "a real QuantityRef under the `quantity` key must still be evidence"
        );
    }

    /// PROVEN COLLISION (task #51). A struct variant is NOT automatically safe unanchored:
    /// `Effect::CastFromZone`'s nine fields ALL carry `#[serde(default)]`, so serde matches it
    /// on the `type` tag ALONE — it is a unit variant in every way that matters here, and
    /// unknown sibling fields are silently dropped.
    ///
    /// `AbilityCondition::CastFromZone { zone }` is CR 601.2a's "if you cast this spell from
    /// your graveyard" — a CONDITION. It carries no effect and no lifetime. Unanchored, the
    /// `Duration_ThisTurn` detector read it as an `Effect::CastFromZone`, i.e. as a one-shot
    /// cast permission whose "this turn" is intrinsic, and dissolved the swallow warning on
    /// cards whose "this turn" is in fact dropped. This is the Boing! defect (#50) in a second
    /// enum.
    #[test]
    fn effect_probe_is_key_anchored_not_tag_matched() {
        // The verbatim shape the parser lowers a cast-zone CONDITION to: an AbilityCondition
        // sitting under the key `condition`, carrying a `zone` that `Effect` does not have.
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"abilities":[{"condition":{"type":"CastFromZone","zone":"Graveyard"}}]}"#,
        );

        assert!(
            evidence.any::<Effect>(|e| matches!(e, Effect::CastFromZone { .. })),
            "unanchored: the AbilityCondition IS accepted as an Effect — the hazard being guarded"
        );
        assert!(
            !evidence.any_effect(|e| matches!(e, Effect::CastFromZone { .. })),
            "key-anchored: a condition under `condition` is NOT an Effect carrier"
        );
    }

    /// The anchored `Effect` probe must still see a REAL effect — otherwise the fix would blind
    /// every detector that asks "does this unit carry effect E?". Built from a real
    /// `ParsedAbilities` (not a hand-written fixture) so it pins the production key spelling.
    #[test]
    fn a_real_effect_under_the_effect_key_is_still_evidence() {
        let def = AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("x", "y"));
        let evidence = UnitEvidence::of(&parsed_with(def));

        assert!(
            evidence.any_effect(|e| matches!(e, Effect::Unimplemented { .. })),
            "a real `AbilityDefinition.effect` carrier must remain visible to the anchored probe"
        );
    }

    /// PROVEN COLLISION (task #51). `ActivationRestriction::RequiresCondition` and
    /// `CastingRestriction::RequiresCondition` are FIELD-IDENTICAL
    /// (`{ condition: Option<ParsedCondition> }`), so the `type` tag discriminates nothing and
    /// each deserializes cleanly as the other. They are different rules — CR 602.5 (activation
    /// prohibited) vs CR 601.2 (casting) — and the `Duration_ThisTurn` detector uses the
    /// ACTIVATION one as proof that an "activate only ... this turn" clause was represented.
    /// A casting restriction is not that proof.
    ///
    /// Both enums are ADJACENTLY tagged (`tag = "type", content = "data"`), so their nodes are
    /// `{"type":X,"data":{..}}` — the payload lives under `data`, and a struct variant will NOT
    /// deserialize without it. That is why the fixture below carries a `data` wrapper: an
    /// internally-tagged shape would not be a valid node for either enum, and the test would
    /// prove nothing.
    #[test]
    fn activation_restriction_probe_is_key_anchored_not_tag_matched() {
        // A CastingRestriction, in its real home: `ParsedAbilities.casting_restrictions`.
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"casting_restrictions":[{"type":"RequiresCondition","data":{"condition":null}}]}"#,
        );

        assert!(
            evidence.any::<ActivationRestriction>(
                |r| matches!(r, ActivationRestriction::RequiresCondition { .. })
            ),
            "unanchored: the CastingRestriction IS accepted as an ActivationRestriction — the hazard"
        );
        assert!(
            !evidence.any_activation_restriction(
                |r| matches!(r, ActivationRestriction::RequiresCondition { .. })
            ),
            "key-anchored: a restriction under `casting_restrictions` is not an ACTIVATION restriction"
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
