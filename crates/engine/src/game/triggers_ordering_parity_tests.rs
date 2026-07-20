//! CR 603.3b: PR-6.75 trigger-ordering conflict-gate tests.
//!
//! Two families:
//!   * The §5.2 **allowlist-parity sweep** — the corpus-wide proof that the new
//!     `ability_rw` conflict gate reproduces the DELETED 12-string serde
//!     allowlist's decision on every printed no-ordering-input trigger, modulo
//!     the seven proven-order-dependent category-(1) rows (the CR 603.3b fix).
//!     A FROZEN in-test copy of the deleted walk is the reference oracle.
//!   * The **discriminators** N-A/N-B/N-B2/N-C/N-D/N-F — hand-built groups run
//!     through the production ordering-soundness authority
//!     (`group_is_order_independent`), each paired so exactly one classification
//!     decision is bracketed (revert-fails recorded in the impl report).
//!
//! N-E (the per-arm profile pairings) already lives in `ability_rw`'s unit tests
//! and is NOT duplicated here.

use super::*;
use crate::game::ability_rw::{
    ability_rw_profile, filter_excludes_source, profiles_conflict, source_census_overlaps_filter,
    trigger_condition_rw_profile, ControllerUniformity, GroupStructure, SourceCensus,
};
use crate::game::ability_utils::{build_resolved_from_def, build_target_slots};
use crate::game::game_object::GameObject;
use crate::test_support::shared_card_db;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AggregateFunction, ChoiceType, Comparator, ControllerRef,
    Effect, ObjectScope, PlayerFilter, PlayerScope, QuantityExpr, QuantityRef, ResolvedAbility,
    SearchSelectionConstraint, TargetFilter, TargetSelectionMode, TriggerCondition,
    TriggerConstraint, TriggerDefinition, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::Supertype;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ZoneChangeRecord};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use std::collections::BTreeSet;

// ===================================================================
// Frozen reference oracle: verbatim copy of the DELETED serde walk
// (was value_contains_trigger_event_context_ref / :3395-3420). Kept here so the
// parity sweep remains provable after the production fn is gone.
// ===================================================================

fn legacy_value_contains_event_context_ref(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(tag) => matches!(
            tag.as_str(),
            "TriggeringSpellController"
                | "TriggeringSpellOwner"
                | "TriggeringPlayer"
                | "TriggeringSource"
                | "ParentTarget"
                | "ParentTargetController"
                | "ParentTargetOwner"
                | "StackSpell"
                | "CostPaidObject"
                | "EventContextAmount"
                | "EventContextSourceCostX"
                | "ManaSpentToCast"
        ),
        serde_json::Value::Array(values) => {
            values.iter().any(legacy_value_contains_event_context_ref)
        }
        serde_json::Value::Object(map) => map.values().any(legacy_value_contains_event_context_ref),
        _ => false,
    }
}

/// The old `ability_uses_trigger_event_context`: serialize the ability, walk for
/// one of the 12 tags.
fn legacy_allowlist(ability: &ResolvedAbility) -> bool {
    serde_json::to_value(ability)
        .map(|v| legacy_value_contains_event_context_ref(&v))
        .unwrap_or(true)
}

/// The AST bears an `Effect::Unimplemented` (serde tag `Unimplemented`).
fn ast_bears_unimplemented(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s == "Unimplemented",
        serde_json::Value::Array(vs) => vs.iter().any(ast_bears_unimplemented),
        serde_json::Value::Object(map) => {
            // `Unimplemented` is a struct-variant key OR a `"type":"Unimplemented"` tag.
            map.contains_key("Unimplemented") || map.values().any(ast_bears_unimplemented)
        }
        _ => false,
    }
}

// ===================================================================
// §5.2 allowlist-parity sweep
// ===================================================================

/// The canonical category-(1) rows (§5.2): provably order-dependent printed
/// groups whose same-event auto→prompt flip IS the CR 603.3b fix. Each carries
/// its in-plan order-dependence proof (see PR-6.75-C0FULL-C1-PLAN.md §5.2). A
/// same-event diff on any OTHER card is an implementation finding, never a new
/// row (STRICT PROOF-GATE).
const CATEGORY_1_ROWS: &[&str] = &[
    // Ouroboroid: each copy's Power{Source} read is fed by the other's
    // PutCounterAll — token/counter totals differ by order.
    "ouroboroid",
    // Docent of Perfection: each copy's Wizard-token write feeds the shared
    // census threshold — WHICH copy transforms depends on order.
    "docent of perfection",
    // Sidequest: Hunt the Mark: token write feeds the shared census threshold.
    "sidequest: hunt the mark",
    // Promise of Tomorrow: first copy's mass return flips the second's CR 603.4
    // re-check — whose stash returns depends on order.
    "promise of tomorrow",
    // Spawn of Mayhem: damage order across the 10-life threshold decides which
    // copy gets the counter.
    "spawn of mayhem",
    // Your Inescapable Doom: PutCounter{Any} write × sibling CountersOn{Source}
    // read — same-kind counter feed (CR 904.3 two-copy scheme deck).
    "your inescapable doom",
    // Complex Automaton: ObjectCount{Permanent} intervening-if read × self-Bounce
    // whose moved census (permanent) overlaps the read filter.
    "complex automaton",
];

/// §7 documented-conservative over-prompts: cards whose sweep decision-diff is a
/// PROVEN-SAFE conservative prompt (auto would also be sound, but no in-scope
/// structural recognizer proves it context-free). Listing here ONLY suppresses the
/// sweep's unexplained-flag — it never changes a runtime ordering decision.
/// Suppression is direction-gated: only old=auto→new=prompt (over-prompt) rows may
/// match; an under-prompt diff is NEVER suppressible (fail-closed, CR 603.3b).
/// Grouped by class; each entry carries its one-line safety argument (§7 ledger
/// source of truth).
const DOCUMENTED_OVER_PROMPT: &[&str] = &[
    // ---- L8-held: a self-scoped write can flip a sibling's re-checked
    // intervening-if (CR 603.4), but the flip is monotone/self-limiting, so
    // identical siblings still commute semantically. The L8 idempotence recognizer
    // is deliberately HELD (a wrong recognizer would UNDER-prompt = rules-wrong;
    // the conservative prompt is fail-closed = rules-correct). ----
    // end-step ZoneChangeCountThisTurn read × self-Sacrifice — self-limiting.
    "chorale of the void",
    // Upkeep ObjectCount(Creature, Battlefield) >= 4 intervening-if × DestroyAll +
    // self-Sacrifice (AST measured). The first copy's board sweep drives the census
    // to 0, so the sibling's CR 603.4 re-check is false: it is removed from the stack,
    // does nothing, and is NOT sacrificed. The write is monotone (the census only
    // falls) and self-limiting, so identical siblings commute up to relabeling — either
    // order sweeps the board and sacrifices exactly one of the two copies.
    "planar collapse",
    // Mill membership write × ZoneCardCount(gy, Creature) self-transform threshold —
    // monotone graveyard growth.
    "deathbonnet sprout",
    // HandSize(you > opp) read × self-return-to-hand — the write monotonically
    // strengthens the condition.
    "death of a thousand stings",
    // Upkeep hand-size read × self-return-to-hand (same shape as death of a
    // thousand stings).
    "exile into darkness",
    // SourceIsTapped read; the Glimmer-token write feeds the Glimmer-count branch —
    // self-limiting.
    "glimmer seeker",
    // ControlCount(SelfRef) read × Meld consumes the shared pair — self-limiting.
    "graf rats",
    // Control(blue) read × CopyTokenOf{self} blue-token write — monotone;
    // already-true stays true.
    "mirror-sigil sergeant",
    // GraveyardSize read × self-return-from-graveyard — self-limiting threshold.
    "persistent marshstalker",
    // ObjectCount(creature card in your graveyard) == 1 intervening-if read ×
    // optional self-return-from-graveyard — self-limiting (second copy breaks the
    // gate; issue #5983 fixture regen surfaced this card in the sweep corpus).
    "nether spirit",
    // Control(another nonland) read × self-Sacrifice — self-limiting.
    "reclusive wight",
    // Not(SourceIsTapped) read × untap-ALL write — idempotent.
    "reveille squad",
    // Token write × ObjectCount == 13 self-sacrifice — equality over symmetric writes.
    "stensia uprising",
    // Mill + self-exile + put-on-top writes × ZoneCardCount(Library == 0) read.
    "stillness in motion",
    // ---- osseous-class: owner-keyed read × owner-misaligned write ----
    // DistinctCardTypes{your gy} GE 4 read × opponent-Sacrifice write that lands in
    // the sacrificed permanent's OWNER's graveyard (CR 701.21a): a control-changed
    // permanent you own makes the write-span truly Any; only the monotone GE
    // comparator commutes it (deferred L8).
    "osseous sticktwister",
    // ---- context-free-unclassifiable singletons ----
    // RevealTop → Destroy{ParentTarget}: the filter-on-revealed read is
    // resolution-local, but the AST has no scope distinguishing it from a live
    // board read.
    "paroxysm",
    // ControlsType{Creature power GE 4} re-check × graveyard→battlefield self-return
    // commutes ONLY because the phoenix's own power (2 < 4) — runtime P/T, invisible
    // to a context-free recognizer.
    "flamewake phoenix",
    // ---- parse-blocked commutes (underneath the mis-parse, the structure
    // commutes) ----
    // RemoveCounter{TriggeringSource} mis-parse (should be SelfRef): each removes
    // its own time counter.
    "deep-sea kraken",
    // HasCounters(oil) mis-reads the source Golem (should be the entering creature):
    // both writes are monotone oil-adds on the shared object.
    "ichorplate golem",
    // ---- owner-misalignment (osseous-class), batch — aligned-safe via sum-symmetry ----
    // dies-batch GainLife{ZoneCardCount(your gy, Creature)} + self-exile-from-gy:
    // the count-read × graveyard-self-exile is a genuine structural RW conflict, so
    // production prompts; but for the OWNER-ALIGNED co-departure (all copies land in
    // your graveyard) each resolution removes exactly itself, so the multiset of
    // per-copy gains {1..N} — and the total — is order-invariant. No in-scope
    // recognizer proves that sum-symmetry, so the prompt is conservative. (Owner-
    // MISaligned copies go to a different graveyard and are correctly prompted by
    // the same conflict — see the impl-report soundness note.)
    "skyfisher spider",
];

/// Batch-depth GENUINE order-dependence (kept SEPARATE from the same-event
/// CATEGORY_1_ROWS — different oracle: these diff against the legacy serde walk).
/// The new prompt is CORRECT (CR 603.3b: the controller chooses the order), i.e.
/// an intended prompt the legacy engine wrongly auto-ordered — not an over-prompt.
const BATCH_GENUINE_ROWS: &[&str] = &[
    // LTB: Sacrifice{Dragons-you-control, count: ObjectCount{same}} → sub
    // ChangeZone{TrackedSet(0) → Battlefield, enters_under: You} (AST measured).
    // Each co-departing member returns ITS OWN tracked exile set, and returned
    // creatures can be Dragons the OTHER member's re-sacrifice then consumes —
    // final state differs by order (CR 603.3b intended prompt). Runtime pin:
    // b2_dotd_member_bound_pin (reads_member_bound refuses batch-T1).
    "day of the dragons",
    // LTB (self_ref): Discard{count: HandSize{Controller}} → sub Draw{count:
    // Intensity{Source}} gated on EffectOutcome(OptionalEffectPerformed) (AST
    // measured). Each co-departing Hellion draws off ITS OWN intensity but discards
    // the SHARED hand, so the second trigger discards the cards the first one just
    // drew: with intensities a != b the final hand (b vs a cards), the graveyard, and
    // the library differ by order — the members are not identical functions (intensity
    // is per-source mutable state), so the commutation proof genuinely fails. The new
    // prompt is the CR 603.3b choice the legacy serde walk wrongly auto-ordered
    // (CR 603.5: each copy's "may" is chosen on resolution). Only visible post-#5721/
    // #5719 — the Draw was Unimplemented until the quantity channel bound Intensity,
    // and the sweep skips Unimplemented-bearing triggers.
    "great desert hellion",
];

/// R3 HIGH-1 (PR #5072 review): SAME-EVENT GENUINE member-bound order-dependence —
/// the REAL under-prompts the maintainer review caught. The base engine auto-ordered
/// ALL same-event groups unconditionally (CR 603.3b DEFERRED); the same-event member-
/// bound discriminator (`profiles_conflict`: `if s.same_event && p.reads_member_bound`)
/// now correctly PROMPTS them. Kept SEPARATE from the conservative class-count below
/// (rider-3 §7 honesty), analogous to `BATCH_GENUINE_ROWS`. Every member reads
/// per-source bound storage AND routes a SHARED object into it, so the identical-
/// function commutation proof genuinely fails (not merely conservatively).
const SAME_EVENT_MEMBER_BOUND_GENUINE: &[&str] = &[
    // Imprint-contention (CR 603.10a look-back): two copies race to imprint the ONE
    // shared triggering creature; only the first exiles it (CR 701.13), and which
    // source's per-source pile holds it is observable via each source's later
    // "return each other card exiled with this".
    "mimic vat",
    "mirror of life trapping",
    // Hand-swap re-capture (CR 603.3b): each copy exiles the shared hand into its own
    // per-source pile then returns its prior pile — copy A feeds copy B, so the
    // hand/pile partition is order-dependent.
    "moonring mirror",
    "duplicity",
    // Shared-pool consumption under per-copy-divergent chosen card types (CR 701.21):
    // an overlap permanent one copy sacrifices denies the other a target. Genuine on
    // rules grounds; its `reads_member_bound` bit comes from a non-obvious AST carrier
    // (the obvious `IsChosenCreatureType` is classified non-member-bound). See §7.
    "world queller",
    // blood tyrant: genuine via a DIFFERENT axis — a `PutCounter{SelfRef}` self-write
    // whose per-copy counter split is order-observable when a player hits 0 life
    // between resolutions (CR 704.5a / CR 800.4a). NON-source-independent; caught
    // incidentally by the member-bound discriminator (its `TrackedSetSize` read).
    "blood tyrant",
];

/// R4 (PR #5072 review): SAME-EVENT GENUINE event-object read/write order-dependence —
/// the honesty list for the same-event event-object discriminator (`profiles_conflict`:
/// `if s.same_event && s.event_object_present && p.reads_and_writes_event_object()`),
/// analogous to `SAME_EVENT_MEMBER_BOUND_GENUINE`. **EMPTY — 0 genuine among the 13
/// flips (all conservative).** GOVERNING THEOREM (why R4 differs from R3): the
/// discriminator groups IDENTICAL siblings, and every R4 write targets either the
/// SHARED event object (`TriggeringSource` / parentless `ParentTarget`) or each copy's
/// OWN disjoint self. Identical `f∘f` on ONE shared object is relabel-invariant; disjoint
/// self-writes never interact — so the final STATE is order-invariant either way. R3's
/// genuine members instead wrote to PER-SOURCE storage (imprint pile A ≠ pile B), an
/// asymmetric destination that IS order-divergent. R4 has no per-source-divergent write,
/// hence 0 genuine. The class below is a pure fail-closed safety over-prompt: sound but
/// never necessary. Full per-card both-orders evidence: `.pr675-r4-classification.md`.
/// A populated-but-unhit entry trips the completeness assert (a future genuine card is
/// added HERE, not manufactured). CR 603.3b + CR 608.2h.
const SAME_EVENT_EVENT_OBJECT_GENUINE: &[&str] = &[];

/// The census of a printed card's source (core types + subtypes + non-token).
fn face_census(face: &CardFace) -> SourceCensus {
    let mut tags: Vec<String> = Vec::new();
    for ct in &face.card_type.core_types {
        tags.push(ct.to_string());
    }
    for st in &face.card_type.subtypes {
        tags.push(st.clone());
    }
    tags.push("nontoken".to_string());
    SourceCensus::from_tags(tags)
}

/// Whether the firing event carries an object (production uses
/// `extract_source_from_event`; phase/turn/global modes carry none).
fn mode_carries_event_object(mode: &TriggerMode) -> bool {
    !matches!(
        mode,
        TriggerMode::Phase
            | TriggerMode::TurnBegin
            | TriggerMode::NewGame
            | TriggerMode::BecomeMonarch
            | TriggerMode::TakesInitiative
            | TriggerMode::LosesGame
    )
}

/// Whether the mode is a battlefield-departure (dies / LTB / sacrifice) — the
/// departure-batch structures are reachable only for these.
fn mode_is_battlefield_departure(
    mode: &TriggerMode,
    def: &crate::types::ability::TriggerDefinition,
) -> bool {
    match mode {
        TriggerMode::LeavesBattlefield | TriggerMode::Destroyed | TriggerMode::Sacrificed => true,
        // Zone-change modes are departures only when origin is the battlefield.
        TriggerMode::ChangesZone | TriggerMode::ChangesZoneAll => {
            def.origin == Some(Zone::Battlefield) || def.origin_zones.contains(&Zone::Battlefield)
        }
        _ => false,
    }
}

/// §5.2 per-trigger structure-reachability (CLASS-A guard): whether a same-event
/// 2-copy group is corpus-REACHABLE for this trigger. Applying a same-event
/// structure where two distinct non-legendary sources can NEVER share the firing
/// event, or where a 2-copy group can never form, is meaningless — a decision
/// diff there must NOT be counted (§5.2). Returns false (exempt from the
/// same-event structures) when:
///   * LEGENDARY — two same-name legendaries can't coexist under one controller
///     (CR 704.5j legend rule), so a 2-copy same-event group never persists; the
///     plan's `same_event` class is measured over NON-legendary cards (§1.3).
///     (gisela, the broken blade; doors of durin.)
///   * PER-SOURCE event mode — the event IS the source's own action, so two
///     distinct sources fire on DISTINCT events: self-scoped `DamageDone`/
///     `DamageDoneOnce` (combat damage is a per-source `DamageDealt` event keyed
///     by `source_id`, events.rs:408 — valeron wardens, acolyte of the inferno),
///     or a Saga-chapter `CounterAdded` on a SelfRef `valid_card` (per-source lore
///     counter). An OBSERVER DamageDone ("whenever a creature deals damage") is
///     NOT exempt (it CAN be shared).
///   * CONDITION self-exclusion (§1.3.1-F, CR 603.4) — the shared intervening-if
///     counts `Another`-self-exclusion objects the SOURCE itself matches and
///     requires that count to be 0; with two copies each sees the other (≥ 1) ⇒
///     each re-check is FALSE at trigger time ⇒ neither fires ⇒ no group forms
///     (thopter assembly; dust stalker).
fn same_event_group_reachable(
    face: &CardFace,
    trig: &TriggerDefinition,
    census: &SourceCensus,
) -> bool {
    if face.card_type.supertypes.contains(&Supertype::Legendary) {
        return false;
    }
    match trig.mode {
        // Damage triggers carry their SUBJECT in `valid_source` (`valid_card` is
        // ALWAYS None for them — make_base leaves it, oracle_trigger sets
        // valid_source), so a SELF-damage trigger is `valid_source == SelfRef`.
        // Combat damage is a per-source `GameEvent::DamageDealt{source_id}`
        // (events.rs:408), so two distinct self-sources never share it ⇒ exempt.
        // An OBSERVER damage trigger (`valid_source` non-SelfRef, e.g. "whenever a
        // creature you control deals damage") CAN fire two copies off ONE source's
        // damage event ⇒ NOT exempt (gating on `valid_card` here would wrongly
        // exempt every observer, since `valid_card` is None for all of them).
        TriggerMode::DamageDone | TriggerMode::DamageDoneOnce
            if matches!(trig.valid_source, Some(TargetFilter::SelfRef)) =>
        {
            return false
        }
        TriggerMode::CounterAdded if matches!(trig.valid_card, Some(TargetFilter::SelfRef)) => {
            return false
        }
        _ => {}
    }
    if let Some(cond) = &trig.condition {
        if condition_excludes_second_copy(cond, census) {
            return false;
        }
    }
    true
}

/// §1.3.1-F (CR 603.4): does the shared intervening-if become FALSE whenever a
/// second identical source exists? True when it counts objects matching an
/// `Another`-self-exclusion filter the source ITSELF matches and requires that
/// count to be 0 (or `< 1`) — each of two copies then sees the other (count ≥ 1),
/// so neither trigger's re-check passes at trigger time.
fn condition_excludes_second_copy(cond: &TriggerCondition, census: &SourceCensus) -> bool {
    match cond {
        TriggerCondition::QuantityComparison {
            lhs,
            rhs,
            comparator,
        } => {
            let Some(filter) = object_count_filter(lhs) else {
                return false;
            };
            let Some(n) = fixed_value(rhs) else {
                return false;
            };
            filter_excludes_source(filter)
                && source_census_overlaps_filter(census, filter)
                && count_ge_one_falsifies(*comparator, n)
        }
        // "controls no Another-X" ≡ count == 0.
        TriggerCondition::ControlsNone { filter } => {
            filter_excludes_source(filter) && source_census_overlaps_filter(census, filter)
        }
        // A single false conjunct falsifies the whole AND ⇒ the trigger can't fire.
        TriggerCondition::And { conditions } => conditions
            .iter()
            .any(|c| condition_excludes_second_copy(c, census)),
        _ => false,
    }
}

fn object_count_filter(q: &QuantityExpr) -> Option<&TargetFilter> {
    match q {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => Some(filter),
        _ => None,
    }
}
fn fixed_value(q: &QuantityExpr) -> Option<i32> {
    match q {
        QuantityExpr::Fixed { value } => Some(*value),
        _ => None,
    }
}
/// Is `count <cmp> n` FALSE for every `count ≥ 1`? (`EQ 0` / `LE ≤0` / `LT ≤1`.)
fn count_ge_one_falsifies(cmp: Comparator, n: i32) -> bool {
    match cmp {
        Comparator::EQ => n == 0,
        Comparator::LE => n <= 0,
        Comparator::LT => n <= 1,
        _ => false,
    }
}

/// A no-ordering-input trigger shape (mirrors `trigger_has_no_ordering_input`):
/// no modal / division / target announcement (CR 603.3c/3d). `build_target_slots`
/// is the production target-collection authority; an error is treated as
/// has-input (conservatively excluded).
fn sweep_no_ordering_input(
    state: &GameState,
    resolved: &ResolvedAbility,
    def: &AbilityDefinition,
) -> bool {
    def.modal.is_none()
        && def.mode_abilities.is_empty()
        && def.target_constraints.is_empty()
        && resolved.multi_target.is_none()
        && resolved.distribution.is_none()
        && build_target_slots(state, resolved)
            .map(|slots| slots.is_empty())
            .unwrap_or(false)
}

fn ability_rw_profile_merged(
    resolved: &ResolvedAbility,
    trig_condition: Option<&TriggerCondition>,
) -> crate::game::ability_rw::RwProfile {
    let mut p = ability_rw_profile(resolved);
    if let Some(tc) = trig_condition {
        p.merge(trigger_condition_rw_profile(tc));
    }
    p
}

/// PR-6.75 (CR 603.3b + CR 500.1): the CLOSED sweep-side controller-privacy
/// predicate. A `Phase` trigger carrying the `OnlyDuringYourTurn` constraint fires
/// only when `state.active_player == controller` (fire-time gate,
/// `triggers.rs:702` → `check_trigger_constraint`), and one `PhaseChanged` event
/// has exactly one active player — so EVERY reachable same-event group of such a
/// trigger is same-controller. Fail-closed for every other shape: "each [player's]"
/// phases parse to `constraint: None` (`oracle_trigger.rs`), opponent possessives
/// to `OnlyDuringOpponentsTurn`, and enchanted/chosen-player forms to a
/// `valid_target` with `constraint: None` — all ⇒ `false` here. The owner-alignment
/// owner-alignment half (`UniformAligned`) is computed LIVE and fail-closed in production; the
/// sweep models the reachable canonical group (an exotic donated-source variant
/// merely over-prompts in production, never under-prompts).
fn trigger_is_controller_private(trig: &TriggerDefinition) -> bool {
    matches!(trig.mode, TriggerMode::Phase)
        && matches!(trig.constraint, Some(TriggerConstraint::OnlyDuringYourTurn))
}

#[test]
fn ordering_parity_sweep() {
    let db = shared_card_db();
    let full_db = std::env::var_os("FORGE_TEST_FULL_DB").is_some();
    // A minimal state solely for `build_target_slots` structural collection.
    let state = GameState::new_two_player(1);
    let src = ObjectId(1);
    let ctrl = PlayerId(0);

    let mut swept = 0usize;
    let mut compared = 0usize;
    let mut unexplained: Vec<String> = Vec::new();
    let mut cat1_hit: BTreeSet<String> = BTreeSet::new();
    let mut over_prompt_hit: BTreeSet<String> = BTreeSet::new();
    let mut batch_genuine_hit: BTreeSet<String> = BTreeSet::new();
    // R3 HIGH-1: same-event member-bound genuine hits (completeness-asserted like
    // BATCH_GENUINE_ROWS) + the conservative class-count's derived membership
    // (BTreeSet ⇒ the eprintln evidence emits sorted, so it's a stable artifact the
    // rebased head can regenerate — rider-1).
    let mut se_mb_genuine_hit: BTreeSet<String> = BTreeSet::new();
    let mut se_member_bound_class: BTreeSet<String> = BTreeSet::new();
    // R4: same-event event-object genuine hits (empty exact-set) + the conservative
    // event-object over-prompt class-count's derived membership (evidence artifact).
    let mut se_event_object_genuine_hit: BTreeSet<String> = BTreeSet::new();
    let mut se_event_object_class: BTreeSet<String> = BTreeSet::new();

    // Non-vacuity floors (full-DB; the committed fixture is a subset).
    let mut floor_batch_obs = 0usize;
    let mut floor_hadcounters_batch_self = 0usize;
    let mut floor_retained_prompt = 0usize;
    let mut floor_t1_source_indep = 0usize;

    for (_key, face) in db.face_iter() {
        let name = face.name.to_lowercase();
        let census = face_census(face);
        for trig in &face.triggers {
            let Some(def) = trig.execute.as_deref() else {
                continue;
            };
            let resolved = build_resolved_from_def(def, src, ctrl);
            let value = serde_json::to_value(&resolved).unwrap_or(serde_json::Value::Null);
            if ast_bears_unimplemented(&value) {
                continue;
            }
            if !sweep_no_ordering_input(&state, &resolved, def) {
                continue;
            }
            swept += 1;

            let profile = ability_rw_profile_merged(&resolved, trig.condition.as_ref());
            let legacy_serde = legacy_allowlist(&resolved);
            let legacy_prompt = profile.legacy_batch_prompt();
            let self_ref = matches!(trig.valid_card, Some(TargetFilter::SelfRef));
            let excludes = trig
                .valid_card
                .as_ref()
                .map(filter_excludes_source)
                .unwrap_or(false);
            let event_present = mode_carries_event_object(&trig.mode);
            let is_departure = mode_is_battlefield_departure(&trig.mode, trig);
            let hadcounters = matches!(trig.condition, Some(TriggerCondition::HadCounters { .. }));

            let mk =
                |same_event: bool,
                 all_same_source: bool,
                 self_departed: bool,
                 controller_uniformity: ControllerUniformity| GroupStructure {
                    same_event,
                    all_same_source,
                    all_sources_self_departed: self_departed,
                    event_object_excludes_sources: if self_ref { false } else { excludes },
                    event_object_present: event_present,
                    source_census: census.clone(),
                    controller_uniformity,
                };

            // --- Same-event, distinct-source observer (S2) ---
            // §5.2: only where a 2-copy same-event group is REACHABLE (CLASS-A
            // guard — skip legendary / per-source-event / condition-self-excluding
            // shapes where the structure can never form). PR-6.75: the modeled
            // canonical S2 group is controller-private exactly for the fire-time-
            // pinned Phase+OnlyDuringYourTurn class (`trigger_is_controller_private`);
            // production additionally computes owner-alignment live and fail-closed.
            if !self_ref && same_event_group_reachable(face, trig, &census) {
                // §5.2: the modeled canonical S2 group is controller-private (fire-
                // time-pinned Phase+OnlyDuringYourTurn) ⇒ `UniformAligned` (owner-
                // alignment computed live in production); every other shape ⇒ `Mixed`
                // (fail-closed). Only `UniformAligned` unlocks the span/fused gate.
                let se = mk(
                    true,
                    false,
                    false,
                    if trigger_is_controller_private(trig) {
                        ControllerUniformity::UniformAligned
                    } else {
                        ControllerUniformity::Mixed
                    },
                );
                let conflict = profiles_conflict(&profile, &se);
                let decision_new = !conflict; // decision_old (same-event) == auto == true
                compared += 1;
                if !decision_new {
                    // A same-event diff (auto -> prompt): a proven category-(1)
                    // genuine flip, a same-event member-bound flip (genuine sub-set,
                    // or the conservative member-bound over-prompt CLASS), or a
                    // documented-conservative over-prompt. This arm is inherently
                    // over-prompt-direction — decision_old == auto (base engine
                    // auto-ordered ALL same-event unconditionally), so it can NEVER
                    // flag an under-prompt (rider-2 safety).
                    if CATEGORY_1_ROWS.contains(&name.as_str()) {
                        cat1_hit.insert(name.clone());
                    } else if SAME_EVENT_MEMBER_BOUND_GENUINE.contains(&name.as_str()) {
                        // Checked BEFORE the class-count so a genuine flip is never
                        // mislabeled conservative (rider-3 honesty).
                        se_mb_genuine_hit.insert(name.clone());
                    } else if SAME_EVENT_EVENT_OBJECT_GENUINE.contains(&name.as_str()) {
                        // R4 honesty list (EMPTY — 0 genuine). Checked before the
                        // event-object class-count for symmetry with the member-bound
                        // genuine list; a future genuine card is added to the const.
                        se_event_object_genuine_hit.insert(name.clone());
                    } else if DOCUMENTED_OVER_PROMPT.contains(&name.as_str()) {
                        over_prompt_hit.insert(name.clone());
                    } else if profile.reads_member_bound() {
                        // R3 HIGH-1: the same-event member-bound over-prompt CLASS.
                        // Predicate-keyed (mirrors the discriminator's own gate
                        // `s.same_event && p.reads_member_bound` exactly — one
                        // predicate, two consumers), membership DERIVED not enumerated
                        // (rider-1: corpus-churn-robust — a 48-name allowlist would
                        // break on every regen). CONSERVATIVE by construction: an
                        // identical-member group whose resolution reads per-source
                        // bound storage cannot be PROVEN to commute, so it prompts
                        // fail-closed. Most members are order-immaterial (disjoint
                        // per-source referents, same controller optimizes both); the
                        // genuinely order-dependent members are carved out above. The
                        // sorted membership is emitted below as the §7 evidence
                        // artifact.
                        se_member_bound_class.insert(name.clone());
                    } else if se.event_object_present && profile.reads_and_writes_event_object() {
                        // R4: the same-event event-object read/write over-prompt CLASS.
                        // Predicate-keyed EXACTLY like the discriminator's own gate
                        // (`s.same_event && s.event_object_present &&
                        // p.reads_and_writes_event_object()` — one predicate, two
                        // consumers), membership DERIVED (rider-1). Placed AFTER the
                        // `reads_member_bound()` branch, so a both-carrier card is
                        // attributed to `se_member_bound_class` (row order = attribution
                        // order) ⇒ the two classes are DISJOINT BY CONSTRUCTION (asserted
                        // below, rider-1). CONSERVATIVE by the governing theorem: every
                        // write targets the SHARED event object (identical f∘f, relabel-
                        // invariant) or disjoint self ⇒ order-invariant final state; 0
                        // genuine (carved out above). NOTE: a Phase-mode trigger such as
                        // `sidequest: catch a fish` sits OUTSIDE this class by the
                        // `event_object_present` conjunct — no live event object ⇒ its
                        // `TriggeringSource` write no-ops (targeting.rs:951) ⇒ it stays
                        // AUTO, NOT a missed member. Sorted membership emitted below as
                        // the §8 evidence artifact.
                        se_event_object_class.insert(name.clone());
                    } else {
                        unexplained.push(format!(
                            "SAME-EVENT auto->prompt on '{name}' (not a category-(1) row)"
                        ));
                    }
                } else if profile.source_independent() {
                    floor_t1_source_indep += 1;
                }
            }

            // --- Departure-batch structures (S3 self-departed / S5 mixed obs) ---
            if is_departure {
                // §5: batch rows model `ControllerUniformity::Uniform`. Production
                // ordering groups are partitioned by `trigger_order_controller`
                // (triggers.rs), so every non-team co-departure group is controller-
                // uniform BY CONSTRUCTION; team-pooled groups (CR 805.7) compute the
                // fact live and fail-closed to `Mixed` ⇒ over-prompt (never under-
                // prompt). `Uniform` (not `UniformAligned`) keeps the span/fused gate
                // OFF — the span model is byte-identical; the only clause it newly
                // unlocks is batch-T1.
                let batch = if self_ref {
                    mk(false, false, true, ControllerUniformity::Uniform) // self-dies
                } else {
                    mk(false, false, false, ControllerUniformity::Uniform) // observer batch
                };
                let batch_conflict = legacy_prompt || profiles_conflict(&profile, &batch);
                let decision_new = !batch_conflict;
                let decision_old = !legacy_serde;
                compared += 1;
                if decision_new != decision_old {
                    // Fail-closed direction gate: only the conservative direction
                    // (old=auto → new=prompt) is allowlistable; an under-prompt diff
                    // (old=prompt → new=auto) is ALWAYS unexplained — a rules-required
                    // prompt is NEVER suppressible (CR 603.3b soundness invariant).
                    let over_prompt_direction = decision_old && !decision_new;
                    if over_prompt_direction && BATCH_GENUINE_ROWS.contains(&name.as_str()) {
                        batch_genuine_hit.insert(name.clone());
                    } else if over_prompt_direction
                        && DOCUMENTED_OVER_PROMPT.contains(&name.as_str())
                    {
                        over_prompt_hit.insert(name.clone());
                    } else {
                        unexplained.push(format!(
                            "BATCH decision diff on '{name}' (old={decision_old}, new={decision_new}, \
                             legacy_serde={legacy_serde}, legacy_prompt={legacy_prompt})"
                        ));
                    }
                }
                // Retained-prompt parity (D5): every legacy-ref trigger must still
                // prompt on the batch path.
                if legacy_prompt {
                    assert!(
                        batch_conflict,
                        "retained-prompt parity: '{name}' carries a legacy event-context ref \
                         but its batch group auto-orders"
                    );
                    floor_retained_prompt += 1;
                }
                if decision_new {
                    if self_ref {
                        // batch_self_srcread floor REMOVED: the {self-departure ∧
                        // Power/Toughness/CountersOn{Source} read} cell is EMPTY
                        // in-corpus — dies-triggers reading "its power/counters" parse
                        // to event-context scopes (TriggeringSource-class), so post-N-G
                        // that whole class correctly prompts and is counted by
                        // retained_prompt; the freeze-auto behavior it witnessed is
                        // pinned by units N-A/N-B.
                        if hadcounters {
                            floor_hadcounters_batch_self += 1;
                        }
                    } else {
                        floor_batch_obs += 1;
                    }
                }
            }
        }
    }

    eprintln!(
        "ordering_parity_sweep: full_db={full_db} swept={swept} compared={compared} \
         unexplained={} cat1_hit={:?} over_prompt_hit={} batch_genuine_hit={} \
         se_mb_genuine={:?} se_member_bound_class={} \
         se_event_object_genuine={:?} se_event_object_class={}",
        unexplained.len(),
        cat1_hit,
        over_prompt_hit.len(),
        batch_genuine_hit.len(),
        se_mb_genuine_hit,
        se_member_bound_class.len(),
        se_event_object_genuine_hit,
        se_event_object_class.len()
    );
    eprintln!(
        "floors: batch_obs={floor_batch_obs} \
         hadcounters_batch_self={floor_hadcounters_batch_self} \
         retained_prompt={floor_retained_prompt} t1_source_indep={floor_t1_source_indep}"
    );
    // R3 HIGH-1 (rider-1): mechanically-emitted DERIVED membership of the same-event
    // member-bound over-prompt CLASS — the §7 / handoff evidence artifact. Sorted
    // (BTreeSet), so it is a stable diff the rebased head regenerates. Enumeration
    // lives HERE (evidence), never in the code allowlist.
    eprintln!(
        "se_member_bound_class members ({}): {:?}",
        se_member_bound_class.len(),
        se_member_bound_class
    );
    // R4 (rider-1): DERIVED membership of the same-event event-object over-prompt CLASS
    // — the §8 / handoff evidence artifact (mirrors se_member_bound_class). Sorted, so a
    // stable diff the rebased head regenerates. Enumeration lives HERE, never in code.
    eprintln!(
        "se_event_object_class members ({}): {:?}",
        se_event_object_class.len(),
        se_event_object_class
    );

    assert!(
        unexplained.is_empty(),
        "STRICT PROOF-GATE: {} unexplained decision diff(s):\n{}",
        unexplained.len(),
        unexplained.join("\n")
    );

    // Non-vacuity: the iteration must actually have classified triggers.
    assert!(swept > 0, "sweep visited zero triggers (fixture missing?)");
    assert!(compared > 0, "sweep compared zero decisions");

    if full_db {
        // §5.2 positive floors (full-DB measured minima @ b1b3c873e; floor =
        // measured − 5% corpus-churn tolerance so a classifier regression trips it).
        // The `batch_self_srcread` floor is REMOVED — its cell is empty in-corpus
        // (see the loop comment); its old ≥40 was a D5 fail-open-hole artifact now
        // counted by `retained_prompt`.

        // batch_obs: measured 494 @ b1b3c873e; floor = ⌊494·0.95⌋. Zero batch-T1
        // dependence (all 5 T1-carried autos are self_ref) — guards sweep
        // reachability + mass-classification health; a classifier regression (M6/M8)
        // collapses it toward 0.
        assert!(
            floor_batch_obs >= 469,
            "batch_obs auto floor: {floor_batch_obs} < 469"
        );
        // hadcounters_batch_self: measured 1 @ b1b3c873e; floor = measured − 0. Sole
        // witness: Promising Duskmage (HadCounters{P1P1} dies → Draw — a pure
        // CR 603.10a frozen look-back read, auto under a valid freeze). N=1 corpus
        // cell; fragility accepted (only full-DB witness of the frozen-look-back auto
        // class). Mutation M5 (freeze_valid=false) drops it to 0.
        assert!(
            floor_hadcounters_batch_self >= 1,
            "HadCounters batch_self auto floor: {floor_hadcounters_batch_self} < 1"
        );
        // retained_prompt: measured 285 @ b1b3c873e; floor = ⌈285·0.95⌉. Makes the D5
        // legacy-prompt override population a real regression tripwire (M4 → 0).
        assert!(
            floor_retained_prompt >= 271,
            "retained-prompt floor: {floor_retained_prompt} < 271"
        );
        // t1_source_indep: measured 2871 @ b1b3c873e; floor = ⌊2871·0.95⌋.
        // Deterministic tripwire for the `source_independent` predicate (M1 → 0).
        // R3 HIGH-1: measured drops to 2830 (−40) — the 40 source-INDEPENDENT members
        // of the same-event member-bound over-prompt CLASS now correctly PROMPT
        // (discriminator) instead of auto-ordering, so they no longer increment this
        // auto counter. R4: a FURTHER −11 (measured 2820 @ rebased corpus) — the
        // source-independent slice of `se_event_object_class` (11 of the 13; the other
        // 2 are non-source-independent, e.g. self-writing) now prompts via the event-
        // object discriminator. The floor is UNCHANGED (2820 > 2727, still passes) —
        // both are accounted movements, NOT blanket bumps (rider-4); the moved
        // populations are exactly the source-independent slices of the two over-prompt
        // classes.
        assert!(
            floor_t1_source_indep >= 2727,
            "T1 source-independent auto floor: {floor_t1_source_indep} < 2727"
        );

        // Ledger completeness (full-DB only — the committed fixture lacks these
        // cards). Every allowlist entry MUST be consumed: an unhit entry is stale
        // bookkeeping (the card cleared — remove it — or its name drifted).
        let expected_over_prompt: BTreeSet<String> = DOCUMENTED_OVER_PROMPT
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            over_prompt_hit, expected_over_prompt,
            "DOCUMENTED_OVER_PROMPT stale/unhit entries"
        );
        let expected_batch_genuine: BTreeSet<String> =
            BATCH_GENUINE_ROWS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            batch_genuine_hit, expected_batch_genuine,
            "BATCH_GENUINE_ROWS stale/unhit entries"
        );
        // R3 HIGH-1: the 6 same-event member-bound GENUINE flips (5 member-bound +
        // blood tyrant). Exact-set like BATCH_GENUINE_ROWS — a genuine card dropping
        // out (parse/corpus drift) OR a new genuine flip appearing forces the §7
        // honesty bookkeeping to update rather than silently landing in the
        // conservative class-count.
        let expected_se_mb_genuine: BTreeSet<String> = SAME_EVENT_MEMBER_BOUND_GENUINE
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            se_mb_genuine_hit, expected_se_mb_genuine,
            "SAME_EVENT_MEMBER_BOUND_GENUINE stale/unhit entries"
        );
        // se_member_bound_class: measured 42 @ corpus bce9695e5; floor = measured −
        // ~15% churn tolerance (`>=`, corpus-churn-robust per rider-1 — the class is
        // predicate-keyed so its membership churns with the corpus). NON-vacuity /
        // mutation tripwire: deleting the discriminator row (`s.same_event &&
        // p.reads_member_bound`) makes every member auto-order ⇒ this count collapses
        // to 0 ⇒ floor trips. Guards the HIGH-1 fix itself against silent regression.
        assert!(
            se_member_bound_class.len() >= 35,
            "same-event member-bound over-prompt class floor: {} < 35",
            se_member_bound_class.len()
        );
        // R4: the same-event event-object GENUINE exact-set — EMPTY (0 genuine among
        // the 13 flips; governing theorem: all writes are to the SHARED event object or
        // disjoint self ⇒ relabel-invariant final state). A future genuine flip (parse/
        // corpus drift) lands in `se_event_object_class` and trips the STRICT PROOF-GATE
        // only if not added HERE — so this exact-set forces the §8 honesty bookkeeping.
        let expected_se_event_object_genuine: BTreeSet<String> = SAME_EVENT_EVENT_OBJECT_GENUINE
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            se_event_object_genuine_hit, expected_se_event_object_genuine,
            "SAME_EVENT_EVENT_OBJECT_GENUINE stale/unhit entries"
        );
        // se_event_object_class: measured 13 @ rebased corpus; floor = ⌊13·0.7⌋ (~30%
        // churn tolerance, `>=`, predicate-keyed so membership churns with the corpus).
        // NON-vacuity / mutation tripwire: deleting the discriminator row (`s.same_event
        // && s.event_object_present && p.reads_and_writes_event_object()`) makes every
        // member auto-order ⇒ this count collapses to 0 ⇒ floor trips. Guards the R4 fix
        // against silent regression.
        assert!(
            se_event_object_class.len() >= 9,
            "same-event event-object over-prompt class floor: {} < 9",
            se_event_object_class.len()
        );
        // rider-1: the two same-event over-prompt classes are DISJOINT by construction
        // (the `reads_member_bound()` classify branch precedes the event-object branch,
        // so a both-carrier card is attributed to `se_member_bound_class`). Asserted so
        // a future reorder that double-counts a card trips here.
        assert!(
            se_member_bound_class.is_disjoint(&se_event_object_class),
            "rider-1: se_member_bound_class ∩ se_event_object_class must be empty; overlap = {:?}",
            se_member_bound_class
                .intersection(&se_event_object_class)
                .collect::<Vec<_>>()
        );
        // cat-1 exact-hit: the 6 measured same-event flips. Your Inescapable Doom is
        // expected-UNHIT (its counter feed parses to Any so the sweep never forms its
        // group); if that parser gap is ever fixed it starts hitting and this assert
        // forces the bookkeeping update.
        let expected_cat1: BTreeSet<String> = [
            "complex automaton",
            "docent of perfection",
            "ouroboroid",
            "promise of tomorrow",
            "sidequest: hunt the mark",
            "spawn of mayhem",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            cat1_hit, expected_cat1,
            "CATEGORY_1_ROWS measured-hit set drift"
        );
    }
}

// ===================================================================
// Discriminators N-A / N-B / N-B2 / N-C / N-D / N-F
// (group_is_order_independent = the production ordering-soundness authority;
//  true = auto/no prompt, false = prompt/OrderTriggers)
// ===================================================================

fn ctx(
    source: u64,
    ability: ResolvedAbility,
    condition: Option<TriggerCondition>,
    event: Option<GameEvent>,
    die_result: Option<i32>,
) -> PendingTriggerContext {
    PendingTriggerContext::single(PendingTrigger {
        source_id: ObjectId(source),
        controller: PlayerId(0),
        condition,
        ability,
        timestamp: 0,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: event,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result,
    })
}

fn ra(effect: Effect) -> ResolvedAbility {
    ResolvedAbility::new(effect, vec![], ObjectId(1), PlayerId(0))
}
fn qfix(v: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value: v }
}
fn qref(r: QuantityRef) -> QuantityExpr {
    QuantityExpr::Ref { qty: r }
}
fn creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}
fn power_src() -> QuantityRef {
    QuantityRef::Power {
        scope: ObjectScope::Source,
    }
}
fn put_counter_all(count: QuantityExpr) -> Effect {
    Effect::PutCounterAll {
        count,
        target: creature(),
        counter_type: CounterType::Plus1Plus1,
    }
}
fn token_power_src() -> Effect {
    Effect::Token {
        name: "Elemental".into(),
        power: crate::types::ability::PtValue::Fixed(1),
        toughness: crate::types::ability::PtValue::Fixed(1),
        types: vec!["Creature".into()],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: qref(power_src()),
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}
fn return_all_creatures_gy_to_bf() -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target: creature(),
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::default(),
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}
fn gain_life_fixed() -> Effect {
    Effect::GainLife {
        amount: qfix(1),
        player: TargetFilter::Controller,
    }
}
fn power_ge(n: i32) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: qref(power_src()),
        rhs: qfix(n),
        comparator: Comparator::GE,
    }
}
fn cond(mut a: ResolvedAbility, c: AbilityCondition) -> ResolvedAbility {
    a.condition = Some(c);
    a
}

/// A self-departure ZoneChanged event (object leaves the battlefield to the
/// graveyard), co-departing with `co`.
fn self_departure(id: u64, co: &[u64]) -> Option<GameEvent> {
    Some(GameEvent::ZoneChanged {
        object_id: ObjectId(id),
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            co_departed: co.iter().map(|&x| ObjectId(x)).collect(),
            ..ZoneChangeRecord::test_minimal(ObjectId(id), Some(Zone::Battlefield), Zone::Graveyard)
        }),
    })
}

/// One shared ETB event of a third object (id 99) — both observers fire on it.
fn shared_etb_event() -> Option<GameEvent> {
    Some(GameEvent::ZoneChanged {
        object_id: ObjectId(99),
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord::test_minimal(
            ObjectId(99),
            Some(Zone::Hand),
            Zone::Battlefield,
        )),
    })
}

fn empty_state() -> GameState {
    GameState::new_two_player(9)
}

/// R4: read the SHARED event object's LIVE power (`EventSource` scope ⇒
/// `reads_event_live`, CR 608.2h current-information read).
fn power_event() -> QuantityRef {
    QuantityRef::Power {
        scope: ObjectScope::EventSource,
    }
}
/// R4: write the SHARED event object (`TriggeringSource` ⇒ `writes_event_object`):
/// +1/+1 counters.
fn put_counter_on_event(count: QuantityExpr) -> Effect {
    Effect::PutCounter {
        counter_type: CounterType::Plus1Plus1,
        count,
        target: TargetFilter::TriggeringSource,
    }
}
/// R4: a same-event firing event that carries NO object — `extract_source_from_event`
/// returns `None` (CR: `Phase` has no event object), so `group_is_order_independent`
/// threads `event_object_present = false`.
fn shared_phase_event() -> Option<GameEvent> {
    Some(GameEvent::PhaseChanged {
        phase: crate::types::phase::Phase::End,
    })
}

/// N-A: two Nested Shamblers (Token{count: Power{Source}}) co-departing in one
/// SBA batch ⇒ their Source-power reads are LKI-frozen ⇒ NO OrderTriggers prompt.
/// (The frozen 3+1 token counts are a quantity-resolution concern pinned by
/// `quantity.rs`'s LKI tests; this asserts the ORDERING verdict this PR owns.)
#[test]
fn n_a_shambler_co_departure_auto_orders() {
    let state = empty_state();
    let group = vec![
        ctx(
            10,
            ra(token_power_src()),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            ra(token_power_src()),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &group),
        "co-departing Shamblers read frozen LKI power ⇒ must auto-order (no prompt)"
    );
}

/// N-B: the frozen-vs-live discriminator. Same AST (PutCounterAll{Power{Source}}
/// — CR 122.1 counters feed P/T) resolves differently by group structure:
///   * co-departure batch ⇒ Source read FROZEN ⇒ no feed ⇒ AUTO.
///   * same-event alive pair ⇒ Source read LIVE ⇒ counter write feeds it ⇒ PROMPT.
///
/// Proves the freeze is applied exactly on the departure-batch path.
#[test]
fn n_b_frozen_vs_live_discriminator() {
    let state = empty_state();
    let ability = || ra(put_counter_all(qref(power_src())));

    let batch = vec![
        ctx(10, ability(), None, self_departure(10, &[11]), None),
        ctx(11, ability(), None, self_departure(11, &[10]), None),
    ];
    assert!(
        group_is_order_independent(&state, &batch),
        "co-departure: frozen Source read ⇒ auto"
    );

    let ev = shared_etb_event();
    let same_event = vec![
        ctx(10, ability(), None, ev.clone(), None),
        ctx(11, ability(), None, ev, None),
    ];
    assert!(
        !group_is_order_independent(&state, &same_event),
        "same-event alive: live Source read fed by sibling counter write ⇒ prompt"
    );
}

/// N-B2: the freeze-invalidation hostile. A co-departing pair that ALSO returns
/// creature cards from the graveyard to the battlefield (external existing-object
/// move, battlefield destination ⇒ re-entry hazard) re-binds a member's ObjectId
/// to live state, so the frozen Source read is invalidated ⇒ PROMPT. The plain
/// pair (no return clause) stays auto ⇒ the row prompts exactly the hazard groups.
#[test]
fn n_b2_freeze_invalidation_hostile() {
    let state = empty_state();
    let hazard = || ra(token_power_src()).sub_ability(ra(return_all_creatures_gy_to_bf()));
    let hazard_group = vec![
        ctx(10, hazard(), None, self_departure(10, &[11]), None),
        ctx(11, hazard(), None, self_departure(11, &[10]), None),
    ];
    assert!(
        !group_is_order_independent(&state, &hazard_group),
        "battlefield-return hazard invalidates the LKI freeze ⇒ prompt"
    );

    let plain = vec![
        ctx(
            10,
            ra(token_power_src()),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            ra(token_power_src()),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &plain),
        "plain Shambler shape (no hazard write) ⇒ auto — the row is hazard-scoped"
    );
}

/// N-C: Case A (CR 603.3b). Two byte-identical "put +1/+1 on each creature you
/// control; draw if this creature's power ≥ 6" off ONE event: the counter write
/// feeds the sibling's live Source-power read ⇒ order-observable ⇒ PROMPT.
#[test]
fn n_c_case_a_same_event_prompts() {
    let state = empty_state();
    let ability = || cond(ra(put_counter_all(qfix(1))), power_ge(6));
    let ev = shared_etb_event();
    let group = vec![
        ctx(10, ability(), None, ev.clone(), None),
        ctx(11, ability(), None, ev, None),
    ];
    assert!(
        !group_is_order_independent(&state, &group),
        "Case A: counter write feeds live power threshold ⇒ prompt (CR 603.3b fix)"
    );
}

/// N-D: intervening-if Case A. The threshold rides the TRIGGER-level condition
/// (CR 603.4 — re-checked at resolution) instead of the ability. The merged
/// `trigger_condition_rw_profile` carries the Source read ⇒ PROMPT. Revert-fail:
/// dropping the trigger-condition merge auto-orders it.
#[test]
fn n_d_intervening_if_case_a_prompts() {
    let state = empty_state();
    let trig_cond = || TriggerCondition::QuantityComparison {
        lhs: qref(power_src()),
        rhs: qfix(6),
        comparator: Comparator::GE,
    };
    let ev = shared_etb_event();
    let group = vec![
        ctx(
            10,
            ra(put_counter_all(qfix(1))),
            Some(trig_cond()),
            ev.clone(),
            None,
        ),
        ctx(
            11,
            ra(put_counter_all(qfix(1))),
            Some(trig_cond()),
            ev,
            None,
        ),
    ];
    assert!(
        !group_is_order_independent(&state, &group),
        "intervening-if Source read fed by sibling counter write ⇒ prompt"
    );
}

/// N-F: die_result conjunct unit pin (CR 706.2 + CR 603.12). Two otherwise-
/// identical no-input source-independent triggers off one event with DIFFERENT
/// stamped die results are NOT the same state transformation ⇒ not order-
/// independent; EQUAL die results are admitted. Revert-fail: removing the
/// conjunct admits the differing pair.
#[test]
fn n_f_die_result_conjunct() {
    let state = empty_state();
    let ev = shared_etb_event();
    let differing = vec![
        ctx(10, ra(gain_life_fixed()), None, ev.clone(), Some(3)),
        ctx(11, ra(gain_life_fixed()), None, ev.clone(), Some(5)),
    ];
    assert!(
        !group_is_order_independent(&state, &differing),
        "differing die_result ⇒ not order-independent"
    );

    let equal = vec![
        ctx(10, ra(gain_life_fixed()), None, ev.clone(), Some(3)),
        ctx(11, ra(gain_life_fixed()), None, ev, Some(3)),
    ];
    assert!(
        group_is_order_independent(&state, &equal),
        "equal die_result + source-independent same-event pair ⇒ admitted (auto)"
    );
}

/// CLASS-A guard, FIX A: the `DamageDone` same-event exemption keys on
/// `valid_source` (damage triggers leave `valid_card` None — they carry their
/// subject in `valid_source`). A SELF-damage trigger (`valid_source == SelfRef`)
/// is per-source ⇒ exempt from the S2 comparison; an OBSERVER damage trigger
/// (`valid_source` non-SelfRef) fires two copies off ONE source's damage event
/// ⇒ compared. Revert-fail: gating on `valid_card` (None for BOTH) would exempt
/// the observer too, voiding the D3 same-event parity proof for observer damage.
#[test]
fn class_a_damage_done_exempts_self_source_not_observer() {
    let face = CardFace::default(); // non-legendary
    let census = SourceCensus::unknown();

    let mut self_dmg = TriggerDefinition::new(TriggerMode::DamageDone);
    self_dmg.valid_source = Some(TargetFilter::SelfRef);
    assert!(
        !same_event_group_reachable(&face, &self_dmg, &census),
        "self-damage (valid_source=SelfRef) ⇒ per-source ⇒ exempt from S2"
    );

    let mut observer = TriggerDefinition::new(TriggerMode::DamageDone);
    observer.valid_source = Some(TargetFilter::Typed(TypedFilter::creature()));
    // valid_card stays None (as for every real damage trigger).
    assert!(
        same_event_group_reachable(&face, &observer, &census),
        "observer damage (valid_source non-SelfRef) ⇒ shared source event ⇒ compared"
    );
}

/// N-G (D5, CR 603.10a): the fail-open batch-prompt hole this commit closes,
/// proven through the production ordering authority. A co-departing pair of
/// identical "when this leaves the battlefield, target player discards" triggers
/// carries `TargetFilter::TriggeringPlayer` in the `Discard` TARGET position —
/// one of the 12 frozen event-context tags. `rw_effect`'s `Discard` arm ignores
/// its `target` field, so before this commit the read/write walk never routed
/// the tag through a legacy leaf detector and `legacy_batch_prompt` stayed false:
/// the departure batch auto-ordered where the shipped engine prompted.
///
/// Revert-fail witness: removing the `p.legacy_batch_prompt =
/// contains_legacy_event_ref(a)` override in `ability_rw_profile` makes the
/// legacy group auto-order and flips the first assertion. The `Controller`
/// control (identical discard, no frozen tag) proves the prompt is driven by the
/// tag itself, not by `Discard`'s effect shape.
#[test]
fn n_g_dropped_target_legacy_ref_retains_batch_prompt() {
    let state = empty_state();
    let discard = |t: TargetFilter| Effect::Discard {
        count: qfix(1),
        target: t,
        unless_filter: None,
        filter: None,
        selection: crate::types::ability::CardSelectionMode::Chosen,
    };

    let legacy_group = vec![
        ctx(
            10,
            ra(discard(TargetFilter::TriggeringPlayer)),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            ra(discard(TargetFilter::TriggeringPlayer)),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        !group_is_order_independent(&state, &legacy_group),
        "Discard{{TriggeringPlayer}} on a departure batch carries a frozen tag ⇒ retain prompt"
    );

    let control_group = vec![
        ctx(
            10,
            ra(discard(TargetFilter::Controller)),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            ra(discard(TargetFilter::Controller)),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &control_group),
        "identical Discard with no frozen tag (Controller) ⇒ auto-order"
    );
}

// ===================================================================
// PR-6.75 controller-uniformity span discriminators (S1–S6), driven through the
// production authority `group_is_order_independent`. POSITIVE (auto) groups
// install source objects owned AND controlled by the pending controller so the
// LIVE controller-uniformity check holds; each NEG breaks exactly ONE axis (span
// disjointness, controller-uniformity, or owner-alignment) and must re-prompt —
// the paired positive reach-guard proves the auto is delivered by the refined
// row, not an upstream fast-path short-circuit.
// ===================================================================

/// Install a battlefield source with explicit owner + controller (the live-state
/// precondition the chokepoint reads: `o.controller == o.owner == c0`).
fn install_source(state: &mut GameState, id: u64, owner: u8, controller: u8) {
    let mut o = GameObject::new(
        ObjectId(id),
        CardId(1),
        PlayerId(owner),
        "Src".to_string(),
        Zone::Battlefield,
    );
    o.owner = PlayerId(owner);
    o.controller = PlayerId(controller);
    state.objects.insert(ObjectId(id), o);
}

/// A pending-trigger context with an explicit controller (S4/S5/S6 vary it).
fn ctx_c(
    source: u64,
    controller: u8,
    ability: ResolvedAbility,
    condition: Option<TriggerCondition>,
    event: Option<GameEvent>,
) -> PendingTriggerContext {
    PendingTriggerContext::single(PendingTrigger {
        source_id: ObjectId(source),
        controller: PlayerId(controller),
        condition,
        ability,
        timestamp: 0,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: event,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    })
}

/// Mirrors a collected trigger: ordering inputs own the exact source observed
/// when it fired, rather than consulting the storage slot again at ordering.
fn ctx_c_from_observed_source(
    state: &GameState,
    source: u64,
    controller: u8,
    mut ability: ResolvedAbility,
    condition: Option<TriggerCondition>,
    event: Option<GameEvent>,
) -> PendingTriggerContext {
    let source_id = ObjectId(source);
    let source_context = trigger_source_context_for_latch(
        state,
        state
            .objects
            .get(&source_id)
            .expect("ordering fixture installs its source"),
    );
    ability.set_trigger_source_recursive(source_context);
    ctx_c(source, controller, ability, condition, event)
}

fn creatures_of(cr: ControllerRef) -> TargetFilter {
    let mut tf = TypedFilter::creature();
    tf.controller = Some(cr);
    TargetFilter::Typed(tf)
}

fn sacrifice_self() -> Effect {
    Effect::Sacrifice {
        target: TargetFilter::SelfRef,
        count: qfix(1),
        min_count: 0,
    }
}
/// Defense-of-the-Heart shape: search the CONTROLLER's own library (no
/// `target_player`) → the phantom write earns a `You` chain move-owner fact.
fn search_own_creatures() -> Effect {
    Effect::SearchLibrary {
        source_zones: vec![Zone::Library],
        filter: creature(),
        count: QuantityExpr::UpTo {
            max: Box::new(qfix(2)),
        },
        reveal: false,
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
        split: None,
    }
}
/// The chained opaque battlefield entry (`Library → Battlefield`, `Any` target,
/// no `enters_under`) that consumes the search's `You` move-owner fact.
fn change_zone_lib_to_bf() -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination: Zone::Battlefield,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::default(),
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}
fn shuffle_ctrl() -> Effect {
    Effect::Shuffle {
        target: TargetFilter::Controller,
    }
}
fn bounce_self() -> Effect {
    Effect::Bounce {
        target: TargetFilter::SelfRef,
        destination: None,
        selection: crate::types::ability::BounceSelection::default(),
    }
}
fn discard_hand(count: QuantityExpr, target: TargetFilter) -> Effect {
    Effect::Discard {
        count,
        target,
        unless_filter: None,
        filter: None,
        selection: crate::types::ability::CardSelectionMode::Chosen,
    }
}
fn obj_count_cmp(filter: TargetFilter, cmp: Comparator, rhs: i32) -> TriggerCondition {
    TriggerCondition::QuantityComparison {
        lhs: qref(QuantityRef::ObjectCount { filter }),
        rhs: qfix(rhs),
        comparator: cmp,
    }
}
fn handsize_cmp(player: PlayerScope, cmp: Comparator, rhs: i32) -> TriggerCondition {
    TriggerCondition::QuantityComparison {
        lhs: qref(QuantityRef::HandSize { player }),
        rhs: qfix(rhs),
        comparator: cmp,
    }
}

/// Defense-of-the-Heart chain: `Sacrifice{SelfRef}` → `SearchLibrary` →
/// `ChangeZone{Library→Bf, Any}` → `Shuffle`.
fn defense_ability() -> ResolvedAbility {
    let cz = ra(change_zone_lib_to_bf()).sub_ability(ra(shuffle_ctrl()));
    let search = ra(search_own_creatures()).sub_ability(cz);
    ra(sacrifice_self()).sub_ability(search)
}

/// S-1 — Defense of the Heart shape. POS: same-event 2-copy, both P0, sources
/// installed owner==controller==P0; the opponents'-board census read is
/// ctrl-disjoint (Opponents) from the your-library battlefield entry (You) ⇒
/// AUTO. NEG reach-guard: flip the read's controller to `You` ⇒ ctrl spans
/// overlap ⇒ PROMPT. The NEG also proves the auto is NOT a `source_independent`
/// fast-path fluke — `Sacrifice{SelfRef}` keeps `source_independent` false in
/// both, so only the membership-ctrl span differs.
#[test]
fn s1_defense_membership_ctrl_span() {
    let mut state = empty_state();
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 0, 0);
    let ev = shared_etb_event();

    let pos_cond = || obj_count_cmp(creatures_of(ControllerRef::Opponent), Comparator::GE, 3);
    let pos = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            defense_ability(),
            Some(pos_cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(
            &state,
            11,
            0,
            defense_ability(),
            Some(pos_cond()),
            ev.clone(),
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "S1 POS: opponents'-board read × your-library entry are ctrl-disjoint ⇒ auto"
    );

    let neg_cond = || obj_count_cmp(creatures_of(ControllerRef::You), Comparator::GE, 3);
    let neg = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            defense_ability(),
            Some(neg_cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(&state, 11, 0, defense_ability(), Some(neg_cond()), ev),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "S1 NEG: your-board read × your-library entry ctrl spans overlap ⇒ prompt"
    );
}

/// S-2 — Rekindled Flame shape (`Bounce{SelfRef}` + `HandSize{Opponent} EQ 0`
/// intervening-if). POS: opp-hand read (Opponents) × your-hand self-bounce write
/// (You) ⇒ AUTO. Sibling NEG: read `HandSize{Controller}` (You) ⇒ hand spans
/// overlap ⇒ PROMPT.
#[test]
fn s2_rekindled_player_hand_span() {
    let mut state = empty_state();
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 0, 0);
    let ev = shared_etb_event();

    let pos_cond = || {
        handsize_cmp(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Min,
            },
            Comparator::EQ,
            0,
        )
    };
    let pos = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            ra(bounce_self()),
            Some(pos_cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(
            &state,
            11,
            0,
            ra(bounce_self()),
            Some(pos_cond()),
            ev.clone(),
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "S2 POS: opp-hand read × your-hand self-bounce are player-disjoint ⇒ auto"
    );

    let neg_cond = || handsize_cmp(PlayerScope::Controller, Comparator::EQ, 0);
    let neg = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            ra(bounce_self()),
            Some(neg_cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(&state, 11, 0, ra(bounce_self()), Some(neg_cond()), ev),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "S2 NEG: your-hand read × your-hand write overlap ⇒ prompt"
    );
}

/// Brink-of-Madness chain: `Sacrifice{SelfRef}` → scoped-Opponent
/// `Discard{count, target: Controller}`.
fn brink_ability(count: QuantityExpr) -> ResolvedAbility {
    let mut discard = ra(discard_hand(count, TargetFilter::Controller));
    discard.player_scope = Some(PlayerFilter::Opponent);
    ra(sacrifice_self()).sub_ability(discard)
}

/// S-3 — Brink of Madness fused RMW discriminator. POS: `count:
/// HandSize{ScopedPlayer}` is the fused "discards their hand" read ⇒ dropped
/// under the gate, leaving the your-hand intervening-if (You) vs the opp-hand
/// Discard write (Opponents) ⇒ AUTO. Fusion NEG: the SAME opp-scoped Discard but
/// `count: HandSize{Opponent}` — a genuine (non-fused) opp-hand observation that
/// is NOT dropped ⇒ read span degrades to Any ⇒ overlaps the write ⇒ PROMPT.
/// Isolates the fusion arm exactly (same write both sides).
#[test]
fn s3_brink_fused_discard_span() {
    let mut state = empty_state();
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 0, 0);
    let ev = shared_etb_event();
    let cond = || handsize_cmp(PlayerScope::Controller, Comparator::EQ, 0);

    let fused = qref(QuantityRef::HandSize {
        player: PlayerScope::ScopedPlayer,
    });
    let pos = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            brink_ability(fused.clone()),
            Some(cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(
            &state,
            11,
            0,
            brink_ability(fused),
            Some(cond()),
            ev.clone(),
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "S3 POS: fused HandSize{{ScopedPlayer}} count dropped ⇒ You-cond × Opp-write disjoint ⇒ auto"
    );

    let unfused = qref(QuantityRef::HandSize {
        player: PlayerScope::Opponent {
            aggregate: AggregateFunction::Min,
        },
    });
    let neg = vec![
        ctx_c_from_observed_source(
            &state,
            10,
            0,
            brink_ability(unfused.clone()),
            Some(cond()),
            ev.clone(),
        ),
        ctx_c_from_observed_source(&state, 11, 0, brink_ability(unfused), Some(cond()), ev),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "S3 NEG: non-fused opp-hand count read is a live player read ⇒ prompt"
    );
}

/// S-4 — load-bearing shared-event observer (the gate is load-bearing). Two
/// Rekindled-shape triggers off ONE event with controllers P0 and P1: mixed
/// controllers ⇒ `Mixed` ⇒ the spans are UNCONSULTED ⇒ the
/// hand row fires ⇒ PROMPT. Revert-fail foil (same shapes, both P0): the ONLY
/// change is the controller/owner of source 11 ⇒ `UniformAligned` ⇒
/// AUTO. Deleting the gate in `profiles_conflict` would flip the P0/P1 case to
/// auto (RED).
#[test]
fn s4_gate_is_load_bearing() {
    let mut state = empty_state();
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 1, 1); // owned + controlled by P1
    let ev = shared_etb_event();
    let cond = || {
        handsize_cmp(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Min,
            },
            Comparator::EQ,
            0,
        )
    };

    let mixed = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 11, 1, ra(bounce_self()), Some(cond()), ev.clone()),
    ];
    assert!(
        !group_is_order_independent(&state, &mixed),
        "S4 NEG: mixed controllers ⇒ Mixed ⇒ spans unconsulted ⇒ prompt"
    );

    // Revert-fail foil: reinstall source 11 under P0 and flip the pending
    // controller — the sole change that makes the group controller-private.
    install_source(&mut state, 11, 0, 0);
    let uniform = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 11, 0, ra(bounce_self()), Some(cond()), ev),
    ];
    assert!(
        group_is_order_independent(&state, &uniform),
        "S4 foil: both P0 ⇒ UniformAligned ⇒ span disjointness clears ⇒ auto"
    );
}

/// S-5 — MULTIPLAYER (3 players). (a) members P0 and P1: mixed controllers ⇒
/// Mixed ⇒ PROMPT (at 3p, P1's opponents include P0, so treating
/// them as controller-private would be unsound — the gate correctly refuses).
/// (b) both members P0: `You == {P0}` vs `Opponents == {P1,P2}` disjointness is
/// NOT a two-player artifact ⇒ AUTO at N players.
#[test]
fn s5_multiplayer_three_players() {
    let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 7);
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 1, 1);
    install_source(&mut state, 12, 0, 0);
    let ev = shared_etb_event();
    let cond = || {
        handsize_cmp(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Min,
            },
            Comparator::EQ,
            0,
        )
    };

    let mixed = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 11, 1, ra(bounce_self()), Some(cond()), ev.clone()),
    ];
    assert!(
        !group_is_order_independent(&state, &mixed),
        "S5a: 3p mixed controllers ⇒ Mixed ⇒ prompt"
    );

    let both_p0 = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 12, 0, ra(bounce_self()), Some(cond()), ev),
    ];
    assert!(
        group_is_order_independent(&state, &both_p0),
        "S5b: 3p both P0 ⇒ You={{P0}} vs Opponents={{P1,P2}} disjoint ⇒ auto (not a 2p artifact)"
    );
}

/// S-6 — owner-alignment discriminator (the `o.owner == c0` conjunct is
/// load-bearing). Rekindled shape, both members controlled by P0, but source 11
/// is OWNED by P1 (donated / control-changed). NEG: owner mismatch ⇒
/// Mixed ⇒ PROMPT — and this is a REAL under-prompt guard: the
/// self-bounce would put source 11 in P1's hand, which the `HandSize{Opponent}`
/// read observes. Revert-fail foil (source 11 owned by P0): UniformAligned
/// ⇒ AUTO. Dropping the owner conjunct would auto the NEG (RED).
#[test]
fn s6_owner_alignment() {
    let mut state = empty_state();
    install_source(&mut state, 10, 0, 0);
    install_source(&mut state, 11, 1, 0); // owner P1, controller P0 (donated)
    let ev = shared_etb_event();
    let cond = || {
        handsize_cmp(
            PlayerScope::Opponent {
                aggregate: AggregateFunction::Min,
            },
            Comparator::EQ,
            0,
        )
    };

    let donated = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 11, 0, ra(bounce_self()), Some(cond()), ev.clone()),
    ];
    assert!(
        !group_is_order_independent(&state, &donated),
        "S6 NEG: source owned by an opponent ⇒ owner-misaligned ⇒ prompt (real under-prompt guard)"
    );

    install_source(&mut state, 11, 0, 0); // now owner == controller == P0
    let aligned = vec![
        ctx_c_from_observed_source(&state, 10, 0, ra(bounce_self()), Some(cond()), ev.clone()),
        ctx_c_from_observed_source(&state, 11, 0, ra(bounce_self()), Some(cond()), ev),
    ];
    assert!(
        group_is_order_independent(&state, &aligned),
        "S6 foil: owner == controller == P0 ⇒ UniformAligned ⇒ auto"
    );
}

// ===================================================================
// PR-6.75 c5 — batch-T1 uniformity theorem discriminators (B-1..B-6 + the
// executable Dodecapod negative), driven through the production authority
// `group_is_order_independent`. Co-departure groups use DEAD sources (NOT
// installed in `state.objects`) so the chokepoint yields `Uniform` (pending
// controllers equal) rather than `UniformAligned` (live owner==controller) — the
// exact production shape of an SBA co-death batch. Each NEG breaks ONE T1 conjunct
// and is paired with a POS reach-guard proving the group reaches the batch-T1
// clause (not short-circuited upstream).
// ===================================================================

/// Mindslicer's fused self-emptying discard ("each player discards their hand") —
/// source-independent, no member-bound storage, no event read.
fn fused_discard_each() -> ResolvedAbility {
    let mut d = ra(discard_hand(
        qref(QuantityRef::HandSize {
            player: PlayerScope::ScopedPlayer,
        }),
        TargetFilter::Controller,
    ));
    d.player_scope = Some(PlayerFilter::All);
    d
}

/// Yukora/Emrakul removes-all: sacrifice all your matching creatures, `count =
/// ObjectCount` of the same filter — a pure f(state, controller).
fn sacrifice_all_own_creatures() -> ResolvedAbility {
    ra(Effect::Sacrifice {
        target: creatures_of(ControllerRef::You),
        count: qref(QuantityRef::ObjectCount {
            filter: creatures_of(ControllerRef::You),
        }),
        min_count: 0,
    })
}

/// A `ChangeZone{target → Battlefield, enters_under: You}` return (Exile → bf).
fn change_zone_return(target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Exile),
        destination: Zone::Battlefield,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: Some(ControllerRef::You),
        enter_tapped: crate::types::zones::EtbTapState::default(),
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

/// Slithermuse's resolution-local choice: `Choose{Opponent, persist:false}` → draw.
/// `persist:false` writes NO source storage ⇒ member-unbound ⇒ T1-clean.
fn choose_opponent_then_draw() -> ResolvedAbility {
    let draw = ra(Effect::Draw {
        count: qfix(1),
        target: TargetFilter::Controller,
    });
    ra(Effect::Choose {
        choice_type: ChoiceType::Opponent { restriction: None },
        persist: false,
        selection: TargetSelectionMode::default(),
    })
    .sub_ability(draw)
}

/// B-1 — the uniformity gate is load-bearing. Two byte-identical mindslicer fused
/// discards co-depart with DEAD sources. POS: both P0 ⇒ `Uniform` ⇒ batch-T1
/// clears ⇒ AUTO. NEG: controllers P0/P1 (a CR 805.7 team-pooled group) ⇒ `Mixed`
/// ⇒ T1 gated OFF ⇒ the fused hand read unions back and overlaps the discard's
/// hand write ⇒ PROMPT. Revert-fail (measured in the impl report): forcing
/// `Uniform`/deleting the `!= Mixed` conjunct flips the NEG to AUTO (RED).
#[test]
fn b1_mindslicer_uniformity_gate() {
    let state = empty_state();
    let pos = vec![
        ctx(
            10,
            fused_discard_each(),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            fused_discard_each(),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "B-1 POS: uniform fused-discard co-departure batch ⇒ batch-T1 auto"
    );

    // CR 805.7 team-pool: divergent pending controllers ⇒ `Mixed` ⇒ T1 off.
    let neg = vec![
        ctx_c(10, 0, fused_discard_each(), None, self_departure(10, &[11])),
        ctx_c(11, 1, fused_discard_each(), None, self_departure(11, &[10])),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "B-1 NEG: mixed-controller (team-pool) batch ⇒ Mixed ⇒ fused read × hand write ⇒ prompt"
    );
}

/// B-2 — the member-bound flag is the Day-of-the-Dragons soundness pin. POS:
/// yukora removes-all, uniform ⇒ T1 clears ⇒ AUTO. NEG: the DotD twin — the SAME
/// removes-all PLUS a sub `ChangeZone{TrackedSet(0) → Battlefield}` return. The
/// per-source tracked set sets `reads_member_bound` ⇒ T1 refused ⇒ the count-read
/// × return-write membership feed (reached ONLY because T1 is blocked) ⇒ PROMPT.
/// Revert-fail (measured): removing `TrackedSet` from `member_bound_target_filter`
/// drops the flag ⇒ T1 fires and short-circuits before the feed ⇒ AUTO (RED).
#[test]
fn b2_dotd_member_bound_pin() {
    let state = empty_state();
    let pos = vec![
        ctx(
            10,
            sacrifice_all_own_creatures(),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            sacrifice_all_own_creatures(),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "B-2 POS: uniform removes-all ⇒ batch-T1 auto"
    );

    let dotd = || {
        sacrifice_all_own_creatures().sub_ability(ra(change_zone_return(
            TargetFilter::TrackedSet {
                id: crate::types::identifiers::TrackedSetId(0),
            },
        )))
    };
    let neg = vec![
        ctx(10, dotd(), None, self_departure(10, &[11]), None),
        ctx(11, dotd(), None, self_departure(11, &[10]), None),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "B-2 NEG: DotD TrackedSet return ⇒ member-bound ⇒ T1 refused ⇒ prompt"
    );
}

/// R3 HIGH-1 (maintainer #5072 review): the SAME-EVENT member-bound discriminator.
/// The batch path refuses `reads_member_bound` (b2 above); this pins the mirrored
/// refusal at same-event depth. A same-event group of DISTINCT sources whose
/// identical `source_independent` resolution reads per-source bound storage
/// (`ChangeZone{TrackedSet → Battlefield}`, CR 603.10a look-back) is NOT one shared
/// f(state) — source 10 reads set(10), source 11 reads set(11) — so CR 603.3b
/// ordering is observable ⇒ PROMPT.
///
/// Revert-fail (both edits independently load-bearing): (1) restoring the bare
/// `p.source_independent()` disjunct at `profiles_conflict`'s same-event fast path
/// takes the member-bound group down the fast path ⇒ AUTO (RED); (2) deleting the
/// `if s.same_event && p.reads_member_bound { return true }` discriminator lets it
/// fall through to the feed rows, which check read/write KIND overlap not per-source
/// storage (the TrackedSet return writes membership with no matching read) ⇒ AUTO
/// (RED). Before this PR's same-event predicate the base engine auto-ordered ALL
/// same-event groups unconditionally, so this closes a hole in THIS PR's gate.
#[test]
fn high1_same_event_member_bound_prompts() {
    let state = empty_state();
    let member_bound = || {
        ra(change_zone_return(TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        }))
    };
    let ev = shared_etb_event();

    // NEG (the fix): same event (object 99), DISTINCT sources 10/11, member-bound
    // source-independent return ⇒ must PROMPT.
    let neg = vec![
        ctx(10, member_bound(), None, ev.clone(), None),
        ctx(11, member_bound(), None, ev.clone(), None),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "HIGH-1 NEG: same-event distinct-source member-bound ⇒ f_A≠f_B ⇒ prompt"
    );

    // POS-a (scope guard): a same-event source-independent NON-member-bound pair
    // (gain 1 life) stays AUTO — the discriminator is scoped to member-bound, not a
    // blanket same-event prompt.
    let pos_unbound = vec![
        ctx(10, ra(gain_life_fixed()), None, ev.clone(), None),
        ctx(11, ra(gain_life_fixed()), None, ev.clone(), None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_unbound),
        "HIGH-1 POS-a: same-event source-independent non-member-bound ⇒ auto"
    );

    // POS-b (all_same_source guard): the SAME member-bound ability on ONE shared
    // source (10) is one shared storage ⇒ f_A = f_B literally ⇒ AUTO. Proves the
    // discriminator only refuses distinct-source member-bound groups.
    let pos_same_source = vec![
        ctx(10, member_bound(), None, ev.clone(), None),
        ctx(10, member_bound(), None, ev, None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_same_source),
        "HIGH-1 POS-b: all_same_source member-bound ⇒ shared storage ⇒ auto"
    );
}

/// R4 (maintainer #5072 review): the SAME-EVENT event-object read/write discriminator
/// AT THE CALLER LEVEL. `group_is_order_independent` derives `event_object_present`
/// from the pending trigger's firing event (`extract_source_from_event`, triggers.rs)
/// and threads it into the GroupStructure — the predicate-level
/// `r4_same_event_event_object_feed_conjunct` (ability_rw) pins `profiles_conflict`
/// but NOT this caller threading. A future caller that mis-threaded
/// `event_object_present` (or dropped the same-event structure) could auto-order the
/// R4 shape while the predicate test stays green — the hot-path seam this pins.
///
/// The feed: two DISTINCT sources fire on ONE shared ETB event (object 99); each
/// identical resolution READS that creature's LIVE power (`Power{EventSource}` ⇒
/// `reads_event_live`, CR 608.2h) and WRITES it (`PutCounter{TriggeringSource}` ⇒
/// `writes_event_object`), so a sibling's write feeds the other's live read ⇒
/// order-observable ⇒ PROMPT. The feed is `source_independent`, so it exercises BOTH
/// edits: without the `:992` fast-path exclusion it would auto-order at the T1
/// disjunct; without the discriminator it falls through every row to the final
/// `return false` (`feeds()` is blind to the KindSet-less live read) ⇒ AUTO (RED).
///
/// Guards prove the discriminator is not a blanket same-event prompt: read-only ⇒ auto
/// (needs the write), write-only ⇒ auto (needs the read), no-event-object ⇒ auto (the
/// `event_object_present` thread — the identical feed prompts WITH an event object and
/// autos WITHOUT), all_same_source ⇒ auto (identical f over one shared object).
#[test]
fn r4_same_event_event_object_prompts_at_caller() {
    let state = empty_state();
    let feed = || ra(put_counter_on_event(qref(power_event())));
    let ev = shared_etb_event();

    // NEG (the fix): distinct sources 10/11, shared event object 99, live read × event-
    // object write ⇒ PROMPT (caller threads event_object_present = true).
    let neg = vec![
        ctx(10, feed(), None, ev.clone(), None),
        ctx(11, feed(), None, ev.clone(), None),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "R4 NEG: same-event distinct-source event-object read×write feed ⇒ prompt"
    );

    // Guard read-only — a live power read that WRITES player life, not the event object
    // (no `writes_event_object`) ⇒ AUTO. Proves the conjunction needs both endpoints.
    let read_only = || {
        ra(Effect::GainLife {
            amount: qref(power_event()),
            player: TargetFilter::Controller,
        })
    };
    let pos_read = vec![
        ctx(10, read_only(), None, ev.clone(), None),
        ctx(11, read_only(), None, ev.clone(), None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_read),
        "R4 guard read-only: event-live read with NO event-object write ⇒ auto"
    );

    // Guard write-only — a fixed-count event-object write with NO live read ⇒ AUTO
    // (an unread write is applied identically by every member, order-invariant).
    let write_only = || ra(put_counter_on_event(qfix(1)));
    let pos_write = vec![
        ctx(10, write_only(), None, ev.clone(), None),
        ctx(11, write_only(), None, ev.clone(), None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_write),
        "R4 guard write-only: event-object write with NO live read ⇒ auto"
    );

    // Guard no-event-object — the SAME feed on a Phase event (`extract_source_from_
    // event` → None ⇒ `event_object_present` threads FALSE) ⇒ AUTO. Directly pins the
    // caller threading: the identical ability prompts WITH an event object (NEG), autos
    // WITHOUT (here). A caller that always-threaded present=true would wrongly prompt.
    let ph = shared_phase_event();
    let pos_no_event = vec![
        ctx(10, feed(), None, ph.clone(), None),
        ctx(11, feed(), None, ph, None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_no_event),
        "R4 guard no-event-object: Phase event ⇒ event_object_present false ⇒ auto"
    );

    // Guard all_same_source — the feed on ONE shared source (10) ⇒ identical f over one
    // shared event object (deterministic accumulation) ⇒ AUTO. Proves the discriminator
    // only refuses DISTINCT-source groups.
    let pos_same_source = vec![
        ctx(10, feed(), None, ev.clone(), None),
        ctx(10, feed(), None, ev, None),
    ];
    assert!(
        group_is_order_independent(&state, &pos_same_source),
        "R4 guard all_same_source: shared source ⇒ f_A = f_B ⇒ auto"
    );
}

/// B-3 — the `source_independent` (self-write) conjunct + slithermuse's
/// `persist:false` clear. POS-a: slithermuse's `Choose{Opponent, persist:false}`
/// then draw, uniform ⇒ AUTO (a resolution-local choice is T1-clean). POS-b:
/// yukora removes-all ⇒ AUTO (the reach-guard: proves the shape reaches T1). NEG: add a
/// sub `ChangeZone{SelfRef, Battlefield → Graveyard}` self-sacrifice ⇒
/// `writes_self` ⇒ `source_independent()` false ⇒ T1 refused ⇒ the `ObjectCount`
/// membership read × the self bf→gy move feed ⇒ PROMPT (skyfisher class).
/// Revert-fail (measured): dropping `source_independent()` from batch-T1 flips the
/// NEG to AUTO (RED).
#[test]
fn b3_source_independent_and_persist_false() {
    let state = empty_state();
    let pos_slithermuse = vec![
        ctx(
            10,
            choose_opponent_then_draw(),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            choose_opponent_then_draw(),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos_slithermuse),
        "B-3 POS-a: slithermuse persist:false choice is T1-clean ⇒ auto"
    );

    let self_sac = || {
        sacrifice_all_own_creatures().sub_ability(ra(Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::default(),
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }))
    };
    let neg = vec![
        ctx(10, self_sac(), None, self_departure(10, &[11]), None),
        ctx(11, self_sac(), None, self_departure(11, &[10]), None),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "B-3 NEG: SelfRef self-move ⇒ writes_self ⇒ source_independent false ⇒ prompt"
    );
}

/// B-5 — the `reads_event_live` conjunct and its clause PLACEMENT (T1 sits ABOVE
/// the freeze-invalidation row). Both members `Draw{count} + ChangeZone{Any →
/// Battlefield}` (a reentry hazard ⇒ freeze invalid). POS: `count: Fixed` ⇒ no
/// event read ⇒ T1 clears ⇒ AUTO (reach-guard: the reentry hazard alone does not
/// prompt a T1-clean uniform batch). NEG: `count: Power{EventSource}` ⇒
/// `reads_event_live` ⇒ T1 refused ⇒ the freeze-invalidation row (reentry hazard ×
/// event-live read) ⇒ PROMPT. Revert-fail (measured): deleting `!reads_event_live`
/// from batch-T1 lets T1 fire and short-circuit BEFORE the freeze row ⇒ AUTO (RED).
#[test]
fn b5_event_live_read_freeze_placement() {
    let state = empty_state();
    let with_count = |count: QuantityExpr| {
        ra(Effect::Draw {
            count,
            target: TargetFilter::Controller,
        })
        .sub_ability(ra(change_zone_return(TargetFilter::Any)))
    };
    let pos = vec![
        ctx(
            10,
            with_count(qfix(1)),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            with_count(qfix(1)),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "B-5 POS: T1-clean uniform batch with a reentry hazard ⇒ auto (reach-guard)"
    );

    let ev_read = || {
        with_count(qref(QuantityRef::Power {
            scope: ObjectScope::EventSource,
        }))
    };
    let neg = vec![
        ctx(10, ev_read(), None, self_departure(10, &[11]), None),
        ctx(11, ev_read(), None, self_departure(11, &[10]), None),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "B-5 NEG: Power{{EventSource}} read × reentry hazard ⇒ freeze row ⇒ prompt"
    );
}

/// B-6 — multiplayer (3 players). A uniform P0 removes-all co-departure group
/// auto-orders at N players (POS); a mixed P0/P1 group prompts (NEG) — the
/// uniformity gate is not a two-player artifact (S-5 style).
#[test]
fn b6_multiplayer_uniform_vs_mixed() {
    let state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 7);
    let pos = vec![
        ctx(
            10,
            sacrifice_all_own_creatures(),
            None,
            self_departure(10, &[11]),
            None,
        ),
        ctx(
            11,
            sacrifice_all_own_creatures(),
            None,
            self_departure(11, &[10]),
            None,
        ),
    ];
    assert!(
        group_is_order_independent(&state, &pos),
        "B-6 POS: 3p uniform P0 removes-all batch ⇒ auto"
    );

    let neg = vec![
        ctx_c(
            10,
            0,
            sacrifice_all_own_creatures(),
            None,
            self_departure(10, &[11]),
        ),
        ctx_c(
            11,
            1,
            sacrifice_all_own_creatures(),
            None,
            self_departure(11, &[10]),
        ),
    ];
    assert!(
        !group_is_order_independent(&state, &neg),
        "B-6 NEG: 3p mixed P0/P1 batch ⇒ Mixed ⇒ prompt"
    );
}

/// ★ CONDITION 1 — the executable Dodecapod negative (the load-bearing negative
/// for the whole controller-uniformity pivot). A genuinely order-observable
/// MIXED-controller co-departure batch: two members controlled by P0 and P1 (a
/// CR 805.7 team-pooled group in a 3-player game), each an `EventSourceControlledBy`
/// -class consumer — a `Discard` whose downstream replacement (Dodecapod-style)
/// reads the DISCARD CAUSE's source controller. Whichever member resolves first
/// stamps ITS OWN source as the cause, so `event_source_controller != affected`
/// flips between orders (events.rs `Discarded.source_id`; replacement.rs
/// `EventSourceControlledBy`). The chokepoint yields `Mixed` ⇒ batch-T1 is gated
/// OFF ⇒ the group STILL PROMPTS (auto-ordering would be an unsound under-prompt).
/// Revert-fail (measured in the impl report): forcing the chokepoint to `Uniform`
/// (or deleting the `!= Mixed` conjunct) auto-orders this mixed batch = RED —
/// proving the uniformity gate is exactly what preserves the mixed-batch prompt.
#[test]
fn condition1_dodecapod_mixed_controller_still_prompts() {
    let state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 7);
    // A source-independent "each player discards their hand" — the Dodecapod
    // discard channel. Under a MIXED batch the cause-source controller is
    // order-observable; the profile itself is T1-clean, so ONLY the `!= Mixed`
    // uniformity gate keeps it prompting.
    let dodecapod = vec![
        ctx_c(10, 0, fused_discard_each(), None, self_departure(10, &[11])),
        ctx_c(11, 1, fused_discard_each(), None, self_departure(11, &[10])),
    ];
    assert!(
        !group_is_order_independent(&state, &dodecapod),
        "Condition-1: mixed-controller Dodecapod discard batch ⇒ Mixed ⇒ STILL PROMPTS \
         (auto would be an unsound under-prompt on the cause-source controller channel)"
    );
}
