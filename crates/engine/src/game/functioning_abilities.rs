//! Single authority for iterating "ability definitions that function right now."
//!
//! Statics, triggers, and replacements each live on `GameObject`s, but they
//! are gated by different CR rules. Every read site that previously
//! iterated `obj.static_definitions` / `obj.trigger_definitions` /
//! `obj.replacement_definitions` directly has to apply these gates itself,
//! which has been a recurring source of bugs. This module centralizes the
//! gating so callers cannot silently drop:
//!
//! - **CR 702.26b** — phased-out permanents' abilities don't function.
//! - **CR 114.4** — objects in the command zone don't function unless they
//!   are emblems.
//! - **CR 604.1 / CR 613.1** — a static ability only applies while its
//!   `condition` evaluates true (continuous re-evaluation).
//!
//! # Zone scope asymmetry
//!
//! - **Statics**: the full CR 113.6 zone-of-function gate lives in the shared
//!   `static_functions_in_zone` predicate (empty `active_zones` defaults to
//!   battlefield-only; a non-empty `active_zones` restricts to exactly the
//!   listed zones; command-zone objects use the emblem-or-opt-in gate). Five
//!   gathers delegate to it — `active_static_definitions` (which also layers
//!   CR 113.6g's stack exception for self-referential
//!   `CantBeCountered`/`CantBeCopied` on top, plus the CR 604.1 / CR 613.1
//!   condition gate), `game_functioning_statics`, `battlefield_functioning_statics`,
//!   `layers::active_combat_assignment_rule_effects_from_static_definitions`,
//!   and `combat::compute_combat_tax`. **`layers::active_continuous_effects_from_static_definitions`
//!   is the one exception** — it keeps its own inline `active_zones`
//!   membership check rather than delegating here, and its off-battlefield
//!   entry point (`active_continuous_effects_from_base_static_source`)
//!   applies a separate caller-side pre-filter
//!   (`base_static_can_source_off_zone_keyword_query`) before reaching it.
//!   That gather does not currently agree with this module's predicate in
//!   every case (notably: it admits a self-referential `affected` definition
//!   regardless of whether `active_zones` declares the object's current
//!   zone, where `static_functions_in_zone` would require the explicit
//!   opt-in per CR 113.6b) — flagged here rather than silently papered over,
//!   since the next person touching either side needs to know they can
//!   diverge. Callers of the five delegating gathers never need to
//!   pre-filter statics by zone; callers of the sixth should read
//!   `layers.rs`'s own comments for its exact rule.
//! - **Triggers**: gated to the battlefield by the caller's choice of
//!   iteration (`battlefield_active_*`). Command-zone emblems pass the
//!   phased-out/command-zone gate for per-object iteration, and non-emblem
//!   command-zone objects may contribute definitions that explicitly opt in
//!   via `trigger_zones`.
//! - **Replacements**: NOT battlefield-scoped. Zone-of-function is a
//!   per-replacement property on `ReplacementDefinition`, so
//!   `active_replacements` scans every object and only applies the
//!   phased-out / command-zone gate. Caller-side zone restriction still
//!   lives in `find_applicable_replacements`, which today filters to
//!   `[Battlefield, Command]` because no in-engine replacement functions
//!   from hand / graveyard / exile. CR 903.9a commander redirection is
//!   handled separately in `zones::move_to_zone` — it is not routed
//!   through `ReplacementDefinition`.
//!
//! # Condition filtering
//!
//! Only `active_static_definitions` filters by `condition`
//! (CR 604.1 / CR 613.1 — statics are evaluated continuously). Trigger
//! intervening-if (CR 603.4) is a two-point check at trigger placement and
//! resolution, and replacement-effect conditions (CR 616) are evaluated at
//! event time. Both of those checks stay at their existing pipeline
//! checkpoints, so these helpers deliberately do NOT filter triggers or
//! replacements by their own `condition` fields.

use crate::game::game_object::GameObject;
use crate::game::layers::evaluate_condition;
use crate::types::ability::{
    ReplacementDefinition, StaticDefinition, TargetFilter, TriggerDefinition, TriggerDefinitionRef,
};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::statics::{StaticMode, StaticModeKind};
use crate::types::zones::Zone;

/// CR 905.4a + CR 113.6b: Face-down hidden-agenda conspiracies do not function
/// even though synthesis stamps their definitions with `Zone::Command`. Other
/// non-emblem command-zone objects keep the existing explicit opt-in path.
fn non_emblem_command_zone_static_functions(obj: &GameObject, def: &StaticDefinition) -> bool {
    if crate::game::conspiracy::is_conspiracy(obj) {
        return crate::game::conspiracy::functions_from_command_zone(obj)
            && static_opts_in_to_command_zone(def);
    }
    static_opts_in_to_command_zone(def)
}

/// CR 905.4a + CR 113.6b: Trigger-side mirror of
/// `non_emblem_command_zone_static_functions`.
pub(crate) fn non_emblem_command_zone_trigger_functions(
    obj: &GameObject,
    def: &TriggerDefinition,
) -> bool {
    if crate::game::conspiracy::is_conspiracy(obj) {
        return crate::game::conspiracy::functions_from_command_zone(obj)
            && trigger_opts_in_to_command_zone(def);
    }
    trigger_opts_in_to_command_zone(def)
}

/// CR 702.26b + CR 114.4: Shared "does this object function at all?" gate.
///
/// CR 702.26b: Phased-out permanents' abilities don't function.
/// CR 114.4: In the command zone, only emblems' abilities function by default.
/// Non-emblem command-zone objects can still contribute individual definitions
/// that explicitly opt in per CR 113.6b. The per-definition `active_zones` /
/// `trigger_zones` overrides are enforced by the static/trigger iterators; this
/// helper only captures the object-level default.
fn object_functions(obj: &GameObject) -> bool {
    if obj.is_phased_out() {
        return false;
    }
    if obj.zone == Zone::Command && !obj.is_emblem {
        return false;
    }
    true
}

/// CR 113.6b + CR 114.4: True when a static on a command-zone object opts in
/// to function from the command zone via its `active_zones` list. Used to
/// extend the CR 114.4 "only emblems function" default with explicit opt-in
/// for Eminence-style abilities ("as long as ~ is in the command zone or on
/// the battlefield"), per CR 113.6b. Phased-out is checked upstream — this
/// helper only encodes the zone/opt-in part of the gate.
pub fn static_opts_in_to_command_zone(def: &StaticDefinition) -> bool {
    def.active_zones.contains(&Zone::Command)
}

/// CR 113.6b + CR 114.4: True when a trigger on a command-zone object opts in
/// to function from the command zone via its `trigger_zones` list. Eminence
/// triggered abilities use this path after the parser derives `Zone::Command`
/// from their intervening-if source-zone condition.
pub fn trigger_opts_in_to_command_zone(def: &TriggerDefinition) -> bool {
    def.trigger_zones.contains(&Zone::Command)
}

/// CR 113.6b + CR 114.4 + CR 311.2 / CR 312.2: object-level command-zone
/// static-effect-source admission. True when this command-zone object
/// contributes at least one static that functions from the command zone: an
/// emblem (CR 114.3/114.4), a face-up conspiracy (CR 905.4), OR any non-emblem
/// object (e.g. an ACTIVE PLANE / phenomenon, which remains in and functions
/// from the command zone per CR 311.2 / CR 312.2) carrying a static that opts
/// in via `active_zones.contains(Command)` (CR 113.6b). This is the single
/// authority consulted by every continuous-effect source gather (the
/// static-source index and the layer gather + its fallback), so a plane's
/// continuous statics (anthems, keyword grants) are visible exactly like an
/// emblem's. `non_emblem_command_zone_static_functions` handles the face-up
/// conspiracy sub-case internally, so emblems, conspiracies, and planes all
/// route through one predicate.
pub fn object_sources_static_from_command_zone(obj: &GameObject) -> bool {
    if obj.zone != Zone::Command {
        return false;
    }
    obj.is_emblem
        || obj
            .static_definitions
            .iter_all()
            .any(|def| non_emblem_command_zone_static_functions(obj, def))
}

/// CR 113.6g: True when a `CantBeCountered`/`CantBeCopied` definition names
/// the object the ability is ON, not some other set of objects. `affected:
/// None` (runtime stamps in `casting.rs` that push a bare, unfiltered
/// definition directly onto the one spell they apply to) and
/// `Some(TargetFilter::SelfRef)` (the printed "this spell can't be countered"
/// self-reference, e.g. Carnage Tyrant) both mean "this ability describes the
/// object it's on." Any other `TargetFilter` (e.g. `Typed`) means the ability
/// instead GRANTS un-counterability / un-copyability to some other set of
/// objects (Allosaurus Shepherd's "Green spells you control can't be
/// countered") and is not self-referential.
pub fn is_self_referential_prohibition(def: &StaticDefinition) -> bool {
    matches!(
        def.mode,
        StaticMode::CantBeCountered | StaticMode::CantBeCopied
    ) && matches!(def.affected.as_ref(), None | Some(TargetFilter::SelfRef))
}

/// CR 113.6 + CR 113.6b + CR 114.3 / CR 114.4: Single-authority zone-of-
/// function gate for a static definition, EXCLUDING the CR 113.6g
/// self-referential `CantBeCountered`/`CantBeCopied` exception (the caller
/// applies that first). Command-zone objects use the emblem-or-opt-in gate;
/// every other zone uses the CR 113.6 default — empty `active_zones`
/// restricts the static to the battlefield, non-empty restricts it to
/// exactly the listed zones. Shared by the statics gathers that delegate to
/// this predicate; `layers::active_continuous_effects_from_static_definitions`
/// remains the documented exception because it applies its own inline
/// `active_zones` gate.
pub(crate) fn static_functions_in_zone(obj: &GameObject, def: &StaticDefinition) -> bool {
    match obj.zone {
        Zone::Command => obj.is_emblem || non_emblem_command_zone_static_functions(obj, def),
        zone => {
            if def.active_zones.is_empty() {
                zone == Zone::Battlefield
            } else {
                def.active_zones.contains(&zone)
            }
        }
    }
}

/// Iterate `StaticDefinition`s on `obj` that are currently functioning, with
/// the CR 702.26b / CR 114.4 gate, the full CR 113.6 zone-of-function gate,
/// and the per-static CR 604.1 / CR 613.1 `condition` gate applied.
///
/// This is the authoritative replacement for `obj.static_definitions.iter_all()`
/// at every read site in the engine.
pub fn active_static_definitions<'a>(
    state: &'a GameState,
    obj: &'a GameObject,
) -> Box<dyn Iterator<Item = &'a StaticDefinition> + 'a> {
    // CR 702.26b: phased-out permanents' abilities never function.
    if obj.is_phased_out() {
        return Box::new(std::iter::empty());
    }
    let source_id = obj.id;
    let controller = obj.controller;
    Box::new(obj.static_definitions.iter_all().filter(move |def| {
        // CR 113.6g: An object's ability that states IT can't be countered
        // or can't be copied functions on the stack — a self-referential
        // exception to the CR 113.6 zone-of-function default below. A
        // permanent's ability that instead GRANTS un-counterability /
        // un-copyability to OTHER objects via a `TargetFilter` (Allosaurus
        // Shepherd's "Green spells you control can't be countered") is not
        // self-referential and must fall through to the ordinary default,
        // so it keeps functioning from the battlefield like any other
        // static. Fixes #1033.
        if def.active_zones.is_empty() && is_self_referential_prohibition(def) {
            if obj.zone != Zone::Stack {
                return false;
            }
        } else if !static_functions_in_zone(obj, def) {
            return false;
        }
        // CR 604.1 / CR 613.1: a static's `condition` must hold for the
        // effect to apply continuously — re-evaluated every time the layers
        // pipeline (or any reader of statics) runs.
        def.condition
            .as_ref()
            .is_none_or(|cond| evaluate_condition(state, cond, controller, source_id))
    }))
}

/// Whole-battlefield iteration of `(source_obj, static_def)` pairs with the
/// full CR gate stack applied. Equivalent to flat-mapping
/// `active_static_definitions` over every battlefield object.
pub fn battlefield_active_statics(
    state: &GameState,
) -> impl Iterator<Item = (&GameObject, &StaticDefinition)> {
    state
        .battlefield
        .iter()
        .filter_map(move |id| state.objects.get(id))
        .flat_map(move |obj| active_static_definitions(state, obj).map(move |def| (obj, def)))
}

/// Game-scope iteration of static abilities that function from normal public
/// sources: battlefield permanents plus command-zone emblems.
pub fn game_active_statics(
    state: &GameState,
) -> impl Iterator<Item = (&GameObject, &StaticDefinition)> {
    state
        .battlefield
        .iter()
        .chain(state.command_zone.iter())
        .filter_map(move |id| state.objects.get(id))
        .flat_map(move |obj| active_static_definitions(state, obj).map(move |def| (obj, def)))
}

/// Game-scope iteration of static abilities that function from public sources,
/// with object-function gates applied but without condition filtering.
///
/// Use this when a caller must evaluate the condition with additional context,
/// such as the affected object for recipient-relative static quantities, or
/// the casting player for cost-modifier scope checks. The CR 113.6 zone-of-
/// function gate is delegated to the shared `static_functions_in_zone`
/// predicate, so this iterator agrees with `active_static_definitions` and the
/// `layers.rs` gathers: command-zone non-emblem objects contribute only their
/// statics that opt in via CR 113.6b `active_zones.contains(Command)`
/// (Eminence), and a battlefield object contributes only statics whose
/// `active_zones` admit the battlefield (empty defaults to battlefield-only).
/// Phased-out objects contribute nothing per CR 702.26b.
///
/// The CR 113.6g self-referential `CantBeCountered`/`CantBeCopied` stack
/// exception is not applied here (that is `active_static_definitions`'
/// responsibility): this iterator only ever scans the battlefield and command
/// zone, never the stack, so the exception cannot apply to any object it sees.
pub fn game_functioning_statics(
    state: &GameState,
) -> impl Iterator<Item = (&GameObject, &StaticDefinition)> {
    state
        .battlefield
        .iter()
        .chain(state.command_zone.iter())
        .filter_map(move |id| state.objects.get(id))
        .filter(|obj| !obj.is_phased_out())
        .flat_map(move |obj| {
            obj.static_definitions
                .iter_all()
                // CR 113.6 + CR 113.6b + CR 114.4: single-authority
                // zone-of-function gate, shared with every other statics
                // gather so they cannot disagree.
                .filter(move |def| static_functions_in_zone(obj, def))
                .map(move |def| (obj, def))
        })
}

/// CR 604.1: loop-invariant existence gate. True iff any currently-functioning
/// static (battlefield permanent or CR 114.4 command-zone emblem; CR 702.26b
/// phased-out excluded) has a mode matching `predicate`. Combat/untap legality
/// loops hoist this ONCE before iterating N permanents so the per-permanent
/// `check_static_ability` re-scan (itself O(N)) is skipped when no such static
/// exists, collapsing O(N^2) to O(N). When one exists the loop falls through to
/// the exact existing per-permanent check, so verdicts are unchanged.
pub fn any_functioning_static_mode(
    state: &GameState,
    predicate: impl Fn(&StaticMode) -> bool,
) -> bool {
    game_functioning_statics(state).any(|(_, def)| predicate(&def.mode))
}

/// O(1) existence query over FUNCTIONING statics: "does any functioning static have
/// mode discriminant `kind`?" Reads the [`GameState::static_mode_presence`] cache, which
/// is rebuilt wholesale from `game_functioning_statics` by the layers pipeline
/// (`layers::refresh_static_mode_presence`), so it has IDENTICAL scoping to
/// `game_functioning_statics`: condition unevaluated (CR 604.1 / CR 613.1 — this is a
/// conservative superset, exactly like `any_functioning_static_mode`); phased-out
/// permanents excluded (CR 702.26b); command-zone statics included per-def opt-in only
/// (CR 114.4 / CR 113.6b). A `true` result is a superset gate — callers MUST fall through
/// to their exact per-object check; a `false` result is precise post-flush and lets the
/// caller short-circuit the O(battlefield) scan. This is the Unit 2/3 migration target for
/// discriminant-only scan gates.
pub fn static_kind_present(state: &GameState, kind: StaticModeKind) -> bool {
    state.static_mode_presence.contains(kind)
}

/// Like `battlefield_active_statics` but WITHOUT condition filtering.
///
/// Applies only the CR 702.26b phased-out gate and the CR 114.4
/// command-zone/emblem gate. Use this when the caller must evaluate a
/// static's `condition` itself under a non-default controller context —
/// e.g., cost-mod statics whose `QuantityComparison` must resolve against
/// the *caster*, not against the source's controller.
///
/// For any other read site, prefer `battlefield_active_statics`, which
/// applies the CR 604.1 / CR 613.1 condition gate on the caller's behalf.
pub fn battlefield_functioning_statics(
    state: &GameState,
) -> impl Iterator<Item = (&GameObject, &StaticDefinition)> {
    state
        .battlefield
        .iter()
        .filter_map(move |id| state.objects.get(id))
        .filter(|obj| object_functions(obj))
        .flat_map(move |obj| {
            obj.static_definitions
                .iter_all()
                // CR 113.6 + CR 113.6b + CR 114.4: single-authority
                // zone-of-function gate, shared with every other statics
                // gather so they cannot disagree. A battlefield object's
                // static restricted to a non-battlefield zone
                // (`active_zones = [Graveyard]`, etc.) does NOT function from
                // the battlefield and must be excluded here too, exactly like
                // `game_functioning_statics` and `active_static_definitions`.
                // Fixes the remaining #1033 sibling-function inconsistency.
                .filter(move |def| static_functions_in_zone(obj, def))
                .map(move |def| (obj, def))
        })
}

/// Battlefield iteration specialised to a particular `StaticMode` shape.
///
/// `extract` pulls the typed payload out of `StaticMode` (replacing the
/// `let StaticMode::X { .. } = &def.mode else { continue };` boilerplate at
/// call sites). Only definitions whose mode matches are yielded, and all CR
/// gating from `active_static_definitions` is applied.
pub fn battlefield_statics_matching<'a, T: 'a>(
    state: &'a GameState,
    extract: fn(&'a StaticMode) -> Option<&'a T>,
) -> impl Iterator<Item = (&'a GameObject, &'a StaticDefinition, &'a T)> {
    battlefield_active_statics(state)
        .filter_map(move |(obj, def)| extract(&def.mode).map(|payload| (obj, def, payload)))
}

/// A functioning live definition plus compatibility-only display metadata.
#[derive(Debug, Clone)]
pub struct ActiveTriggerDefinition<'a> {
    pub live_index: usize,
    pub definition_ref: TriggerDefinitionRef,
    pub definition: &'a TriggerDefinition,
}

/// Iterate identity-bearing `TriggerDefinition`s on `obj` with the CR 702.26b
/// / CR 114.4 gate applied. `live_index` is presentation metadata only; every
/// runtime identity consumer must use `definition_ref`.
///
/// CR 603.4 intervening-if is deliberately NOT filtered here — it is a
/// two-point check (at placement and at resolution) handled by the trigger
/// pipeline. Helper consumers still need that check at those checkpoints.
pub fn active_trigger_definitions<'a>(
    state: &'a GameState,
    obj: &'a GameObject,
) -> Box<dyn Iterator<Item = ActiveTriggerDefinition<'a>> + 'a> {
    // CR 800.4a: objects owned by a player who left the game have left with
    // that player. They remain serialized in the Exile zone for the terminal
    // game snapshot, but cannot continue contributing trigger occurrences.
    if !crate::game::players::is_alive(state, obj.owner) {
        return Box::new(std::iter::empty());
    }
    if obj.is_phased_out() {
        return Box::new(std::iter::empty());
    }
    let zone = obj.zone;
    let is_emblem = obj.is_emblem;
    let source = ObjectIncarnationRef::from_object(obj);
    Box::new(
        obj.trigger_definitions
            .iter_all()
            .enumerate()
            .filter(move |(_, entry)| {
                if zone == Zone::Command && !is_emblem {
                    return non_emblem_command_zone_trigger_functions(obj, entry.definition());
                }
                true
            })
            .map(move |(live_index, entry)| ActiveTriggerDefinition {
                live_index,
                definition_ref: TriggerDefinitionRef {
                    source,
                    occurrence: entry.occurrence.clone(),
                },
                definition: entry.definition(),
            }),
    )
}

/// Whole-battlefield iteration of `(index, source_obj, trigger_def)`
/// triples. The index is stable against the source object's
/// `trigger_definitions` so callers can round-trip to a `TriggerId`.
pub fn battlefield_active_triggers(
    state: &GameState,
) -> impl Iterator<Item = (&GameObject, ActiveTriggerDefinition<'_>)> {
    state
        .battlefield
        .iter()
        .filter_map(move |id| state.objects.get(id))
        .flat_map(move |obj| {
            active_trigger_definitions(state, obj).map(move |active| (obj, active))
        })
}

/// All-zones iteration of `(index, source_obj, replacement_def)` triples
/// with the CR 702.26b / CR 114.4 gate applied.
///
/// This is deliberately NOT battlefield-scoped — zone-of-function is a
/// per-replacement property governed by each `ReplacementDefinition`'s
/// own `destination_zone` / `valid_card` metadata. The helper only
/// enforces the shared phased-out / command-zone gate. CR 616 event-
/// time evaluation remains in the replacement pipeline itself.
///
/// Zones callers actually scan today:
/// - `find_applicable_replacements` in `game/replacement.rs` restricts
///   to `[Battlefield, Command]` plus the entering card (CR 614.12
///   self-replacement on ETB) or the discarded card (CR 702.35a
///   Madness self-replacement from hand).
/// - **CR 903.9a commander redirection** is not routed through
///   `ReplacementDefinition` at all; it is a hard-coded redirect in
///   `game/zones.rs::move_to_zone`. The helper's scan is future-proofed
///   for per-replacement zones but no current caller needs it.
pub fn active_replacements(
    state: &GameState,
) -> impl Iterator<Item = (usize, &GameObject, &ReplacementDefinition)> {
    state.objects.values().flat_map(move |obj| {
        // Phased-out / command-zone gate still applies even though
        // replacements are not battlefield-scoped.
        let functioning = object_functions(obj);
        obj.replacement_definitions
            .iter_all()
            .enumerate()
            .filter(move |_| functioning)
            .map(move |(idx, def)| (idx, obj, def))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ReplacementDefinition, StaticCondition, StaticDefinition, TriggerDefinition, TypedFilter,
    };
    use crate::types::format::FormatConfig;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;

    fn new_state() -> GameState {
        GameState::new(FormatConfig::standard(), 2, 0)
    }

    fn put_on_battlefield(state: &mut GameState, obj: GameObject) -> ObjectId {
        let id = obj.id;
        state.objects.insert(id, obj);
        state.battlefield.push_back(id);
        id
    }

    fn make_obj(id: u64, zone: Zone) -> GameObject {
        GameObject::new(
            ObjectId(id),
            CardId(id),
            PlayerId(0),
            format!("TestObj{id}"),
            zone,
        )
    }

    /// CR 113.6b + CR 311.2: a non-emblem command-zone object (active plane) is
    /// admitted as a static-effect source ONLY when it carries a static that
    /// opts into the command zone via `active_zones.contains(Command)`. A
    /// battlefield-default (empty `active_zones`) static on such an object is NOT
    /// admitted — validates the admission helper, the level synthesis stamps at.
    #[test]
    fn object_sources_static_from_command_zone_requires_command_optin() {
        // Command-zone object with a Command-stamped continuous static → admitted.
        let mut plane = make_obj(1, Zone::Command);
        plane.static_definitions =
            vec![StaticDefinition::new(StaticMode::Continuous).active_zones(vec![Zone::Command])]
                .into();
        assert!(object_sources_static_from_command_zone(&plane));

        // Same object, but the static defaults to the battlefield (empty
        // active_zones) → NOT admitted (a stray battlefield static can't leak).
        let mut battlefield_default = make_obj(2, Zone::Command);
        battlefield_default.static_definitions =
            vec![StaticDefinition::new(StaticMode::Continuous)].into();
        assert!(!object_sources_static_from_command_zone(
            &battlefield_default
        ));

        // An emblem in the command zone is always admitted.
        let mut emblem = make_obj(3, Zone::Command);
        emblem.is_emblem = true;
        assert!(object_sources_static_from_command_zone(&emblem));

        // A battlefield object is never admitted through THIS command-zone gate.
        let mut bf = make_obj(4, Zone::Battlefield);
        bf.static_definitions =
            vec![StaticDefinition::new(StaticMode::Continuous).active_zones(vec![Zone::Command])]
                .into();
        assert!(!object_sources_static_from_command_zone(&bf));
    }

    #[test]
    fn phased_out_object_returns_no_active_statics() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        obj.phase_status = crate::game::game_object::PhaseStatus::PhasedOut {
            cause: crate::game::game_object::PhaseOutCause::Directly,
        };
        assert_eq!(active_static_definitions(&state, &obj).count(), 0);
    }

    #[test]
    fn phased_out_object_returns_no_active_triggers() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.trigger_definitions = vec![TriggerDefinition::new(TriggerMode::ChangesZone)].into();
        obj.phase_status = crate::game::game_object::PhaseStatus::PhasedOut {
            cause: crate::game::game_object::PhaseOutCause::Directly,
        };
        assert_eq!(active_trigger_definitions(&state, &obj).count(), 0);
    }

    #[test]
    fn eliminated_owner_returns_no_active_triggers() {
        let mut state = new_state();
        state.players[0].is_eliminated = true;
        let mut obj = make_obj(1, Zone::Exile);
        obj.trigger_definitions =
            vec![TriggerDefinition::new(TriggerMode::SpellCast).trigger_zones(vec![Zone::Exile])]
                .into();

        assert_eq!(active_trigger_definitions(&state, &obj).count(), 0);
    }

    #[test]
    fn phased_out_object_returns_no_active_replacements() {
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::DamageDone)].into();
        obj.phase_status = crate::game::game_object::PhaseStatus::PhasedOut {
            cause: crate::game::game_object::PhaseOutCause::Directly,
        };
        put_on_battlefield(&mut state, obj);
        assert_eq!(active_replacements(&state).count(), 0);
    }

    #[test]
    fn command_zone_non_emblem_returns_no_active_statics() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Command);
        obj.is_emblem = false;
        obj.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        assert_eq!(active_static_definitions(&state, &obj).count(), 0);
    }

    #[test]
    fn command_zone_emblem_returns_active_statics() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Command);
        obj.is_emblem = true;
        obj.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        assert_eq!(active_static_definitions(&state, &obj).count(), 1);
    }

    /// CR 113.6b: A static on a non-emblem command-zone object functions
    /// when (and only when) it lists `Zone::Command` in its `active_zones`.
    /// Eminence statics opt in this way; sibling statics on the same
    /// commander that DO NOT opt in remain blocked by CR 114.4.
    #[test]
    fn command_zone_non_emblem_yields_only_active_zone_opt_in_statics() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Command);
        obj.is_emblem = false;
        // Two statics: one default (battlefield-only), one with explicit
        // command-zone opt-in via active_zones.
        let battlefield_only = StaticDefinition::new(StaticMode::Continuous);
        let eminence_optin = StaticDefinition::new(StaticMode::Continuous)
            .active_zones(vec![Zone::Battlefield, Zone::Command]);
        obj.static_definitions = vec![battlefield_only, eminence_optin].into();
        // Only the opt-in static survives the command-zone gate per CR 113.6b.
        assert_eq!(active_static_definitions(&state, &obj).count(), 1);
    }

    /// CR 113.6b: A trigger on a non-emblem command-zone object functions when
    /// it lists `Zone::Command` in `trigger_zones`. This is the trigger-side
    /// counterpart to Eminence statics' `active_zones` opt-in.
    #[test]
    fn command_zone_non_emblem_yields_only_trigger_zone_opt_in_triggers() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Command);
        obj.is_emblem = false;
        let battlefield_only = TriggerDefinition::new(TriggerMode::ChangesZone);
        let eminence_optin =
            TriggerDefinition::new(TriggerMode::SpellCast).trigger_zones(vec![Zone::Command]);
        obj.trigger_definitions = vec![battlefield_only, eminence_optin].into();

        let triggers: Vec<_> = active_trigger_definitions(&state, &obj).collect();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].live_index, 1);
        assert!(triggers[0]
            .definition
            .trigger_zones
            .contains(&Zone::Command));
    }

    /// Symmetric coverage for the cost-mod / "without condition filtering"
    /// iterator: a non-emblem command-zone object must contribute only its
    /// `active_zones.contains(Command)` statics to `game_functioning_statics`.
    #[test]
    fn game_functioning_statics_command_zone_non_emblem_requires_opt_in() {
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Command);
        obj.is_emblem = false;
        let battlefield_only = StaticDefinition::new(StaticMode::Continuous);
        let eminence_optin = StaticDefinition::new(StaticMode::Continuous)
            .active_zones(vec![Zone::Battlefield, Zone::Command]);
        obj.static_definitions = vec![battlefield_only, eminence_optin].into();
        state.command_zone.push_back(obj.id);
        state.objects.insert(obj.id, obj);
        // Only the opt-in static appears in the global iterator.
        let pairs: Vec<_> = game_functioning_statics(&state)
            .filter(|(obj, _)| obj.id == ObjectId(1))
            .collect();
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].1.active_zones.contains(&Zone::Command));
    }

    /// CR 113.6: `game_functioning_statics` must apply the shared zone-of-
    /// function gate to BATTLEFIELD objects too, not just command-zone ones.
    /// A battlefield object carrying a static restricted to a non-battlefield
    /// zone (`active_zones = [Graveyard]`) does NOT function from the
    /// battlefield, so it must be excluded — while a sibling static with empty
    /// `active_zones` (battlefield default) on the same object is still
    /// yielded. Pre-migration `game_functioning_statics` returned `true`
    /// unconditionally for any non-command object, leaking the graveyard-only
    /// static; this asserts the migration to `static_functions_in_zone`
    /// closed that gap in lockstep with `active_static_definitions`. Fixes the
    /// remaining #1033 sibling-function inconsistency.
    #[test]
    fn game_functioning_statics_battlefield_respects_active_zones() {
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        // Graveyard-only static: must NOT function from the battlefield.
        let graveyard_only =
            StaticDefinition::new(StaticMode::Continuous).active_zones(vec![Zone::Graveyard]);
        // Battlefield-default static (empty active_zones): must function.
        let battlefield_default = StaticDefinition::new(StaticMode::Continuous);
        obj.static_definitions = vec![graveyard_only, battlefield_default].into();
        put_on_battlefield(&mut state, obj);

        let pairs: Vec<_> = game_functioning_statics(&state)
            .filter(|(obj, _)| obj.id == ObjectId(1))
            .collect();
        // Only the battlefield-default static survives the zone gate.
        assert_eq!(
            pairs.len(),
            1,
            "graveyard-only static must be excluded from a battlefield object"
        );
        assert!(
            pairs[0].1.active_zones.is_empty(),
            "the surviving static must be the battlefield-default one"
        );
    }

    /// CR 113.6: general zone-of-function regression at the helper level
    /// (the Underworld-Breach-class bug, independent of the full
    /// cast-eligibility integration test in the PR). A Graveyard-zone object
    /// with a `Continuous` static and empty `active_zones` functions only from
    /// the battlefield by default, so `active_static_definitions` yields
    /// nothing for it. Pre-fix the missing gate let it leak. Fixes #1033.
    #[test]
    fn graveyard_continuous_static_with_empty_active_zones_does_not_function() {
        let state = new_state();
        let mut obj = make_obj(1, Zone::Graveyard);
        obj.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        assert_eq!(active_static_definitions(&state, &obj).count(), 0);
    }

    /// CR 113.6g: A self-referential `CantBeCountered` (Carnage Tyrant's "This
    /// spell can't be countered", modeled with `affected: Some(SelfRef)`)
    /// functions from the stack and NOT from the battlefield.
    #[test]
    fn cant_be_countered_self_ref_functions_on_stack_only() {
        let state = new_state();
        let def =
            StaticDefinition::new(StaticMode::CantBeCountered).affected(TargetFilter::SelfRef);
        // Case 1: on the stack → functions (Carnage-Tyrant shape).
        let mut on_stack = make_obj(1, Zone::Stack);
        on_stack.static_definitions = vec![def.clone()].into();
        assert_eq!(active_static_definitions(&state, &on_stack).count(), 1);
        // Case 2: on the battlefield → does NOT function.
        let mut on_bf = make_obj(2, Zone::Battlefield);
        on_bf.static_definitions = vec![def].into();
        assert_eq!(active_static_definitions(&state, &on_bf).count(), 0);
    }

    /// CR 113.6g: The `casting.rs` bare-stamp shape — a `CantBeCountered`
    /// definition with `affected: None` pushed directly onto the one spell it
    /// applies to. Distinct code path from the `Some(SelfRef)` case
    /// (`None != Some(SelfRef)`), so it needs its own fixture. Same behavior:
    /// functions from the stack, not the battlefield.
    #[test]
    fn cant_be_countered_bare_stamp_functions_on_stack_only() {
        let state = new_state();
        // affected: None — the runtime bare-stamp shape.
        let def = StaticDefinition::new(StaticMode::CantBeCountered);
        // Case 3: on the stack → functions.
        let mut on_stack = make_obj(1, Zone::Stack);
        on_stack.static_definitions = vec![def.clone()].into();
        assert_eq!(active_static_definitions(&state, &on_stack).count(), 1);
        // Case 4: on the battlefield → does NOT function.
        let mut on_bf = make_obj(2, Zone::Battlefield);
        on_bf.static_definitions = vec![def].into();
        assert_eq!(active_static_definitions(&state, &on_bf).count(), 0);
    }

    /// CR 113.6g: Blocker-4 fixture — Allosaurus Shepherd carries BOTH a
    /// self-referential `CantBeCountered` line (`affected: Some(SelfRef)`) and
    /// a granting line that makes OTHER objects un-counterable
    /// (`affected: Some(Typed(...))`, "Green spells you control can't be
    /// countered"). The two co-resident definitions must be gated
    /// independently per-definition, not per-object or per-mode: on the stack
    /// only the self-referential def functions; on the battlefield only the
    /// granting def functions.
    #[test]
    fn cant_be_countered_self_ref_and_granting_def_gated_independently() {
        let state = new_state();
        let self_ref_def =
            StaticDefinition::new(StaticMode::CantBeCountered).affected(TargetFilter::SelfRef);
        let granting_def = StaticDefinition::new(StaticMode::CantBeCountered)
            .affected(TargetFilter::Typed(TypedFilter::creature()));
        // On the stack: only the self-referential def functions.
        let mut on_stack = make_obj(1, Zone::Stack);
        on_stack.static_definitions = vec![self_ref_def.clone(), granting_def.clone()].into();
        let stack_defs: Vec<_> = active_static_definitions(&state, &on_stack).collect();
        assert_eq!(stack_defs.len(), 1);
        assert!(matches!(
            stack_defs[0].affected,
            Some(TargetFilter::SelfRef)
        ));
        // On the battlefield: only the granting (Typed) def functions — it
        // grants un-counterability to other objects like any battlefield static.
        let mut on_bf = make_obj(2, Zone::Battlefield);
        on_bf.static_definitions = vec![self_ref_def, granting_def].into();
        let bf_defs: Vec<_> = active_static_definitions(&state, &on_bf).collect();
        assert_eq!(bf_defs.len(), 1);
        assert!(matches!(bf_defs[0].affected, Some(TargetFilter::Typed(_))));
    }

    /// CR 113.6g: Mode-symmetry check — the stack exception is not
    /// counter-specific. A self-referential `CantBeCopied` functions from the
    /// stack and not from the battlefield, exactly like `CantBeCountered`.
    #[test]
    fn cant_be_copied_self_ref_functions_on_stack_only() {
        let state = new_state();
        let def = StaticDefinition::new(StaticMode::CantBeCopied).affected(TargetFilter::SelfRef);
        // Case 6a: on the stack → functions.
        let mut on_stack = make_obj(1, Zone::Stack);
        on_stack.static_definitions = vec![def.clone()].into();
        assert_eq!(active_static_definitions(&state, &on_stack).count(), 1);
        // Case 6b: on the battlefield → does NOT function.
        let mut on_bf = make_obj(2, Zone::Battlefield);
        on_bf.static_definitions = vec![def].into();
        assert_eq!(active_static_definitions(&state, &on_bf).count(), 0);
    }

    #[test]
    fn condition_false_static_is_filtered() {
        // IsMonarch evaluates false when state.monarch is None (default).
        let state = new_state();
        assert!(state.monarch.is_none());
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![
            StaticDefinition::new(StaticMode::Continuous).condition(StaticCondition::IsMonarch)
        ]
        .into();
        assert_eq!(active_static_definitions(&state, &obj).count(), 0);
    }

    #[test]
    fn condition_true_static_is_yielded() {
        let mut state = new_state();
        state.monarch = Some(PlayerId(0));
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![
            StaticDefinition::new(StaticMode::Continuous).condition(StaticCondition::IsMonarch)
        ]
        .into();
        assert_eq!(active_static_definitions(&state, &obj).count(), 1);
    }

    #[test]
    fn trigger_with_false_condition_is_not_filtered_by_helper() {
        // CR 603.4 intervening-if is checked at placement/resolution, NOT
        // at iteration. The helper must yield the trigger regardless of its
        // `condition` field so the pipeline can decide.
        let state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        let trig = TriggerDefinition {
            condition: Some(crate::types::ability::TriggerCondition::IsMonarch),
            ..TriggerDefinition::new(TriggerMode::ChangesZone)
        };
        obj.trigger_definitions = vec![trig].into();
        // Helper yields it despite controller not being monarch.
        assert_eq!(active_trigger_definitions(&state, &obj).count(), 1);
    }

    #[test]
    fn replacement_with_condition_is_not_filtered_by_helper() {
        // CR 616 event-time evaluation of replacement `condition` stays in
        // the replacement pipeline; this helper does not filter on it.
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        let repl = ReplacementDefinition {
            condition: Some(crate::types::ability::ReplacementCondition::UnlessMultipleOpponents),
            ..ReplacementDefinition::new(ReplacementEvent::DamageDone)
        };
        obj.replacement_definitions = vec![repl].into();
        put_on_battlefield(&mut state, obj);
        assert_eq!(active_replacements(&state).count(), 1);
    }

    #[test]
    fn battlefield_active_statics_scans_all_battlefield_objects_with_gating() {
        let mut state = new_state();
        let mut a = make_obj(1, Zone::Battlefield);
        a.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        let mut b = make_obj(2, Zone::Battlefield);
        b.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        b.phase_status = crate::game::game_object::PhaseStatus::PhasedOut {
            cause: crate::game::game_object::PhaseOutCause::Directly,
        };
        let mut c = make_obj(3, Zone::Battlefield);
        c.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        put_on_battlefield(&mut state, a);
        put_on_battlefield(&mut state, b);
        put_on_battlefield(&mut state, c);
        let pairs: Vec<_> = battlefield_active_statics(&state).collect();
        assert_eq!(pairs.len(), 2);
        let ids: Vec<u64> = pairs.iter().map(|(obj, _)| obj.id.0).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
        assert!(!ids.contains(&2));
    }

    #[test]
    fn active_replacements_includes_graveyard_and_exile_objects() {
        // CR 903.9: commander-zone / graveyard / exile replacements must be
        // visible to the iterator — replacements are not battlefield-scoped.
        let mut state = new_state();
        let mut gy = make_obj(1, Zone::Graveyard);
        gy.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::DamageDone)].into();
        let mut ex = make_obj(2, Zone::Exile);
        ex.replacement_definitions =
            vec![ReplacementDefinition::new(ReplacementEvent::DamageDone)].into();
        state.objects.insert(gy.id, gy);
        state.objects.insert(ex.id, ex);
        let ids: Vec<u64> = active_replacements(&state)
            .map(|(_, obj, _)| obj.id.0)
            .collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    // The phased-out Azusa test stays here because
    // `additional_land_drops` is a direct caller of the helper — the
    // assertion runs through a real consumer, not the helper itself.
    // The analogous caller-level tests for Torpor Orb (triggers), Grafdigger's
    // Cage (zones::move_to_zone), command-zone-commander-triggers (triggers),
    // and false-condition anthem (layers) live in their respective modules'
    // #[cfg(test)] blocks where they drive the real pipeline.

    fn phase_out_by_id(state: &mut GameState, id: ObjectId) {
        let mut events = Vec::new();
        crate::game::phasing::phase_out_object(
            state,
            id,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut events,
        );
    }

    #[test]
    fn phased_out_azusa_does_not_grant_extra_land_drops() {
        let mut state = new_state();
        let mut azusa = make_obj(1, Zone::Battlefield);
        azusa.static_definitions = vec![StaticDefinition::new(StaticMode::AdditionalLandDrop {
            count: 2,
        })]
        .into();
        let id = put_on_battlefield(&mut state, azusa);
        phase_out_by_id(&mut state, id);
        // additional_land_drops now routes through battlefield_active_statics
        // so phased-out Azusa contributes zero.
        let drops = crate::game::static_abilities::additional_land_drops(&state, PlayerId(0));
        assert_eq!(
            drops, 0,
            "Phased-out Azusa must not grant any extra land drops"
        );
    }

    #[test]
    fn battlefield_functioning_statics_does_not_filter_condition() {
        // `battlefield_functioning_statics` applies only the phased-out /
        // command-zone gate. A static with a false `condition` must still be
        // yielded so callers (e.g. cost-mod) can re-evaluate it under their
        // own controller context — whereas `battlefield_active_statics` drops
        // it per CR 604.1 / CR 613.1.
        let mut state = new_state();
        assert!(state.monarch.is_none());
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![
            StaticDefinition::new(StaticMode::Continuous).condition(StaticCondition::IsMonarch)
        ]
        .into();
        put_on_battlefield(&mut state, obj);

        assert_eq!(
            battlefield_functioning_statics(&state).count(),
            1,
            "functioning-only iterator must yield the false-condition static"
        );
        assert_eq!(
            battlefield_active_statics(&state).count(),
            0,
            "condition-gated iterator must drop the false-condition static"
        );
    }

    /// CR 113.6 + CR 113.6b: `battlefield_functioning_statics` must apply the
    /// shared zone-of-function gate per-definition, not just the object-level
    /// `object_functions` gate. A battlefield object carrying a static
    /// restricted to a non-battlefield zone (`active_zones = [Graveyard]`) does
    /// NOT function from the battlefield, so it must be excluded — while a
    /// sibling static with empty `active_zones` (battlefield default) on the
    /// same object is still yielded (the positive reach-guard proving the
    /// negative is not vacuous). Pre-migration this iterator yielded
    /// `obj.static_definitions.iter_all()` completely unfiltered, leaking the
    /// graveyard-only static into all four downstream consumers
    /// (`collect_block_restriction_statics`, `collect_must_be_blocked_statics`,
    /// `apply_cost_floor_inner`, `apply_cant_have_keyword_denials`). This
    /// asserts the migration to `static_functions_in_zone` closed that gap in
    /// lockstep with `game_functioning_statics` and `active_static_definitions`.
    /// Fixes the remaining #1033 sibling-function inconsistency.
    #[test]
    fn battlefield_functioning_statics_respects_active_zones() {
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        // Graveyard-only static: must NOT function from the battlefield.
        let graveyard_only =
            StaticDefinition::new(StaticMode::Continuous).active_zones(vec![Zone::Graveyard]);
        // Battlefield-default static (empty active_zones): must function
        // (positive reach-guard so the negative assertion is not vacuous).
        let battlefield_default = StaticDefinition::new(StaticMode::Continuous);
        obj.static_definitions = vec![graveyard_only, battlefield_default].into();
        put_on_battlefield(&mut state, obj);

        let pairs: Vec<_> = battlefield_functioning_statics(&state)
            .filter(|(obj, _)| obj.id == ObjectId(1))
            .collect();
        // Only the battlefield-default static survives the zone gate.
        assert_eq!(
            pairs.len(),
            1,
            "graveyard-only static must be excluded from a battlefield object"
        );
        assert!(
            pairs[0].1.active_zones.is_empty(),
            "the surviving static must be the battlefield-default one"
        );
    }

    #[test]
    fn battlefield_functioning_statics_still_filters_phased_out() {
        let mut state = new_state();
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![StaticDefinition::new(StaticMode::Continuous)].into();
        obj.phase_status = crate::game::game_object::PhaseStatus::PhasedOut {
            cause: crate::game::game_object::PhaseOutCause::Directly,
        };
        put_on_battlefield(&mut state, obj);
        assert_eq!(battlefield_functioning_statics(&state).count(), 0);
    }

    #[test]
    fn condition_false_static_does_not_apply() {
        // CR 604.1 / CR 613.1: A static whose `condition` evaluates false is
        // filtered out by the helper — verified end-to-end with a condition
        // that is false by default (IsMonarch when no monarch).
        let state = new_state();
        assert!(state.monarch.is_none());
        let mut obj = make_obj(1, Zone::Battlefield);
        obj.static_definitions = vec![
            StaticDefinition::new(StaticMode::Continuous).condition(StaticCondition::IsMonarch)
        ]
        .into();
        assert_eq!(
            active_static_definitions(&state, &obj).count(),
            0,
            "Static with false condition must not be yielded"
        );
    }
}
