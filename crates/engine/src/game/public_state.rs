use crate::types::ability::{Effect, EffectKind, QuantityExpr, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PublicStateDirty, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::derived::derive_display_state;
use super::layers::flush_layers;
use super::turn_control;

/// Finalize rules-visible game state at an engine action boundary.
///
/// This is the single authoritative place that synchronizes `priority_player`
/// from `waiting_for` and evaluates layers when dirty. Display-only derivation
/// is split into [`finalize_display_state`] so engine-owned fast-forward loops
/// can keep rules state current while batching expensive display recomputes.
pub fn finalize_rules_state(state: &mut GameState) {
    // CR 614.12a + CR 615.5: Backward-compat for the 2026-05-09 audit M4
    // post-replacement-continuation slot fold. Idempotent on already-migrated
    // states; cheap on every other invocation.
    state.migrate_post_replacement_continuation();
    normalize_legacy_attach_waiting_for(state);
    sync_priority_player_from_waiting_for(state);
    flush_layers(state);
}

/// Finalize display-only cached state before exposing state to consumers.
pub fn finalize_display_state(state: &mut GameState) {
    derive_display_state(state);
    clear_public_state_dirty(state);
}

/// Finalize outward-facing game state before it leaves the engine boundary.
pub fn finalize_public_state(state: &mut GameState) {
    finalize_rules_state(state);
    finalize_display_state(state);
}

pub fn sync_waiting_for(state: &mut GameState, waiting_for: &WaitingFor) {
    state.waiting_for = waiting_for.clone();
    normalize_legacy_attach_waiting_for(state);
    sync_priority_player_from_waiting_for(state);
}

fn normalize_legacy_attach_waiting_for(state: &mut GameState) {
    let WaitingFor::EffectZoneChoice {
        effect_kind: EffectKind::Attach,
        count: 1,
        min_count: 1,
        up_to: false,
        cards,
        ..
    } = &state.waiting_for
    else {
        return;
    };

    let Some(cont) = state.pending_continuation.as_ref() else {
        return;
    };
    if !matches!(&cont.chain.effect, Effect::Attach { .. }) || !cont.chain.targeting_is_optional() {
        return;
    }

    let normalized_count = cont
        .chain
        .multi_target
        .as_ref()
        .map(|spec| match &spec.max {
            Some(QuantityExpr::Fixed { value }) if *value >= 0 => *value as usize,
            Some(_) | None => cards.len(),
        })
        .unwrap_or(1)
        .min(cards.len().max(1));

    if let WaitingFor::EffectZoneChoice {
        count,
        min_count,
        up_to,
        ..
    } = &mut state.waiting_for
    {
        *count = normalized_count;
        *min_count = 0;
        *up_to = true;
    }
}

fn sync_priority_player_from_waiting_for(state: &mut GameState) {
    if let Some(player) = state.waiting_for.acting_player() {
        state.priority_player = turn_control::authorized_submitter_for_player(state, player);
    }
}

pub fn mark_public_state_all_dirty(state: &mut GameState) {
    state.public_state_dirty = PublicStateDirty::all_dirty();
}

pub fn mark_public_state_object_dirty(state: &mut GameState, object_id: ObjectId) {
    if !state.public_state_dirty.all_objects_dirty {
        state.public_state_dirty.dirty_objects.insert(object_id);
    }
}

pub fn mark_public_state_player_dirty(state: &mut GameState, player_id: PlayerId) {
    if !state.public_state_dirty.all_players_dirty {
        state.public_state_dirty.dirty_players.insert(player_id);
    }
}

pub fn mark_battlefield_display_dirty(state: &mut GameState) {
    state.public_state_dirty.battlefield_display_dirty = true;
}

pub fn mark_mana_display_dirty(state: &mut GameState) {
    state.public_state_dirty.mana_display_dirty = true;
}

/// Mark `object_id` dirty and, if it is a battlefield permanent whose mana
/// availability can be displayed (a land, or any permanent with a mana
/// ability), also raise the mana-display signal.
///
/// Mana availability (`has_mana_ability` / `mana_ability_index` /
/// `available_mana_pips`) is BOARD-GLOBAL: a non-mana event on a mana source
/// (Gemstone Mine depletion `CounterRemoved`, damage to a creature-land) can
/// change its displayed mana state, and `derive_display_state` recomputes it
/// only under the mana gate. Raising the mana signal HERE — and only for
/// genuine mana sources — keeps a creature-token entry (no mana ability) from
/// triggering the board-wide auto-tap sweep, preserving the token-storm hot
/// path while fixing the under-mark for mana lands/dorks.
fn mark_object_dirty_with_mana(state: &mut GameState, object_id: ObjectId) {
    mark_public_state_object_dirty(state, object_id);
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Battlefield
            && (obj.card_types.core_types.contains(&CoreType::Land)
                || obj
                    .abilities
                    .iter()
                    .any(super::mana_abilities::is_mana_ability))
        {
            mark_mana_display_dirty(state);
        }
    }
}

/// Translate an action's emitted events into the minimal set of public-state
/// dirty marks needed for `derive_display_state` / `evaluate_layers` to produce
/// output IDENTICAL to a full all-dirty recompute.
///
/// CONSERVATIVE BY CONSTRUCTION: any event that could ripple board-wide via a
/// continuous/static effect (CR 611.1, CR 613.1) escalates to all-dirty and
/// returns. Under-marking is a correctness bug (stale displayed P/T, summoning
/// sickness, mana availability); over-marking only costs time. When in doubt,
/// all-dirty.
///
/// The `match` over `GameEvent` is intentionally wildcard-free: a future event
/// variant must be classified here or the engine will not compile, preventing
/// silent under-marking.
pub fn mark_public_state_from_events(state: &mut GameState, events: &[GameEvent]) {
    // GATE 1 — static/continuous ripple.
    // CR 611.1 + CR 613.1: A continuous effect can modify any object's
    // characteristics across the whole board; if a FULL layer re-evaluation is
    // pending this action, display state for the entire board is potentially
    // stale, so recompute all of it. This covers anthems, type-changers,
    // keyword-granters, CDAs, control changes, P/T setters, and every transient
    // continuous effect — they all request a full layer flush.
    //
    // An `EnteredObjects` request is an incremental layer flush that only
    // re-derives the entering objects (see `flush_layers`); it does NOT ripple
    // board-wide, so it falls through to GATE 2. `finalize_public_state` runs
    // `flush_layers` immediately after this function, and the incremental path
    // marks each entered object (and the battlefield display) dirty itself, so
    // the entered objects' display is recomputed. The entry's own
    // `ZoneChanged` event is also handled by GATE 2 below.
    match &state.layers_dirty {
        crate::types::game_state::LayersDirty::Full => {
            mark_public_state_all_dirty(state);
            return;
        }
        crate::types::game_state::LayersDirty::Clean
        | crate::types::game_state::LayersDirty::EnteredObjects(_) => {}
    }

    // GATE 2 — per-event targeted marking. `layers_dirty` was NOT set, so no
    // continuous effect changed board-wide; only the objects/players named by
    // these events can have stale display fields.
    for event in events {
        match event {
            GameEvent::ZoneChanged {
                object_id, to, from, ..
            } => {
                mark_object_dirty_with_mana(state, *object_id);
                if *to == Zone::Battlefield || *from == Some(Zone::Battlefield) {
                    mark_battlefield_display_dirty(state);
                }
                // A mana source leaving the battlefield changes the pool/tap
                // simulation for the SIBLING lands that remain, so re-sweep mana
                // availability. Gated on the leaving object's type (it is now off
                // the battlefield, so `mark_object_dirty_with_mana`'s zone check
                // cannot catch it) — a creature leaving never raises this, so the
                // token-storm + sacrifice path is unaffected.
                if *from == Some(Zone::Battlefield) {
                    if let Some(obj) = state.objects.get(object_id) {
                        if obj.card_types.core_types.contains(&CoreType::Land)
                            || obj
                                .abilities
                                .iter()
                                .any(super::mana_abilities::is_mana_ability)
                        {
                            mark_mana_display_dirty(state);
                        }
                    }
                }
            }
            GameEvent::TokenCreated { object_id, .. }
            | GameEvent::ObjectConjured { object_id, .. } => {
                // Mana-aware: a creature token (Scute storm) does NOT raise the
                // mana signal, so the board-wide auto-tap sweep is skipped; a
                // Treasure/land token does raise it.
                mark_object_dirty_with_mana(state, *object_id);
                mark_battlefield_display_dirty(state);
            }
            GameEvent::PermanentTapped {
                object_id,
                caused_by,
            } => {
                mark_public_state_object_dirty(state, *object_id);
                if let Some(cause) = caused_by {
                    mark_public_state_object_dirty(state, *cause);
                }
                mark_mana_display_dirty(state);
            }
            GameEvent::PermanentUntapped { object_id } => {
                mark_public_state_object_dirty(state, *object_id);
                mark_mana_display_dirty(state);
            }
            // CR 701.43a: exerting adds a CantUntap transient (which sets
            // layers_dirty → Gate 1); mark the exerted object directly so its
            // display reflects the exert even on the layers-clean path.
            GameEvent::CreatureExerted { object_id }
            | GameEvent::Foretold { object_id, .. }
            | GameEvent::BecameForetold { object_id } => {
                mark_public_state_object_dirty(state, *object_id);
            }
            GameEvent::CreatureEnlisted {
                attacker, tapped, ..
            } => {
                mark_public_state_object_dirty(state, *attacker);
                mark_public_state_object_dirty(state, *tapped);
            }
            GameEvent::ManaAdded { player_id, .. }
            | GameEvent::ManaPoolEmptied { player_id, .. }
            | GameEvent::ManaRecolored { player_id, .. } => {
                mark_public_state_player_dirty(state, *player_id);
                mark_mana_display_dirty(state);
            }
            GameEvent::TappedForMana {
                player_id,
                source_id,
                ..
            } => {
                mark_public_state_player_dirty(state, *player_id);
                mark_public_state_object_dirty(state, *source_id);
                mark_mana_display_dirty(state);
            }
            GameEvent::ManaExpended { player_id, .. } => {
                mark_public_state_player_dirty(state, *player_id);
                mark_mana_display_dirty(state);
            }
            // CR 702.140c + CR 730.2: a merge changes the surviving permanent's
            // displayed characteristics. `merge_object_onto` already marks
            // `layers_dirty` (Gate 1 caught it), but mark the merged object here
            // too so this arm is sound even if a future caller skips the full mark.
            GameEvent::Mutated { merged_id, .. } => {
                mark_object_dirty_with_mana(state, *merged_id);
                mark_battlefield_display_dirty(state);
            }
            // Unstable Host/Augment combine uses the same merged-permanent
            // display surface as mutate, but it is a distinct game event.
            GameEvent::Augmented { merged_id, .. } => {
                mark_object_dirty_with_mana(state, *merged_id);
                mark_battlefield_display_dirty(state);
            }
            GameEvent::CounterAdded { object_id, .. }
            | GameEvent::ObjectIntensified { object_id, .. }
            | GameEvent::CounterRemoved { object_id, .. }
            | GameEvent::ControllerChanged { object_id, .. }
            | GameEvent::Evolved { object_id } => {
                // +1/+1 counters set `layers_dirty` (counters.rs) → Gate 1 caught
                // them; this arm fires only for counters that did not touch
                // layers. A depletion/charge counter on a mana land (Gemstone
                // Mine) changes its displayed pips, so route through the
                // mana-aware mark.
                mark_object_dirty_with_mana(state, *object_id);
            }
            GameEvent::DamageDealt { target, .. } | GameEvent::DamagePrevented { target, .. } => {
                match target {
                    // A creature-land (Dryad Arbor, animated manland) taking
                    // damage may change its displayed mana availability.
                    TargetRef::Object(id) => mark_object_dirty_with_mana(state, *id),
                    TargetRef::Player(id) => mark_public_state_player_dirty(state, *id),
                }
            }
            GameEvent::DamageCleared { object_id } => {
                mark_object_dirty_with_mana(state, *object_id);
            }
            GameEvent::Detained { object_id } => {
                // CR 701.35a: Detaining a mana source makes its mana ability
                // un-activatable (`can_activate_mana_ability_now` checks
                // `detained_by`), so route through the mana-aware mark to raise
                // the mana gate and flip the displayed `has_mana_ability`.
                mark_object_dirty_with_mana(state, *object_id);
            }
            GameEvent::LifeChanged { player_id, .. } => {
                mark_public_state_player_dirty(state, *player_id);
            }
            GameEvent::PlayerCounterChanged { player, .. }
            | GameEvent::EnergyChanged { player, .. }
            | GameEvent::SpeedChanged { player, .. } => {
                mark_public_state_player_dirty(state, *player);
            }
            GameEvent::CardsDrawn { player_id, .. } => {
                mark_public_state_player_dirty(state, *player_id);
            }
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            }
            | GameEvent::Discarded {
                player_id,
                object_id,
                ..
            }
            | GameEvent::Cycled {
                player_id,
                object_id,
            } => {
                mark_public_state_player_dirty(state, *player_id);
                mark_public_state_object_dirty(state, *object_id);
            }
            GameEvent::PermanentSacrificed {
                object_id,
                player_id,
            } => {
                // The paired battlefield→graveyard `ZoneChanged` marks the object
                // + battlefield display; mark the controlling player too.
                mark_public_state_object_dirty(state, *object_id);
                mark_public_state_player_dirty(state, *player_id);
            }
            GameEvent::CreatureDestroyed { object_id } => {
                // Paired `ZoneChanged` to graveyard handles battlefield display.
                mark_public_state_object_dirty(state, *object_id);
            }
            GameEvent::MonarchChanged { player_id }
            | GameEvent::CityBlessingGained { player_id }
            | GameEvent::InitiativeTaken { player_id }
            | GameEvent::AttractionOpened { player_id, .. }
            | GameEvent::ContraptionAssembled { player_id, .. }
            | GameEvent::AttractionsRolledToVisit { player_id, .. }
            | GameEvent::AttractionVisited { player_id, .. }
            | GameEvent::ContraptionCranked { player_id, .. }
            | GameEvent::RingTemptsYou { player_id } => {
                mark_public_state_player_dirty(state, *player_id);
            }
            // CR 702.26: Phasing changes which objects/statics are active and
            // reshapes the active-static set; conservatively recompute all.
            GameEvent::PermanentPhasedOut { .. }
            | GameEvent::PermanentPhasedIn { .. }
            | GameEvent::PlayerPhasedOut { .. }
            | GameEvent::PlayerPhasedIn { .. }
            // Transform changes copiable values (Layer 1) and can flip statics
            // on/off; conservatively all-dirty.
            | GameEvent::Transformed { .. }
            | GameEvent::Specialized { .. }
            | GameEvent::TurnedFaceUp { .. }
            // Turning a permanent face down resets its copiable values to a 2/2
            // face-down body (Layer 1) and changes its visibility; recompute.
            | GameEvent::TurnedFaceDown { .. } => {
                mark_public_state_all_dirty(state);
                return;
            }
            // CR 302.6: Turn start clears summoning sickness board-wide WITHOUT
            // setting `layers_dirty` (turns.rs clears `summoning_sick` on all of
            // the active player's permanents emitting only `TurnStarted`), so
            // Gate 1 does not catch it. Recompute all display state. See the
            // existing CR 302.6 annotation at `derived.rs`. MANDATORY all-dirty.
            GameEvent::TurnStarted { .. } => {
                mark_public_state_all_dirty(state);
                return;
            }
            // No display-field impact, OR already covered by `layers_dirty`
            // (Gate 1) / a paired object event above. These events change no
            // field `derive_display_state` computes. Grouped explicitly (never
            // `_ => {}`) so a new event variant must be classified to compile.
            GameEvent::GameStarted
            | GameEvent::PhaseChanged { .. }
            | GameEvent::PriorityPassed { .. }
            | GameEvent::SpellCast { .. }
            | GameEvent::SpellCopied { .. }
            | GameEvent::XValueChosen { .. }
            | GameEvent::AbilityActivated { .. }
            | GameEvent::PlayerLost { .. }
            | GameEvent::MulliganStarted
            | GameEvent::LandPlayed { .. }
            | GameEvent::StackPushed { .. }
            | GameEvent::StackResolved { .. }
            | GameEvent::GameOver { .. }
            // CR 732.2: a halted-resolution notification dirties no display state.
            | GameEvent::ResolutionHalted { .. }
            | GameEvent::SpellCountered { .. }
            | GameEvent::EffectResolved { .. }
            | GameEvent::Unattached { .. }
            | GameEvent::AttackersDeclared { .. }
            | GameEvent::BlockersDeclared { .. }
            | GameEvent::CombatTaxPaid { .. }
            | GameEvent::CombatTaxDeclined { .. }
            | GameEvent::BecomesTarget { .. }
            | GameEvent::VehicleCrewed { .. }
            | GameEvent::Stationed { .. }
            | GameEvent::Saddled { .. }
            | GameEvent::ReplacementApplied { .. }
            | GameEvent::DayNightChanged { .. }
            | GameEvent::CardsRevealed { .. }
            | GameEvent::CombatDamageDealtToPlayer { .. }
            | GameEvent::PlayerEliminated { .. }
            | GameEvent::CrimeCommitted { .. }
            | GameEvent::PlayerPerformedAction { .. }
            | GameEvent::Regenerated { .. }
            | GameEvent::CreatureSuspected { .. }
            | GameEvent::CreatureNoLongerSuspected { .. }
            | GameEvent::BecamePrepared { .. }
            | GameEvent::BecameUnprepared { .. }
            | GameEvent::CaseSolved { .. }
            | GameEvent::ClassLevelGained { .. }
            | GameEvent::DieRolled { .. }
            | GameEvent::CoinFlipped { .. }
            // CR 103.1: starting-player contest carries no public-state delta;
            // it is rendered from the structured event log, not derived state.
            | GameEvent::StartingPlayerContest { .. }
            | GameEvent::RoomEntered { .. }
            | GameEvent::RoomDoorUnlocked { .. }
            | GameEvent::BecomesPlotted { .. }
            | GameEvent::DungeonCompleted { .. }
            // Planechase events: the planar deck / controller are authoritative
            // state serialized directly; they carry no per-object display delta.
            | GameEvent::Planeswalked { .. }
            | GameEvent::ChaosEnsued { .. }
            | GameEvent::PlanarDieRolled { .. }
            // Archenemy events: the scheme deck / archenemy are authoritative
            // state serialized directly; they carry no per-object display delta.
            | GameEvent::SchemeSetInMotion { .. }
            | GameEvent::SchemeAbandoned { .. }
            | GameEvent::Firebend { .. }
            | GameEvent::Airbend { .. }
            | GameEvent::Earthbend { .. }
            | GameEvent::Waterbend { .. }
            | GameEvent::CompanionRevealed { .. }
            | GameEvent::CompanionMovedToHand { .. }
            | GameEvent::NinjutsuActivated { .. }
            | GameEvent::KeywordAbilityActivated { .. }
            | GameEvent::CreatureExploited { .. }
            | GameEvent::Clash { .. }
            | GameEvent::VoteCast { .. }
            | GameEvent::VoteResolved { .. }
            | GameEvent::PowerToughnessChanged { .. }
            | GameEvent::CascadeMissed { .. }
            | GameEvent::DebugActionUsed { .. }
            | GameEvent::DebugPermissionGranted { .. }
            | GameEvent::DebugPermissionRevoked { .. }
            | GameEvent::StickerPlaced { .. } => {}
        }
    }
}

pub fn bump_state_revision(state: &mut GameState) {
    state.state_revision = state.state_revision.wrapping_add(1);
}

pub fn clear_public_state_dirty(state: &mut GameState) {
    state.public_state_dirty = PublicStateDirty::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::game_state::{CastOfferKind, PendingContinuation};
    use crate::types::identifiers::ObjectId;

    #[test]
    fn sync_waiting_for_updates_priority_player_for_resolution_choices() {
        let mut state = GameState::new_two_player(42);
        state.priority_player = PlayerId(1);

        sync_waiting_for(
            &mut state,
            &WaitingFor::CastOffer {
                player: PlayerId(0),
                kind: CastOfferKind::Discover {
                    hit_card: ObjectId(10),
                    exiled_misses: Vec::new(),
                    discover_value: 0,
                },
            },
        );

        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn finalize_public_state_updates_priority_player_for_resolution_choices() {
        let mut state = GameState::new_two_player(42);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::CastOffer {
            player: PlayerId(0),
            kind: CastOfferKind::Discover {
                hit_card: ObjectId(10),
                exiled_misses: Vec::new(),
                discover_value: 0,
            },
        };

        finalize_public_state(&mut state);

        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn finalize_public_state_updates_priority_player_for_ring_bearer_choice() {
        let mut state = GameState::new_two_player(42);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::ChooseRingBearer {
            player: PlayerId(0),
            candidates: vec![ObjectId(10), ObjectId(11)],
        };

        finalize_public_state(&mut state);

        assert_eq!(state.priority_player, PlayerId(0));
    }

    #[test]
    fn finalize_rules_state_normalizes_legacy_optional_attach_choice() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Attach {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(crate::types::ability::ControllerRef::You),
                ),
                target: TargetFilter::TriggeringSource,
            },
            vec![],
            ObjectId(19),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::unlimited(0));
        state.pending_continuation = Some(PendingContinuation::new(Box::new(ability)));
        state.waiting_for = WaitingFor::EffectZoneChoice {
            enters_modified_if: None,
            player: PlayerId(0),
            cards: vec![ObjectId(5), ObjectId(11)],
            count: 1,
            min_count: 1,
            up_to: false,
            source_id: ObjectId(19),
            effect_kind: EffectKind::Attach,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
        };

        finalize_rules_state(&mut state);

        match state.waiting_for {
            WaitingFor::EffectZoneChoice {
                count,
                min_count,
                up_to,
                effect_kind,
                ..
            } => {
                assert_eq!(effect_kind, EffectKind::Attach);
                assert_eq!(count, 2);
                assert_eq!(min_count, 0);
                assert!(up_to);
            }
            other => panic!("expected normalized Attach choice, got {other:?}"),
        }
    }

    // ── Event-driven dirty-marking (perf fix) ────────────────────────────────

    use crate::game::effects::token_copy;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        ResolvedAbility, TargetFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::counter::CounterType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaColor;
    use crate::types::zones::Zone;

    /// Build a Scute-Swarm-like creature (vanilla, no statics) on the
    /// battlefield, then resolve the REAL copy-token resolver against it.
    /// Returns (state, copy_source_id, emitted events).
    fn resolve_real_copy_token(state: &mut GameState) -> (ObjectId, Vec<GameEvent>) {
        let source_id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Scute Swarm".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(1);
            source.base_toughness = Some(1);
            source.power = Some(1);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::Green];
            source.color = vec![ManaColor::Green];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Insect".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        token_copy::resolve(state, &ability, &mut events).unwrap();
        (source_id, events)
    }

    /// PERF-PATH: once layers are stable (no continuous source — Gate 1 false),
    /// a real copy-token resolution marks only the new token + battlefield
    /// display, NOT the whole board. Routes through the real `token_copy`
    /// resolver so a regression that emits the wrong event turns this red.
    #[test]
    fn copy_token_resolution_marks_only_new_token_when_layers_stable() {
        let mut state = GameState::new_two_player(42);
        let (_source_id, events) = resolve_real_copy_token(&mut state);

        let new_token_id = ObjectId(state.next_object_id - 1);

        // The real resolver sets `layers_dirty` (token_copy.rs entry). The drain
        // boundary reaches `finalize_public_state` only after an intervening
        // layer pass has run and cleared the flag, leaving layers stable. Model
        // that by running the real layer evaluation (which clears `layers_dirty`)
        // before classifying — exactly the state `apply()` reaches when layers
        // are not pending.
        assert!(
            state.layers_dirty.is_dirty(),
            "real copy resolver must set layers_dirty"
        );
        evaluate_layers(&mut state);
        assert!(
            !state.layers_dirty.is_dirty(),
            "evaluate_layers clears layers_dirty"
        );

        clear_public_state_dirty(&mut state);
        mark_public_state_from_events(&mut state, &events);

        let dirty = &state.public_state_dirty;
        assert!(
            !dirty.all_objects_dirty,
            "should not escalate to all-objects when layers are stable"
        );
        assert!(
            dirty.dirty_objects.contains(&new_token_id),
            "the new copy token must be marked dirty"
        );
        assert!(
            dirty.battlefield_display_dirty,
            "a battlefield entry must mark battlefield display"
        );
        assert!(!dirty.all_players_dirty, "no player-wide change occurred");
    }

    /// A real battlefield entry now leaves `layers_dirty` in the
    /// `EnteredObjects` state (the resolver requests an incremental re-derive of
    /// just the entering token). Gate 1 must NOT escalate to all-dirty for that
    /// case — it falls through to Gate 2 targeted marking, and the subsequent
    /// `flush_layers` incremental path produces the same display as a full
    /// recompute (asserted via the differential test below).
    #[test]
    fn battlefield_entry_with_layers_pending_stays_targeted() {
        let mut state = GameState::new_two_player(42);
        // Settle layers to Clean first: a fresh state initializes `Full`, which
        // would absorb the entry's `mark_entered` (Full subsumes EnteredObjects).
        // The real engine reaches this site with layers already flushed.
        flush_layers(&mut state);
        let (_source_id, events) = resolve_real_copy_token(&mut state);

        assert!(
            matches!(
                state.layers_dirty,
                crate::types::game_state::LayersDirty::EnteredObjects(_)
            ),
            "real entry must request an incremental layer re-derive (Gate 1 input)"
        );

        clear_public_state_dirty(&mut state);
        mark_public_state_from_events(&mut state, &events);

        assert!(
            !state.public_state_dirty.all_objects_dirty,
            "Gate 1 must NOT escalate to all-objects for an incremental entry"
        );
    }

    /// EnteredObjects-path display == full-recompute display. Drives the real
    /// incremental `flush_layers` path through `finalize_public_state` and
    /// compares per-object display fields against a forced all-dirty
    /// full-evaluate finalize on the same pre-flush state.
    #[test]
    fn incremental_entry_display_matches_full_recompute() {
        let mut base = GameState::new_two_player(42);
        // Settle layers to Clean so the entry below requests EnteredObjects
        // (a fresh state's initial Full would subsume it).
        flush_layers(&mut base);
        let (_src, events) = resolve_real_copy_token(&mut base);
        assert!(
            matches!(
                base.layers_dirty,
                crate::types::game_state::LayersDirty::EnteredObjects(_)
            ),
            "entry must request incremental re-derive"
        );

        // Incremental path: targeted marking + finalize (runs flush_layers
        // incremental).
        let mut incremental = base.clone();
        clear_public_state_dirty(&mut incremental);
        mark_public_state_from_events(&mut incremental, &events);
        finalize_public_state(&mut incremental);

        // Full path: force a full layer flush + all-dirty recompute on the same
        // pre-flush state.
        let mut forced = base.clone();
        forced.layers_dirty = crate::types::game_state::LayersDirty::Full;
        mark_public_state_all_dirty(&mut forced);
        finalize_public_state(&mut forced);

        assert_display_state_eq(&incremental, &forced, "incremental copy entry");
    }

    /// DIFFERENTIAL: targeted marking + finalize produces per-object/per-player
    /// display fields IDENTICAL to a forced all-dirty finalize, across both
    /// Gate-1 branches (layers-stable targeted, layers-pending fallback) and a
    /// TurnStarted transition. This is the Section-3 invariant guard.
    #[test]
    fn targeted_marking_matches_forced_all_dirty() {
        // Branch 1: layers stable — targeted path collapses to {new_id}.
        let mut base = GameState::new_two_player(42);
        let (_src, events) = resolve_real_copy_token(&mut base);
        evaluate_layers(&mut base);
        base.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut targeted = base.clone();
        clear_public_state_dirty(&mut targeted);
        mark_public_state_from_events(&mut targeted, &events);
        finalize_public_state(&mut targeted);

        let mut forced = base.clone();
        mark_public_state_all_dirty(&mut forced);
        finalize_public_state(&mut forced);

        assert_display_state_eq(&targeted, &forced, "layers-stable copy entry");

        // Branch 2: TurnStarted must escalate to all-dirty on both paths and
        // produce identical summoning-sickness display.
        let mut ts_targeted = base.clone();
        clear_public_state_dirty(&mut ts_targeted);
        mark_public_state_from_events(
            &mut ts_targeted,
            &[GameEvent::TurnStarted {
                player_id: PlayerId(0),
                turn_number: 2,
            }],
        );
        assert!(
            ts_targeted.public_state_dirty.all_objects_dirty,
            "TurnStarted must escalate to all-dirty"
        );
        finalize_public_state(&mut ts_targeted);

        let mut ts_forced = base.clone();
        mark_public_state_all_dirty(&mut ts_forced);
        finalize_public_state(&mut ts_forced);

        assert_display_state_eq(&ts_targeted, &ts_forced, "TurnStarted transition");
    }

    fn assert_display_state_eq(a: &GameState, b: &GameState, label: &str) {
        for (id, obj_a) in a.objects.iter() {
            let obj_b = b.objects.get(id).expect("object present in both");
            assert_eq!(
                obj_a.has_summoning_sickness, obj_b.has_summoning_sickness,
                "{label}: summoning sickness mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.unimplemented_mechanics, obj_b.unimplemented_mechanics,
                "{label}: unimplemented mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.has_mana_ability, obj_b.has_mana_ability,
                "{label}: mana ability mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.mana_ability_index, obj_b.mana_ability_index,
                "{label}: mana ability index mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.available_mana_pips, obj_b.available_mana_pips,
                "{label}: mana pips mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.devotion, obj_b.devotion,
                "{label}: devotion mismatch for {id:?}"
            );
            assert_eq!(
                obj_a.commander_tax, obj_b.commander_tax,
                "{label}: commander tax mismatch for {id:?}"
            );
        }
        for (pa, pb) in a.players.iter().zip(b.players.iter()) {
            assert_eq!(
                pa.can_look_at_top_of_library, pb.can_look_at_top_of_library,
                "{label}: peek mismatch for {:?}",
                pa.id
            );
            assert_eq!(
                pa.commander_color_identity, pb.commander_color_identity,
                "{label}: commander identity mismatch for {:?}",
                pa.id
            );
        }
    }

    /// Build a tap-for-mana land on the battlefield. Returns its id.
    fn make_mana_land(state: &mut GameState) -> ObjectId {
        let land_id = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let land = state.objects.get_mut(&land_id).unwrap();
        land.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
        land.card_types = land.base_card_types.clone();
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        land.abilities = std::sync::Arc::new(vec![ability]);
        land_id
    }

    /// DISCRIMINATING (FINDINGS A & B): a battlefield mana land's displayed
    /// mana availability (`has_mana_ability` / `mana_ability_index` /
    /// `available_mana_pips`) is BOARD-GLOBAL and must be re-derived when a
    /// NON-mana event names that land (Gemstone Mine depletion `CounterRemoved`,
    /// damage to a creature-land). The land's mana state here goes stale (it
    /// becomes tapped), and only the mana-display gate re-derives it.
    ///
    /// Without the fix, `CounterRemoved` does not raise `mana_display_dirty`,
    /// the board-wide mana sweep is skipped, and the stale `has_mana_ability`
    /// persists → mismatch with a forced all-dirty finalize (RED). With the fix
    /// the event raises the mana signal, the sweep runs, and the values match
    /// (GREEN). Routed through the REAL event path, not a hand-built dirty set.
    #[test]
    fn non_mana_event_on_mana_land_refreshes_board_global_mana_state() {
        let mut base = GameState::new_two_player(42);
        let land_id = make_mana_land(&mut base);
        // Derived baseline: untapped land displays a mana ability + pips.
        mark_public_state_all_dirty(&mut base);
        finalize_public_state(&mut base);
        assert!(base.objects[&land_id].has_mana_ability);
        assert!(!base.objects[&land_id].available_mana_pips.is_empty());

        // Make the displayed mana state STALE: tap the land directly (not via a
        // mana/tap event) so a fresh derive would show `has_mana_ability=false`
        // and empty pips, but the cached display still reads the untapped values.
        let mut targeted = base.clone();
        targeted.objects.get_mut(&land_id).unwrap().tapped = true;

        // Targeted path: ONLY a non-mana event naming the land.
        clear_public_state_dirty(&mut targeted);
        mark_public_state_from_events(
            &mut targeted,
            &[GameEvent::CounterRemoved {
                object_id: land_id,
                counter_type: CounterType::Generic("depletion".to_string()),
                count: 1,
            }],
        );
        // Direct observable of the fix: the marking layer raised the mana gate.
        assert!(
            targeted.public_state_dirty.mana_display_dirty,
            "a non-mana event on a mana land must raise the mana-display signal"
        );
        finalize_public_state(&mut targeted);

        // Forced all-dirty oracle (same stale starting point).
        let mut forced = base.clone();
        forced.objects.get_mut(&land_id).unwrap().tapped = true;
        mark_public_state_all_dirty(&mut forced);
        finalize_public_state(&mut forced);

        // Targeted display must equal the forced all-dirty oracle on every
        // board-global mana field.
        assert_eq!(
            targeted.objects[&land_id].has_mana_ability, forced.objects[&land_id].has_mana_ability,
            "has_mana_ability must match forced all-dirty after a non-mana event"
        );
        assert_eq!(
            targeted.objects[&land_id].mana_ability_index,
            forced.objects[&land_id].mana_ability_index,
            "mana_ability_index must match forced all-dirty"
        );
        assert_eq!(
            targeted.objects[&land_id].available_mana_pips,
            forced.objects[&land_id].available_mana_pips,
            "available_mana_pips must match forced all-dirty"
        );
        // And the stale value was actually corrected: the now-tapped land must
        // no longer advertise an activatable mana ability (it did before the
        // event). This is the field that turns RED without the fix.
        assert!(
            !targeted.objects[&land_id].has_mana_ability,
            "tapped mana land must not display an activatable mana ability"
        );
    }

    /// Build a mana dork (a creature with a `{T}: Add` ability) on the
    /// battlefield. Returns its id.
    fn make_mana_dork(state: &mut GameState) -> ObjectId {
        let dork_id = create_object(
            state,
            CardId(3),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let dork = state.objects.get_mut(&dork_id).unwrap();
        dork.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Elf".to_string(), "Druid".to_string()],
        };
        dork.card_types = dork.base_card_types.clone();
        dork.base_power = Some(1);
        dork.base_toughness = Some(1);
        dork.power = Some(1);
        dork.toughness = Some(1);
        // Enters under control before this turn so it is not summoning-sick and
        // can tap for mana.
        dork.entered_battlefield_turn = Some(0);
        dork.summoning_sick = false;
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        dork.abilities = std::sync::Arc::new(vec![ability]);
        dork_id
    }

    /// DISCRIMINATING (FINDING C): detaining a mana dork (CR 701.35a) makes its
    /// mana ability un-activatable (`can_activate_mana_ability_now` checks
    /// `detained_by`), but `detain.rs` emits no mana/tap event — only the new
    /// `Detained` event. The displayed `has_mana_ability` must flip to false.
    ///
    /// Without the `Detained` arm routing through the mana-aware mark, the event
    /// is inert, the mana gate is not raised, the board-wide sweep is skipped,
    /// and the stale `has_mana_ability=true` persists → mismatch with a forced
    /// all-dirty oracle (RED). Drives the REAL `detain::resolve` so detain.rs's
    /// `Detained` emission is exercised end-to-end, then classifies the emitted
    /// events through the real path.
    #[test]
    fn detain_mana_dork_clears_mana_ability_display() {
        use crate::game::effects::detain;
        use crate::types::ability::TargetRef;

        let mut base = GameState::new_two_player(42);
        let dork_id = make_mana_dork(&mut base);
        // Derived baseline: untapped, undetained dork displays a mana ability.
        mark_public_state_all_dirty(&mut base);
        finalize_public_state(&mut base);
        assert!(
            base.objects[&dork_id].has_mana_ability,
            "baseline: undetained mana dork must display a mana ability"
        );

        // Targeted path: drive the REAL detain resolver (PlayerId(1) detains the
        // dork), which emits the `Detained` event and sets `detained_by`.
        let mut targeted = base.clone();
        let ability = ResolvedAbility::new(
            Effect::Detain {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(dork_id)],
            ObjectId(999),
            PlayerId(1),
        );
        let mut events = Vec::new();
        detain::resolve(&mut targeted, &ability, &mut events).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::Detained { object_id } if *object_id == dork_id)),
            "detain resolver must emit Detained"
        );
        assert!(targeted.objects[&dork_id]
            .detained_by
            .contains(&PlayerId(1)));

        clear_public_state_dirty(&mut targeted);
        mark_public_state_from_events(&mut targeted, &events);
        assert!(
            targeted.public_state_dirty.mana_display_dirty,
            "Detained on a mana source must raise the mana-display signal"
        );
        finalize_public_state(&mut targeted);

        // Forced all-dirty oracle (same detained starting point).
        let mut forced = base.clone();
        forced
            .objects
            .get_mut(&dork_id)
            .unwrap()
            .detained_by
            .insert(PlayerId(1));
        mark_public_state_all_dirty(&mut forced);
        finalize_public_state(&mut forced);

        assert_eq!(
            targeted.objects[&dork_id].has_mana_ability, forced.objects[&dork_id].has_mana_ability,
            "has_mana_ability must match forced all-dirty after Detained"
        );
        // The detained dork must no longer advertise an activatable mana ability.
        assert!(
            !targeted.objects[&dork_id].has_mana_ability,
            "detained mana dork must not display an activatable mana ability"
        );
    }
}
