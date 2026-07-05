use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::game::conditions::{
    eval_has_city_blessing, eval_is_initiative, eval_is_monarch,
    eval_source_attached_to_controlled_creature, eval_source_entered_this_turn,
    eval_source_is_tapped,
};
use crate::game::filter;
use crate::game::speed::has_max_speed;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityKind, CardTypeSetSource, ChosenAttribute, ControllerRef,
    CopyRetargetPermission, CostPaidObjectSnapshot, EachDamageRecipient, Effect, EffectError,
    EffectKind, EffectOutcomeSignal, EffectScope, FilterProp, OpponentMayScope, PlayerFilter,
    PlayerScope, QuantityExpr, QuantityRef, RepeatContinuation, ResolvedAbility,
    RevealUntilDisposition, SacrificeCost, SacrificeRequirement, SharedQuality,
    SharedQualityRelation, SubAbilityLink, TapStateChange, TargetFilter, TargetRef, ThisWayCause,
};
#[cfg(test)]
use crate::types::ability::{AttackScope, AttackSubject};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{
    AutoMayChoice, CastOfferKind, ClauseMinimumSnapshot, DayNight, GameState, LKISnapshot,
    MayTriggerAutoChoiceKey, PendingContinuation, PendingCopyTokenBatch,
    PendingRepeatedOptionalPayment, WaitingFor, ZoneChangeRecord,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::mana::ManaCost;
use crate::types::player::{Player, PlayerId};
use crate::types::zones::Zone;

pub mod adapt;
pub mod add_restriction;
pub mod add_target_replacement;
pub mod additional_phase;
pub mod amass;
pub mod animate;
pub mod attach;
pub mod attractions;
pub mod awaken;
pub mod become_blocked;
pub mod become_copy;
pub mod become_monarch;
pub mod behold;
pub mod blight;
pub mod bolster;
pub mod bounce;
pub mod cascade;
pub mod cast_copy_of_card;
pub mod cast_from_zone;
pub mod change_targets;
pub mod change_zone;
pub mod chaos_ensues;
pub mod choose;
pub mod choose_and_sacrifice_rest;
pub mod choose_card;
pub mod choose_counter_adjustment;
pub mod choose_counter_kind;
pub mod choose_damage_source;
pub mod choose_from_zone;
pub mod choose_objects_into_tracked_set;
pub mod choose_one_of;
pub mod clash;
pub mod cleanup;
pub mod collect_evidence;
pub mod conjure;
pub mod connive;
pub mod control_next_turn;
pub mod copy_spell;
pub mod copy_token_blocking;
pub mod counter;
pub mod counters;
pub mod create_damage_replacement;
pub mod create_draw_replacement;
pub mod create_emblem;
pub mod create_planeswalk_replacement;
pub mod create_token_copy_from_pool;
pub mod deal_damage;
pub mod delayed_trigger;
pub mod destroy;
pub mod detain;
pub mod dig;
pub mod discard;
pub mod discover;
pub mod double;
pub mod draw;
pub mod drawn_this_turn_choice;
pub mod each_player_copy_chosen;
pub mod effect;
pub mod encore;
pub mod end_combat_phase;
pub(super) mod end_phase;
pub mod end_the_turn;
pub mod endure;
pub mod energy;
pub mod epic;
pub mod exile_resolving_spell;
pub mod put_chosen_counter;
// Tests for `epic` live in a sibling file (declared here, not in `epic.rs`, so
// `epic.rs` stays implementation-only).
#[cfg(test)]
#[path = "epic_tests.rs"]
mod epic_tests;
pub mod exchange_control;
// Tests for `intensify` live in a sibling file (declared here, not in
// `intensify.rs`, so `intensify.rs` stays implementation-only).
pub mod cloak;
pub mod exchange_life;
pub mod exchange_life_totals;
pub mod exile_from_top_until;
pub mod exile_top;
pub mod exploit;
pub mod explore;
pub mod extra_turn;
pub mod fight;
pub mod flip_coin;
pub mod forage;
pub mod force_attack;
pub mod force_block;
pub mod free_cast_from_zones;
pub mod gain_activated_abilities;
pub mod gain_control;
pub mod gift_delivery;
pub mod goad;
pub mod grant_extra_loyalty_activations;
pub mod grant_permission;
pub mod harness;
pub mod heist;
pub mod hideaway;
pub mod incubate;
pub mod intensify;
pub mod perpetual;
// Tests for `heist` live in a sibling file (declared here, not in `heist.rs`,
// so `heist.rs` stays implementation-only — no inline `#[cfg(test)]` token).
#[cfg(test)]
#[path = "heist_tests.rs"]
mod heist_tests;
#[cfg(test)]
#[path = "intensify_tests.rs"]
mod intensify_tests;
pub mod investigate;
pub mod learn;
pub mod life;
pub mod mana;
pub mod manifest;
pub mod manifest_dread;
pub mod mill;
pub mod monstrosity;
pub mod myriad;
pub mod opponent_guess;
pub mod overload;
pub mod pair_with;
pub mod paradigm;
pub mod pay;
pub mod phase_out;
pub mod planeswalk;
pub mod player_counter;
pub mod populate;
pub mod prepare;
pub mod prevent_damage;
pub mod proliferate;
pub mod pump;
pub mod put_on_top;
pub mod put_on_top_or_bottom;
pub mod rad_counters;
pub mod rebound;
pub mod regenerate;
pub mod register_bending;
pub mod remember_card;
pub mod remove_all_damage;
pub mod remove_from_combat;
pub mod renown;
pub mod return_as_aura;
pub mod reveal;
pub mod reveal_from_hand;
pub mod reveal_hand;
pub mod reveal_top;
pub mod reveal_until;
pub mod reverse_turn_order;
pub mod ring;
pub mod ripple;
pub mod roll_die;
pub mod sacrifice;
pub mod saddle;
pub mod scry;
pub mod search_library;
pub mod search_outside_game;
pub mod seek;
pub mod separate_piles;
pub mod set_class_level;
pub mod set_room_door_lock;
pub mod shuffle;
pub mod skip_next_step;
pub mod skip_next_turn;
pub mod solve_case;
pub mod specialize;
pub mod speed_effects;
pub mod spellbook;
pub mod stickers;
pub mod turn_face_down;
pub mod turn_face_up;
// Tests for `spellbook` live in a sibling file (declared here, not in
// `spellbook.rs`, so `spellbook.rs` stays implementation-only).
#[cfg(test)]
#[path = "spellbook_tests.rs"]
mod spellbook_tests;
pub mod surveil;
pub mod suspect;
pub mod swap_chosen_labels;
pub mod switch_pt;
pub mod tap_untap;
pub mod time_travel;
pub mod token;
pub mod token_copy;
pub mod transform_effect;
pub mod tribute;
pub mod venture;
pub mod vote;
pub mod win_lose;

/// Resolve object targets for effect handlers that operate directly on the
/// resolving ability's target slots.
///
/// `ParentTarget` preserves the historical "all inherited object targets"
/// behavior for broad anaphors. `ParentTargetSlot` is the precise CR 608.2c
/// form for later instructions that refer to a specific earlier target slot
/// after intervening actions may have changed object zones.
pub(crate) fn effect_object_targets(
    target_filter: &TargetFilter,
    fallback_targets: &[TargetRef],
) -> Vec<ObjectId> {
    match target_filter {
        TargetFilter::SpecificObject { id } => vec![*id],
        TargetFilter::ParentTargetSlot { index } => fallback_targets
            .get(*index)
            .and_then(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .into_iter()
            .collect(),
        _ => fallback_targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(obj_id) => Some(*obj_id),
                TargetRef::Player(_) => None,
            })
            .collect(),
    }
}

/// Resolve the battlefield object(s) an effect's `target` slot designates,
/// falling back to a zone scan when the ability declared no explicit object
/// target (mass / population forms). Shared by the `turn_face_up` and
/// `turn_face_down` handlers, whose target resolution is identical: prefer the
/// explicit chosen targets, otherwise enumerate the filter's zone (default
/// battlefield) and keep the objects the filter matches.
pub(crate) fn resolved_battlefield_object_ids(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    let resolved = crate::game::targeting::resolved_targets(ability, target, state);
    let explicit = effect_object_targets(target, &resolved);
    if !explicit.is_empty() {
        return explicit;
    }

    // CR 601.2c + CR 608.2b: A *targeting* ability affects only the objects the
    // controller chose as targets. When the ability carries a target-count spec
    // ("any number of target ..." / "up to N target ...") or is otherwise
    // optionally targeted ("up to one target"), an empty resolved set means the
    // controller chose zero targets (or no legal target remains) — the effect
    // must affect nothing. It must NOT fall through to the mass/population zone
    // scan below, which would illegally affect every object matching the filter
    // (e.g. Illithid Harvester turning every tapped nontoken creature face
    // down). The scan is reserved for genuinely non-targeted forms (`SelfRef`,
    // `ExiledBySource`, untargeted "each face-down permanent" population).
    if ability.multi_target.is_some() || ability.optional_targeting {
        return Vec::new();
    }

    let zone = target
        .extract_in_zone()
        .unwrap_or(crate::types::zones::Zone::Battlefield);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    crate::game::targeting::zone_object_ids(state, zone)
        .into_iter()
        .filter(|id| crate::game::filter::matches_target_filter(state, *id, target, &ctx))
        .collect()
}

pub(crate) fn target_filter_controller_scope(filter: &TargetFilter) -> Option<ControllerRef> {
    match filter {
        TargetFilter::Typed(tf) => tf.controller.clone(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(target_filter_controller_scope)
        }
        TargetFilter::Not { filter } => target_filter_controller_scope(filter),
        _ => None,
    }
}

pub(crate) fn matches_player_scope(
    state: &GameState,
    player: PlayerId,
    scope: &PlayerFilter,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| {
            !p.is_eliminated
                && match scope {
                    PlayerFilter::Controller => p.id == controller,
                    PlayerFilter::All => true,
                    // CR 608.2c + CR 109.4: predicate form — a player matches
                    // unless it matches the `exclude` anchor. Exact for excludes
                    // resolvable without ability targets (Controller, Opponent,
                    // …); ability-target-dependent anchors
                    // (ParentObjectTargetController) return `false` from this
                    // generic predicate, so the player_scope driver routes
                    // `AllExcept` through the ability-aware
                    // `speed_effects::players_for_filter` instead, which is
                    // authoritative for `AllExcept` effect-iteration.
                    PlayerFilter::AllExcept { exclude } => {
                        !matches_player_scope(state, p.id, exclude, controller, source_id)
                    }
                    PlayerFilter::Opponent => p.id != controller,
                    PlayerFilter::DefendingPlayer => {
                        crate::game::targeting::resolve_event_context_target_for_event_or_state(
                            state,
                            &TargetFilter::DefendingPlayer,
                            source_id,
                            state.current_trigger_event.as_ref(),
                        )
                        .is_some_and(
                            |target| matches!(target, TargetRef::Player(pid) if pid == p.id),
                        )
                    }
                    PlayerFilter::OpponentLostLife => {
                        p.id != controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != controller && p.life_gained_this_turn > 0
                    }
                    // CR 104.5 / CR 800.4: Players who lost have left the game;
                    // this filter is quantity-only and has no live effect recipient.
                    PlayerFilter::HasLostTheGame => false,
                    // CR 506.2 + CR 508.6: Count-only filter (Suppressor Skyguard's
                    // intervening-if); it has no live effect-recipient meaning, so
                    // no player ever matches it as an effect target.
                    PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => false,
                    // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i +
                    // CR 120.2a/120.2b: Each opponent who was dealt damage of the
                    // given kind this turn, optionally restricted to a matching
                    // source.
                    PlayerFilter::OpponentDealtDamage { kind, source } => {
                        crate::game::quantity::opponent_dealt_damage_matches(
                            state, p.id, controller, *kind, source, source_id,
                        )
                    }
                    // CR 508.6: opponent the subject attacked within scope.
                    PlayerFilter::OpponentAttacked { subject, scope } => {
                        p.id != controller
                            && state
                                .opponent_attacked(*subject, *scope, controller, source_id, p.id)
                    }
                    // CR 508.6 + CR 102.2 + CR 508.1b: opponent of the controller
                    // who is attacking the enchanted/defending player this combat.
                    // The "that player" anchor is the trigger source's AttachedTo
                    // host (the aura source is never itself an attacker, so it can
                    // never appear in `combat.attackers` for `DefendingPlayer`),
                    // resolved via the shared event-context target resolver.
                    PlayerFilter::OpponentAttackingEnchantedPlayer => {
                        p.id != controller
                            && enchanted_player_anchor(state, source_id).is_some_and(|enchanted| {
                                state.player_attacked_player_this_combat(p.id, enchanted)
                            })
                    }
                    PlayerFilter::HighestSpeed => {
                        let highest_speed = state
                            .players
                            .iter()
                            .filter(|player| !player.is_eliminated)
                            .map(|player| crate::game::speed::effective_speed(state, player.id))
                            .max()
                            .unwrap_or(0);
                        crate::game::speed::effective_speed(state, p.id) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                    // CR 608.2c + CR 701.38: Match each player who cast a
                    // vote for `choices[choice_index]` in the most recent
                    // vote of the current top-level resolution. Mirrors the
                    // `ZoneChangedThisWay` arm — a transient ledger
                    // (`last_vote_ballots`) is consulted directly.
                    PlayerFilter::VotedFor { choice_index } => state
                        .last_vote_ballots
                        .iter()
                        .any(|(voter, idx)| *voter == p.id && *idx == *choice_index),
                    PlayerFilter::PerformedActionThisWay { relation, action } => {
                        crate::game::players::matches_relation(state, p.id, controller, *relation)
                            && crate::game::players::performed_action_this_way(state, p.id, *action)
                    }
                    PlayerFilter::OwnersOfCardsExiledBySource => {
                        crate::game::players::owns_card_exiled_by_source(state, p.id, source_id)
                    }
                    // CR 603.7c: Match only the triggering player extracted from
                    // `state.current_trigger_event`.
                    PlayerFilter::TriggeringPlayer => state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                        .is_some_and(|pid| pid == p.id),
                    // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
                    // Falls back to plain Opponent semantics when no trigger event is in scope.
                    PlayerFilter::OpponentOtherThanTriggering => {
                        if !crate::game::players::is_opponent(state, controller, p.id) {
                            return false;
                        }
                        let triggering = state.current_trigger_event.as_ref().and_then(|e| {
                            crate::game::targeting::extract_player_from_event(e, state)
                        });
                        triggering.is_none_or(|pid| pid != p.id)
                    }
                    // CR 102.2 + CR 102.3 + CR 603.2: Each opponent of the
                    // *triggering* (casting) player. The base anchor is the
                    // caster, not the controller, so this legitimately includes
                    // the source's controller as a recipient. Opponent-ness is
                    // CR 102.3-aware (teammates in 2HG are not opponents) via
                    // `players::is_opponent`. Fail closed when no trigger event
                    // is in scope (caster anchor undefined).
                    PlayerFilter::OpponentOfTriggeringPlayer => state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
                        .is_some_and(|caster| {
                            crate::game::players::is_opponent(state, caster, p.id)
                        }),
                    // CR 109.4: the parent-object-target anchor requires the
                    // resolving `ResolvedAbility` (for `ability.targets`),
                    // which this generic scope predicate does not carry. The
                    // `ChangeSpeed` resolver routes this filter through
                    // `speed_effects::players_for_filter` instead, which has
                    // the ability in scope. Unreachable here.
                    PlayerFilter::ParentObjectTargetController => false,
                    // CR 608.2c + CR 109.4: the chosen-player anchor requires the
                    // resolving `ResolvedAbility` (for `ability.chosen_players`),
                    // which this generic scope predicate does not carry.
                    // `choose_one_of::choosing_players` resolves it directly
                    // (it has the ability in scope). Unreachable here — fail
                    // closed, mirroring the `ParentObjectTargetController` arm.
                    PlayerFilter::ChosenPlayer { .. } => false,
                    // CR 108.3 + CR 109.4: the parent-object-target OWNER anchor
                    // likewise requires `ability.targets` to resolve via
                    // `ability_utils::parent_target_owner`. Resolved in
                    // `choose_one_of::choosing_players`; unreachable here.
                    PlayerFilter::ParentObjectTargetOwner => false,
                    // CR 109.4 + CR 109.5: "each [player class] who controls
                    // [comparator] [count] [filter]" — the candidate must
                    // satisfy both the `relation` predicate and the
                    // controlled-permanent count comparison.
                    PlayerFilter::ControlsCount {
                        relation,
                        filter,
                        comparator,
                        count,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state, count, controller, source_id,
                        );
                        crate::game::players::matches_relation(state, p.id, controller, *relation)
                            && player_control_count_compares(
                                state,
                                p.id,
                                filter,
                                *comparator,
                                threshold,
                                source_id,
                            )
                    }
                    // CR 402.1 / 119.1 / 119.3 / 122.1f / 404.1: "each [player class]
                    // whose [scalar attr] [comparator] [value]" — the candidate
                    // satisfies both `relation` and the per-candidate scalar
                    // comparison. `value` is the controller-relative threshold,
                    // resolved once; `attr` is read directly off the candidate.
                    PlayerFilter::PlayerAttribute {
                        relation,
                        attr,
                        comparator,
                        value,
                    } => {
                        let threshold = crate::game::quantity::resolve_quantity(
                            state, value, controller, source_id,
                        );
                        crate::game::players::matches_relation(state, p.id, controller, *relation)
                            && candidate_player_scalar_with_state(state, p, controller, attr)
                                .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
                    }
                }
        })
}

/// CR 301.5 + CR 303.4 + CR 508.6: Resolve the "that player" anchor for the
/// Commander 2017 curse cycle's "each opponent attacking that player" rider —
/// the enchanted/defending player the "whenever enchanted player is attacked"
/// trigger fired on. The trigger source is an Aura attached to that player, so
/// the anchor is the source's `AttachedTo` host. Routed through the shared
/// event-context target resolver (rather than reading `attached_to` inline) so
/// the aura host resolution stays in one authoritative place. Returns `None`
/// when the source isn't attached to a player (defensive — the parser only
/// stamps `OpponentAttackingEnchantedPlayer` on the curse cycle's aura riders).
pub(crate) fn enchanted_player_anchor(state: &GameState, source_id: ObjectId) -> Option<PlayerId> {
    match crate::game::targeting::resolve_event_context_target_for_event_or_state(
        state,
        &TargetFilter::AttachedTo,
        source_id,
        state.current_trigger_event.as_ref(),
    )? {
        TargetRef::Player(pid) => Some(pid),
        TargetRef::Object(_) => None,
    }
}

/// CR 109.4 + CR 109.5: Evaluate the controlled-permanent count predicate of
/// `PlayerFilter::ControlsCount` for one candidate player. Counts battlefield
/// permanents the candidate controls that match `filter`, then compares that
/// count to `threshold` under `comparator`.
///
/// This exactly preserves the old presence semantics: `{ GE, 1 }` is the old
/// `Controls` (count >= 1) and `{ EQ, 0 }` is the old `ControlsNone`
/// (count == 0). Comparative "more X than you" phrasings pass `{ GT, n }` where
/// `n` is the controller's own resolved count.
///
/// The `filter` ("an Elf", "an artifact", …) carries no controller axis — the
/// control relationship is enforced here by `obj.controller == player`, so the
/// shared `matches_target_filter` evaluates only the printed object qualities.
pub(crate) fn player_control_count_compares(
    state: &GameState,
    candidate_player: PlayerId,
    filter: &TargetFilter,
    comparator: crate::types::ability::Comparator,
    threshold: i32,
    source_id: ObjectId,
) -> bool {
    let ctx = filter::FilterContext::from_source_with_controller(source_id, candidate_player);
    let count = state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == candidate_player)
                && filter::matches_target_filter(state, **id, filter, &ctx)
        })
        .count();
    comparator.evaluate(
        crate::game::arithmetic::usize_to_i32_saturating(count),
        threshold,
    )
}

/// CR 402.1 / 119.1 / 119.3 / 122.1f / 404.1: Read scalar `attr` for one
/// candidate player DIRECTLY off the candidate `Player` (NOT via the
/// controller-scoped `resolve_quantity`), so `PlayerFilter::PlayerAttribute`
/// reads each player's own hand size / life total / life lost / graveyard /
/// player-counter rather than the controller's. Returns `None` for any
/// non-scalar `QuantityRef`; the parser
/// invariant guarantees only the scalar subset reaches here, and `None` fails
/// the candidate predicate closed.
pub(crate) fn candidate_player_scalar(p: &Player, attr: &QuantityRef) -> Option<i32> {
    use crate::game::arithmetic::{u32_to_i32_saturating, usize_to_i32_saturating};
    match attr {
        // CR 402.1: cards in the candidate's hand.
        QuantityRef::HandSize { .. } => Some(usize_to_i32_saturating(p.hand.len())),
        // CR 119.3: life lost this turn is tracked per candidate player.
        QuantityRef::LifeLostThisTurn { .. } => Some(u32_to_i32_saturating(p.life_lost_this_turn)),
        // CR 404.1: cards in the candidate's graveyard.
        QuantityRef::GraveyardSize { .. } => Some(usize_to_i32_saturating(p.graveyard.len())),
        // CR 122.1f (poison) + CR 122.1: the candidate's named player-counter total.
        QuantityRef::PlayerCounter { kind, .. } => {
            Some(u32_to_i32_saturating(p.player_counter(kind)))
        }
        // CR 121.1: cards drawn this turn is tracked per candidate player.
        QuantityRef::CardsDrawnThisTurn { .. } => {
            Some(u32_to_i32_saturating(p.cards_drawn_this_turn))
        }
        _ => None,
    }
}

/// CR 402.1 / 119.1 / 403.3 / 608.2h: Per-candidate scalar lookup that needs game-state
/// backing (battlefield entry ledger). Used by `PlayerFilter::PlayerAttribute`
/// in `resolve_player_count` when `candidate_player_scalar` returns `None`.
pub(crate) fn candidate_player_scalar_with_state(
    state: &crate::types::game_state::GameState,
    candidate: &Player,
    controller: crate::types::player::PlayerId,
    attr: &QuantityRef,
) -> Option<i32> {
    if let Some(value) = candidate_player_scalar(candidate, attr) {
        return Some(value);
    }
    match attr {
        // CR 119.1 + CR 810.9a: the candidate's life total reads the team's
        // shared total in a team format (off-team it is the candidate's own
        // life). Stateful because `team_life_total` folds over teammates'
        // `Player::life` via the live game state.
        QuantityRef::LifeTotal { .. } => {
            Some(crate::game::players::team_life_total(state, candidate.id))
        }
        QuantityRef::BattlefieldEntriesThisTurn { filter, .. } => {
            Some(crate::game::arithmetic::usize_to_i32_saturating(
                state
                    .battlefield_entries_this_turn
                    .iter()
                    .filter(|record| {
                        record.controller == candidate.id
                            && crate::game::restrictions::battlefield_entry_matches_filter(
                                record, filter, controller, None,
                            )
                    })
                    .count(),
            ))
        }
        _ => None,
    }
}

/// Record the outer effect's `EffectKind` on the current `pending_continuation`
/// so the drain re-emits the parent `EffectResolved` event that the non-pause
/// tail of the resolver would have emitted. Must be called after
/// `append_to_pending_continuation` has stashed the chain — if no continuation
/// has been stashed the parent event is dropped (the chain is the carrier).
pub(crate) fn mark_pending_continuation_parent(state: &mut GameState, kind: EffectKind) {
    if let Some(cont) = state.pending_continuation.as_mut() {
        cont.parent_kind = Some(kind);
    }
}

/// Drain `state.pending_continuation`: resolve the stashed chain, then emit
/// the stashed parent `EffectResolved` event (if any) so trigger matchers
/// keyed on the outer effect's kind (`EffectKind::Fight`, `DamageAll`,
/// `DamageEachPlayer`, etc.) fire the same way they do on the non-pause
/// path. Safe to call when no continuation is pending (no-op).
///
/// All `pending_continuation.take()` sites should use this helper rather
/// than rolling their own `take + resolve_ability_chain`, so the parent
/// event is never silently dropped.
pub(crate) fn drain_pending_continuation(state: &mut GameState, events: &mut Vec<GameEvent>) {
    counters::drain_pending_counter_moves(state, events);
    counters::drain_pending_counter_removals(state, events);
    counters::drain_pending_counter_additions(state, events);
    if waits_for_resolution_choice(&state.waiting_for) {
        return;
    }
    // CR 101.4 + CR 608.2c: A `ChooseFromZone { EachPlayer }` iteration that is
    // still mid-flight (more players to prompt) must not let the parked
    // continuation ("put those cards onto the battlefield") run until every
    // player's graveyard pick has accumulated into the tracked set. The
    // per-player drain re-parks the next prompt; this guard ensures the
    // continuation waits for the whole sweep (Breach the Multiverse).
    if state.pending_per_player_zone_choice.is_some() {
        return;
    }
    // CR 608.2c (issue #1093 review) + CR 614.12b + CR 614.1c + CR 614.13:
    // Resume a paused multi-target `ChangeZone` iteration (issue #535) BEFORE the
    // continuation. A chained "create/draw that many" consumer
    // (`QuantityRef::EventContextAmount`) must read the FINAL moved count —
    // stamped when the iteration completes — not the stale pre-pause count. This
    // mirrors the non-pause path, where `change_zone::resolve` stamps
    // `last_effect_count` and emits the ChangeZone `EffectResolved` BEFORE the
    // chained sub-ability runs. (Also kept before `pending_repeat_iteration`
    // below: the outer `repeat_for` loop must not advance until the inner
    // ChangeZone completes and emits its `EffectResolved`.)
    if !waits_for_resolution_choice(&state.waiting_for) {
        drain_pending_change_zone_iteration(state, events);
    }
    // The continuation — the completed ChangeZone's chained downstream, or any
    // other parked chain — runs only once the inner iteration finished without
    // re-pausing on a further per-target replacement choice.
    if !waits_for_resolution_choice(&state.waiting_for) {
        if let Some(cont) = state.pending_continuation.take() {
            let PendingContinuation { chain, parent_kind } = cont;
            let source_id = chain.source_id;
            let _ = resolve_ability_chain(state, &chain, events, 1);
            if let Some(kind) = parent_kind {
                events.push(GameEvent::EffectResolved { kind, source_id });
            }
        }
    }
    // CR 701.38d: Resume per-ballot vote iteration after an interactive
    // choice resolves. Must run after change_zone_iteration (which may be
    // nested inside a ballot body) and before repeat_iteration.
    if !waits_for_resolution_choice(&state.waiting_for) {
        vote::drain_pending_vote_ballot_iteration(state, events);
    }
    // CR 609.3 + CR 109.5: After the per-iteration chain drains, drive any
    // remaining `repeat_for` iterations. Each resumed iteration may itself
    // pause and re-stash via the loop in `resolve_ability_chain`, producing a
    // chain of resumed iterations until the loop completes.
    if !waits_for_resolution_choice(&state.waiting_for) {
        drain_pending_repeat_iteration(state, events);
    }
    if !waits_for_resolution_choice(&state.waiting_for) {
        choose_one_of::resume_pending(state, events);
    }
    // CR 608.2c + CR 107.1c: After the iteration's choice and any chained
    // continuation have fully drained (state is back at priority), resume a
    // paused "repeat this process" loop — re-set the `ControllerChoice` repeat
    // prompt.
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.pending_continuation.is_none()
        && state.pending_repeat_iteration.is_none()
    {
        drain_pending_repeat_until(state);
    }
    // CR 614.12a + issue #4886 (review #4): the originating token-choice
    // applied seed must outlive EVERY drain in this function — including
    // `drain_pending_repeat_until`, which re-enters `resolve_ability_chain`
    // (lines 721 / 744) and can emit further token proposals. Clearing before
    // that drain (the previous bug) wiped the seed before a repeated token
    // proposal, reopening the self-replacement loop. Clear ONLY at true
    // full-drain: back at priority with no pending continuation, no pending
    // repeat iteration, AND no pending repeat-until frame. By this point no
    // ability is still resolving, so no token proposal can still need the seed.
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.pending_continuation.is_none()
        && state.pending_repeat_iteration.is_none()
        && state.pending_repeat_until.is_none()
    {
        state.post_replacement_token_choice_applied = None;
    }
}

/// CR 608.2c + CR 107.1c: Resume a "repeat this process" loop that paused when
/// an iteration's process entered an interactive `WaitingFor` state. Called by
/// `drain_pending_continuation` once the iteration's choice (and any chained
/// continuation) has fully drained.
fn drain_pending_repeat_until(state: &mut GameState) {
    let Some(pending) = state.pending_repeat_until.take() else {
        return;
    };
    let crate::types::game_state::PendingRepeatUntil { ability } = pending;
    match &ability.repeat_until {
        // CR 107.1c: the iteration's choice has resolved — prompt the
        // controller whether to repeat the process.
        Some(RepeatContinuation::ControllerChoice) => {
            state.waiting_for = WaitingFor::RepeatDecision {
                player: ability.controller,
                ability,
            };
        }
        Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand,
            stop_on_duplicate_exiled_names,
        }) => {
            if should_stop_repeat_until(
                state,
                &ability,
                *stop_on_put_to_hand,
                *stop_on_duplicate_exiled_names,
            ) {
                return;
            }
            let mut events = Vec::new();
            let _ = resolve_ability_chain(state, &ability, &mut events, 1);
        }
        // CR 608.2c: resume a paused `WhileCondition` loop after the iteration's
        // interactive choice (Claim Jumper's library search) has drained. The
        // stashed ability carries the remaining cap on its own `repeat_until`;
        // re-evaluate the condition and re-enter `resolve_ability_chain` (which
        // re-runs the body and re-checks the loop) only when both gates hold.
        Some(RepeatContinuation::WhileCondition {
            condition,
            max_iterations,
        }) => {
            let mut remaining = *max_iterations;
            if !should_repeat_while_condition(state, &ability, condition, &mut remaining) {
                return;
            }
            // Thread the decremented cap into the re-entered loop so a bounded
            // "once" repeat is not granted an extra iteration on resume.
            let mut next = (*ability).clone();
            next.repeat_until = Some(RepeatContinuation::WhileCondition {
                condition: condition.clone(),
                max_iterations: remaining,
            });
            let mut events = Vec::new();
            let _ = resolve_ability_chain(state, &next, &mut events, 1);
        }
        None => {}
    }
}

/// CR 608.2c + CR 107.1c: Stop predicates for `RepeatContinuation::UntilStopConditions`.
fn should_stop_repeat_until(
    state: &GameState,
    ability: &ResolvedAbility,
    stop_on_put_to_hand: bool,
    stop_on_duplicate_exiled_names: bool,
) -> bool {
    if stop_on_put_to_hand {
        let controller = ability.controller;
        let put_to_hand = state
            .cards_exiled_with_source_this_turn
            .get(&ability.source_id)
            .into_iter()
            .flatten()
            .any(|&id| {
                state
                    .objects
                    .get(&id)
                    .is_some_and(|obj| obj.zone == Zone::Hand && obj.controller == controller)
            });
        if put_to_hand {
            return true;
        }
    }
    stop_on_duplicate_exiled_names
        && crate::game::exile_links::duplicate_name_among_exiled_by_source(state, ability.source_id)
}

/// CR 303.4f + CR 614.12b + CR 614.1c + CR 614.13: Resume a multi-target
/// `ChangeZone` loop paused when an object's ETB triggered a per-permanent
/// replacement choice (issue #535) or an Aura host choice. Drives the
/// remaining objects through `process_one_zone_move`; re-stashes and breaks on
/// a further pause; emits the trailing `EffectResolved` event when the loop
/// completes.
fn drain_pending_change_zone_iteration(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(pending) = state.pending_change_zone_iteration.take() {
        let crate::types::game_state::PendingChangeZoneIteration {
            remaining,
            source_id,
            controller,
            origin,
            destination,
            enter_transformed,
            enter_tapped,
            enters_under_player,
            enters_attacking,
            enter_with_counters,
            conditional_enter_with_counters,
            duration,
            track_exiled_by_source,
            mut moved_count,
            face_down_profile,
            library_placement,
            enters_modified_if,
            effect_kind,
        } = pending;
        // CR 608.2c: the object that paused this iteration on a replacement CHOICE
        // (`pending_change_zone_in_flight`) was delivered out-of-band by the
        // replacement resume, NOT by the `remaining` loop below. Count it now — iff
        // it actually reached this iteration's destination (a same-zone no-op or a
        // prevented/redirected move is excluded by the `before != destination` and
        // post-move zone checks) — so a downstream `QuantityRef::EventContextAmount`
        // ("that many") includes the member that prompted the replacement.
        if let Some((in_flight_id, before)) = state.pending_change_zone_in_flight.take() {
            if let Some(count) = moved_count.as_mut() {
                if before != destination
                    && state
                        .objects
                        .get(&in_flight_id)
                        .is_some_and(|object| object.zone == destination)
                {
                    *count += 1;
                }
            }
        }
        // CR 603.10a: scope this drain pass's battlefield-exit events so the
        // members moved in THIS resume can be stamped as a co-departed group and
        // their observer triggers collected. NOTE (no-field DEFERRED residual):
        // members moved in a PRIOR pause segment (before this resume) cannot be
        // grouped with these without a co_departed_group carrier field on
        // PendingChangeZoneIteration — the cross-pause observation gap is
        // documented by an ignored test. See plan STEP 4b.
        let events_before_drain = events.len();
        let mut paused = false;
        for (i, obj_id) in remaining.iter().enumerate() {
            let per_obj_enter_counters =
                crate::game::effects::change_zone::enter_with_counters_for_pending_object(
                    state,
                    source_id,
                    *obj_id,
                    &enter_with_counters,
                    &conditional_enter_with_counters,
                );
            let ctx = crate::game::effects::change_zone::ChangeZoneIterationCtx {
                source_id,
                controller,
                origin,
                destination,
                enter_transformed,
                enter_tapped,
                enters_under_player,
                enters_attacking,
                enter_with_counters: per_obj_enter_counters,
                conditional_enter_with_counters: vec![],
                duration: duration.clone(),
                track_exiled_by_source,
                // CR 708.2a + CR 708.3: thread the preserved face-down profile back
                // into the resume ctx so a face-down move that parked on a
                // per-permanent replacement-ordering / as-enters choice resumes
                // FACE DOWN with the same characteristics (Yedora-style return),
                // instead of exposing the real object face up. Mirrors the
                // `enter_tapped`/`enter_transformed`/`enters_under_player` carry-through.
                face_down_profile: face_down_profile.clone(),
                library_placement: library_placement.clone(),
                // CR 614.12: thread the moved-object type gate into the per-object
                // resume ctx so a resumed member is still gated on its type
                // (Summoner's Grimoire), matching the synchronous move path.
                enters_modified_if: enters_modified_if.clone(),
            };
            let before_zone = state.objects.get(obj_id).map(|object| object.zone);
            match crate::game::effects::change_zone::process_one_zone_move(
                state, &ctx, *obj_id, events,
            ) {
                crate::game::effects::change_zone::ZoneMoveResult::Done => {
                    if let Some(count) = moved_count.as_mut() {
                        if before_zone != Some(ctx.destination)
                            && state
                                .objects
                                .get(obj_id)
                                .is_some_and(|object| object.zone == ctx.destination)
                        {
                            *count += 1;
                        }
                    }
                }
                crate::game::effects::change_zone::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    if let Some(count) = moved_count.as_mut() {
                        if before_zone != Some(ctx.destination)
                            && state
                                .objects
                                .get(obj_id)
                                .is_some_and(|object| object.zone == ctx.destination)
                        {
                            *count += 1;
                        }
                    }
                    state.pending_change_zone_iteration =
                        Some(crate::types::game_state::PendingChangeZoneIteration {
                            remaining: remaining[i + 1..].to_vec(),
                            source_id: ctx.source_id,
                            controller: ctx.controller,
                            origin: ctx.origin,
                            destination: ctx.destination,
                            enter_transformed: ctx.enter_transformed,
                            enter_tapped: ctx.enter_tapped,
                            enters_under_player: ctx.enters_under_player,
                            enters_attacking: ctx.enters_attacking,
                            enter_with_counters: enter_with_counters.clone(),
                            conditional_enter_with_counters: conditional_enter_with_counters
                                .clone(),
                            duration: ctx.duration.clone(),
                            track_exiled_by_source: ctx.track_exiled_by_source,
                            moved_count,
                            // CR 708.2a + CR 708.3: preserve the face-down profile
                            // across a further pause so resumed members stay face down.
                            face_down_profile: ctx.face_down_profile.clone(),
                            library_placement: ctx.library_placement.clone(),
                            // CR 614.12: preserve the moved-object type gate
                            // across a further pause.
                            enters_modified_if: ctx.enters_modified_if.clone(),
                            effect_kind,
                        });
                    paused = true;
                    break;
                }
                crate::game::effects::change_zone::ZoneMoveResult::NeedsChoice(player) => {
                    state.pending_change_zone_iteration =
                        Some(crate::types::game_state::PendingChangeZoneIteration {
                            remaining: remaining[i + 1..].to_vec(),
                            source_id: ctx.source_id,
                            controller: ctx.controller,
                            origin: ctx.origin,
                            destination: ctx.destination,
                            enter_transformed: ctx.enter_transformed,
                            enter_tapped: ctx.enter_tapped,
                            enters_under_player: ctx.enters_under_player,
                            enters_attacking: ctx.enters_attacking,
                            enter_with_counters: enter_with_counters.clone(),
                            conditional_enter_with_counters: conditional_enter_with_counters
                                .clone(),
                            duration: ctx.duration.clone(),
                            track_exiled_by_source: ctx.track_exiled_by_source,
                            moved_count,
                            // CR 708.2a + CR 708.3: preserve the face-down profile
                            // across a further pause so resumed members stay face down.
                            face_down_profile: ctx.face_down_profile.clone(),
                            library_placement: ctx.library_placement.clone(),
                            // CR 614.12: preserve the moved-object type gate
                            // across a further pause.
                            enters_modified_if: ctx.enters_modified_if.clone(),
                            effect_kind,
                        });
                    // CR 608.2c: a further member paused mid-move on a replacement
                    // choice; it will be delivered by the resume, not this drain's
                    // `remaining` loop. Record it as in-flight (with its pre-move zone)
                    // so the NEXT drain pass counts it once it reaches the destination.
                    if let Some(before) = before_zone {
                        state.pending_change_zone_in_flight = Some((*obj_id, before));
                    }
                    // CR 614.12a: park (don't clobber) — a Devour as-enters sacrifice
                    // may already have surfaced its own `EffectZoneChoice` during the
                    // resumed member's entry.
                    crate::game::replacement::park_waiting_for(state, player);
                    paused = true;
                    break;
                }
            }
        }
        if paused {
            // CR 603.10a: paused again on a further choice. Stamp the members
            // this pass moved so any co-departing observer among them observes
            // the rest, then B2-park their observer triggers: `waiting_for` is
            // now a choice (not Priority), so `run_post_action_pipeline` will
            // not scan these events — deferring keeps issue #423 dies-triggers
            // from being lost across the pause.
            crate::game::zones::stamp_simultaneous_from_slice(
                state,
                &mut events[events_before_drain..],
            );
            let trigger_events: Vec<GameEvent> = events[events_before_drain..]
                .iter()
                .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
                .cloned()
                .collect();
            crate::game::triggers::collect_triggers_into_deferred(state, &trigger_events);
            break;
        }
        // Loop completed — stamp the members this pass moved (CR 603.10a) so a
        // co-departing observer among the resumed group observes the rest, then
        // emit the trailing EffectResolved event that the non-pause path emits at
        // the tail of `change_zone::resolve`.
        crate::game::zones::stamp_simultaneous_from_slice(
            state,
            &mut events[events_before_drain..],
        );
        // CR 614.13a: the resumed mass/targeted co-entry finished without pausing —
        // the whole ChangeZone entry event is complete, so clear the pre-entry
        // Devour snapshot. NOT cleared on the `paused` break above (a further
        // devourer's sacrifice and the remaining members still need it).
        let _ = state.devour_eligible_snapshot.take();
        if let Some(count) = moved_count {
            state.last_effect_count = Some(count);
        }
        events.push(GameEvent::EffectResolved {
            kind: effect_kind,
            source_id,
        });
        // CR 603.2 + CR 603.3b: the resume settled the iteration. When the move
        // landed us back at Priority (no further replacement choice), B1-drain the
        // deferred observer triggers parked during earlier pause segments plus the
        // ones this resume produced; otherwise leave them parked for the next drain.
        let trigger_events: Vec<GameEvent> = events[events_before_drain..]
            .iter()
            .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
            .cloned()
            .collect();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            crate::game::triggers::collect_triggers_into_deferred(state, &trigger_events);
            crate::game::triggers::drain_deferred_trigger_queue(state, events);
        } else {
            crate::game::triggers::collect_triggers_into_deferred(state, &trigger_events);
        }
    }
}

/// CR 609.3 + CR 109.5: Resume a paused `repeat_for` loop. Each iteration
/// may itself pause (re-stashing into `pending_repeat_iteration`); the outer
/// driver in `drain_pending_continuation` re-enters this on the next choice
/// resolution. If an iteration completes synchronously and a further
/// iteration also completes synchronously, this function drives them all
/// in-line so the loop only pauses again when an inner effect actually
/// transitions to a player-choice state.
fn drain_pending_repeat_iteration(state: &mut GameState, events: &mut Vec<GameEvent>) {
    while let Some(pending) = state.pending_repeat_iteration.take() {
        let crate::types::game_state::PendingRepeatIteration {
            ability,
            tracked_members,
            iterated_counter_kinds,
            next_iteration,
            total_iterations,
        } = pending;
        let initial_waiting_for = state.waiting_for.clone();
        let initial_continuation_present = state.pending_continuation.is_some();
        let mut iteration = next_iteration;
        let mut paused = false;
        while iteration < total_iterations {
            let mut iter_ability;
            // CR 109.5 / CR 122.1 + CR 608.2c: clone when EITHER a tracked
            // member rebind (parent-target loop) OR a counter-kind rebind
            // (DistinctCounterKindsAmong loop) applies to this iteration.
            let member = tracked_members.get(iteration).copied();
            let kind = iterated_counter_kinds.get(iteration).cloned();
            let iter_effective: &ResolvedAbility = if member.is_some() || kind.is_some() {
                iter_ability = (*ability).clone();
                if let Some(member) = member {
                    rebind_first_object_target(&mut iter_ability.targets, member);
                }
                if let Some(kind) = kind {
                    rebind_iterated_counter_kind(&mut iter_ability, kind);
                }
                &iter_ability
            } else {
                &ability
            };
            // CR 609.3 + CR 109.5: Drive the FULL chain (parent effect +
            // sub_ability + line-1660 continuation wiring) for each resumed
            // iteration, mirroring iteration 0's path. Calling `resolve_effect`
            // here would skip the sub_ability stash, so e.g. Winds of Abandon's
            // put-onto-battlefield + shuffle continuation would never run for
            // opponents 2+. The stashed `ability` had `repeat_for` cleared at
            // stash time so this call resolves a single iteration only.
            //
            // Pass depth=1 to preserve chain-local state (`chain_tracked_set_id`,
            // `last_revealed_ids`, `last_zone_changed_ids`, `last_effect_amount`)
            // that the depth==0 prelude in `resolve_ability_chain` would
            // otherwise reset. The resumed iteration is logically continuing the
            // outer chain, not starting a fresh top-level resolution.
            let _ = resolve_ability_chain(state, iter_effective, events, 1);
            // CR 609.3: Iteration may transition to a player-choice state OR
            // synchronously install a `pending_continuation` (e.g. when the
            // sub_ability chain wires itself for later drain). Either signals
            // that this iteration is not yet fully resolved — re-stash the
            // remaining iterations and break so the outer
            // `drain_pending_continuation` can run the continuation, then
            // re-enter this drain for the next iteration. Without re-stashing
            // on the synchronous-continuation case, subsequent iterations are
            // silently dropped.
            let entered_choice = state.waiting_for != initial_waiting_for;
            let installed_continuation =
                !initial_continuation_present && state.pending_continuation.is_some();
            if entered_choice || installed_continuation {
                let next = iteration + 1;
                if next < total_iterations {
                    state.pending_repeat_iteration =
                        Some(crate::types::game_state::PendingRepeatIteration {
                            ability: ability.clone(),
                            tracked_members: tracked_members.clone(),
                            iterated_counter_kinds: iterated_counter_kinds.clone(),
                            next_iteration: next,
                            total_iterations,
                        });
                }
                paused = true;
                break;
            }
            iteration += 1;
        }
        if paused {
            // Loop paused mid-iteration; the next call to
            // `drain_pending_continuation` will resume.
            break;
        }
    }
}

pub(crate) fn append_to_pending_continuation(
    state: &mut GameState,
    tail: Option<Box<ResolvedAbility>>,
) {
    let Some(tail) = tail else {
        return;
    };

    if let Some(existing) = state.pending_continuation.as_mut() {
        let mut cursor = existing.chain.as_mut();
        let tail = Some(tail);
        loop {
            if cursor.sub_ability.is_none() {
                cursor.sub_ability = tail;
                break;
            }
            cursor = cursor
                .sub_ability
                .as_mut()
                .expect("sub_ability checked above")
                .as_mut();
        }
    } else {
        state.pending_continuation = Some(PendingContinuation::new(tail));
    }
}

fn prepend_to_pending_continuation(state: &mut GameState, mut head: ResolvedAbility) {
    if let Some(existing) = state.pending_continuation.take() {
        let PendingContinuation { chain, parent_kind } = existing;
        super::ability_utils::append_to_sub_chain(&mut head, *chain);
        state.pending_continuation = Some(PendingContinuation {
            chain: Box::new(head),
            parent_kind,
        });
    } else {
        state.pending_continuation = Some(PendingContinuation::new(Box::new(head)));
    }
}

pub(crate) fn prepend_remaining_pay_cost_continuation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    payer: PlayerId,
    remaining_cost: AbilityCost,
) {
    let mut remaining_payment = ability.clone();
    remaining_payment.controller = payer;
    remaining_payment.optional = false;
    remaining_payment.optional_for = None;
    remaining_payment.effect = Effect::PayCost {
        cost: remaining_cost,
        scale: None,
        payer: TargetFilter::Controller,
    };
    remaining_payment.sub_ability = None;

    if let Some(sub) = ability.sub_ability.as_ref() {
        let mut sub_clone = sub.as_ref().clone();
        if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
            sub_clone.targets = ability.targets.clone();
        }
        apply_parent_chain_context(&mut sub_clone, ability, None, state);
        super::ability_utils::append_to_sub_chain(&mut remaining_payment, sub_clone);
    }

    // CR 118.12 + CR 608.2c: when payment pauses before later sub-costs are
    // paid, resume by paying those costs before following the original
    // sub-ability chain.
    prepend_to_pending_continuation(state, remaining_payment);
}

pub(crate) fn parent_referent_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    // CR 608.2c + CR 400.7j: Later instructions in one resolving effect may
    // refer to a single object the earlier instruction sacrificed, moved to a
    // public zone, or revealed, even after that object changed zones.
    if let Some(snapshot) = sacrificed_object_context_from_events(state, events) {
        return Some(snapshot);
    }

    if let Some(snapshot) = moved_object_context_from_events(events) {
        return Some(snapshot);
    }

    if let Some(snapshot) = stack_pushed_object_context_from_events(state, events) {
        return Some(snapshot);
    }

    if let Some(snapshot) = revealed_object_context_from_events(state, events) {
        return Some(snapshot);
    }

    // CR 608.2c: a later instruction's "that creature" may refer to a single
    // creature an earlier instruction in the same resolution tapped — e.g.
    // Enlist's "+X/+0 … where X is the tapped creature's power". Tried before
    // the damage referent so sacrifice/move/reveal/tap referents continue to
    // take precedence. The tapped permanent stays on the battlefield, so it is
    // snapshot live.
    if let Some(snapshot) = tapped_object_context_from_events(state, events) {
        return Some(snapshot);
    }

    // CR 608.2c: the weakest referent — a later instruction's "that creature"
    // may refer to the single creature an earlier instruction in the same
    // resolution dealt damage to (e.g. fight-back templates: "~ deals damage
    // equal to its power to target creature. That creature deals damage equal
    // to its power to ~"). A damaged permanent stays on the battlefield, so its
    // claim is the weakest; sacrifice/move/reveal/tap referents from the same
    // resolution still win.
    damaged_object_context_from_events(state, events)
}

fn amassed_army_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    // CR 701.47c + CR 608.2c: "the Army you amassed" / "the amassed Army"
    // names the Army chosen by the most recent amass instruction in this
    // resolving ability. This helper only stamps the serialized ability
    // carrier; quantity resolution reads `ResolvedAbility.amassed_army_object`,
    // not the event log.
    events.iter().rev().find_map(|event| match event {
        GameEvent::ArmyAmassed { object_id, .. } => {
            state
                .objects
                .get(object_id)
                .map(|obj| CostPaidObjectSnapshot {
                    object_id: *object_id,
                    lki: obj.snapshot_public_characteristics(),
                })
        }
        _ => None,
    })
}

/// CR 608.2c: capture a single creature an earlier instruction dealt damage to
/// as the resolution's anaphoric referent (the "fight-back clause"). A
/// multi-target damage parent (e.g. Living Inferno, which deals damage to each
/// creature) has no singular "that creature", so it yields no snapshot —
/// Living Inferno is intentionally NOT fixed here; it needs distributive
/// per-creature resolution this referent does not attempt. Player-targeted
/// damage (`TargetRef::Player`) is filtered out. The damaged permanent stays on
/// the battlefield, so it is snapshot live.
fn damaged_object_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    let mut seen = HashSet::new();
    let mut damaged = events.iter().filter_map(|event| match event {
        // CR 608.2c: only object-targeted damage introduces a "that creature".
        GameEvent::DamageDealt {
            target: TargetRef::Object(object_id),
            ..
        } if seen.insert(*object_id) => {
            state
                .objects
                .get(object_id)
                .map(|obj| CostPaidObjectSnapshot {
                    object_id: *object_id,
                    lki: obj.snapshot_for_mana_spent(),
                })
        }
        _ => None,
    });
    // CR 608.2c: single-object guard — a multi-target damage parent has no
    // singular referent.
    let first = damaged.next()?;
    damaged.next().is_none().then_some(first)
}

/// CR 608.2c: capture a single creature tapped by the parent instruction as the
/// resolution's anaphoric referent. A multi-permanent tap has no singular "that
/// creature", so it yields no snapshot (mirroring the sacrifice/move guards).
fn tapped_object_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    let mut seen = HashSet::new();
    let mut tapped = events.iter().filter_map(|event| match event {
        GameEvent::PermanentTapped { object_id, .. } if seen.insert(*object_id) => state
            .objects
            .get(object_id)
            .map(|obj| CostPaidObjectSnapshot {
                object_id: *object_id,
                lki: obj.snapshot_for_mana_spent(),
            }),
        _ => None,
    });
    let first = tapped.next()?;
    tapped.next().is_none().then_some(first)
}

fn sacrificed_object_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    // CR 608.2k: A `ParentTarget` referent must be "a specific untargeted object"
    // (singular). When the parent effect sacrificed more than one permanent there
    // is no single resolvable subject, so yield no snapshot — mirroring the guard
    // in `moved_object_context_from_events`.
    let mut sacrificed = events.iter().filter_map(|event| match event {
        GameEvent::PermanentSacrificed { object_id, .. } => state
            .lki_cache
            .get(object_id)
            .cloned()
            .map(|lki| CostPaidObjectSnapshot {
                object_id: *object_id,
                lki,
            }),
        _ => None,
    });
    let first = sacrificed.next()?;
    sacrificed.next().is_none().then_some(first)
}

fn moved_object_context_from_events(events: &[GameEvent]) -> Option<CostPaidObjectSnapshot> {
    let mut moved = events.iter().filter_map(|event| match event {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(_),
            to,
            record,
        } if is_public_zone(*to) => Some(CostPaidObjectSnapshot {
            object_id: *object_id,
            lki: lki_snapshot_from_zone_change_record(record),
        }),
        _ => None,
    });
    let first = moved.next()?;
    moved.next().is_none().then_some(first)
}

/// CR 707.10 + CR 608.2c: A `CopySpell` that puts a copy onto the stack
/// introduces a singular object a chained `ParentTarget` consumer (Isochron
/// Scepter's free cast) binds to.
fn stack_pushed_object_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    let mut pushed = events.iter().filter_map(|event| match event {
        GameEvent::StackPushed { object_id } => {
            state
                .objects
                .get(object_id)
                .map(|obj| CostPaidObjectSnapshot {
                    object_id: *object_id,
                    lki: obj.snapshot_for_mana_spent(),
                })
        }
        _ => None,
    });
    let first = pushed.next()?;
    pushed.next().is_none().then_some(first)
}

/// CR 608.2c + CR 608.2h + CR 701.20b: A `reveal` instruction introduces an
/// object that a later anaphoric pronoun ("its mana value") in the same
/// ability binds to. Revealing does not move the card (CR 701.20b), so it
/// emits `CardsRevealed`, not `ZoneChanged` — capture the referent here.
/// The snapshot is taken now because a chained `ChangeZone` may move the
/// card to a hidden zone (Hand); CR 608.2h mandates last-known-information
/// once that happens. Single-card guard: a multi-card reveal has no
/// singular "it".
fn revealed_object_context_from_events(
    state: &GameState,
    events: &[GameEvent],
) -> Option<CostPaidObjectSnapshot> {
    let mut revealed = events.iter().filter_map(|event| match event {
        GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids),
        _ => None,
    });
    let card_ids = revealed.next()?;
    let [card_id] = card_ids.as_slice() else {
        return None;
    };
    let obj = state.objects.get(card_id)?;
    let snapshot = CostPaidObjectSnapshot {
        object_id: *card_id,
        lki: obj.snapshot_for_mana_spent(),
    };
    // Second `CardsRevealed` event → ambiguous "it" → no referent.
    revealed.next().is_none().then_some(snapshot)
}

fn lki_snapshot_from_zone_change_record(record: &ZoneChangeRecord) -> LKISnapshot {
    LKISnapshot {
        name: record.name.clone(),
        power: record.power,
        toughness: record.toughness,
        // CR 208.4b + CR 613.4b: Carry the layer-7b base values from the
        // zone-change snapshot so a later `PtComparison { scope: Base }`
        // evaluation on this LKI reads the base, not the current, value.
        base_power: record.base_power,
        base_toughness: record.base_toughness,
        mana_value: record.mana_value,
        controller: record.controller,
        owner: record.owner,
        card_types: record.core_types.clone(),
        subtypes: record.subtypes.clone(),
        supertypes: record.supertypes.clone(),
        keywords: record.keywords.clone(),
        colors: record.colors.clone(),
        chosen_attributes: Vec::new(),
        counters: Default::default(),
        tapped: false,
        // CR 701.60b: Carry suspected status from the zone-change snapshot.
        is_suspected: record.is_suspected,
    }
}

fn is_public_zone(zone: crate::types::zones::Zone) -> bool {
    !matches!(
        zone,
        crate::types::zones::Zone::Library | crate::types::zones::Zone::Hand
    )
}

/// CR 603.12 + CR 601.2c: Freeze the reflexive event count into the pending
/// trigger's `subject_match_count` at the moment the "When you do" sub-ability
/// fires. The reflexive ability's "up to that many target ..." bound is an
/// `EventContextAmount` that resolves against the count of subjects affected by
/// the triggering action (e.g. the number of Treasures sacrificed). At
/// creation time `last_effect_count` (and the rest of the event-context
/// cascade) is still live, so resolving `EventContextAmount` here captures the
/// real count. The reflexive triggered ability resolves later in a fresh
/// `apply()` where that scratch state has been cleared; `subject_match_count`
/// is rehydrated into `current_trigger_match_count` (CR 603.2c) and resolves
/// the number of targets at target-assign time. Without this freeze the bound
/// collapses to 0 — yielding "Unused selected target slots" or a silently
/// wrong amount. Resolves via the single authoritative `resolve_quantity`
/// cascade (`QuantityRef::EventContextAmount`) — never re-counts. `None` when
/// the live count resolves to 0 (no event context), matching the prior
/// behavior for reflexive abilities with no event-count bound.
fn freeze_reflexive_event_count(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> Option<u32> {
    let count = crate::game::quantity::resolve_quantity(
        state,
        &QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        controller,
        source_id,
    );
    u32::try_from(count).ok().filter(|&c| c > 0)
}

/// CR 608.2c: A conditional clause's `else_ability` target slots are not
/// collected when the trigger or spell ability is put on the stack — only the
/// gated head's targets are announced (CR 603.3d). When the `if` branch fails
/// and the else branch runs, bind its targets at resolution: auto-bind when
/// exactly one legal combination exists (CR 115.4), or prompt via
/// `ChooseFromZoneChoice` when multiple object targets are legal (Bolster class).
///
/// Returns `true` when resolution paused on a player choice.
fn try_begin_deferred_else_branch_target_selection(
    state: &mut GameState,
    else_resolved: &mut ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    if !else_resolved.targets.is_empty() {
        return Ok(false);
    }
    let slots = crate::game::ability_utils::build_target_slots(state, else_resolved)
        .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
    if slots.is_empty() {
        return Ok(false);
    }
    let constraints = &else_resolved.target_constraints;
    if let Some(selected) = crate::game::ability_utils::auto_select_targets_for_ability(
        state,
        else_resolved,
        &slots,
        constraints,
    )
    .map_err(|e| EffectError::InvalidParam(e.to_string()))?
    {
        crate::game::ability_utils::assign_targets_in_chain(state, else_resolved, &selected)
            .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
        return Ok(false);
    }
    if !crate::game::ability_utils::has_legal_target_assignment_for_ability(
        state,
        else_resolved,
        &slots,
        constraints,
    ) {
        return Ok(false);
    }
    // CR 608.2d: ambiguous single-object-target else branches (Wick: "put a +1/+1
    // counter on a Snail you control" with two Snails) prompt the controller to
    // choose, mirroring the Bolster keyword-action tie-break path.
    if slots.len() == 1 {
        let candidates: Vec<crate::types::identifiers::ObjectId> = slots[0]
            .legal_targets
            .iter()
            .filter_map(|t| match t {
                crate::types::ability::TargetRef::Object(id) => Some(*id),
                crate::types::ability::TargetRef::Player(_) => None,
            })
            .collect();
        if !candidates.is_empty() {
            state.pending_continuation =
                Some(PendingContinuation::new(Box::new(else_resolved.clone())));
            state.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player: else_resolved.controller,
                cards: candidates,
                count: 1,
                up_to: false,
                constraint: None,
                source_id: else_resolved.source_id,
            };
            return Ok(true);
        }
    }
    Ok(false)
}

/// CR 603.12: Begin reflexive target selection for a `WhenYouDo` /
/// `QuantityCheck` ability whose targets were deferred to resolution time.
/// Returns `true` when `WaitingFor::TriggerTargetSelection` (or inline random
/// resolution) was entered.
fn try_begin_reflexive_target_selection(
    state: &mut GameState,
    reflexive: &ResolvedAbility,
    parent: Option<&ResolvedAbility>,
    effect_context_object: Option<&CostPaidObjectSnapshot>,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<bool, EffectError> {
    if !reflexive.targets.is_empty() {
        return Ok(false);
    }

    // CR 608.2c + CR 603.12: The reflexive triggered ability is put on the
    // stack during the parent ability's resolution, so target legality must see
    // the same resolution-scoped referents that the pending trigger will later
    // carry. Stamp one clone before `build_target_slots` so chosen-player and
    // amassed-Army filters enumerate legal targets from the actual parent event.
    let reflexive_context_owned;
    let reflexive = if let Some(parent) = parent {
        let mut owned = reflexive.clone();
        apply_parent_chain_context(&mut owned, parent, effect_context_object, state);
        reflexive_context_owned = owned;
        &reflexive_context_owned
    } else {
        reflexive
    };

    // CR 700.2b + CR 603.3c: A reflexive MODAL trigger (Caesar, Legion's
    // Emperor) chooses its mode(s) when it is put on the stack — after the
    // optional cost was paid. Its own effect is a target-less modal marker, so
    // the generic target-slot path below would early-return and resolve the
    // modes unconditionally. Instead, push the reflexive ability as its own
    // pending trigger carrying the modal + per-mode abilities, then defer to the
    // shared modal-trigger router, which prompts `WaitingFor::AbilityModeChoice`
    // and only then collects each chosen mode's targets.
    if reflexive.modal.is_some() && !reflexive.mode_abilities.is_empty() {
        let reflexive_clone = reflexive.clone();
        let trigger_description = reflexive_clone
            .description
            .clone()
            .or_else(|| parent.and_then(|p| p.description.clone()));
        let source_id = parent.map(|p| p.source_id).unwrap_or(reflexive.source_id);
        let controller = parent.map(|p| p.controller).unwrap_or(reflexive.controller);

        let pending = crate::game::triggers::PendingTrigger {
            source_id,
            controller,
            condition: None,
            ability: reflexive_clone,
            timestamp: state.turn_number,
            target_constraints: reflexive.target_constraints.clone(),
            distribute: None,
            trigger_event: state.current_trigger_event.clone(),
            modal: reflexive.modal.clone(),
            mode_abilities: reflexive.mode_abilities.clone(),
            description: trigger_description,
            may_trigger_origin: None,
            // CR 603.12 + CR 601.2c: freeze the live event count (e.g. number
            // sacrificed) so an "up to that many target ..." bound survives
            // into the later fresh-`apply()` target-assign.
            subject_match_count: freeze_reflexive_event_count(state, controller, source_id),
            die_result: state.die_result_this_resolution,
        };
        let trigger_events =
            crate::game::triggers::take_pending_trigger_event_batch(state, &pending);
        let pending_for_state = pending.clone();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack_with_event_batch(
            state,
            pending,
            trigger_events,
            events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);

        match crate::game::engine::begin_pending_trigger_target_selection(state)
            .map_err(|e| EffectError::InvalidParam(e.to_string()))?
        {
            Some(wf) => {
                state.waiting_for = wf;
                return Ok(true);
            }
            // CR 700.2b: all modes illegal -> the ability can't be put on the
            // stack; the router already cleaned up the pushed entry. Do NOT fall
            // through (that would dangle a cleared pending_trigger). Return done.
            None => return Ok(true),
        }
    }

    let target_slots = crate::game::ability_utils::build_target_slots(state, reflexive)
        .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
    if target_slots.is_empty() {
        return Ok(false);
    }

    if matches!(
        reflexive.target_selection_mode,
        crate::types::ability::TargetSelectionMode::Random
    ) {
        // CR 115.1d + CR 603.12: Random-mode reflexive triggers still choose
        // the targets for the reflexive triggered ability; the seeded RNG
        // supplies that choice without entering an interactive prompt.
        let chosen = crate::game::ability_utils::random_select_targets_for_ability(
            state,
            &target_slots,
            &reflexive.target_constraints,
        )
        .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
        let mut reflexive_clone = reflexive.clone();
        crate::game::ability_utils::assign_targets_in_chain(state, &mut reflexive_clone, &chosen)
            .map_err(|e| EffectError::InvalidParam(e.to_string()))?;
        resolve_ability_chain(state, &reflexive_clone, events, depth + 1)?;
        return Ok(true);
    }

    let selection = crate::game::ability_utils::begin_target_selection_for_ability(
        state,
        reflexive,
        &target_slots,
        &reflexive.target_constraints,
    )
    .map_err(|e| EffectError::InvalidParam(e.to_string()))?;

    let reflexive_clone = reflexive.clone();
    let trigger_description = reflexive_clone
        .description
        .clone()
        .or_else(|| parent.and_then(|p| p.description.clone()));
    let source_id = parent.map(|p| p.source_id).unwrap_or(reflexive.source_id);
    let controller = parent.map(|p| p.controller).unwrap_or(reflexive.controller);

    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        controller,
        condition: None,
        ability: reflexive_clone,
        timestamp: state.turn_number,
        target_constraints: reflexive.target_constraints.clone(),
        distribute: None,
        trigger_event: state.current_trigger_event.clone(),
        modal: None,
        mode_abilities: vec![],
        description: trigger_description.clone(),
        may_trigger_origin: None,
        // CR 603.12 + CR 601.2c: freeze the live event count (e.g. number of
        // subjects sacrificed) so an "up to that many target ..." bound — an
        // `EventContextAmount` resolved against the firing action — survives
        // into the later fresh-`apply()` target-assign instead of collapsing
        // to 0.
        subject_match_count: freeze_reflexive_event_count(state, controller, source_id),
        // CR 706.2 + CR 603.12: capture the live die-roll result from the
        // creating ability so the reflexive entry can re-stamp it when it
        // resolves as its own stack object.
        die_result: state.die_result_this_resolution,
    };
    let trigger_events = crate::game::triggers::take_pending_trigger_event_batch(state, &pending);
    let pending_for_state = pending.clone();
    let prompt_trigger_event = pending_for_state.trigger_event.clone();
    let prompt_trigger_events = trigger_events.clone();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack_with_event_batch(
        state,
        pending,
        trigger_events,
        events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    // CR 115.1d + CR 603.3d: the reflexive triggered ability is on the stack
    // before targets are chosen; finalization mutates this pending entry once
    // the controller completes TriggerTargetSelection.
    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: controller,
        trigger_controller: Some(controller),
        trigger_event: prompt_trigger_event,
        trigger_events: prompt_trigger_events,
        target_slots,
        mode_labels: Vec::new(),
        target_constraints: reflexive.target_constraints.clone(),
        selection,
        source_id: Some(source_id),
        description: trigger_description,
    };
    Ok(true)
}

/// CR 120.1 + CR 608.2c + CR 115.10a: the "one-sided fight" chain shape — a
/// boost head ("Target creature you control gets +N/+M …") followed by a
/// `DealDamage`/`DamageAll` sub whose `damage_source = Target` ("It deals damage
/// equal to its power to target creature an opponent controls"). The anaphoric
/// "It"/"its power" names the creature the boost head chose, NOT the sub's own
/// fresh recipient (CR 608.2c — read the whole text). The sub carries only its
/// recipient in `targets` (it was assigned its own target slot in declaration
/// order), so the damage source (and the `Power{Anaphoric}` amount the runtime
/// resolves to `targets[0]`) would otherwise read the recipient. This predicate
/// recognizes that sub so the chain descent can prepend the parent's chosen
/// object target, restoring the `targets = [source, recipient]` contract the
/// `deal_damage` resolver (CR 120.1: the object that deals damage is the source)
/// and the `quantity::resolve_object_pt` one-sided-fight fallback both expect.
fn is_one_sided_fight_damage_sub(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DealDamage {
            damage_source: Some(crate::types::ability::DamageSource::Target),
            ..
        } | Effect::DamageAll {
            damage_source: Some(crate::types::ability::DamageSource::Target),
            ..
        }
    )
}

/// CR 120.1 + CR 601.2c: True when a sub-ability is the multi-source per-power
/// damage clause ("each deal damage equal to their power to <recipient>"). The
/// parent's whole object-target set is prepended ahead of the sub's recipient so
/// every source deals its own power (see the `EachTarget` resolver).
fn is_each_target_damage_sub(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DealDamage {
            damage_source: Some(crate::types::ability::DamageSource::EachTarget),
            ..
        } | Effect::DamageAll {
            damage_source: Some(crate::types::ability::DamageSource::EachTarget),
            ..
        }
    )
}

/// The first `TargetRef::Object` in a target list (the chain head's chosen
/// creature for the one-sided-fight prepend).
fn first_object_target(targets: &[TargetRef]) -> Option<ObjectId> {
    targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    })
}

fn apply_parent_chain_context(
    child: &mut ResolvedAbility,
    parent: &ResolvedAbility,
    effect_context_object: Option<&CostPaidObjectSnapshot>,
    state: &mut GameState,
) {
    child.context = parent.context.clone();
    // CR 401.5 + CR 608.2c (issue #1365): `state.last_dig_found_nothing` is
    // true for the narrow window between a `Dig` resolving an empty library
    // and the very next parent->child hand-off — this IS that hand-off, so
    // stamp the typed, per-ability signal onto `child` and consume the
    // transient global flag immediately. Consuming here (rather than where
    // `child` is later resolved) means the signal can never reach a second,
    // unrelated hand-off later in the same resolution, and a child that was
    // never handed off this way (e.g. a freshly-built ability in a test)
    // simply keeps the default `false` regardless of stray global state.
    if state.last_dig_found_nothing {
        child.dig_found_nothing_for_parent_target = true;
        state.last_dig_found_nothing = false;
    }
    // CR 608.2c: A sub-ability is part of the same printed ability instance as
    // its parent; its instructions are followed in order during a single
    // resolution. Propagate the parent's `ability_index` so chain-level
    // `AbilityCondition::NthResolutionThisTurn` gates can identify "this ability"
    // when evaluated on a chained sub-ability. The per-turn resolution counter is
    // keyed on `(source_id, ability_index)`; without this the sub carries no
    // index and the gate always evaluates false, so e.g. Nissa, Resurgent
    // Animist's "Then if this is the second time this ability has resolved this
    // turn, reveal ..." never fires its second-resolution half. Sub-abilities
    // always resolve at depth > 0, so propagating the index never causes a
    // spurious counter bump (that happens only at the depth-0 top-level
    // resolution). Guarded on `is_none()` to never clobber an explicit index.
    if child.ability_index.is_none() {
        child.ability_index = parent.ability_index;
    }
    // CR 608.2c + CR 109.4: Carry the resolution-scoped chosen-players list
    // down the chain so `ControllerRef::ChosenPlayer { index }` and later
    // `Choose(Player)` instructions resolve against players chosen by earlier
    // `Choose(Player)` instructions in the same resolution. Only propagate
    // when the parent has accumulated choices and the child has not already
    // received a longer list (the `NamedChoice` answer handler appends to the
    // continuation chain directly, which can run ahead of this copy).
    if !parent.chosen_players.is_empty() && parent.chosen_players.len() > child.chosen_players.len()
    {
        child.set_chosen_players_recursive(&parent.chosen_players);
    }
    if let Some(snapshot) = parent.amassed_army_object.as_ref() {
        child.set_amassed_army_object_recursive(snapshot.clone());
    }
    if let Some(snapshot) = effect_context_object {
        child.set_effect_context_object_recursive(snapshot.clone());
    }
}

fn waits_for_resolution_choice(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::ScryChoice { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::NamedChoice { .. }
            // CR 608.2d + CR 608.2e: an opponent's guess is a deferred outcome —
            // the answer arrives via a `WaitingFor` round-trip, so the
            // `Guessed`-gated branch chain must auto-stash here and re-evaluate on
            // drain once the outcome is stamped (the same deferred pattern as
            // `NamedChoice` / Scry "If you do, Y").
            | WaitingFor::OpponentGuess { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::MultiTargetSelection { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::UnlessPayment { .. }
            | WaitingFor::UnlessPaymentChooseCost { .. }
            | WaitingFor::PairChoice { .. }
            | WaitingFor::OpponentMayChoice { .. }
            | WaitingFor::TributeChoice { .. }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
                ..
            }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::GraveyardPaidCast { .. },
                ..
            }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { .. },
                ..
            }
            // CR 608.2g + CR 608.2c: Invoke Calamity's free-cast window pauses
            // resolution; its "Exile ~" sub-ability must run only after the
            // window finishes, so stash it as a continuation here.
            | WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::ProliferateChoice { .. }
            | WaitingFor::TimeTravelChoice { .. }
            | WaitingFor::ChooseObjectsSelection { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::CopyRetarget { .. }
            | WaitingFor::DistributeAmong { .. }
            | WaitingFor::MoveCountersDistribution { .. }
            // CR 608.2c + CR 608.2h: a "remove any number of counters" prompt
            // pauses resolution; Tetravus's chained "create that many ... tokens"
            // sub-ability must run only after the removal count is known, so
            // stash it as a continuation here (drained after the removal stamps
            // `last_effect_count`).
            | WaitingFor::RemoveCountersChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
            | WaitingFor::RetargetChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::ChooseManaColor { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::EachPlayerCopyChosenSelection { .. }
            | WaitingFor::KeepWithinTotalPowerChoice { .. }
            | WaitingFor::LearnChoice { .. }
            // Digital-only Alchemy spellbook choice pauses resolution; stash
            // the printed tail until SubmitSpellbookDraft resumes the chain.
            | WaitingFor::SpellbookDraft { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::BeholdChoice { .. }
    )
}

pub(super) fn resolve_optional_effect_decision(
    state: &mut GameState,
    mut ability: ResolvedAbility,
    choice: AutoMayChoice,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    ability.optional = false;
    match choice {
        AutoMayChoice::Accept => {
            ability.context.optional_effect_performed = true;
            state
                .player_actions_this_way
                .insert((ability.controller, PlayerActionKind::AcceptedOptionalEffect));
            resolve_ability_chain(state, &ability, events, depth)?;
            // CR 608.2c: When an optional effect's prompt suspended the parent
            // chain, the "If you do" sibling continuation was stashed BEFORE the
            // player chose — so its context still carries
            // `optional_effect_performed = false`. Now that the effect has been
            // performed, propagate the signal into the stashed continuation so
            // its `IfYouDo` gate evaluates true (e.g. Ral, Monsoon Mage:
            // "you may exile Ral. If you do, return him transformed").
            if let Some(cont) = state.pending_continuation.as_mut() {
                cont.chain.set_optional_effect_performed_recursive(true);
            }
        }
        AutoMayChoice::Decline => {
            let decline_branch = ability.else_ability.as_deref().or_else(|| {
                let sub = ability.sub_ability.as_deref()?;
                // CR 608.2c: a conditioned decline branch (IfYouDo /
                // Otherwise / composite) resolves on decline — authoritative
                // check.
                let selected = should_resolve_subability_on_optional_decline(sub)
                    // CR 608.2c: a separate-sentence sibling is the next
                    // printed instruction and resolves regardless of the
                    // optional decision — BUT only when it is not a
                    // reflexive trigger. CR 603.12: a reflexive ("When you
                    // do, …") sub's "do" did not occur when the action was
                    // declined, so it must NOT fire even though it is a
                    // separate sentence (issue #3179: Swashbuckler
                    // Extraordinaire's declined Treasure sacrifice must not
                    // resolve the double-strike reflexive). CastFromZone's
                    // graveyard-redirect rider is not a printed follow-up to
                    // execute on decline; it is permission metadata consumed
                    // only if the graveyard spell is actually cast.
                    || (sub.sub_link == SubAbilityLink::SequentialSibling
                        && !sub_ability_is_reflexive(sub)
                        && !(matches!(&ability.effect, Effect::CastFromZone { .. })
                            && (cast_from_zone::graveyard_destination_rider(sub).is_some()
                                // CR 614.1c + CR 122.1: the enters-with-counter
                                // rider is permission metadata (Osteomancer
                                // Adept, The Tomb of Aclazotz), not a printed
                                // follow-up to execute on decline.
                                || cast_from_zone::is_enters_with_counter_rider_subability(sub))));
                if !selected {
                    return None;
                }
                // CR 608.2c: An `IfYouDo` head with no `else_ability` gates its
                // ENTIRE accept body ("if you do, A and B") on the optional
                // decision. On decline, run ONLY the trailing
                // `Not(OptionalEffectPerformed)` clause ("if you don't, C") —
                // never the accept-only instructions chained under the head
                // (Omnath, Locus of All: declining the reveal must put the card
                // in hand without adding mana). For every other selected shape
                // (direct `Not` head, `else_ability` head whose condition-false
                // path runs the else, composite `And`/`Or` head re-evaluated
                // post-decline, or a `SequentialSibling`) the head itself is the
                // branch to resolve.
                if sub
                    .condition
                    .as_ref()
                    .is_some_and(|c| c.is_optional_effect_performed())
                    && sub.else_ability.is_none()
                {
                    Some(nested_optional_decline_clause(sub).unwrap_or(sub))
                } else {
                    Some(sub)
                }
            });
            if let Some(branch) = decline_branch {
                let mut resolved = branch.clone();
                // CR 608.2c: inherit the parent's resolved object targets ONLY
                // when the decline clause's effect actually anaphors the parent
                // target (`ParentTarget`) — e.g. Omnath, Locus of All's "if you
                // don't, put IT into your hand", where the looked-at card is the
                // parent's target. Mirrors the sub-ability target threading the
                // condition-false descent applies in `resolve_ability_chain`,
                // required here because an extracted nested decline clause is
                // resolved directly rather than reached through that descent. The
                // `effect_refs_parent_target` guard keeps a decline clause that
                // carries its OWN target filter (e.g. a Chaos-Wand cleanup that
                // returns `ExiledBySource` cards) from being clobbered with the
                // parent's targets.
                if resolved.targets.is_empty()
                    && !ability.targets.is_empty()
                    && effect_refs_parent_target(&resolved.effect)
                {
                    resolved.targets = ability.targets.clone();
                }
                resolved.context = ability.context.clone();
                // CR 608.2c: This optional effect was DECLINED — the decline
                // branch's `Not{IfYouDo}` / `IfYouDo` gate must evaluate
                // relative to *this* decision, not a stale `true` inherited
                // from an enclosing optional effect that was accepted (e.g.
                // Braids: the controller accepted the outer sacrifice, so the
                // chain context carries `optional_effect_performed = true` —
                // but each opponent's own decline must read `false`).
                resolved.context.optional_effect_performed = false;
                resolve_ability_chain(state, &resolved, events, depth)?;
            }
        }
    }
    Ok(())
}

/// Whether a sub-ability condition references a per-iteration outcome gate —
/// "was the effect performed" (`IfYouDo` / `IfAPlayerDoes`, CR 118.12
/// optional-cost branch) or "did the current scope iteration succeed"
/// (`IfCurrentScopeSucceeded`, CR 101.3 + CR 118.12 mandatory-cost branch),
/// including a `Not`-wrapped form or a composite `And`/`Or` that contains
/// one. Such conditions cannot be evaluated while the parent effect is
/// suspended for a player choice — the answer is not yet known — so the
/// sub-ability must be deferred as a continuation rather than gated eagerly.
/// They are also load-bearing for `detach_after_player_scope_local_chain`:
/// a per-iteration outcome gate has meaning ONLY relative to its surrounding
/// scoped iteration, so the sub-ability must stay inside the scoped template
/// rather than detach as an unscoped post-loop tail. A composite like
/// `Or { [IfYouDo, QuantityCheck] }` (Armored Kincaller) also qualifies:
/// declining the optional effect leaves `IfYouDo` false, but the sibling
/// disjunct may still be satisfied, so the sub-ability must be re-evaluated
/// rather than dropped. Predicate helper, not rule-implementing code.
fn condition_depends_on_effect_performed(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::EffectOutcome { .. } => true,
        AbilityCondition::Not { condition } => condition_depends_on_effect_performed(condition),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(condition_depends_on_effect_performed)
        }
        _ => false,
    }
}

/// CR 603.12 + CR 608.2c: Whether a reflexive condition reads the per-resolution
/// `last_zone_changed_ids` ledger ("if a [noun] was [verb]ed this way"). Unlike
/// `condition_depends_on_effect_performed` (which gates on the
/// `optional_effect_performed` flag), this class is evaluated against the set of
/// objects the parent effect MOVED — a set that does not exist yet when the move
/// pauses for an interactive choice (e.g. a `discard a card` with hand > 1, or a
/// `sacrifice a permanent` pick). It must therefore be deferred across the choice
/// alongside the `WhenYouDo` reflexive, then re-evaluated once the choice
/// resolves and `last_zone_changed_ids` reflects the moved objects. Predicate
/// helper, not rule-implementing code.
fn condition_depends_on_zone_change_this_way(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::ZoneChangedThisWay { .. } => true,
        AbilityCondition::Not { condition } => condition_depends_on_zone_change_this_way(condition),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => conditions
            .iter()
            .any(condition_depends_on_zone_change_this_way),
        _ => false,
    }
}

/// CR 608.2c + CR 400.7j: Whether a condition reads the parent's *suspended-
/// selection result object* — the found/revealed card that a `SearchLibrary`
/// injects as the continuation target only after the player responds
/// (`engine_resolution_choices.rs`, `cont.chain.targets = continuation_targets`).
/// A `TargetMatchesFilter` gate on "that card" ("the revealed card is the chosen
/// type") cannot be evaluated until then, so it must be deferred WITH its
/// condition rather than eagerly read against absent targets. Recurses And/Or/Not
/// like the sibling `condition_depends_on_effect_performed` predicate. Predicate
/// helper, not rule-implementing code — the CR annotation lives at the gate.
fn condition_depends_on_result_object(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::TargetMatchesFilter { .. } => true,
        AbilityCondition::Not { condition } => condition_depends_on_result_object(condition),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(condition_depends_on_result_object)
        }
        _ => false,
    }
}

/// CR 603.12: Whether a sub-ability is a *reflexive* trigger — its "do"
/// depends on whether the just-prompted action actually occurred during this
/// resolution. A reflexive sub MUST NOT resolve when the optional parent was
/// declined (the "do" did not happen), regardless of its sentence-boundary
/// `sub_link`. Covers the bare `WhenYouDo` reflexive (CR 603.12 Heart-Piercer
/// Manticore) and any condition that reads a per-iteration effect outcome
/// (`IfYouDo` / composite `Or{[IfYouDo,…]}`). Predicate helper, not rule code.
fn sub_ability_is_reflexive(sub: &ResolvedAbility) -> bool {
    match &sub.condition {
        Some(AbilityCondition::WhenYouDo) => true,
        Some(condition) => condition_depends_on_effect_performed(condition),
        None => false,
    }
}

fn condition_contains_city_blessing(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::HasCityBlessing => true,
        AbilityCondition::Not { condition } => condition_contains_city_blessing(condition),
        AbilityCondition::ConditionInstead { inner } => condition_contains_city_blessing(inner),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(condition_contains_city_blessing)
        }
        _ => false,
    }
}

/// CR 608.2c: Whether a parent effect computes its own "if you do" outcome
/// signal (`optional_effect_performed`) rather than that signal meaning "the
/// mandatory action occurred."
///
/// For most effects, "[Action]. If you do, [rider]." means the rider fires iff
/// the action was performed. An *optional* ("you may") parent sets the flag when
/// the controller accepts. A handful of effects instead compute the flag from a
/// random/choice OUTCOME — a coin flip's win/loss, a clash's win/tie, a dig's
/// kept selection — and resolve their gated branch against that computed flag
/// (see `flip_coin.rs`, `clash.rs`, `engine_resolution_choices.rs`). For those,
/// a mandatory parent does NOT imply "performed" (a lost flip is mandatory but
/// did not "win"), so the default-true rule below must exclude them.
fn effect_manages_own_outcome_flag(effect: &Effect) -> bool {
    // Redundant-but-safe: coin/die win-loss riders gate on `EventOutcomeWon` /
    // `WhenYouDo`, which are NOT in `condition_depends_on_effect_performed`, so
    // the seed-block guard above would already skip them. Excluding the
    // FlipCoin*/RollDie/Clash/Dig set here is defensive belt-and-suspenders — a
    // future reader should not assume those paths depend on this exclusion.
    matches!(
        effect,
        Effect::FlipCoin { .. }
            | Effect::FlipCoins { .. }
            | Effect::FlipCoinUntilLose { .. }
            | Effect::Clash
            | Effect::RollDie { .. }
            | Effect::Dig { .. }
            // CR 701.4a + CR 608.2c: behold computes its own performed outcome
            // (whiff → `cost_payment_failed_flag`; beheld → the rider fires), so
            // the mandatory-rider seed must not race it.
            | Effect::Behold { .. }
    )
}

fn effect_writes_last_revealed_ids(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::RevealTop { .. }
            | Effect::Dig { .. }
            | Effect::RevealUntil { .. }
            | Effect::Clash
            | Effect::TurnFaceUp { .. }
            // CR 701.20: a targeted object reveal records the revealed object so
            // a chained "If it's a creature card, …" rider and an anaphoric
            // "turn it face up" follow-up read it (Hauntwoods Shrieker).
            | Effect::Reveal { .. }
    )
}

fn target_filter_for_last_revealed_sub(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::CastFromZone { target, .. } => Some(target),
        Effect::PutAtLibraryPosition { target, .. } => Some(target),
        _ => None,
    }
}

fn inject_last_revealed_targets(
    state: &GameState,
    parent_ability: &ResolvedAbility,
    sub: &ResolvedAbility,
) -> Vec<TargetRef> {
    if let Some(filter) = target_filter_for_last_revealed_sub(&sub.effect) {
        let ctx = crate::game::filter::FilterContext::from_ability(parent_ability);
        return crate::game::filter::last_revealed_library_ids_matching(state, filter, &ctx)
            .into_iter()
            .map(TargetRef::Object)
            .collect();
    }
    state
        .last_revealed_ids
        .iter()
        .map(|&id| TargetRef::Object(id))
        .collect()
}

/// CR 608.2c: Locate the `Not(OptionalEffectPerformed)` decline clause anywhere
/// in an `IfYouDo` head's accept-body sub-chain. A parent optional ability has a
/// single `sub_ability` link, so the accept body ("If you do, A and B") and the
/// decline continuation ("If you don't, C") form one linked chain: when the
/// accept body has two or more instructions, the decline clause C is nested
/// BELOW them rather than as the head's direct child (Omnath, Locus of All:
/// "If you do, add mana AND put it into hand. If you don't, put it into hand").
/// Walking the chain finds C at any depth. Returning the precise decline node
/// lets the resolver run ONLY it on decline — never the accept-only instructions
/// gated by "if you do" (CR 608.2c: follow the instructions in the order
/// written). For the single-instruction Springheart Nantuko shape (C is the
/// direct child) this returns that same direct child. Locator helper.
fn nested_optional_decline_clause(ability: &ResolvedAbility) -> Option<&ResolvedAbility> {
    let mut current = ability.sub_ability.as_deref();
    while let Some(node) = current {
        if node
            .condition
            .as_ref()
            .is_some_and(AbilityCondition::is_not_optional_effect_performed)
        {
            return Some(node);
        }
        current = node.sub_ability.as_deref();
    }
    None
}

fn should_resolve_subability_on_optional_decline(ability: &ResolvedAbility) -> bool {
    match ability.condition {
        Some(AbilityCondition::Not { ref condition })
            if condition.is_optional_effect_performed() =>
        {
            true
        }
        Some(AbilityCondition::Not { .. }) => false,
        // CR 609.3: An `IfYouDo` sub-ability is a valid decline branch when it
        // carries an alternative for the "you didn't" case — either as an
        // explicit `else_ability` ("If you do X. Otherwise Y.") OR as a nested
        // `sub_ability` whose own condition is the `Not`-wrapped performed gate
        // ("If you do X. If you didn't, Y."). Springheart Nantuko has the
        // latter shape: `CopyTokenOf {IfYouDo}` → `Token(Insect) {Not(IfYouDo)}`.
        // Selecting the `IfYouDo` head here lets `resolve_ability_chain`'s
        // condition-false path descend into the `Not(IfYouDo)` tail so the
        // Insect token is still created when the optional pay is declined.
        //
        // CR 608.2c: A `player_scope`-scoped `IfYouDo` sub is the next printed
        // instruction after the parent action ("If you do, each opponent may …"),
        // not a decline alternative for the parent's optional decision. Braids,
        // Arisen Nightmare: declining the controller's sacrifice must skip the
        // entire scoped opponent chain — its nested `Not{IfYouDo}` decline body
        // is per-opponent, not a fallback for the controller's decline.
        Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        }) => {
            // CR 608.2c: The `Not(OptionalEffectPerformed)` decline clause may be
            // the head's direct child (single-instruction accept body —
            // Springheart Nantuko) or a deeper descendant (multi-instruction
            // accept body — Omnath, Locus of All). `nested_optional_decline_clause`
            // walks the chain to find it at any depth.
            ability.else_ability.is_some()
                || (ability.player_scope.is_none()
                    && nested_optional_decline_clause(ability).is_some())
        }
        // CR 608.2c + CR 608.2d: A composite `And`/`Or` condition that contains
        // a performed-gate is a valid decline branch — declining the
        // resolution-time "you may" leaves `IfYouDo` false, but a sibling
        // clause may still be satisfied.
        // Armored Kincaller: `GainLife { Or { [IfYouDo, QuantityCheck] } }` —
        // declining the optional reveal must still gain life when the
        // "control another Dinosaur" disjunct holds. Descending lets
        // `resolve_ability_chain`'s top-level condition check re-evaluate the
        // composite against the post-decline state. A composite with no
        // performed-gate is not a decline branch (its truth is unchanged by
        // the decline) and falls through to the `false` arm below.
        Some(
            AbilityCondition::And { ref conditions } | AbilityCondition::Or { ref conditions },
        ) => conditions.iter().any(condition_depends_on_effect_performed),
        // Every other condition shape: declining the optional effect does not
        // select a sub-ability branch. Exhaustive — a new `AbilityCondition`
        // variant must be classified here deliberately, not silently defaulted.
        None
        | Some(
            AbilityCondition::AdditionalCostPaid { .. }
            | AbilityCondition::AdditionalCostPaidInstead
            | AbilityCondition::AlternativeManaCostPaid
            | AbilityCondition::EventOutcomeWon
            | AbilityCondition::WhenYouDo
            | AbilityCondition::CastFromZone { .. }
            | AbilityCondition::CastDuringPhase { .. }
            // CR 608.2c: a live current-phase gate, not a decline-alternative
            // selector — declining the optional effect does not pick a
            // `CurrentPhaseIs` branch. Dose of Dawnglow's `Not`-wrapped form is
            // classified earlier by the `Some(Not { .. }) => false` arm above.
            | AbilityCondition::CurrentPhaseIs { .. }
            | AbilityCondition::CastTimingPermission { .. }
            | AbilityCondition::ManaColorSpent { .. }
            | AbilityCondition::RevealedHasCardType { .. }
            | AbilityCondition::ObjectsShareQuality { .. }
            | AbilityCondition::TargetSharesNameWithOtherExiledThisWay { .. }
            | AbilityCondition::SourceEnteredThisTurn
            | AbilityCondition::CastVariantPaid { .. }
            | AbilityCondition::CastVariantPaidInstead { .. }
            | AbilityCondition::QuantityCheck { .. }
            | AbilityCondition::PreviousEffectAmount { .. }
            | AbilityCondition::HasMaxSpeed
            | AbilityCondition::IsMonarch
            | AbilityCondition::IsInitiative
            | AbilityCondition::HasCityBlessing
            | AbilityCondition::IsRingBearer
            | AbilityCondition::TargetHasKeywordInstead { .. }
            | AbilityCondition::TargetMatchesFilter { .. }
            // CR 601.2c: a presence guard, not a decline-alternative selector —
            // declining the optional effect does not pick a `HasObjectTarget` branch.
            | AbilityCondition::HasObjectTarget
            | AbilityCondition::TriggeringSpellTargetsFilter { .. }
            | AbilityCondition::SourceMatchesFilter { .. }
            | AbilityCondition::ZoneChangeObjectMatchesFilter { .. }
            | AbilityCondition::ControllerControlsMatching { .. }
            | AbilityCondition::ControllerControlledMatchingAsCast { .. }
            | AbilityCondition::IsYourTurn
            | AbilityCondition::WasStartingPlayer { .. }
            | AbilityCondition::SpellCastWithVariantThisTurn { .. }
            | AbilityCondition::FirstCombatPhaseOfTurn
            | AbilityCondition::FirstEndStepOfTurn
            | AbilityCondition::ZoneChangedThisWay { .. }
            | AbilityCondition::CostPaidObjectMatchesFilter { .. }
            | AbilityCondition::SourceIsTapped
            | AbilityCondition::SourceAttachedToCreature
            | AbilityCondition::ConditionInstead { .. }
            | AbilityCondition::DayNightIsNeither
            | AbilityCondition::DayNightIs { .. }
            | AbilityCondition::NthResolutionThisTurn { .. }
            | AbilityCondition::SourceLacksKeyword { .. }
            | AbilityCondition::ScopedPlayerMatches { .. }
            | AbilityCondition::EffectOutcome {
                signal:
                    EffectOutcomeSignal::CurrentScopeSucceeded
                    // CR 608.2d: a `Guessed` gate is a positive guess-outcome
                    // branch, not an optional-decline alternative — declining an
                    // optional effect never selects it.
                    | EffectOutcomeSignal::Guessed { .. },
            },
        ) => false,
    }
}

fn is_player_scope_local_continuation(parent: &Effect, child: &Effect) -> bool {
    // CR 109.5 + CR 608.2c: Compound-subject distribution — when the parent
    // and child effects of a player_scope-tagged ability both reference an
    // iteration-bound recipient (`OriginalController` or `ScopedPlayer`), the
    // pair is the parser-emitted "you and that player each <body>" chain
    // (Master of Ceremonies). Both halves must run inside the SAME scoped
    // iteration (so their recipients rebind together) — never detached as the
    // unscoped tail. Detecting via recipient filter rather than effect kind
    // keeps the rule generic across body families (Token, Draw, etc.).
    if effect_has_iteration_bound_recipient(parent) && effect_has_iteration_bound_recipient(child) {
        return true;
    }
    matches!(
        (parent, child),
        (
            Effect::SearchLibrary { .. },
            Effect::ChangeZone {
                origin: Some(crate::types::zones::Zone::Library),
                ..
            }
        ) | (Effect::SearchLibrary { .. }, Effect::Shuffle { .. })
            | (
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Library),
                    ..
                },
                Effect::Shuffle { .. }
            )
    )
}

/// CR 109.5 + CR 115.10 + CR 119.3: Detect that an effect's recipient is bound
/// to the surrounding `player_scope` iteration — either `OriginalController`
/// (CR 109.5: the printed ability controller, fixed) or `ScopedPlayer` (CR
/// 115.10: the per-iteration acting player). Used to keep parser-distributed
/// chains inside the scoped template: compound-subject "you and that player
/// each ..." distribution, and per-opponent decline-consequence bodies. A
/// `ScopedPlayer`-recipient `LoseLife` is the same iteration-bound recipient
/// class as `Draw`/`Discard`/`Mill`/`Token` — not a separate pattern.
fn effect_has_iteration_bound_recipient(effect: &Effect) -> bool {
    let recipient = match effect {
        Effect::Token { owner, .. } => owner,
        Effect::Draw { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Mill { target, .. } => target,
        // CR 119.3 + CR 115.10: a directed LoseLife whose recipient is the
        // scoped opponent or the printed controller is iteration-bound exactly
        // like Draw/Discard/Mill — keep its continuation inside the scope.
        Effect::LoseLife { target, .. } => match target.as_ref() {
            Some(t) => t,
            None => return false,
        },
        _ => return false,
    };
    matches!(
        recipient,
        TargetFilter::OriginalController | TargetFilter::ScopedPlayer
    )
}

/// CR 608.2c + CR 701.20b: An effect that introduces a single per-player object
/// referent which a later anaphoric clause ("that card", "it", "its mana value")
/// in the same iteration binds to. `RevealTop` is the canonical case: it reveals
/// one card per scoped player, captured into `effect_context_object` and exposed
/// to later clauses as `Demonstrative`/`Anaphoric`/`ParentTarget`. This referent
/// is rebuilt fresh each iteration, so a consuming sub-clause MUST run inside the
/// same iteration as its introducer.
fn effect_introduces_per_player_object_referent(effect: &Effect) -> bool {
    matches!(effect, Effect::RevealTop { .. })
}

/// CR 608.2c: True when an effect (or its quantity/target) reads the parent's
/// per-player object referent anaphorically — a `Demonstrative`/`Anaphoric`
/// object scope in a quantity, or a `ParentTarget`/`ParentTargetSlot` target.
/// Distinguishes a per-player anaphoric consumer (Duskmantle Seer's
/// `LoseLife { that card's mana value }` and `ChangeZone { ParentTarget }`) from
/// a cross-player aggregate sub-clause (Windfall's `Draw { PreviousEffectAmount }`),
/// which references the table-wide outcome and must NOT be merged into one
/// iteration.
fn effect_consumes_parent_object_referent(effect: &Effect) -> bool {
    // An anaphoric consumer reads the parent's per-player object either as a
    // quantity ("loses life equal to that card's mana value") or as a target
    // ("then puts it into their hand" → `ChangeZone { target: ParentTarget }`).
    let mut quantities = Vec::new();
    collect_effect_quantity_exprs(effect, &mut quantities);
    let amount_is_anaphoric = quantities
        .iter()
        .any(|qty| quantity_expr_references_demonstrative(qty));
    let target_is_anaphoric = match effect {
        Effect::ChangeZone { target, .. } => filter_is_parent_object_anaphor(target),
        _ => false,
    };
    amount_is_anaphoric || target_is_anaphoric
}

fn quantity_expr_references_demonstrative(qty: &QuantityExpr) -> bool {
    match qty {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => quantity_ref_references_demonstrative(qty),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        }
        | QuantityExpr::DivideRounded { inner, .. } => {
            quantity_expr_references_demonstrative(inner)
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_expr_references_demonstrative)
        }
        QuantityExpr::Difference { left, right } => {
            quantity_expr_references_demonstrative(left)
                || quantity_expr_references_demonstrative(right)
        }
    }
}

fn quantity_ref_references_demonstrative(qty: &QuantityRef) -> bool {
    use crate::types::ability::ObjectScope;
    let scope = match qty {
        QuantityRef::ObjectManaValue { scope }
        | QuantityRef::Power { scope }
        | QuantityRef::Toughness { scope }
        | QuantityRef::CountersOn { scope, .. }
        | QuantityRef::ObjectColorCount { scope }
        | QuantityRef::ObjectNameWordCount { scope }
        | QuantityRef::ObjectTypelineComponentCount { scope }
        | QuantityRef::ManaSymbolsInManaCost { scope, .. } => scope,
        _ => return false,
    };
    // CR 608.2c: `OtherRevealedCard` is a per-player object referent too — the
    // cross-reader reads it by exclusion FROM `effect_context_object` (the
    // iteration's own revealed card), so the lose clause is a per-player-referent
    // consumer that must run inside the same fan-out iteration as the reveal.
    matches!(
        scope,
        ObjectScope::Demonstrative | ObjectScope::Anaphoric | ObjectScope::OtherRevealedCard
    )
}

fn filter_is_parent_object_anaphor(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
    )
}

fn detach_after_player_scope_local_chain(
    node: &mut ResolvedAbility,
    scope: &PlayerFilter,
    referent_introduced: bool,
) -> Option<Box<ResolvedAbility>> {
    let mut next = node.sub_ability.take()?;
    // CR 115.10 + CR 608.2c: a decline-branch sub-ability whose own condition is
    // a performed-gate (IfYouDo / IfAPlayerDoes / Not-wrapped / composite) is
    // per-opponent by construction — its gate only has meaning relative to the
    // scoped player's own optional decision. It must stay inside the scoped
    // template, never detach as the unscoped tail.
    let next_is_performed_gated = next
        .condition
        .as_ref()
        .is_some_and(condition_depends_on_effect_performed);
    // CR 608.2c: When the parser distributes "each player reveals the top card
    // of their library, loses life equal to that card's mana value, then puts
    // it into their hand" it stamps the SAME `player_scope` onto every clause.
    // The reveal introduces a PER-PLAYER object referent (`effect_context_object`)
    // that EVERY following clause binds anaphorically ("that card's mana value",
    // "it") — each consumer refers to the upstream introducer, not the clause
    // immediately before it. That referent is rebuilt each iteration, so once
    // an introducer has been seen, all downstream co-scoped anaphoric consumers
    // MUST run inside the SAME iteration: never detached as the unscoped tail
    // (which would run once) and never re-entering the driver to fan out a
    // second loop (their redundant `player_scope` is cleared). This is strictly
    // narrower than "shares the parent's scope": a co-scoped sub that reads a
    // CROSS-PLAYER aggregate instead (Windfall's "then draws that many cards" →
    // `PreviousEffectAmount`, the greatest discarded table-wide) is NOT a
    // referent consumer, so it still runs as its own post-all-iterations loop.
    let referent_in_scope =
        referent_introduced || effect_introduces_per_player_object_referent(&node.effect);
    let next_is_co_scoped_anaphoric_consumer = next.player_scope.as_ref() == Some(scope)
        && referent_in_scope
        && effect_consumes_parent_object_referent(&next.effect);
    if next_is_co_scoped_anaphoric_consumer {
        next.player_scope = None;
    }
    // "Each opponent may X and Y" makes the whole same-sentence X/Y clause
    // optional for that opponent. Keep the continuation inside the scoped
    // template so accepting the offer performs both instructions.
    let next_is_optional_clause_continuation =
        node.optional && next.sub_link == SubAbilityLink::ContinuationStep;
    if next_is_performed_gated
        || next_is_co_scoped_anaphoric_consumer
        || next_is_optional_clause_continuation
        || is_player_scope_local_continuation(&node.effect, &next.effect)
    {
        let tail = detach_after_player_scope_local_chain(&mut next, scope, referent_in_scope);
        node.sub_ability = Some(next);
        tail
    } else {
        Some(next)
    }
}

fn split_player_scope_chain(
    ability: &ResolvedAbility,
    scope: &PlayerFilter,
) -> (ResolvedAbility, Option<Box<ResolvedAbility>>) {
    let mut scoped = ability.clone();
    scoped.player_scope = None;
    let tail = detach_after_player_scope_local_chain(&mut scoped, scope, false);
    (scoped, tail)
}

/// CR 608.2e: Collect cross-player equalization quantity references from a
/// `QuantityExpr`. These are the refs whose value would shift as an APNAP
/// fan-out mutates the board — `ControlledByEachPlayer` (battlefield extremum)
/// and `HandSize { AllPlayers }` (hand extremum). The per-player `left` operand
/// of a `Difference` is intentionally NOT collected: it must re-resolve per
/// iterating player.
fn collect_clause_minimum_refs<'a>(expr: &'a QuantityExpr, out: &mut Vec<&'a QuantityRef>) {
    match expr {
        QuantityExpr::Ref { qty } => {
            if matches!(
                qty,
                QuantityRef::ControlledByEachPlayer { .. }
                    | QuantityRef::HandSize {
                        player: PlayerScope::AllPlayers { .. }
                    }
            ) {
                out.push(qty);
            }
        }
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => collect_clause_minimum_refs(inner, out),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for e in exprs {
                collect_clause_minimum_refs(e, out);
            }
        }
        QuantityExpr::Difference { left, right } => {
            collect_clause_minimum_refs(left, out);
            collect_clause_minimum_refs(right, out);
        }
    }
}

/// CR 608.2e (§8): Capture this `player_scope` link's equalization extrema
/// against the board as it stands NOW — before the APNAP fan-out begins. The
/// snapshot is stored on `state.clause_minimum_snapshot` and consulted by the
/// `ControlledByEachPlayer` / `HandSize { AllPlayers }` resolver arms so every
/// player in the fan-out sees the same pre-clause minimum.
///
/// Always overwrites `state.clause_minimum_snapshot` — to `Some` when the
/// clause carries a cross-player extremum, to `None` otherwise. This makes
/// every `player_scope` link entry a fresh per-link reset point, so clause N+1
/// (whose `after_scope` recursion re-enters the driver) never inherits clause
/// N's frozen value. It must NOT be cleared on the interactive-pause path:
/// when clause N pauses mid-fan-out, the remaining clause-N players resume via
/// `pending_continuation` as bare single-scoped effects that do NOT re-enter
/// this driver, so they depend on the snapshot persisting. The depth-0
/// chain-entry clear in `resolve_ability_chain` disposes of any residual
/// snapshot once the whole resolution ends.
///
/// The snapshot field is cleared to `None` as the very first step here, before
/// any ref is resolved. This makes the live-resolve below genuinely happen with
/// no stale snapshot in `state` — clause N+1's capture starts from a clean
/// slate and never reads clause N's frozen value, so each ref evaluates live
/// exactly once here against the clause's pre-clause board. The safety is
/// structural rather than relying on the three cards' clauses using
/// pairwise-distinct `QuantityRef` keys.
fn capture_clause_minimum_snapshot(state: &mut GameState, scoped_template: &ResolvedAbility) {
    // CR 608.2e: values are locked when the clause starts resolving, so each
    // clause must capture against its own pre-clause board.
    //
    // Per-link reset: clear any previous clause's snapshot before resolving so
    // the live-resolve below sees a clean slate and a stale value is never
    // consulted, even when clause N+1 shares a `QuantityRef` key with clause N.
    //
    // The unconditional clear is LOAD-BEARING for clause-independence within a
    // single Balance resolution: clause N's snapshot may legitimately still be
    // present here (clause N's `after_scope` recursion intentionally leaves it
    // for `apply()` to dispose of), and we must NOT inherit it into clause
    // N+1's capture. This also relies on the single-cell invariant documented
    // on `ClauseMinimumSnapshot` — any previous snapshot is consumed here by
    // being overwritten, which is sound only because no nested player-scope
    // ability-chain resolution occurs during a clause's fan-out (see the
    // type's doc-comment). If that invariant is ever broken in the future,
    // the field must become a Vec stack and this unconditional clear must
    // become a stack push.
    state.clause_minimum_snapshot = None;
    let mut exprs = Vec::new();
    collect_ability_quantity_exprs(scoped_template, &mut exprs);
    let mut refs: Vec<&QuantityRef> = Vec::new();
    for expr in exprs {
        collect_clause_minimum_refs(expr, &mut refs);
    }
    if refs.is_empty() {
        return;
    }
    let mut snapshot = ClauseMinimumSnapshot::default();
    for qty in refs {
        let value = crate::game::quantity::resolve_quantity_with_targets(
            state,
            &QuantityExpr::Ref { qty: qty.clone() },
            scoped_template,
        );
        snapshot.insert(qty.clone(), value);
    }
    state.clause_minimum_snapshot = Some(snapshot);
}

fn collect_ability_quantity_exprs<'a>(
    ability: &'a ResolvedAbility,
    out: &mut Vec<&'a QuantityExpr>,
) {
    let mut current = Some(ability);
    while let Some(node) = current {
        collect_effect_quantity_exprs(&node.effect, out);
        current = node.sub_ability.as_deref();
    }
}

/// The dynamic quantity expressions of an effect, if it carries any whose
/// equalization extrema must be clause-snapshot.
fn collect_effect_quantity_exprs<'a>(effect: &'a Effect, out: &mut Vec<&'a QuantityExpr>) {
    match effect {
        Effect::ChangeSpeed { amount, .. }
        | Effect::DealDamage { amount, .. }
        | Effect::Draw { count: amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::Sacrifice { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::Scry { count: amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::Dig { count: amount, .. }
        | Effect::Surveil { count: amount, .. }
        | Effect::Discard { count: amount, .. }
        | Effect::SearchLibrary { count: amount, .. }
        | Effect::SearchOutsideGame { count: amount, .. }
        | Effect::ExileTop { count: amount, .. }
        | Effect::CopyTokenOf { count: amount, .. }
        | Effect::PutCounter { count: amount, .. }
        | Effect::PutCounterAll { count: amount, .. }
        | Effect::AddPendingETBCounters { count: amount, .. }
        | Effect::FlipCoins { count: amount, .. }
        | Effect::Seek { count: amount, .. }
        | Effect::SetLifeTotal { amount, .. }
        | Effect::Manifest { count: amount, .. }
        | Effect::Cloak { count: amount, .. }
        | Effect::GivePlayerCounter { count: amount, .. }
        | Effect::GainEnergy { amount, .. }
        | Effect::Discover {
            mana_value_limit: amount,
            ..
        }
        | Effect::PutAtLibraryPosition { count: amount, .. }
        | Effect::GrantExtraLoyaltyActivations { amount, .. }
        | Effect::SkipNextTurn { count: amount, .. }
        | Effect::SkipNextStep { count: amount, .. }
        | Effect::Incubate { count: amount, .. }
        | Effect::Amass { count: amount, .. }
        | Effect::Monstrosity { count: amount, .. }
        | Effect::Renown { count: amount, .. }
        | Effect::Bolster { count: amount, .. }
        | Effect::Adapt { count: amount, .. } => out.push(amount),
        Effect::Token {
            count,
            enter_with_counters,
            ..
        } => {
            out.push(count);
            for (_, count) in enter_with_counters {
                out.push(count);
            }
        }
        Effect::Conjure { cards, .. } => {
            for card in cards {
                out.push(&card.count);
            }
        }
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            ..
        } => {
            out.push(count);
            out.push(life_payment);
        }
        Effect::PreventDamage { amount_dynamic, .. }
        | Effect::RevealHand {
            count: amount_dynamic,
            ..
        }
        | Effect::BounceAll {
            count: amount_dynamic,
            ..
        } => {
            if let Some(amount) = amount_dynamic {
                out.push(amount);
            }
        }
        Effect::MoveCounters {
            count: Some(count), ..
        } => out.push(count),
        Effect::ExileFromTopUntil {
            until: crate::types::ability::UntilCondition::CumulativeThreshold { threshold, .. },
            ..
        } => out.push(threshold),
        _ => {}
    }
}

/// CR 601.2c: Extract SharesQuality filter properties from an effect's target filter.
/// Returns the typed qualities that require group validation.
fn extract_shares_quality_props(
    filter: &TargetFilter,
) -> Vec<(&SharedQuality, SharedQualityRelation)> {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::SharesQuality {
                    quality,
                    reference: None,
                    relation,
                } => Some((quality, *relation)),
                _ => None,
            })
            .collect(),
        TargetFilter::And { filters } => filters
            .iter()
            .flat_map(extract_shares_quality_props)
            .collect(),
        _ => vec![],
    }
}

/// CR 608.2b: Extract the target filter from an effect for SharesQuality validation.
fn effect_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    effect.target_filter()
}

// ── Batch resolution (Tier 3) ────────────────────────────────────────────
//
// Driver-level collapse of N contiguous, identical, observer-free
// triggered-ability resolutions into a single execution pass. The eligibility
// predicate is layered (Layer A run-identity in `game/stack.rs`, Layer B
// handler purity here + in `token.rs`, Layer C observer-order-invariance in
// `game/stack.rs`). All gates default to "not batchable" — only the Token
// handler opts in, and only for provably-equivalent runs. See the planning
// trace in `game/stack.rs::resolve_next`.

/// Handler-specific execution data for a batch. Each variant carries exactly
/// what the execute step needs to reproduce N one-by-one resolutions.
pub(crate) enum BatchExecutionPlan {
    /// Resolve `Effect::Token` `run_len` times by replaying the existing
    /// per-resolution body. Carries the resolved per-resolution `TokenSpec`
    /// so Layer C (`game/stack.rs::observers_are_batch_safe`) can build the
    /// real ZoneChanged/TokenCreated probe events from its true
    /// characteristics (HIGH-1).
    Token {
        spec: crate::types::proposed_event::TokenSpec,
        run_len: u32,
    },
    /// CR 608.2c + CR 707.2: Resolve a met copy-instead swap (`CopyTokenOf`)
    /// `prefix_len` times by replaying `token_copy::resolve` on the swapped
    /// ability. `swapped` is the once-applied instead-swap (CR 608.2c — done
    /// ONCE in `try_resolve_batch`, not per iteration). `probe_spec` carries the
    /// prefix's shared copiable values so Layer C can build the real
    /// ZoneChanged/TokenCreated probe events from the produced token's true
    /// characteristics.
    CopyToken {
        copy_batch: PendingCopyTokenBatch,
        effect_kind: EffectKind,
        source_id: ObjectId,
        probe_spec: crate::types::proposed_event::TokenSpec,
        probe_mana_value: u32,
        prefix_len: u32,
    },
}

/// A proven-safe batch plan returned by `try_resolve_batch`. The driver
/// consumes `consumed` stack entries and applies the plan once.
pub(crate) struct BatchPlan {
    plan: BatchExecutionPlan,
    /// Number of stack entries this batch consumes (drives the pop loop and
    /// the auto-pass baseline decrement, §7.2).
    consumed: u32,
}

impl BatchPlan {
    /// Build a Token batch plan: resolve the base `Effect::Token` `run_len`
    /// times, producing the single per-resolution `spec` each iteration.
    pub(crate) fn token(spec: crate::types::proposed_event::TokenSpec, run_len: u32) -> Self {
        BatchPlan {
            plan: BatchExecutionPlan::Token { spec, run_len },
            consumed: run_len,
        }
    }

    /// CR 608.2c + CR 707.2: Build a copy-prefix batch plan: resolve the
    /// swapped `CopyTokenOf` `prefix_len` times, producing one copy token each
    /// iteration. Consumes `prefix_len` stack entries (may be < the full run).
    pub(crate) fn copy_token(
        copy_batch: PendingCopyTokenBatch,
        effect_kind: EffectKind,
        source_id: ObjectId,
        probe_spec: crate::types::proposed_event::TokenSpec,
        probe_mana_value: u32,
        prefix_len: u32,
    ) -> Self {
        BatchPlan {
            plan: BatchExecutionPlan::CopyToken {
                copy_batch,
                effect_kind,
                source_id,
                probe_spec,
                probe_mana_value,
                prefix_len,
            },
            consumed: prefix_len,
        }
    }

    pub(crate) fn consumed(&self) -> u32 {
        self.consumed
    }

    /// CR 603.6a: the resolved token spec(s) this batch will produce, exposed
    /// so Layer C can build the REAL ZoneChanged/TokenCreated probe events
    /// from each spec's true `core_types` — never a hand-fixed key set.
    pub(crate) fn produced_token_specs(&self) -> Vec<&crate::types::proposed_event::TokenSpec> {
        match &self.plan {
            BatchExecutionPlan::Token { spec, .. } => vec![spec],
            BatchExecutionPlan::CopyToken { probe_spec, .. } => vec![probe_spec],
        }
    }

    pub(crate) fn produced_token_mana_values(&self) -> Vec<u32> {
        match &self.plan {
            BatchExecutionPlan::Token { .. } => vec![0],
            BatchExecutionPlan::CopyToken {
                probe_mana_value, ..
            } => vec![*probe_mana_value],
        }
    }

    /// CR 608.2: Apply the batch by replaying the per-resolution handler body
    /// `run_len` times. The pipeline checkpoint (process_triggers + SBA) is
    /// hoisted to once-after by the driver, but the per-token creation +
    /// replacement + ETB bookkeeping stays at full N-fold multiplicity (§5.2).
    pub(crate) fn execute(
        &self,
        state: &mut GameState,
        ability: &ResolvedAbility,
        events: &mut Vec<GameEvent>,
    ) {
        match &self.plan {
            BatchExecutionPlan::Token { run_len, .. } => {
                for _ in 0..*run_len {
                    let _ = token::resolve(state, ability, events);
                }
            }
            // CR 608.2c + CR 707.2: Replay the swapped `CopyTokenOf` resolver
            // `prefix_len` times. Like the base Token arm, this intentionally
            // bypasses `resolve_ability_chain`'s depth-0 prelude (resolution-
            // scoped clears, NthResolutionThisTurn counter) — the instead-swap
            // was applied ONCE in `try_resolve_batch`, and each copy is an
            // independent per-token creation at full multiplicity (§5.2).
            BatchExecutionPlan::CopyToken {
                copy_batch,
                effect_kind,
                source_id,
                prefix_len,
                ..
            } => {
                token_copy::drive_copy_token_batches(
                    state,
                    VecDeque::from([copy_batch.clone()]),
                    *effect_kind,
                    *source_id,
                    events,
                );
                for _ in 1..*prefix_len {
                    events.push(GameEvent::EffectResolved {
                        kind: *effect_kind,
                        source_id: *source_id,
                    });
                }
            }
        }
    }
}

/// CR 608.2 + CR 608.2c: Returns a `BatchPlan` iff this effect instance is
/// provably state-invariant across `run_len` identical resolutions for its
/// OWN inputs — i.e. resolving it `run_len` times one-by-one would produce the
/// same per-resolution decision and token spec as one batched application.
/// Returns `None` (the default) for every effect not explicitly proven
/// batch-safe.
///
/// This gate covers ONLY the effect's own inputs (Layer B), INCLUDING the
/// §2.2a emits-exactly-{ZoneChanged,TokenCreated} gate (`spec_emits_only_etb_pair`)
/// and the §2.3a produced-token-non-observer gate applied inside the Token arm
/// so a returned plan's spec emits exactly the ETB pair. The driver must ALSO
/// pass the battlefield-wide observer-order-invariance gate (Layer C,
/// `game/stack.rs::observers_are_batch_safe`) before batching — that probe is
/// complete by construction precisely because the spec emits only the ETB pair.
pub(crate) fn try_resolve_batch(
    state: &GameState,
    ability: &ResolvedAbility,
    run_len: u32,
    run_source_ids: &[crate::types::identifiers::ObjectId],
) -> Option<BatchPlan> {
    match &ability.effect {
        Effect::Token { .. } => token::try_resolve_batch(state, ability, run_len, run_source_ids),
        // Exhaustive conservative default: every other effect is non-batchable
        // in v1. The wildcard encodes "opt-in," not a forgotten arm — new
        // batch-aware handlers add an explicit arm above.
        _ => None,
    }
}

/// Dispatch to the appropriate effect handler using typed pattern matching.
///
/// Canonical single-effect dispatch — one exhaustive match over `Effect`.
/// Production callers outside `effects/` must enter through
/// [`resolve_ability_chain`], which additionally handles ability-level
/// conditions, `optional`, and chained sub-abilities. Calling a per-effect
/// `<module>::resolve` directly bypasses those semantics; direct calls are
/// reserved for tests and for dispatch inside this module tree.
pub fn resolve_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::StartYourEngines { .. } => speed_effects::resolve_start(state, ability, events),
        Effect::ChangeSpeed { .. } => speed_effects::resolve_change_speed(state, ability, events),
        Effect::DealDamage { .. } => deal_damage::resolve(state, ability, events),
        Effect::ApplyPostReplacementDamage { .. } => {
            deal_damage::resolve_post_replacement(state, ability, events)
        }
        Effect::EachDealsDamageEqualToPower { .. } => {
            deal_damage::resolve_each_deals_equal_to_power(state, ability, events)
        }
        Effect::EachSourceDealsDamage { .. } => {
            deal_damage::resolve_each_source_deals_damage(state, ability, events)
        }
        Effect::Draw { .. } => draw::resolve(state, ability, events),
        Effect::Pump { .. } => pump::resolve(state, ability, events),
        Effect::PairWith { .. } => pair_with::resolve(state, ability, events),
        Effect::Destroy { .. } => destroy::resolve(state, ability, events),
        Effect::Regenerate { .. } => regenerate::resolve(state, ability, events),
        Effect::RemoveAllDamage { .. } => remove_all_damage::resolve(state, ability, events),
        Effect::Counter { .. } => counter::resolve(state, ability, events),
        Effect::CounterAll { .. } => counter::resolve_all(state, ability, events),
        Effect::Token { .. } => token::resolve(state, ability, events),
        Effect::GainLife { .. } => life::resolve_gain(state, ability, events),
        Effect::LoseLife { .. } => life::resolve_lose(state, ability, events),
        // CR 701.26a/b: scope (Single vs All) and state (Tap vs Untap) are
        // dispatched inside `resolve_set_tap_state`.
        Effect::SetTapState { .. } => tap_untap::resolve_set_tap_state(state, ability, events),
        Effect::RemoveCounter { .. } => counters::resolve_remove(state, ability, events),
        Effect::Sacrifice { .. } => sacrifice::resolve(state, ability, events),
        Effect::DiscardCard { .. } => discard::resolve(state, ability, events),
        Effect::Mill { .. } => mill::resolve(state, ability, events),
        Effect::Scry { .. } => scry::resolve(state, ability, events),
        Effect::PumpAll { .. } => pump::resolve_all(state, ability, events),
        Effect::DamageAll { .. } => deal_damage::resolve_all(state, ability, events),
        Effect::DamageEachPlayer { .. } => deal_damage::resolve_each_player(state, ability, events),
        Effect::DestroyAll { .. } => destroy::resolve_all(state, ability, events),
        Effect::ChangeZone { .. } => change_zone::resolve(state, ability, events),
        Effect::ChangeZoneAll { .. } => change_zone::resolve_all(state, ability, events),
        Effect::Dig { .. } => dig::resolve(state, ability, events),
        Effect::GainControl { .. } => gain_control::resolve(state, ability, events),
        Effect::GainControlAll { .. } => gain_control::resolve_all(state, ability, events),
        Effect::Goad { .. } | Effect::GoadAll { .. } => goad::resolve(state, ability, events),
        Effect::Detain { .. } => detain::resolve(state, ability, events),
        Effect::SetRoomDoorLock { .. } => set_room_door_lock::resolve(state, ability, events),
        Effect::ExchangeControl { .. } => exchange_control::resolve(state, ability, events),
        Effect::Attach { .. } => attach::resolve(state, ability, events),
        Effect::UnattachAll { .. } => attach::resolve_unattach_all(state, ability, events),
        Effect::ControlNextTurn { .. } => control_next_turn::resolve(state, ability, events),
        Effect::Surveil { .. } => surveil::resolve(state, ability, events),
        Effect::Fight { .. } => fight::resolve(state, ability, events),
        Effect::Bounce { .. } => bounce::resolve(state, ability, events),
        Effect::BounceAll { .. } => bounce::resolve_all(state, ability, events),
        Effect::Explore => explore::resolve(state, ability, events),
        Effect::ExploreAll { .. } => explore::resolve_all(state, ability, events),
        Effect::Investigate => investigate::resolve(state, ability, events),
        // CR 702.104a: Tribute — the chosen opponent decides pay/decline via
        // WaitingFor::TributeChoice (reuses GameAction::DecideOptionalEffect).
        Effect::Tribute { .. } => tribute::resolve(state, ability, events),
        // CR 701.56a: Time travel — interactive counter manipulation on suspended/time-countered permanents.
        // Currently a no-op; full interactive implementation requires WaitingFor infrastructure.
        Effect::TimeTravel => time_travel::resolve(state, ability, events),
        Effect::BecomeMonarch => become_monarch::resolve(state, ability, events),
        // CR 101.3 + CR 608.2: An instruction with no game action. Emit
        // `EffectResolved` so the chain continues, and do nothing else.
        Effect::NoOp => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::NoOp,
                source_id: ability.source_id,
            });
            Ok(())
        }
        Effect::Proliferate => proliferate::resolve(state, ability, events),
        Effect::ProliferateTarget { .. } => proliferate::resolve_target(state, ability, events),
        Effect::EndTheTurn => end_the_turn::resolve(state, ability, events),
        Effect::EndCombatPhase => end_combat_phase::resolve(state, ability, events),
        Effect::Populate => populate::resolve(state, ability, events),
        Effect::Clash => clash::resolve(state, ability, events),
        Effect::Behold { .. } => behold::resolve(state, ability, events),
        // CR 701.38: Council's-dilemma voting — see effects/vote.rs.
        Effect::Vote { .. } => vote::resolve(state, ability, events),
        // CR 700.3 + CR 608: Pile-separation primitive — see effects/separate_piles.rs.
        Effect::SeparateIntoPiles { .. } => separate_piles::resolve(state, ability, events),
        Effect::SwitchPT { .. } => switch_pt::resolve(state, ability, events),
        Effect::CopySpell { .. } => copy_spell::resolve(state, ability, events),
        Effect::EpicCopy { .. } => epic::resolve(state, ability, events),
        Effect::CastCopyOfCard { .. } => cast_copy_of_card::resolve(state, ability, events),
        Effect::CopyTokenOf { .. } => token_copy::resolve(state, ability, events),
        Effect::CreateTokenCopyFromPool { .. } => {
            create_token_copy_from_pool::resolve(state, ability, events)
        }
        Effect::Myriad => myriad::resolve(state, ability, events),
        Effect::ExileHaunting { .. } => crate::game::haunt::resolve(state, ability, events),
        Effect::Encore => encore::resolve(state, ability, events),
        Effect::CombineHost { .. } => {
            crate::game::augment::resolve_combine_host(state, ability, events)
        }
        Effect::ChooseAugmentAndCombineWithHost { .. } => {
            crate::game::augment::resolve_choose_augment_and_combine(state, ability, events)
        }
        Effect::Meld { .. } => crate::game::meld::perform_meld(state, ability, events),
        Effect::HideawayConceal { .. } => hideaway::resolve(state, ability, events),
        Effect::CopyTokenBlockingAttacker { .. } => {
            copy_token_blocking::resolve(state, ability, events)
        }
        Effect::BecomeCopy { .. } => become_copy::resolve(state, ability, events),
        Effect::GainActivatedAbilitiesOfTarget { .. } => {
            gain_activated_abilities::resolve(state, ability, events)
        }
        Effect::ChooseCard { .. } => choose_card::resolve(state, ability, events),
        Effect::PutCounter { .. } => counters::resolve_add(state, ability, events),
        Effect::PutCounterAll { .. } => counters::resolve_add_all(state, ability, events),
        Effect::MultiplyCounter { .. } => counters::resolve_multiply(state, ability, events),
        Effect::DoublePT { .. } => pump::resolve_double_pt(state, ability, events),
        Effect::DoublePTAll { .. } => pump::resolve_double_pt_all(state, ability, events),
        Effect::MoveCounters { .. } => counters::resolve_move(state, ability, events),
        Effect::Animate { .. } => animate::resolve(state, ability, events),
        Effect::ReturnAsAura { .. } => return_as_aura::resolve(state, ability, events),
        Effect::RegisterBending { .. } => register_bending::resolve(state, ability, events),
        Effect::GenericEffect { .. } => effect::resolve(state, ability, events),
        Effect::Cleanup { .. } => cleanup::resolve(state, ability, events),
        Effect::Mana { .. } => mana::resolve(state, ability, events),
        Effect::Discard { .. } => discard::resolve(state, ability, events),
        Effect::Shuffle { .. } => shuffle::resolve(state, ability, events),
        Effect::Transform { .. } => transform_effect::resolve(state, ability, events),
        Effect::SearchLibrary { .. } => search_library::resolve(state, ability, events),
        Effect::SearchOutsideGame { .. } => search_outside_game::resolve(state, ability, events),
        Effect::Seek { .. } => seek::resolve(state, ability, events),
        Effect::RevealHand { .. } => reveal_hand::resolve(state, ability, events),
        Effect::RevealFromHand { .. } => reveal_from_hand::resolve(state, ability, events),
        Effect::Reveal { .. } => reveal::resolve(state, ability, events),
        Effect::RevealTop { .. } => reveal_top::resolve(state, ability, events),
        Effect::ExileTop { .. } => exile_top::resolve(state, ability, events),
        Effect::TargetOnly { .. } => Ok(()), // no-op: targeting is established at cast time
        Effect::Choose { .. } => choose::resolve(state, ability, events),
        Effect::OpponentGuess { .. } => opponent_guess::resolve(state, ability, events),
        Effect::SwapChosenLabels { .. } => swap_chosen_labels::resolve(state, ability, events),
        Effect::ChooseCounterKind { .. } => choose_counter_kind::resolve(state, ability, events),
        Effect::PutChosenCounter { .. } => put_chosen_counter::resolve(state, ability, events),
        Effect::ChooseDamageSource { .. } => choose_damage_source::resolve(state, ability, events),
        Effect::Suspect { .. } => suspect::resolve(state, ability, events),
        Effect::Unsuspect { .. } => suspect::resolve_unsuspect(state, ability, events),
        Effect::Connive { .. } => connive::resolve(state, ability, events),
        Effect::PhaseOut { .. } => phase_out::resolve(state, ability, events),
        Effect::PhaseIn { .. } => phase_out::resolve_phase_in(state, ability, events),
        Effect::ForceBlock { .. } => force_block::resolve(state, ability, events),
        Effect::ForceAttack { .. } => force_attack::resolve(state, ability, events),
        Effect::SolveCase => solve_case::resolve(state, ability, events),
        Effect::BecomePrepared { .. } => prepare::resolve_become_prepared(state, ability, events),
        Effect::BecomeUnprepared { .. } => {
            prepare::resolve_become_unprepared(state, ability, events)
        }
        Effect::BecomeSaddled { .. } => saddle::resolve(state, ability, events),
        Effect::BecomeBlocked { .. } => {
            become_blocked::resolve_become_blocked(state, ability, events)
        }
        Effect::SetClassLevel { .. } => set_class_level::resolve(state, ability, events),
        Effect::CreateDelayedTrigger { .. } => delayed_trigger::resolve(state, ability, events),
        Effect::AddTargetReplacement { .. } => {
            add_target_replacement::resolve(state, ability, events)
        }
        Effect::AddRestriction { .. } => add_restriction::resolve(state, ability, events),
        Effect::ReduceNextSpellCost { .. } => {
            resolve_reduce_next_spell_cost(state, ability, events)
        }
        Effect::GrantNextSpellAbility { .. } => {
            resolve_grant_next_spell_ability(state, ability, events)
        }
        Effect::AddPendingETBCounters { .. } => {
            resolve_add_pending_etb_counters(state, ability, events)
        }
        Effect::AddPendingEntersModifications { .. } => {
            resolve_add_pending_enters_modifications(state, ability, events)
        }
        Effect::CreateEmblem { .. } => create_emblem::resolve(state, ability, events),
        Effect::PayCost { .. } => pay::resolve(state, ability, events),
        Effect::CastFromZone { .. } => cast_from_zone::resolve(state, ability, events),
        Effect::FreeCastFromZones { .. } => free_cast_from_zones::resolve(state, ability, events),
        Effect::ExileResolvingSpellInsteadOfGraveyard => {
            exile_resolving_spell::resolve(state, ability, events)
        }
        Effect::PreventDamage { .. } => prevent_damage::resolve(state, ability, events),
        Effect::CreateDamageReplacement { .. } => {
            create_damage_replacement::resolve(state, ability, events)
        }
        Effect::CreateDrawReplacement { .. } => {
            create_draw_replacement::resolve(state, ability, events)
        }
        Effect::CreatePlaneswalkReplacement { .. } => {
            create_planeswalk_replacement::resolve(state, ability, events)
        }
        Effect::LoseTheGame { .. } => win_lose::resolve_lose(state, ability, events),
        Effect::WinTheGame { .. } => win_lose::resolve_win(state, ability, events),
        Effect::RollDie { .. } => roll_die::resolve(state, ability, events),
        Effect::FlipCoin { .. } => flip_coin::resolve(state, ability, events),
        Effect::FlipCoins { .. } => flip_coin::resolve_flip_coins(state, ability, events),
        Effect::FlipCoinUntilLose { .. } => flip_coin::resolve_until_lose(state, ability, events),
        Effect::RingTemptsYou => ring::resolve(state, ability, events),
        Effect::GrantCastingPermission { .. } => grant_permission::resolve(state, ability, events),
        Effect::ChooseFromZone { .. } => choose_from_zone::resolve(state, ability, events),
        Effect::RememberCard { .. } => remember_card::resolve(state, ability, events),
        Effect::ForEachCategoryExile { .. } => {
            choose_from_zone::resolve_for_each_category(state, ability, events)
        }
        Effect::ChooseObjectsIntoTrackedSet { .. } => {
            choose_objects_into_tracked_set::resolve(state, ability, events)
        }
        Effect::ChooseAndSacrificeRest { .. } => {
            choose_and_sacrifice_rest::resolve(state, ability, events)
        }
        Effect::EachPlayerCopyChosen { .. } => {
            each_player_copy_chosen::resolve(state, ability, events)
        }
        Effect::Exploit { .. } => exploit::resolve(state, ability, events),
        Effect::GainEnergy { .. } => energy::resolve_gain(state, ability, events),
        Effect::GivePlayerCounter { .. } => player_counter::resolve(state, ability, events),
        Effect::LoseAllPlayerCounters { .. } => {
            player_counter::resolve_lose_all(state, ability, events)
        }
        Effect::AdditionalPhase { .. } => additional_phase::resolve(state, ability, events),
        Effect::ExileFromTopUntil { .. } => exile_from_top_until::resolve(state, ability, events),
        Effect::RevealUntil { .. } => reveal_until::resolve(state, ability, events),
        Effect::Discover { .. } => discover::resolve(state, ability, events),
        // Heist (Arena digital-only): look step. Raises ChooseFromZoneChoice
        // over random nonland cards from the targeted opponent's library and
        // stashes a HeistExile continuation.
        Effect::Heist { .. } => heist::resolve(state, ability, events),
        // Heist finalizer continuation: exile the chosen card face down, link
        // it, and grant a permanent any-color cast-from-exile permission.
        Effect::HeistExile => heist::resolve_exile(state, ability, events),
        // CR 702.85a: Cascade — synthesized from the keyword at trigger time;
        // resolver performs the exile-until loop and sets CascadeChoice.
        Effect::Cascade => cascade::resolve(state, ability, events),
        Effect::Ripple { .. } => ripple::resolve(state, ability, events),
        // CR 702.94a: Miracle trigger resolution — offer the cast from hand.
        Effect::MiracleCast { ref cost } => {
            state.waiting_for = WaitingFor::CastOffer {
                player: ability.controller,
                kind: CastOfferKind::Miracle {
                    object_id: ability.source_id,
                    cost: cost.clone(),
                },
            };
            Ok(())
        }
        // CR 702.35a: Madness trigger resolution — offer the cast from exile.
        Effect::MadnessCast { ref cost } => {
            state.waiting_for = WaitingFor::CastOffer {
                player: ability.controller,
                kind: CastOfferKind::Madness {
                    object_id: ability.source_id,
                    cost: cost.clone(),
                },
            };
            Ok(())
        }
        Effect::PutAtLibraryPosition { .. } => put_on_top::resolve(state, ability, events),
        Effect::ChooseDrawnThisTurnPayOrTopdeck { .. } => {
            drawn_this_turn_choice::resolve(state, ability, events)
        }
        Effect::PutOnTopOrBottom { .. } => put_on_top_or_bottom::resolve(state, ability, events),
        Effect::GiftDelivery { .. } => gift_delivery::resolve(state, ability, events),
        Effect::ChangeTargets { .. } => change_targets::resolve(state, ability, events),
        Effect::Incubate { .. } => incubate::resolve(state, ability, events),
        Effect::Amass { .. } => amass::resolve(state, ability, events),
        Effect::Monstrosity { .. } => monstrosity::resolve(state, ability, events),
        Effect::Specialize => specialize::resolve(state, ability, events),
        Effect::Renown { .. } => renown::resolve(state, ability, events),
        Effect::Adapt { .. } => adapt::resolve(state, ability, events),
        Effect::Bolster { .. } => bolster::resolve(state, ability, events),
        Effect::Manifest { .. } => manifest::resolve(state, ability, events),
        Effect::ManifestDread => manifest_dread::resolve(state, ability, events),
        Effect::Cloak { .. } => cloak::resolve(state, ability, events),
        Effect::TurnFaceUp { .. } => turn_face_up::resolve(state, ability, events),
        Effect::TurnFaceDown { .. } => turn_face_down::resolve(state, ability, events),
        Effect::ExtraTurn { .. } => extra_turn::resolve(state, ability, events),
        Effect::GrantExtraLoyaltyActivations { .. } => {
            grant_extra_loyalty_activations::resolve(state, ability, events)
        }
        Effect::SkipNextStep { .. } => skip_next_step::resolve(state, ability, events),
        Effect::SkipNextTurn { .. } => skip_next_turn::resolve(state, ability, events),
        Effect::Double { .. } => double::resolve(state, ability, events),
        Effect::RuntimeHandled { .. } => Ok(()), // Handled by dedicated engine path
        Effect::Learn => learn::resolve(state, ability, events),
        Effect::BlightEffect { .. } => blight::resolve(state, ability, events),
        Effect::Endure { .. } => endure::resolve(state, ability, events),
        Effect::Forage => forage::resolve(state, ability, events),
        Effect::Harness => harness::resolve(state, ability, events),
        Effect::CollectEvidence { .. } => collect_evidence::resolve(state, ability, events),
        Effect::SetLifeTotal { .. } => life::resolve_set_life_total(state, ability, events),
        Effect::ExchangeLifeWithStat { .. } => exchange_life::resolve(state, ability, events),
        Effect::ExchangeLifeTotals { .. } => exchange_life_totals::resolve(state, ability, events),
        Effect::SetDayNight { to } => {
            crate::game::day_night::resolve_set_day_night(state, *to, events);
            Ok(())
        }
        Effect::GiveControl { .. } => gain_control::resolve_give(state, ability, events),
        Effect::RemoveFromCombat { .. } => remove_from_combat::resolve(state, ability, events),
        Effect::VentureIntoDungeon => venture::resolve(state, ability, events),
        Effect::VentureInto { dungeon } => {
            venture::resolve_venture_into(state, ability, *dungeon, events)
        }
        Effect::TakeTheInitiative => venture::resolve_take_initiative(state, ability, events),
        Effect::Planeswalk => planeswalk::resolve(state, ability, events),
        Effect::ChaosEnsues => chaos_ensues::resolve(state, ability, events),
        Effect::ReverseTurnOrder => reverse_turn_order::resolve(state, ability, events),
        Effect::OpenAttractions { .. } | Effect::RollToVisitAttractions => {
            attractions::resolve(state, ability, events)
        }
        Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::ReassembleContraptionOnSprocket { .. } => {
            crate::game::contraptions::resolve(state, ability, events)
        }
        Effect::PutSticker { .. } | Effect::ApplySticker { .. } => {
            stickers::resolve(state, ability, events)
        }
        Effect::ProcessRadCounters => rad_counters::resolve(state, ability, events),
        Effect::Conjure { .. } => conjure::resolve(state, ability, events),
        Effect::Intensify { .. } => intensify::resolve(state, ability, events),
        Effect::ApplyPerpetual { .. } => perpetual::resolve(state, ability, events),
        Effect::DraftFromSpellbook { .. } => spellbook::resolve(state, ability, events),
        Effect::ChooseOneOf { .. } => choose_one_of::resolve(state, ability, events),
        Effect::ChooseCounterAdjustment { .. } => {
            choose_counter_adjustment::resolve(state, ability, events)
        }
        Effect::Unimplemented { name, .. } => {
            // Log warning and return Ok (no-op) for unimplemented effects
            eprintln!("Warning: Unimplemented effect: {}", name);
            // Diagnostics: record the source card so the telemetry `game_summary`
            // surface can report which cards hit an unimplemented effect this game.
            // Oracle id when available; object name fallback covers tokens/emblems
            // (`printed_ref == None`).
            if let Some(obj) = state.objects.get(&ability.source_id) {
                let id = obj
                    .printed_ref
                    .as_ref()
                    .map(|p| p.oracle_id.clone())
                    .unwrap_or_else(|| obj.name.clone());
                state.unimplemented_oracle_ids.insert(id);
            }
            Ok(())
        }
    }
}

/// Returns true if the given effect has a handler in the engine.
/// `Unimplemented` effects are the only genuinely unsupported effects.
/// `RuntimeHandled` effects are supported but handled by dedicated engine paths.
pub fn is_known_effect(effect: &Effect) -> bool {
    !matches!(effect, Effect::Unimplemented { .. })
}

/// CR 603.7: Check if any descendant sub_ability needs tracked set recording.
///
/// A descendant consumes the tracked set when any of its quantity or filter
/// positions reference the most recent set — via `QuantityRef::TrackedSetSize`
/// (e.g., "for each creature destroyed this way") or
/// `TargetFilter::TrackedSet { .. }` (e.g., "those cards"). Two flag-driven
/// cases are also consumers: `CreateDelayedTrigger { uses_tracked_set: true }`
/// binds the set to the delayed trigger's later resolution, and
/// `ChooseFromZone` selects out of it.
///
/// The walk is **transitive** across continuation branches — a grandchild
/// referencing `TrackedSet(0)` causes every zone-changing ancestor in the
/// chain to publish, which (combined with chain-unification at publish
/// time) merges all affected objects into a single tracked set. This is
/// what makes compound exile (Suspend Aggression's
/// "Exile target nonland permanent and the top card of your library ...
/// for each of those cards") expose both exiled objects to the grant.
pub(crate) fn next_sub_needs_tracked_set(ability: &ResolvedAbility) -> bool {
    ability
        .sub_ability
        .as_deref()
        .is_some_and(ability_or_branch_references_tracked_set)
}

/// CR 608.2c: Does `ability` (or any of its continuation branches) consume the
/// chain's tracked set — e.g. a `GrantCastingPermission { target: TrackedSet }`
/// ("you may play that card") chained after an interactive `ChooseFromZone`?
/// The interactive `ChooseFromZoneChoice` answer handler uses this to decide
/// whether the chosen cards must be published as the fresh tracked set the
/// continuation reads (End-Blaze Epiphany: "choose a card exiled this way …
/// you may play that card").
pub(crate) fn chain_references_tracked_set(ability: &ResolvedAbility) -> bool {
    ability_or_branch_references_tracked_set(ability)
}

fn ability_or_branch_references_tracked_set(ability: &ResolvedAbility) -> bool {
    let consumes = matches!(
        &ability.effect,
        Effect::CreateDelayedTrigger {
            uses_tracked_set: true,
            ..
        } | Effect::ChooseFromZone { .. }
    ) || effect_references_tracked_set(&ability.effect)
        // CR 608.2c + CR 609.3: `repeat_for` is a loop-count quantity on the
        // ResolvedAbility, not inside Effect — e.g. "for each nonland card
        // discarded this way, create a token" uses `repeat_for: TrackedSetSize`.
        // Without this check, the forced-discard path (no WaitingFor pause)
        // never publishes the tracked set, so the downstream token loop sees
        // size 0 and creates no tokens (Seasoned Pyromancer bug #740).
        || ability
            .repeat_for
            .as_ref()
            .is_some_and(quantity_expr_references_tracked_set);

    consumes
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_or_branch_references_tracked_set)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_or_branch_references_tracked_set)
}

/// Returns true if the effect references the most recent tracked set through
/// any quantity (`QuantityRef::TrackedSetSize`) or filter (`TargetFilter::TrackedSet`)
/// position. Walks all quantity and filter fields — works for any effect in the
/// class (GainLife, DealDamage, Token, Mill, Draw, PutCounter, GrantCastingPermission, …)
/// without enumerating variants.
fn effect_references_tracked_set(effect: &Effect) -> bool {
    // Quantity positions — walk every QuantityExpr field on the effect.
    let quantity_hits_tracked = |qty: &QuantityExpr| quantity_expr_references_tracked_set(qty);
    let has_quantity_hit = match effect {
        Effect::DealDamage { amount, .. } => quantity_hits_tracked(amount),
        Effect::DamageAll { amount, .. } => quantity_hits_tracked(amount),
        Effect::DamageEachPlayer { amount, .. } => quantity_hits_tracked(amount),
        Effect::Draw { count, .. } => quantity_hits_tracked(count),
        Effect::Mill { count, .. } => quantity_hits_tracked(count),
        Effect::Scry { count, .. } => quantity_hits_tracked(count),
        Effect::Dig { count, .. } => quantity_hits_tracked(count),
        Effect::Surveil { count, .. } => quantity_hits_tracked(count),
        Effect::GainLife { amount, .. } => quantity_hits_tracked(amount),
        Effect::LoseLife { amount, .. } => quantity_hits_tracked(amount),
        Effect::ChangeSpeed { amount, .. } => quantity_hits_tracked(amount),
        Effect::PutCounter { count, .. } => quantity_hits_tracked(count),
        Effect::PutCounterAll { count, .. } => quantity_hits_tracked(count),
        Effect::Token { count, .. } => quantity_hits_tracked(count),
        _ => false,
    };
    if has_quantity_hit {
        return true;
    }

    // Filter positions — the effect's primary target filter may be TrackedSet.
    if let Some(filter) = effect.target_filter() {
        if filter_references_tracked_set(filter) {
            return true;
        }
    }
    if let Effect::ChangeZoneAll { target, .. } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    if let Effect::PutCounterAll { target, .. } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    if let Effect::GoadAll { target } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    if let Effect::SetTapState {
        scope: EffectScope::All,
        target,
        ..
    } = effect
    {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    // `GrantCastingPermission` has a `target` field that is not exposed by
    // `Effect::target_filter()` (it selects objects to grant permission to,
    // not spell/ability targets). Inspect directly so "the rest" / "those
    // cards" sub-abilities chained off exile-all effects still record the set.
    if let Effect::GrantCastingPermission { target, .. } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    // CR 608.2c + CR 707.2: `CopyTokenOf` may carry `TrackedSet` on
    // `target` while `target_filter()` surfaces `owner` (context-ref copy
    // sources). Sin, Spira's Punishment — random exile publishes the set for
    // the chained copy.
    if let Effect::CopyTokenOf { target, .. } = effect {
        if filter_references_tracked_set(target) {
            return true;
        }
    }
    if let Effect::GenericEffect {
        static_abilities, ..
    } = effect
    {
        // CR 608.2c + CR 613: A continuous grant whose `affected` filter either
        // names the tracked set directly (`TrackedSet`) or is the `ParentTarget`
        // anaphor ("They gain trample…", Najeela — issue #2898) consumes the
        // chain's tracked object set. `ParentTarget` with no inherited targets
        // resolves against `chain_tracked_set_id` in `effect.rs`, so the parent
        // instruction (e.g. `Untap all attacking creatures`) must publish that
        // set for the grant to bind to the affected permanents.
        if static_abilities.iter().any(|static_def| {
            static_def.affected.as_ref().is_some_and(|affected| {
                filter_references_tracked_set(affected)
                    || matches!(affected, TargetFilter::ParentTarget)
            })
        }) {
            return true;
        }
    }
    false
}

fn quantity_expr_references_tracked_set(qty: &QuantityExpr) -> bool {
    match qty {
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Ref { qty } => {
            matches!(
                qty,
                QuantityRef::TrackedSetSize
                    | QuantityRef::FilteredTrackedSetSize { .. }
                    | QuantityRef::TrackedSetAggregate { .. }
                    | QuantityRef::DistinctCardTypes {
                        source: CardTypeSetSource::TrackedSet { .. }
                    }
                    | QuantityRef::DistinctSubtypes {
                        source: CardTypeSetSource::TrackedSet { .. },
                        ..
                    }
            )
        }
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. } => quantity_expr_references_tracked_set(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_expr_references_tracked_set)
        }
        QuantityExpr::UpTo { max } => quantity_expr_references_tracked_set(max),
        QuantityExpr::Power { exponent, .. } => quantity_expr_references_tracked_set(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_references_tracked_set(left)
                || quantity_expr_references_tracked_set(right)
        }
    }
}

fn filter_references_tracked_set(filter: &TargetFilter) -> bool {
    match filter {
        // CR 603.7: Both the bare tracked-set filter and its type-filtered
        // intersection ("X cards revealed this way", "from among the milled
        // cards") consume the most recent tracked set — either form on a
        // sub-ability means the parent effect must publish its affected set.
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => true,
        TargetFilter::Not { filter } => filter_references_tracked_set(filter),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_references_tracked_set)
        }
        _ => false,
    }
}

fn effect_uses_implicit_tracked_set_targets(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::GrantCastingPermission {
            target: TargetFilter::TrackedSet { .. },
            ..
        } | Effect::CastCopyOfCard {
            target: TargetFilter::TrackedSet { .. },
            ..
        } | Effect::PutAtLibraryPosition {
            target: TargetFilter::ExiledBySource,
            ..
        }
    )
}

/// CR 707.10: A `CopySpell { SelfRef }` sub-ability after a `forward_result`
/// parent copies the resolving spell, not the object the parent just moved.
/// Rebinding `source_id` to the forwarded permanent breaks the
/// `resolving_stack_entry` fallback in `copy_spell::resolve` once the spell
/// has left the stack (Sevinne's Reclamation — issue #2860).
fn copy_spell_self_ref_keeps_resolving_spell_source(sub: &ResolvedAbility) -> bool {
    matches!(
        &sub.effect,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            ..
        }
    )
}

/// CR 608.2c + CR 614.6: Pair each object affected by `effect` with the producer
/// ACTION that made it part of the tracked set, mirroring
/// [`affected_objects_from_events`] one-for-one but retaining a per-object
/// [`ThisWayCause`] so [`publish_tracked_set_with_causes`] can stamp the
/// member-cause side map. The cause is derived from the resolving EFFECT KIND,
/// NOT the member's final landing zone, so it is stable under a replacement that
/// redirects the destination (CR 614.1 / CR 614.6) and never collides with
/// another action that shares the same destination:
///
///   - Destroy / DestroyAll → `Destroyed` (CR 701.8a).
///   - Sacrifice → `Sacrificed` (CR 701.21a) — still `Sacrificed` even when a
///     replacement sends the permanent to Exile instead of the graveyard.
///   - Mill → `Milled` (CR 701.17a).
///   - Discard / DiscardCard → `Discarded` (CR 701.9a).
///   - ChangeZone / ChangeZoneAll → by destination: Exile → `Exiled`
///     (CR 701.13a), Battlefield → `Returned` (CR 400.7), Hand → `Bounced`
///     (CR 400.7); other destinations are not "this way"-referenced verbs, so
///     they carry no cause (consumed only by `caused_by: None`).
///   - BounceAll → `Bounced` if its destination is Hand, `Returned` if
///     Battlefield (default Hand → `Bounced`, CR 400.7 / CR 611.2c).
///   - ExileTop / ExileFromTopUntil → `Exiled` (CR 701.13a).
///   - RevealUntil's kept card / counter / reveal / tap-untap producers do not
///     name a "<verb>ed this way" set; they carry no cause and are consumed only
///     by `caused_by: None` (selection-set) downstream references.
fn affected_objects_with_causes(
    effect: &Effect,
    events: &[GameEvent],
    fallback_targets: &[TargetRef],
) -> Vec<(ObjectId, Option<ThisWayCause>)> {
    let ids = affected_objects_from_events(effect, events, fallback_targets);
    // CR 608.2c: the cause is a property of the EFFECT being resolved, not of any
    // individual member's event — so every member of this publish shares one
    // cause. `None` for producers that do not name a "this way" verb (reveals,
    // taps, counters, the RevealUntil kept card, and zone changes to a
    // destination no consumer references), which are read only by
    // `caused_by: None`.
    let cause = this_way_cause_for_effect(effect);
    ids.into_iter().map(|id| (id, cause)).collect()
}

/// CR 608.2c + CR 614.6: Map a resolving effect to the producer-action cause
/// stamped onto the tracked-set members it publishes. Derived purely from the
/// effect kind (and its declared destination), so it is independent of any
/// replacement that later redirects the members' landing zone.
fn this_way_cause_for_effect(effect: &Effect) -> Option<ThisWayCause> {
    use crate::types::zones::Zone;
    // CR 400.7: a generic zone change names a "this way" verb only for the
    // destinations a consumer references — Exile (exiled), Battlefield
    // (returned/put onto the battlefield), Hand (bounced/returned to hand).
    let cause_for_zone = |destination: Zone| match destination {
        Zone::Exile => Some(ThisWayCause::Exiled),
        Zone::Battlefield => Some(ThisWayCause::Returned),
        Zone::Hand => Some(ThisWayCause::Bounced),
        _ => None,
    };
    match effect {
        Effect::Destroy { .. } | Effect::DestroyAll { .. } => Some(ThisWayCause::Destroyed),
        Effect::Sacrifice { .. } => Some(ThisWayCause::Sacrificed),
        Effect::Mill { .. } => Some(ThisWayCause::Milled),
        Effect::Discard { .. } | Effect::DiscardCard { .. } => Some(ThisWayCause::Discarded),
        Effect::ChangeZone { destination, .. } | Effect::ChangeZoneAll { destination, .. } => {
            cause_for_zone(*destination)
        }
        // CR 611.2c: mass-bounce destination defaults to Hand.
        Effect::BounceAll { destination, .. } => cause_for_zone(destination.unwrap_or(Zone::Hand)),
        Effect::ExileTop { .. } | Effect::ExileFromTopUntil { .. } => Some(ThisWayCause::Exiled),
        // Reveals, taps, counter producers, the RevealUntil kept card, and any
        // other producer do not name a "<verb>ed this way" set — leave them
        // unstamped (matched only by `caused_by: None`).
        _ => None,
    }
}

fn affected_objects_from_events(
    effect: &Effect,
    events: &[GameEvent],
    fallback_targets: &[TargetRef],
) -> Vec<ObjectId> {
    match effect {
        Effect::GainControl { .. } => fallback_targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect(),
        Effect::GainControlAll { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ControllerChanged { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect(),
        Effect::Destroy { .. } | Effect::DestroyAll { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CreatureDestroyed { object_id } => Some(*object_id),
                _ => None,
            })
            .collect(),
        // CR 608.2c + CR 701.19c: damage publishes the damaged objects so a
        // downstream "<noun> dealt damage this way can't be regenerated"
        // sub-ability binds to exactly those creatures (Incinerate/Flamebreak/
        // Jaya Ballard, Task Mage). Object targets only — a player carries no
        // regen shield and the static is inert on players. CR 120.3/120.6: only
        // creatures actually dealt a nonzero amount were "dealt damage this way"
        // (fully-prevented damage doesn't count), so require `amount > 0`.
        Effect::DealDamage { .. } | Effect::DamageAll { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    target: TargetRef::Object(id),
                    amount,
                    ..
                } if *amount > 0 => Some(*id),
                _ => None,
            })
            .collect(),
        Effect::Sacrifice { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect(),
        Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::MoveCounters { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CounterAdded { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect(),
        // CR 701.26a/b + CR 608.2c: tap/untap publishes the affected set for
        // downstream continuations that bind to "those creatures" / "they".
        // Single-target tap/untap feeds "each of those <type>" riders (Urge to
        // Feed class). Mass (`All`) tap/untap feeds `affected: ParentTarget`
        // keyword grants chained on the same instruction — Najeela's "Untap all
        // attacking creatures. They gain trample, lifelink, and haste"
        // (issue #2898). Both scopes read the same tap/untap events, so the
        // untapped permanents become the chain's tracked set.
        Effect::SetTapState { .. } => {
            let from_events: Vec<ObjectId> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::PermanentTapped { object_id, .. }
                    | GameEvent::PermanentUntapped { object_id, .. } => Some(*object_id),
                    _ => None,
                })
                .collect();
            if !from_events.is_empty() {
                from_events
            } else {
                fallback_targets
                    .iter()
                    .filter_map(|target| match target {
                        TargetRef::Object(id) => Some(*id),
                        TargetRef::Player(_) => None,
                    })
                    .collect()
            }
        }
        // CR 701.20b + CR 608.2c: Reveal instructions do not move cards, so they
        // emit `CardsRevealed` rather than `ZoneChanged`. Publish the revealed
        // card ids for downstream "from among the revealed cards"
        // `ChooseFromZone` continuations (Atraxa, Grand Unifier class).
        Effect::RevealTop { .. } | Effect::RevealHand { .. } | Effect::Clash => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.as_slice()),
                _ => None,
            })
            .flat_map(|ids| ids.iter().copied())
            .collect(),
        // CR 701.20b: reveal-only RevealUntil (Sanar) publishes the full revealed
        // pile from `CardsRevealed` — no zone changes occurred.
        Effect::RevealUntil {
            matched_disposition: RevealUntilDisposition::RevealOnly,
            ..
        } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.as_slice()),
                _ => None,
            })
            .flat_map(|ids| ids.iter().copied())
            .collect(),
        _ => {
            let dest_zone = match effect {
                Effect::ChangeZone { destination, .. }
                | Effect::ChangeZoneAll { destination, .. } => Some(*destination),
                // CR 701.17a + CR 701.17c: Milled cards land in the `Mill`'s
                // destination (Graveyard by default). Scoping the tracked set
                // to that zone makes a downstream "from among the milled cards"
                // sub-ability resolve against exactly the milled cards.
                Effect::Mill { destination, .. } => Some(*destination),
                // CR 701.20a + CR 608.2f: The kept card lands in `kept_destination`; scope the
                // tracked set to that zone so downstream TrackedSet consumers (e.g. IC's
                // ChangeZoneAll{Exile→Battlefield}) see only the kept card, not the rest pile.
                Effect::RevealUntil {
                    kept_destination, ..
                } => Some(*kept_destination),
                Effect::ExileTop { .. } | Effect::ExileFromTopUntil { .. } => {
                    Some(crate::types::zones::Zone::Exile)
                }
                // CR 701.9a: discarded cards land in the graveyard; "for each
                // card discarded this way" counts exactly those (CR 701.9c
                // excludes a discard redirected by a replacement to another
                // zone — e.g. Madness — which must not be tracked here).
                Effect::Discard { .. } | Effect::DiscardCard { .. } => {
                    Some(crate::types::zones::Zone::Graveyard)
                }
                // CR 400.7 + CR 611.2c: Mass-bounce destination defaults to
                // Hand; downstream "those creatures" / "for each of those
                // permanents" tracking must filter by the actual landing zone.
                Effect::BounceAll { destination, .. } => {
                    Some(destination.unwrap_or(crate::types::zones::Zone::Hand))
                }
                _ => None,
            };
            events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::ZoneChanged { object_id, to, .. }
                        if dest_zone.is_none_or(|d| *to == d) =>
                    {
                        Some(*object_id)
                    }
                    _ => None,
                })
                .collect()
        }
    }
}

fn mandatory_parent_effect_performed(effect: &Effect, events: &[GameEvent]) -> bool {
    match effect {
        Effect::Destroy { .. } | Effect::DestroyAll { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(crate::types::zones::Zone::Battlefield),
                    ..
                }
            )
        }),
        Effect::Sacrifice { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::PermanentSacrificed { .. })),
        Effect::Mill { .. }
        | Effect::ChangeZone { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFromTopUntil { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged { .. })),
        Effect::Counter { .. } | Effect::CounterAll { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCountered { .. })),
        Effect::DealDamage { .. } | Effect::DamageAll { .. } | Effect::Fight { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt { .. })),
        Effect::Discard { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::Discarded { .. })),
        Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::MoveCounters { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        Effect::RemoveCounter { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterRemoved { .. })),
        Effect::Token { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::TokenCreated { .. })),
        // CR 707.10 + CR 608.2c: to copy a spell is to put a copy of it onto the
        // stack, so a `CopySpell` "did anything" iff a copy was actually pushed.
        // This drives the reflexive "if you don't copy a spell this way,
        // <effect>" negation (Shiko and Narset, Unified): the rider fires only
        // when NO copy was made. `copy_spell::resolve` emits `StackPushed` for
        // the new copy on success, and returns early WITHOUT it when the source
        // can't be copied — so the event's presence is the authoritative "a copy
        // was made" signal. Without this arm CopySpell fell into the `_ => true`
        // default, which claimed a copy always happened and wrongly suppressed
        // the draw on the no-copy branch.
        Effect::CopySpell { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })),
        // CR 701.26a: a tap "did anything" iff some permanent became tapped.
        Effect::SetTapState {
            state: TapStateChange::Tap,
            ..
        } => events
            .iter()
            .any(|event| matches!(event, GameEvent::PermanentTapped { .. })),
        // CR 701.26b: an untap "did anything" iff some permanent became untapped.
        Effect::SetTapState {
            state: TapStateChange::Untap,
            ..
        } => events
            .iter()
            .any(|event| matches!(event, GameEvent::PermanentUntapped { .. })),
        Effect::GainLife { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::LifeChanged { amount, .. } if *amount > 0)),
        Effect::LoseLife { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::LifeChanged { amount, .. } if *amount < 0)),
        Effect::Draw { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })),
        Effect::Scry { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Scry,
                    ..
                }
            )
        }),
        Effect::Surveil { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Surveil,
                    ..
                }
            )
        }),
        Effect::Investigate => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Investigate,
                    ..
                }
            )
        }),
        Effect::Proliferate => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Proliferate,
                    ..
                }
            )
        }),
        Effect::Shuffle { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )
        }),
        Effect::SearchLibrary { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::SearchedLibrary,
                    ..
                }
            )
        }),
        // CR 110.2 + CR 608.2c: "that player gains control of ~. If they do, …"
        // gates the rider on whether control actually changed (Kain, Traitorous
        // Dragoon). `resolve_give` emits `EffectResolved` and, when the
        // recipient differs from the object's current controller,
        // `ControllerChanged`.
        Effect::GiveControl { .. } => events.iter().any(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::GiveControl,
                    ..
                } | GameEvent::ControllerChanged { .. }
            )
        }),
        // CR 708.7 + CR 608.2c: A resolving "turn this creature face up" (Etrata,
        // Deadly Fugitive's granted ability) "did anything" iff a permanent
        // actually became face up. `turn_face_up::resolve` emits `TurnedFaceUp`
        // only on success — it is skipped when the object was already face up or
        // when a `CantBeTurnedFaceUp` static (CR 116.2b + CR 708.7) blocks it.
        // The event's presence is therefore the authoritative "the turn-up
        // happened" signal, which drives Etrata's "If you can't, exile it …"
        // rider: that rider gates on `Not { OptionalEffectPerformed }`, so it
        // fires ONLY when no `TurnedFaceUp` occurred. Without this arm the effect
        // would fall into the `_ => true` default and always claim success,
        // suppressing the exile branch even when the turn-up was blocked.
        Effect::TurnFaceUp { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::TurnedFaceUp { .. })),
        // CR 708.2b + CR 608.2c: The inverse sibling of TurnFaceUp. A resolving
        // "turn this ... face down" "did anything" iff a permanent actually
        // became face down. `turn_face_down::resolve` emits `TurnedFaceDown`
        // only on success — it is skipped when the object was already face down
        // (CR 708.2b) or when the object is a double-faced/melded permanent
        // (CR 712.16 + CR 730.2j), which can never be turned face down. The
        // event's presence is therefore the authoritative "the turn-down
        // happened" signal, so a "Turn target ... face down. If you do, …"
        // rider gated on `OptionalEffectPerformed` correctly suppresses its
        // branch when the turn-down was blocked. Without this arm the effect
        // would fall into the `_ => true` default and always claim success.
        Effect::TurnFaceDown { .. } => events
            .iter()
            .any(|event| matches!(event, GameEvent::TurnedFaceDown { .. })),
        _ => true,
    }
}

pub(crate) fn publish_tracked_set(state: &mut GameState, affected_ids: Vec<ObjectId>) {
    // CR 603.7 + CR 608.2c: Chain unification. If an ancestor in this
    // resolution chain already published a tracked set, extend that set with
    // the current publish so compound zone-changing effects expose every
    // affected object to a single downstream "those cards" reference.
    // Otherwise start a new chain-scoped set.
    if let Some(chain_id) = state.chain_tracked_set_id {
        state
            .tracked_object_sets
            .entry(chain_id)
            .or_default()
            .extend(affected_ids);
    } else {
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, affected_ids);
        state.chain_tracked_set_id = Some(set_id);
    }
}

/// CR 608.2c + CR 614.6: Publish the chain tracked set together with the
/// per-object producer-action *cause* recorded by the producing effect.
/// Producers publish/extend exactly as [`publish_tracked_set`] does (so every
/// existing tracked-set reader is unchanged), but each member is additionally
/// stamped — when its producer names a "this way" verb — into the
/// [`GameState::tracked_set_member_causes`] side map keyed by `(chain set,
/// object)`. A downstream "this way" consumer bound to a specific action
/// (`caused_by: Some(_)` on [`TargetFilter::TrackedSetFiltered`] /
/// [`QuantityRef::FilteredTrackedSetSize`]) consults that side map so it counts
/// only the members the matching action produced — e.g. Living Death's "exiled
/// this way" return reads only the `Exiled` members of a merged exile→sacrifice
/// chain set, while a sibling "sacrificed this way" life-gain reads only the
/// `Sacrificed` members of the very same set. Because the stamp is the ACTION,
/// it is unaffected by a replacement that redirects a member's destination
/// (CR 614.6) and never collides with a same-destination action (issue #2932).
/// Members whose producer carries no "this way" verb (`None`) are not stamped
/// and remain visible only to `caused_by: None` references.
pub(crate) fn publish_tracked_set_with_causes(
    state: &mut GameState,
    affected: Vec<(ObjectId, Option<ThisWayCause>)>,
) {
    let ids: Vec<ObjectId> = affected.iter().map(|(id, _)| *id).collect();
    // Publish/extend the id-only set first (identical to `publish_tracked_set`),
    // which establishes or reuses `chain_tracked_set_id`.
    publish_tracked_set(state, ids);
    // CR 608.2c: stamp each member's producer action under the now-current chain
    // set so an action-bound "this way" consumer can discriminate producers that
    // contributed to the same merged set.
    if let Some(chain_id) = state.chain_tracked_set_id {
        let causes = state.tracked_set_member_causes.entry(chain_id).or_default();
        for (id, cause) in affected {
            if let Some(cause) = cause {
                causes.insert(id, cause);
            }
        }
    }
}

/// CR 603.7: A player-chosen "those creatures" set is a fresh resolution
/// scope — never extend an ancestor chain set.
///
/// Unlike [`publish_tracked_set`] (which extends `chain_tracked_set_id` when an
/// ancestor in the resolution chain already published), this *always*
/// allocates a strictly-greater `TrackedSetId` and rebinds
/// `chain_tracked_set_id` so the same-chain `PayCost { ScaledMana }` and the
/// `IfYouDo`/`Untap{TrackedSet}` tail both unify on the freshly-chosen set.
/// Used by `Effect::ChooseObjectsIntoTrackedSet` — an interactive selection is
/// the semantic START of a new scope, not a continuation of a prior one.
pub(crate) fn publish_fresh_tracked_set(
    state: &mut GameState,
    affected_ids: Vec<ObjectId>,
) -> TrackedSetId {
    let set_id = TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(set_id, affected_ids);
    state.chain_tracked_set_id = Some(set_id);
    set_id
}

/// CR 603.7 + CR 109.5: Returns `true` when the effect resolves an acting
/// subject relative to the parent target — i.e., any effect-target slot
/// reachable via [`effect_target_filter`] contains
/// `TargetFilter::ParentTargetController` or `TargetFilter::ParentTarget`.
/// Used by the `repeat_for: TrackedSetSize` loop to decide whether
/// per-iteration parent rebinding is required.
///
/// CR 109.5: "you/your" on an object refers to its controller; for an iterated
/// effect that derives the acting subject from the parent target (e.g., "its
/// controller" on Winds of Abandon's per-creature search), each iteration must
/// rebind the parent reference to the i-th tracked-set member so the per-iter
/// subject resolves correctly.
///
/// Generic: scans whatever target filter the effect exposes via
/// `effect_target_filter`, so any future effect family that carries a
/// parent-target filter (search, draw, life-gain by parent's controller, etc.)
/// participates without code changes here. `Effect::target_filter()` already
/// surfaces `SearchLibrary::target_player`, so iterated-search variants are
/// covered through the same single path.
fn effect_refs_parent_target(effect: &Effect) -> bool {
    effect_parent_ref_slots(effect)
        .iter()
        .any(|filter| filter_refs_parent_target(filter))
}

/// Every object-target filter slot of an effect that may carry a parent-ref,
/// INCLUDING slots `target_filter()` hides. Single source of truth for which
/// slots a member-driven loop inspects for rebinding.
///
/// `target_filter()` surfaces exactly one slot, and for some effects it is not
/// the parent-ref-bearing one:
///  * `Effect::CopyTokenOf` surfaces the token *owner*, not the copy *source*,
///    when the source is a context ref (CR 707.2) — Second Harvest's "for each
///    token you control, create a token that's a copy of that permanent" needs
///    its `target` (copy source) inspected. The arm fires ONLY when the source
///    is a context ref (`source_filter: None && target.is_context_ref()`), which
///    is exactly when `target_filter()` returns `owner` instead of `target`, so
///    `target` is never double-counted.
///  * `Effect::Token` surfaces a *targetable* `attach_to` (the single-target
///    "attached to target creature" host) but hides a *context-ref* `attach_to`
///    (`ParentTarget`), surfacing `owner` instead. The arm fires ONLY when
///    `attach_to.is_context_ref()` — exactly when `target_filter()` returns
///    `owner` rather than `attach_to` — so the filter is never double-counted.
///    Asinine Antics' "for each opponent creature, create a Cursed Role attached
///    to that creature" rebinds through this hidden `ParentTarget` slot.
///  * `Effect::Attach` surfaces `target` but hides `attachment`; the arm fires
///    only when `attachment.is_context_ref()`, mirroring the guards above (a
///    non-context-ref `attachment` can never be a parent-ref, so excluding it
///    cannot change the gate result).
///  * `Effect::UnattachAll` surfaces `target` but hides `attachment`, exactly as
///    `Effect::Attach` does; the same context-ref guard applies. A delayed
///    "when you lose control of this, unattach it" trigger rebinds the per-source
///    `attachment` through this hidden slot (Stolen Uniform, Ogre Geargrabber);
///    without the arm the `_ => {}` fallback snapshots nothing and it resolves inert.
///
/// NOTE: the `_ => {}` arm means "no hidden object slot beyond `target_filter()`".
/// Any FUTURE effect that hides an object slot behind `target_filter()` MUST add
/// an arm here, or its for-each parent-ref form won't rebind.
fn effect_parent_ref_slots(effect: &Effect) -> Vec<&TargetFilter> {
    let mut slots: Vec<&TargetFilter> = effect_target_filter(effect).into_iter().collect();
    match effect {
        Effect::CopyTokenOf {
            target,
            source_filter: None,
            ..
        } if target.is_context_ref() => slots.push(target),
        Effect::Token {
            attach_to: Some(f), ..
        } if f.is_context_ref() => slots.push(f),
        Effect::Attach { attachment, .. } if attachment.is_context_ref() => slots.push(attachment),
        Effect::UnattachAll { attachment, .. } if attachment.is_context_ref() => {
            slots.push(attachment)
        }
        _ => {}
    }
    slots
}

/// True if any object-target slot of the effect references the per-iteration
/// object via a parent context ref. Member-driven `repeat_for: ObjectCount`
/// loops use this to decide whether to rebind the parent target each iteration.
fn filter_refs_same_name_as_parent_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::SameNameAsParentTarget)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_refs_same_name_as_parent_target)
        }
        TargetFilter::Not { filter } => filter_refs_same_name_as_parent_target(filter),
        _ => false,
    }
}

fn effect_iterates_over_parent_target(effect: &Effect) -> bool {
    if effect_parent_ref_slots(effect)
        .iter()
        .any(|f| filter_refs_parent_target(f))
    {
        return true;
    }
    // CR 608.2c: Doubling Chant — `repeat_for: ObjectCount` over creatures you
    // control with a `SearchLibrary` filter using `SameNameAsParentTarget` must
    // rebind the parent target each iteration even though the parent-ref lives on
    // the search filter, not a `TargetFilter::ParentTarget` slot.
    matches!(effect, Effect::SearchLibrary { filter, .. } if filter_refs_same_name_as_parent_target(filter))
}

/// Recurse into compound filters so a wrapped `ParentTargetController` is
/// detected wherever it appears (`Or { filters: [..., ParentTargetController, ...] }`).
fn filter_refs_parent_target(filter: &TargetFilter) -> bool {
    match filter {
        // CR 603.7c + CR 608.2c: a `ParentTargetSlot { index }` delayed effect
        // must snapshot the parent targets at creation, exactly like the broad
        // `ParentTarget` anaphor — the index is honored later by
        // `effect_object_targets` when the snapshot fires.
        TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. } => true,
        TargetFilter::Typed(typed) => matches!(
            typed.controller,
            Some(ControllerRef::ParentTargetController)
        ),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_refs_parent_target)
        }
        TargetFilter::Not { filter } => filter_refs_parent_target(filter),
        _ => false,
    }
}

/// True if the filter directly or recursively references `TargetFilter::TriggeringSource`.
///
/// Used by `delayed_trigger::resolve()` to gate the event-context snapshot for
/// delayed triggers whose inner effect targets the trigger's source object via
/// the "it" anaphor (e.g. "return it to the battlefield").
///
/// Checks all object-target slots via `effect_parent_ref_slots`, including
/// hidden slots that `effect_target_filter` does not surface (e.g.,
/// `Attach.attachment`).
fn filter_refs_triggering_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::TriggeringSource => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_refs_triggering_source)
        }
        TargetFilter::Not { filter } => filter_refs_triggering_source(filter),
        _ => false,
    }
}

fn effect_refs_triggering_source(effect: &Effect) -> bool {
    effect_parent_ref_slots(effect)
        .iter()
        .any(|f| filter_refs_triggering_source(f))
}

fn ability_refs_triggering_source(ability: &ResolvedAbility) -> bool {
    effect_refs_triggering_source(&ability.effect)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_refs_triggering_source)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_refs_triggering_source)
}

/// CR 603.7 + CR 109.5: Replace the first `TargetRef::Object` in a target
/// slice with the supplied object id. Used by the `repeat_for: TrackedSetSize`
/// per-iteration rebind so the i-th iteration's parent reference (e.g.,
/// `ParentTargetController` resolution in `search_library::resolve_library_owner`)
/// binds to the i-th tracked-set member, making "its controller" (CR 109.5)
/// resolve to the i-th object's controller per iteration.
fn rebind_first_object_target(
    targets: &mut Vec<TargetRef>,
    new_id: crate::types::identifiers::ObjectId,
) {
    if let Some(slot) = targets
        .iter_mut()
        .find(|t| matches!(t, TargetRef::Object(_)))
    {
        *slot = TargetRef::Object(new_id);
    } else {
        targets.push(TargetRef::Object(new_id));
    }
}

/// CR 122.1 + CR 608.2c: Rebind a counter-kind-driven `ChooseOneOf` to the
/// current iteration's counter kind. For each branch tagged
/// `iteration_kind_binding == Some(RebindToIteratedKind)`, rewrites that
/// branch's `Effect::PutCounter` counter type to `kind`. The fixed branch
/// (binding `None`, e.g. "+1/+1") is left untouched. Used by the
/// `repeat_for: DistinctCounterKindsAmong` loop so each iteration's dynamic
/// branch puts "a counter of that kind" (CR 608.2d resolution choice).
/// CR 608.2c + CR 608.2d: True when this ability's `repeat_for` is a
/// `DistinctCounterKindsAmong` loop — the per-counter-kind iteration source
/// (Bribe Taker). The "you may" on such an ability applies INDEPENDENTLY to
/// each iterated kind (the controller may decline kind A and accept kind B —
/// see the card's official ruling), so the up-front single-gate at the top of
/// `resolve_chain_body` is suppressed for this shape and optionality is fired
/// per-iteration inside the `repeat_for` loop instead.
fn has_kind_driven_repeat(ability: &ResolvedAbility) -> bool {
    matches!(
        ability.repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong { .. },
        })
    )
}

/// CR 608.2c + CR 608.2d: Doubling Chant — `repeat_for: ObjectCount` with a
/// per-iteration parent ref (`SameNameAsParentTarget` on `SearchLibrary`) makes
/// its "you may" apply per iterated creature, not once up front. Suppress the
/// single optional gate in `resolve_chain_body` and fire optionality inside the
/// `repeat_for` loop instead (mirrors `has_kind_driven_repeat`).
fn has_member_driven_repeat(ability: &ResolvedAbility) -> bool {
    matches!(
        ability.repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { .. },
        })
    ) && effect_iterates_over_parent_target(&ability.effect)
}

fn has_member_driven_repeat_after_hydration(state: &GameState, ability: &ResolvedAbility) -> bool {
    has_member_driven_repeat(&ability_with_event_context_targets(state, ability))
}

/// CR 608.2d: "A player can't choose an impossible option." An optional effect
/// whose only reachable outcome is a no-op must not be offered as a "you may"
/// prompt at all. Currently the sole such class is `Effect::PutChosenCounter`
/// reached after a `ChooseCounterKind` whose 0-kind branch fired: the iterated
/// permanent had no counters, so no `ChosenAttribute::Counter` was persisted onto
/// the source and "put an additional counter of that kind" has no "that kind" to
/// add. The Caves of Androzani II/III iterates over EVERY non-Saga permanent, so
/// without this guard the controller is flooded with impossible yes/no prompts
/// for lands and counterless creatures. Suppressing the prompt lets the effect
/// resolve as its defined no-op instead (CR 122.6).
fn optional_effect_is_infeasible(state: &GameState, ability: &ResolvedAbility) -> bool {
    match &ability.effect {
        Effect::PutChosenCounter { .. } => {
            !state.objects.get(&ability.source_id).is_some_and(|src| {
                src.chosen_attributes
                    .iter()
                    .any(|attr| matches!(attr, ChosenAttribute::Counter(_)))
            })
        }
        _ => false,
    }
}

/// CR 603.12a + CR 608.2c: True when this ability is a "you may pay {cost} up to
/// N times. When you do, [reflexive]" process (Hawkeye, Master Marksman — "Trick
/// Arrows"). Unlike a generic `repeat_for` loop (one up-front "you may" then N
/// mandatory iterations), each payment is offered SEPARATELY and the reflexive
/// triggers exactly once for one-or-more payments, sized by the payment count
/// (CR 700.2d). Such an ability is driven by `drive_repeated_optional_payment`
/// instead of the up-front optional gate + `repeated_full_chain` loop. Mirrors
/// `has_kind_driven_repeat` / `has_member_driven_repeat`.
///
/// The cost is restricted to the SYNCHRONOUS mana class (see
/// `is_synchronous_mana_pay_cost`): the driver pays each iteration in line and
/// reads the failure flag immediately, so a cost that PAUSES (sets
/// `WaitingFor::PayAmountChoice` for unannounced-X / pay-any-amount, or returns
/// `PaymentOutcome::Paused` for interactive `Discard`/`Sacrifice`/replacement
/// payments) cannot be driven here — it would mis-count K and clobber its own
/// continuation. Any repeated-optional ability whose cost pauses falls through to
/// the generic `repeat_for` path and stays honestly unimplemented (no false
/// green) until the driver gains pause-resume plumbing.
fn is_repeated_optional_payment(ability: &ResolvedAbility) -> bool {
    ability.optional
        && is_synchronous_mana_pay_cost(&ability.effect)
        && matches!(ability.repeat_for, Some(QuantityExpr::Fixed { .. }))
        && ability
            .sub_ability
            .as_ref()
            .is_some_and(|sub| sub.condition == Some(AbilityCondition::WhenYouDo))
}

/// CR 118.1: A `PayCost` whose cost is a pure, fully-static mana cost — the only
/// resolution-time payment class guaranteed synchronous (auto-tap + pool
/// deduction returns Paid/Failed, never a `PayAmountChoice`/`Paused` pause).
/// Excludes:
/// - mana costs carrying an unannounced `{X}` (which prompt `PayAmountChoice`,
///   pay.rs `cost_has_x` arm),
/// - a per-object `scale` (resolution-only multiplier, not the repeated-optional
///   shape),
/// - every non-mana `AbilityCost` (Discard/Sacrifice/PayLife/PayEnergy/Composite/
///   …) — these either branch interactively or pause.
///
/// A mana payment intercepted by a payment-replacement effect is the one residual
/// pause vector; it is outside the current corpus and the predicate cannot see it
/// at AST time. A future pause-resume driver would lift this.
fn is_synchronous_mana_pay_cost(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::PayCost {
            cost: AbilityCost::Mana { cost },
            scale: None,
            ..
        } if !crate::game::casting_costs::cost_has_x(cost)
    )
}

/// CR 603.12a: Fire the first per-iteration payment prompt for a
/// repeated-optional-payment process and stash the continuation. Each
/// subsequent prompt + the once-after-loop reflexive are driven by
/// `resolve_repeated_optional_payment_choice` as the player answers each
/// `DecideOptionalEffect`. Returns having set `WaitingFor::OptionalEffectChoice`
/// (or, for an "up to 0 times" bound, a no-op resolution with K = 0).
fn drive_repeated_optional_payment(
    state: &mut GameState,
    ability: &ResolvedAbility,
) -> Result<(), EffectError> {
    // CR 700.2d: the "up to N" payment budget.
    let n = match &ability.repeat_for {
        Some(QuantityExpr::Fixed { value }) => (*value).max(0),
        _ => 0,
    };
    let Some(reflexive) = ability.sub_ability.as_ref() else {
        return Ok(());
    };
    if n == 0 {
        // CR 603.12a: with no payment opportunity, K stays 0 — the reflexive
        // never triggers and the modal is never offered.
        return Ok(());
    }
    // The PayCost-only unit prompted/paid each iteration: clear `repeat_for` and
    // `sub_ability` so resolving it neither re-enters this driver nor
    // re-resolves the reflexive, and clear `optional` (the prompt itself is the
    // optionality — on accept the payment is performed unconditionally).
    let mut payment_unit = ability.clone();
    payment_unit.repeat_for = None;
    payment_unit.sub_ability = None;
    payment_unit.optional = false;
    // Strip any resolution-time condition from the per-iteration payment unit:
    // the top-level ability's condition (if any) is evaluated once in the
    // depth==0 prelude and must not re-gate each individual {1} payment.
    payment_unit.condition = None;

    state.pending_repeated_optional_payment = Some(Box::new(PendingRepeatedOptionalPayment {
        payment_unit: Box::new(payment_unit),
        reflexive: Box::new((**reflexive).clone()),
        remaining: (n - 1) as u32,
    }));
    state.waiting_for = WaitingFor::OptionalEffectChoice {
        player: ability.controller,
        source_id: ability.source_id,
        description: ability.description.clone(),
        may_trigger_key: None,
    };
    Ok(())
}

/// CR 603.12a + CR 608.2c: Resume a repeated-optional-payment process for one
/// `DecideOptionalEffect`. On accept, pay the per-iteration cost (resolution-
/// time mana payment is synchronous — auto-tap, never pauses) and count it
/// toward K iff it succeeded; then offer the next payment if budget remains.
/// On decline (or after the last payment), resolve the reflexive modal exactly
/// once iff K >= 1. Called by `handle_optional_effect_choice`.
pub(super) fn resolve_repeated_optional_payment_choice(
    state: &mut GameState,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some(pending) = state.pending_repeated_optional_payment.take() else {
        return Ok(());
    };
    let PendingRepeatedOptionalPayment {
        payment_unit,
        reflexive,
        remaining,
    } = *pending;

    if accept {
        // CR 608.2c: clear the resolution failure flag before THIS payment so a
        // prior iteration's failure can't be misread as this payment's outcome
        // (which would under-count K on a fail-then-succeed sweep).
        state.cost_payment_failed_flag = false;
        // CR 118.1: pay the cost. Depth >= 1 keeps the resolution-
        // local K counter (cleared only by the depth==0 prelude) alive.
        resolve_ability_chain(state, &payment_unit, events, 1)?;
        // CR 603.12a: count only successful payments toward K.
        if !state.cost_payment_failed_flag {
            state.optional_cost_payments_this_resolution = state
                .optional_cost_payments_this_resolution
                .saturating_add(1);
            // CR 118.3 + CR 603.12a: only a payment that actually succeeded can
            // lead to another "you may pay {1} [again]" offer. A failed payment
            // — the player accepted but lacked the resources (CR 118.3) — does
            // not satisfy the "if you do" rider, so the sequence ends here and
            // the reflexive resolves once with the payments already made (it is
            // not offered another payment opportunity).
            if remaining > 0 {
                // CR 700.2d: offer the next "up to N" payment.
                let player = payment_unit.controller;
                let source_id = payment_unit.source_id;
                let description = payment_unit.description.clone();
                state.pending_repeated_optional_payment =
                    Some(Box::new(PendingRepeatedOptionalPayment {
                        payment_unit,
                        reflexive,
                        remaining: remaining - 1,
                    }));
                state.waiting_for = WaitingFor::OptionalEffectChoice {
                    player,
                    source_id,
                    description,
                    may_trigger_key: None,
                };
                return Ok(());
            }
        }
    }
    // CR 603.12a: a decline ends the repeated payment early; either way the
    // reflexive resolves exactly once iff at least one payment succeeded.
    finish_repeated_optional_payment(state, &reflexive, events)
}

/// CR 603.12a: resolve the reflexive modal exactly once iff K >= 1. K == 0 ⇒ no
/// "do" occurred ⇒ the modal is never offered. Resolved at depth >= 1 so the
/// `depth == 0` prelude does not zero the K counter before
/// `modal_choice_for_player` reads it as the dynamic modal cap (CR 700.2d).
fn finish_repeated_optional_payment(
    state: &mut GameState,
    reflexive: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if state.optional_cost_payments_this_resolution >= 1 {
        // CR 608.2c: clear the per-iteration failure flag before the reflexive so
        // a loop-terminating decline can't suppress it. (The WhenYouDo gate is
        // flag-independent for the reflexive's `GenericEffect`, but clear
        // defensively for the general class.)
        state.cost_payment_failed_flag = false;
        resolve_ability_chain(state, reflexive, events, 1)?;
    }
    Ok(())
}

/// CR 608.2c + CR 609.3: True when a counted repeat loop must wrap scoped
/// and/or unless-pay instructions (Torment of Hailfire — "Repeat X times.
/// Each opponent loses 3 life unless …"). The repeat count is the outermost
/// process; `player_scope` and `unless_pay` resolve inside each iteration.
fn repeat_for_outermost_with_scope_or_unless(ability: &ResolvedAbility) -> bool {
    ability.repeat_for.is_some()
        && !has_kind_driven_repeat(ability)
        && !has_member_driven_repeat(ability)
        && (ability.player_scope.is_some() || ability.unless_pay.is_some())
}

/// CR 609.3: Drive a `repeat_for` loop whose iterations each run the full
/// `resolve_chain_body` (scoped fan-out + unless-pay + effect) with
/// `repeat_for` cleared so the inner pass does not re-enter this driver.
fn drive_repeat_for_outermost(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    let hydrated = hydrate_event_context_targets(state, ability);
    let effective = hydrated.as_ref();
    let base_iterations = if let Some(ref qty) = effective.repeat_for {
        crate::game::quantity::resolve_quantity_with_targets(state, qty, effective).max(0) as usize
    } else {
        1
    };

    let initial_waiting_for = state.waiting_for.clone();
    let mut iteration = 0usize;
    while iteration < base_iterations {
        let mut iter_ability = effective.clone();
        iter_ability.repeat_for = None;
        resolve_chain_body(state, &iter_ability, events, depth)?;
        if state.waiting_for != initial_waiting_for {
            let next_iteration = iteration + 1;
            if next_iteration < base_iterations {
                let mut resume = effective.clone();
                resume.repeat_for = None;
                state.pending_repeat_iteration =
                    Some(crate::types::game_state::PendingRepeatIteration {
                        ability: Box::new(resume),
                        tracked_members: Vec::new(),
                        iterated_counter_kinds: Vec::new(),
                        next_iteration,
                        total_iterations: base_iterations,
                    });
            }
            break;
        }
        iteration += 1;
    }
    Ok(())
}

fn rebind_iterated_counter_kind(
    ability: &mut ResolvedAbility,
    kind: crate::types::counter::CounterType,
) {
    if let Effect::ChooseOneOf { branches, .. } = &mut ability.effect {
        for branch in branches.iter_mut() {
            if branch.iteration_kind_binding
                == Some(crate::types::ability::IterationKindBinding::RebindToIteratedKind)
            {
                // CR 122.1: rebind both the add (`PutCounter`) and remove
                // (`RemoveCounter`) leaves to the iterated kind. Dramatist's
                // Puppet / Quarry Hauler offer a per-kind add-OR-remove choice,
                // so the remove branch must also track the current kind.
                match branch.effect.as_mut() {
                    Effect::PutCounter { counter_type, .. } => *counter_type = kind.clone(),
                    Effect::RemoveCounter { counter_type, .. } => {
                        *counter_type = Some(kind.clone())
                    }
                    _ => {}
                }
            }
        }
    }
}

pub(crate) fn resolved_object_filter(
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> TargetFilter {
    filter::normalize_contextual_filter(target_filter, &ability.targets)
}

fn filter_uses_relative_controller_you(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.controller == Some(ControllerRef::You),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_uses_relative_controller_you)
        }
        TargetFilter::Not { filter } => filter_uses_relative_controller_you(filter),
        _ => false,
    }
}

/// CR 503.1a + CR 608.2d (issue #1535): True when the filter is scoped to the
/// resolution's scoped player — e.g. "that player ... a card they control"
/// bound by an "at the beginning of each player's upkeep, that player may ..."
/// trigger (Braids, Conjurer Adept). Such a filter must resolve its acting
/// player and candidate pool against the per-iteration scoped player, not the
/// ability's controller.
fn filter_uses_relative_controller_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.controller == Some(ControllerRef::ScopedPlayer)
                || tf.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer
                        }
                    )
                })
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_uses_relative_controller_scoped)
        }
        TargetFilter::Not { filter } => filter_uses_relative_controller_scoped(filter),
        _ => false,
    }
}

pub(crate) fn controller_for_relative_filter(
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> PlayerId {
    // CR 503.1a + CR 608.2d (issue #1535): a filter scoped to the per-iteration
    // scoped player ("that player ... from their hand" under "each player's
    // upkeep") resolves to that scoped player, not the ability's controller.
    if let Some(scoped) = ability.scoped_player {
        if filter_uses_relative_controller_scoped(target_filter) {
            return scoped;
        }
    }
    if filter_uses_relative_controller_you(target_filter)
        && ability.scoped_player.is_none()
        && ability
            .targets
            .iter()
            .any(|target| matches!(target, TargetRef::Player(_)))
    {
        ability.target_player()
    } else {
        ability.controller
    }
}

/// CR 121.1 + CR 615.5 + CR 609.7: Resolve the acting player for an effect
/// whose target slot may be a context-ref. Mirrors `life::resolve_life_loss_target`
/// for the Draw/Scry/Surveil class — those handlers historically short-circuited
/// to `ability.controller` whenever `target.is_context_ref()`, which is wrong
/// for `PostReplacementSourceController` (the prevented event's source has its
/// own controller, distinct from the replacement's controller).
///
/// Resolution order:
/// 1. First `TargetRef::Player` in `ability.targets` (chosen at announcement).
/// 2. `resolve_event_context_target` on the filter — reads `state` slots like
///    `current_trigger_event` (TriggeringSpellController) and
///    `post_replacement_event_source` (PostReplacementSourceController).
/// 3. Fall back to `ability.controller` (preserves prior semantics for context
///    refs whose state slots are empty in the current resolution window).
pub(crate) fn resolve_player_for_context_ref(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
) -> PlayerId {
    if matches!(target_filter, TargetFilter::ScopedPlayer) {
        return ability.scoped_player.unwrap_or(ability.controller);
    }

    // CR 608.2c + CR 109.4: A player-only reference to the Nth chosen player
    // ("choose a player to draw a card") resolves from the resolution-scoped
    // chosen-players list. Falls back to `ability.controller` when the index
    // is out of range (fewer eligible players were chosen — e.g. Gluntch's
    // third choice skipped in a two-player game).
    if let Some(index) = target_filter.chosen_player_index() {
        return ability
            .chosen_players
            .get(index as usize)
            .copied()
            .unwrap_or(ability.controller);
    }

    // CR 115.1: For non-context-ref filters (e.g. `TargetFilter::Player` from
    // "target player draws"), the drawing player was chosen at announcement
    // and lives in `ability.targets`. Context-ref filters (Controller,
    // PostReplacementSourceController, …) MUST NOT consult `ability.targets`:
    // chain target-propagation (resolve_ability_chain) inherits parent targets
    // into a sub-ability with empty targets, so a sub Draw whose filter is
    // `Controller` would otherwise pick up the parent's Player target.
    if !target_filter.is_context_ref() {
        if let Some(player) = ability.targets.iter().find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        }) {
            return player;
        }
    }
    // CR 608.2c: A chained player anaphor can use `ParentTarget` when the
    // parent target was itself a player ("that library" after "target player's
    // library"). Object-target uses of `ParentTarget` continue through the
    // object/controller resolution paths below.
    if matches!(target_filter, TargetFilter::ParentTarget) {
        if let Some(player) = ability.targets.iter().find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        }) {
            return player;
        }
    }
    // CR 115.1d + CR 608.2c: Parent-target controller/owner anaphors bind to
    // targets inherited from the parent instruction when present (Assassin's
    // Trophy, Amphin Mutineer). This must precede `resolve_event_context_target`:
    // that helper's `ParentTargetController` arm resolves the *trigger event*
    // source's controller (the entering permanent), not the parent ability's
    // chosen target (the exiled creature).
    if matches!(target_filter, TargetFilter::ParentTargetController) {
        if let Some(player) = crate::game::ability_utils::parent_target_controller(ability, state) {
            return player;
        }
    }
    if matches!(target_filter, TargetFilter::ParentTargetOwner) {
        if let Some(player) = crate::game::ability_utils::parent_target_owner(ability, state) {
            return player;
        }
    }
    if matches!(target_filter, TargetFilter::SourceChosenPlayer) {
        // CR 607.2d + CR 608.2c: Resolve "the chosen player" from the
        // source's linked persisted choice.
        if let Some(player) =
            crate::game::game_object::source_chosen_player(state, ability.source_id)
        {
            return player;
        }
    }
    if let Some(target_ref) = crate::game::targeting::resolve_event_context_target(
        state,
        target_filter,
        ability.source_id,
    ) {
        return match target_ref {
            TargetRef::Player(player) => player,
            TargetRef::Object(id) => state
                .objects
                .get(&id)
                .map(|obj| obj.controller)
                .unwrap_or(ability.controller),
        };
    }
    if matches!(target_filter, TargetFilter::Controller) {
        return ability.controller;
    }
    // CR 109.5: `OriginalController` resolves the player who put the spell or
    // ability on the stack, even when a surrounding `player_scope` iteration
    // (e.g., `PlayerFilter::VotedFor` for Master of Ceremonies) has rebound
    // `ability.controller` to an iterated voter. Mirrors the quantity-layer
    // behavior in `resolve_quantity_with_targets`. Used by parser-level
    // distribution of compound subjects ("you and that player each Y") so the
    // first half ("you") consistently fires for the printed ability controller.
    if matches!(target_filter, TargetFilter::OriginalController) {
        return ability.original_controller.unwrap_or(ability.controller);
    }
    // CR 108.3 + CR 608.2c: `ParentTargetOwner` AttachedTo fallback when no
    // inherited targets and no trigger-event referent (Enslave phase trigger).
    if matches!(target_filter, TargetFilter::ParentTargetOwner) {
        if let Some(player) =
            crate::game::targeting::resolve_effect_player_ref(state, ability, target_filter)
        {
            return player;
        }
    }
    ability.controller
}

/// CR 117.3a: Determine which player receives the "may" prompt for an optional
/// effect. Most optional effects go to the caster (CR 609.3). Subject-anchored
/// optional effects — "its controller may search their library" (Assassin's
/// Trophy, Path to Exile, Ghost Quarter, Oblation, …) — route the prompt to the
/// acting subject (the target permanent's controller). This mirrors the
/// `resolve_library_owner` logic in `search_library.rs` but applies generally
/// to any optional effect whose embedded player-scope target is a context-ref.
fn optional_prompt_player(state: &GameState, ability: &ResolvedAbility) -> PlayerId {
    if let Effect::PayCost { payer, .. } = &ability.effect {
        if let Some(player) =
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        {
            return player;
        }
    }
    if let Effect::Sacrifice { target, .. } = &ability.effect {
        if target_filter_controller_scope(target) == Some(ControllerRef::ParentTargetController) {
            if let Some(player) = crate::game::targeting::resolve_effect_player_ref(
                state,
                ability,
                &TargetFilter::ParentTargetController,
            ) {
                return player;
            }
        }
    }

    // CR 707.10 + CR 707.10c: "That player may copy this spell" (the Chain
    // cycle — Chain of Acid / Plasma / Smog / Vapor). The optional copy
    // sub-ability is offered to, and resolved by, the *targeted* player — not
    // the original spell's caster. The targeted player arrives as a
    // `TargetRef::Player` inherited from the parent effect's target list.
    if matches!(ability.effect, Effect::CopySpell { .. }) {
        if let Some(player) = ability.targets.iter().find_map(|t| match t {
            TargetRef::Player(player) => Some(*player),
            TargetRef::Object(_) => None,
        }) {
            return player;
        }
    }

    // CR 608.2c (issue #822): Subject-anchored SearchLibrary — "its controller
    // may search their library" (Erode, Path to Exile) — prompts the parent
    // target's controller. Route through the same LKI-aware central resolver
    // the `Sacrifice` branch above uses instead of reading `obj.controller`
    // directly: Destroy/Exile always moves the target off the battlefield
    // before this optional gate is evaluated, and `reset_for_battlefield_exit`
    // resets `base_controller` to the object's owner at that point, so a live
    // lookup silently prompts the owner instead of the controller whenever a
    // stolen/control-changed creature is the one destroyed or exiled.
    if let Effect::SearchLibrary {
        target_player: Some(TargetFilter::ParentTargetController),
        ..
    } = &ability.effect
    {
        if let Some(player) = crate::game::targeting::resolve_effect_player_ref(
            state,
            ability,
            &TargetFilter::ParentTargetController,
        ) {
            return player;
        }
    }

    // CR 503.1a + CR 608.2d (issue #1535): per-player upkeep optional effects
    // ("that player may put a card from their hand ...") route the prompt to
    // the scoped player, not the ability's controller (Braids, Conjurer Adept).
    if let Some(scoped) = ability.scoped_player {
        if let Effect::ChangeZone { target, .. } = &ability.effect {
            if filter_uses_relative_controller_scoped(target) {
                return scoped;
            }
        }
        if ability
            .effect
            .target_filter()
            .is_some_and(filter_uses_relative_controller_scoped)
        {
            return scoped;
        }
    }

    ability.controller
}

fn ability_with_event_context_targets(
    state: &GameState,
    ability: &ResolvedAbility,
) -> ResolvedAbility {
    let mut pending = ability.clone();
    if matches!(pending.effect, Effect::Myriad) && pending.targets.is_empty() {
        if let Some(defending_player) = myriad::defending_player_from_attack_event(
            state.current_trigger_event.as_ref(),
            pending.source_id,
        ) {
            pending.targets.push(TargetRef::Player(defending_player));
        }
        return pending;
    }
    if pending.targets.is_empty() {
        if let Some(filter) = pending.effect.target_filter() {
            if filter.is_context_ref() {
                if let Some(target) = crate::game::targeting::resolve_event_context_target(
                    state,
                    filter,
                    pending.source_id,
                ) {
                    pending.targets.push(target);
                }
            }
        }
    }
    pending
}

/// CR 603.2: When an ability's `targets` are still empty at resolution but its
/// effect carries an event-context recipient (`TriggeringPlayer`, etc.), bind
/// that referent into `targets` before payer/effect resolution. Shared by the
/// unless-pay interceptor and the main effect path (issue #2361).
fn hydrate_event_context_targets<'a>(
    state: &GameState,
    ability: &'a ResolvedAbility,
) -> Cow<'a, ResolvedAbility> {
    if !ability.targets.is_empty() {
        return Cow::Borrowed(ability);
    }
    let Some(filter) = extract_event_context_filter(&ability.effect) else {
        return Cow::Borrowed(ability);
    };
    let Some(target_ref) =
        crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
    else {
        return Cow::Borrowed(ability);
    };
    let mut resolved = ability.clone();
    resolved.targets = vec![target_ref];
    Cow::Owned(resolved)
}

/// CR 603.2: Filters that auto-resolve from `state.current_trigger_event` during
/// hydration / unless-pay payer resolution (issue #2361, Kain #1335).
fn hydratable_event_context_filter(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSource
            | TargetFilter::DefendingPlayer
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTarget
            | TargetFilter::StackSpell
    )
}

/// CR 603.2: Extract an event-context target filter from an effect, if present.
/// Returns the filter only for event-context variants (TriggeringSpellController, etc.)
/// that auto-resolve from `state.current_trigger_event` at resolution time.
fn extract_event_context_filter(effect: &Effect) -> Option<&TargetFilter> {
    // CR 110.2 + CR 603.7c: `GiveControl` carries both an object `target` and a
    // `recipient`. Kain ("that player gains control of Kain") binds the recipient
    // to `TriggeringPlayer` while the object is `SelfRef` — only the recipient
    // is an event-context player ref and must be hydrated into `ability.targets`
    // when empty (issue #1335).
    if let Effect::GiveControl { target, recipient } = effect {
        if hydratable_event_context_filter(recipient) {
            return Some(recipient);
        }
        if hydratable_event_context_filter(target) {
            return Some(target);
        }
        return None;
    }

    let filter = match effect {
        Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::PairWith { target }
        | Effect::Destroy { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::RemoveAllDamage { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::MoveCounters { target, .. }
        | Effect::ChangeZone { target, .. }
        | Effect::RevealHand { target, .. }
        | Effect::Reveal { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Attach { target, .. }
        | Effect::UnattachAll { target, .. }
        | Effect::Transform { target, .. }
        | Effect::CopySpell { target, .. }
        | Effect::CastCopyOfCard { target, .. }
        | Effect::CopyTokenOf { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::CastFromZone { target, .. }
        | Effect::PreventDamage { target, .. }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target, .. }
        // Symmetry with PhaseOut across the PhaseOut/PhaseIn walker class. No
        // current card hydrates a `PhaseIn` target from event context (The
        // Pandorica snapshots `PhaseIn{ParentTarget}` into `ability.targets` at
        // CreateDelayedTrigger resolution rather than carrying a
        // TriggeringPlayer/DefendingPlayer filter), but listing it keeps the
        // walker complete should such a card appear.
        | Effect::PhaseIn { target, .. }
        | Effect::ForceBlock { target, .. }
        | Effect::ForceAttack { target, .. }
        | Effect::PutAtLibraryPosition { target, .. }
        | Effect::PutOnTopOrBottom { target, .. }
        | Effect::ChangeTargets { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::GrantExtraLoyaltyActivations { target, .. }
        | Effect::Double { target, .. }
        // CR 608.2k + CR 603.7c: "that player" sub-effects carry an event-context
        // target (TriggeringPlayer/DefendingPlayer/etc.) that auto-resolves from
        // the current trigger event at resolution time — not a fresh target choice.
        | Effect::Discard { target, .. }
        | Effect::DiscardCard { target, .. }
        | Effect::Mill { target, .. }
        // CR 121.1 + CR 603.7c: "they draw a card" off an opponent-subject
        // trigger (Firemane Commando) carries `target: TriggeringPlayer`, which
        // must auto-bind from the current trigger event so the drawing player
        // is the acting opponent — not the trigger controller.
        | Effect::Draw { target, .. }
        | Effect::Shuffle { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::LoseAllPlayerCounters { target, .. }
        // Additional player-targeted effects: when chained off a "that player"
        // subject in trigger context, their target is an event-context ref
        // (e.g., TriggeringPlayer) rather than a fresh target prompt.
        | Effect::SetLifeTotal { target, .. }
        | Effect::SkipNextTurn { target, .. }
        | Effect::SkipNextStep { target, .. }
        | Effect::ControlNextTurn { target, .. }
        | Effect::AdditionalPhase { target, .. }
        | Effect::Detain { target, .. }
        | Effect::TargetOnly { target } => target,
        // CR 701.26a/b + CR 603.7c: only the single-permanent tap/untap exposes
        // an event-context target. The mass (`All`) scope's `target` is a
        // population filter, not a per-event target ref — it must not be
        // auto-resolved here (matching the legacy `TapAll`/`UntapAll`, which had
        // no event-context target at all).
        Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        } => target,
        // CR 603.7c + CR 608.2c: `GenericEffect` carries an optional `target` that may
        // be an event-context ref (e.g., `TriggeringSource` for "that land doesn't untap
        // during its controller's next untap step" on a TapsForMana trigger). Routing it
        // through the event-context resolver binds the transient continuous effect to
        // the specific triggering object, mirroring targeted pump/bounce semantics.
        Effect::GenericEffect {
            target: Some(ref filter),
            ..
        } => filter,
        Effect::Token { owner, .. } => owner,
        Effect::RevealTop { player, .. } => player,
        Effect::ExileTop { player, .. } => player,
        // CR 119.3 + CR 603.7c: LoseLife with event-context target (e.g., TriggeringPlayer
        // from "whenever an opponent draws a card, they lose 2 life").
        Effect::LoseLife {
            target: Some(ref filter),
            ..
        } => filter,
        // CR 120.1 + CR 603.2: `EachSourceDealsDamage`'s `Shared` recipient may be
        // an event-context ref (`TriggeringSource` — Sarkhan; `ParentTarget` —
        // Missy) that auto-resolves from the current trigger event at resolution.
        // `EachController` and the deferred per-source recipients carry no
        // event-context target.
        Effect::EachSourceDealsDamage {
            recipient: EachDamageRecipient::Shared(ref filter),
            ..
        } => filter,
        _ => return None,
    };

    if hydratable_event_context_filter(filter) {
        Some(filter)
    } else {
        None
    }
}

fn previous_effect_amount_from_events(
    state: &GameState,
    ability: &ResolvedAbility,
    events: &[GameEvent],
) -> Option<i32> {
    let amount = match &ability.effect {
        Effect::DealDamage { .. } | Effect::DamageAll { .. } | Effect::DamageEachPlayer { .. } => {
            events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::DamageDealt { amount, .. } => {
                        Some(crate::game::arithmetic::u32_to_i32_saturating(*amount))
                    }
                    _ => None,
                })
                .sum()
        }
        Effect::Fight { .. } => {
            // CR 120.10 + CR 701.14a: "add that much {R}" (The Last Agni Kai)
            // reads the excess dealt to the *fought* creature — the fight's
            // damage recipient, which in a dual-target fight is the second
            // chosen target, not the fighting subject. Use the shared fighter
            // resolver so this stays in lockstep with
            // `previous_effect_excess_amount_from_events`.
            let fought_creature = fight::resolve_fight_fighters(state, ability)
                .ok()
                .flatten()
                .map(|(_subject, fought)| fought);
            events
                .iter()
                .filter_map(|event| match (event, fought_creature) {
                    (
                        GameEvent::DamageDealt {
                            target: TargetRef::Object(id),
                            excess,
                            ..
                        },
                        Some(fought_creature),
                    ) if *id == fought_creature => {
                        Some(crate::game::arithmetic::u32_to_i32_saturating(*excess))
                    }
                    _ => None,
                })
                .sum()
        }
        Effect::LoseLife { .. } | Effect::PayCost { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged { amount, .. } if *amount < 0 => Some(-*amount),
                _ => None,
            })
            .sum(),
        Effect::GainLife { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged { amount, .. } if *amount > 0 => Some(*amount),
                _ => None,
            })
            .sum(),
        Effect::RemoveCounter { .. } => events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CounterRemoved { count, .. } => {
                    Some(crate::game::arithmetic::u32_to_i32_saturating(*count))
                }
                _ => None,
            })
            .sum(),
        // CR 706.2 + CR 706.4 + CR 608.2c: `roll_die::resolve` is the single
        // authority for the scalar value a follow-up `PreviousEffectAmount` or
        // `EventContextAmount` consumer reads. That avoids re-deriving from an
        // event slice that may contain result-table branch effects or nested
        // rolls interleaved with the outer dice.
        Effect::RollDie { .. } => return state.die_result_this_resolution,
        _ => 0,
    };

    (amount > 0).then_some(amount)
}

/// CR 120.10: Resolution-local excess-damage twin of
/// `previous_effect_amount_from_events`. Sums the `excess` channel of the
/// `DamageDealt` events produced by the preceding damage effect so the
/// "if excess damage was dealt … this way" condition class (CR 120.10) reads
/// overkill-beyond-lethal rather than the total (CR 120.6). `Fight` is included
/// and scoped to the fight target, mirroring the `Fight` arm of
/// `previous_effect_amount_from_events`.
///
/// Kept as a sibling of `previous_effect_amount_from_events` (which stays
/// byte-unchanged) because the channel is a property of the *downstream*
/// `PreviousEffectAmount` condition, not known at stamp time: both channels are
/// stamped eagerly and `evaluate_condition` selects. Returns `None` when no
/// excess was dealt.
fn previous_effect_excess_amount_from_events(
    state: &GameState,
    ability: &ResolvedAbility,
    events: &[GameEvent],
) -> Option<i32> {
    let excess: i32 = match &ability.effect {
        Effect::DealDamage { .. } | Effect::DamageAll { .. } | Effect::DamageEachPlayer { .. } => {
            events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::DamageDealt { excess, .. } => {
                        Some(crate::game::arithmetic::u32_to_i32_saturating(*excess))
                    }
                    _ => None,
                })
                .sum()
        }
        Effect::Fight { .. } => {
            // CR 120.10 + CR 701.14a: The Fight excess condition names the
            // *fought* creature (The Last Agni Kai: "if the creature the
            // opponent controls is dealt excess damage this way"), which is the
            // fight's damage recipient — never the fighting subject. Read the
            // recipient from the shared fighter resolver so a dual-target fight
            // ("target creature you control fights target creature an opponent
            // controls") sums excess dealt to the correct creature instead of
            // the first chosen target.
            let fought_creature = fight::resolve_fight_fighters(state, ability)
                .ok()
                .flatten()
                .map(|(_subject, fought)| fought);
            events
                .iter()
                .filter_map(|event| match (event, fought_creature) {
                    (
                        GameEvent::DamageDealt {
                            target: TargetRef::Object(id),
                            excess,
                            ..
                        },
                        Some(fought_creature),
                    ) if *id == fought_creature => {
                        Some(crate::game::arithmetic::u32_to_i32_saturating(*excess))
                    }
                    _ => None,
                })
                .sum()
        }
        _ => 0,
    };

    (excess > 0).then_some(excess)
}

fn previous_effect_counts_by_player_from_events(
    effect: &Effect,
    events: &[GameEvent],
) -> HashMap<PlayerId, i32> {
    let mut counts = HashMap::new();
    if matches!(effect, Effect::Discard { .. } | Effect::DiscardCard { .. }) {
        for event in events {
            if let GameEvent::Discarded { player_id, .. } = event {
                *counts.entry(*player_id).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn mark_exile_choice_tracks_by_source(state: &mut GameState, source: ObjectId) {
    if let WaitingFor::EffectZoneChoice {
        source_id,
        destination: Some(crate::types::zones::Zone::Exile),
        track_exiled_by_source,
        ..
    } = &mut state.waiting_for
    {
        if *source_id == source {
            *track_exiled_by_source = true;
        }
    }
}

/// Resolve an ability and follow its sub_ability chain using typed nested structs.
/// No SVar lookup, no parse_ability(). The depth is bounded by the data structure.
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    // Safety limit to prevent stack overflow on pathological data
    if depth > 20 {
        return Err(EffectError::ChainTooDeep);
    }

    // Clear stale revealed IDs at the top-level chain entry to prevent leaking
    // across unrelated ability resolutions.
    if depth == 0 {
        state.last_revealed_ids.clear();
        // CR 401.5 + CR 608.2c: Defense in depth — `apply_parent_chain_context`
        // already consumes this at the very next parent->child hand-off after a
        // Dig sets it, but a Dig with no sub_ability at all would otherwise leave
        // it dangling true until some unrelated LATER resolution's first hand-off
        // (e.g. Avenging Angel's LTB self-return). Reset at every fresh
        // resolution so that can never happen.
        state.last_dig_found_nothing = false;
        // CR 701.20e: A new top-level resolution ends any prior private "look at"
        // peek window — the looked-at card from an unrelated resolution must not
        // stay visible. Cleared here (depth 0 only) so a resumed optional-reveal
        // decision (which re-enters at depth 1) preserves the peek it depends on.
        state.private_look_ids.clear();
        state.private_look_player = None;
        state.last_zone_changed_ids.clear();
        // CR 608.2c + CR 701.38: Per-resolution ballot ledger; populated by
        // `vote::resolve_tally` and read by `PlayerFilter::VotedFor`. Clear
        // alongside `last_zone_changed_ids` so cross-resolution leakage is
        // impossible.
        state.last_vote_ballots = crate::im::Vector::new();
        state.last_effect_amount = None;
        // CR 120.10: resolution-local excess channel resets with its total twin.
        state.last_effect_excess_amount = None;
        // NOTE: `state.die_result_this_resolution` is intentionally NOT cleared
        // here. `roll_die::resolve` stamps it AFTER this depth-0 prelude runs
        // (the prelude runs once at chain top, before `RollDie` executes), so
        // the inline class still reads a live value. Clearing it here would wipe
        // the value carried onto a reflexive "When you do … the result"
        // sub-ability entry before that entry resolves (CR 603.12). Cross-
        // resolution isolation comes from the four `stack.rs` reset sites and
        // the `engine.rs` apply() clear. (CR 706.2 + CR 706.4 + CR 603.12)
        state.last_effect_counts_by_player.clear();
        state.exiled_from_hand_this_resolution = 0;
        // CR 603.12a: the repeated-optional-payment count is resolution-local.
        // Reset it per top-level resolution so a prior Hawkeye/Frillback tap
        // can't leak K into the next one. MUST be depth-0 only: the
        // repeated-optional-payment driver resolves its reflexive modal at
        // depth >= 1 so `modal_choice_for_player` reads the live K before this
        // prelude can wipe it (CR 700.2d clamp).
        state.optional_cost_payments_this_resolution = 0;
        // CR 608.2e: The clause-local equalization snapshot is resolution-
        // scoped. It is overwritten per `player_scope` link within a chain
        // (and survives the interactive `EffectZoneChoice` drain, which
        // resumes at depth 1), so clearing it only at depth-0 chain entry
        // disposes of any residue without disturbing an in-flight Balance.
        state.clause_minimum_snapshot = None;
        // CR 603.7: Chain-local tracked-set identity — resets per top-level
        // ability resolution so compound zone changes within one chain
        // coalesce into a single tracked set, while unrelated resolutions
        // stay isolated.
        state.chain_tracked_set_id = None;
        // CR 608.2c + CR 109.5: Player-action accumulator resets per
        // top-level chain so "each opponent who searched this way" only sees
        // players who acted in the current resolution.
        state.player_actions_this_way.clear();
    }

    // BeginGame abilities are handled by mulligan setup, not normal stack resolution.
    // CR 103.5b: Mulligan-time abilities (Serum Powder, No-Regrets Egret) likewise never
    // resolve through the stack — their runtime path lives in `mulligan.rs`.
    if matches!(ability.kind, AbilityKind::BeginGame) && !state.resolving_begin_game_abilities {
        return Ok(());
    }
    if matches!(ability.kind, AbilityKind::Mulligan) {
        return Ok(());
    }

    // CR 603.4: Bump the per-ability per-turn resolution counter at the start of
    // top-level resolution so that `AbilityCondition::NthResolutionThisTurn`
    // gates can see the current resolution included in the count. Sub-abilities
    // (depth > 0) share the parent's count — they belong to the same printed
    // ability instance. Synthesized/runtime-only abilities (prowess, firebending)
    // and activated abilities lack an `ability_index` stamp and skip this hook.
    if depth == 0 {
        if let Some(idx) = ability.ability_index {
            let count = state
                .ability_resolutions_this_turn
                .entry((ability.source_id, idx))
                .or_insert(0);
            *count += 1;
        }
    }

    // CR 608.2c + CR 107.1c: "Repeat this process" dispatch — the non-count
    // companion to `repeat_for`. Instead of a fixed iteration count, a
    // predicate decides per-iteration whether to re-follow the whole
    // resolution chain. The dispatch is ITERATIVE (not recursive): `depth`
    // never accumulates, the `depth > 20` guard is never approached, and the
    // `depth == 0` prelude above ran exactly once — a repeated process is one
    // resolution (CR 608.2c), so per-resolution accumulators and the
    // resolution counter must not re-fire per iteration.
    debug_assert!(
        !(ability.repeat_for.is_some() && ability.repeat_until.is_some()),
        "repeat_for (count) and repeat_until (predicate) are mutually exclusive"
    );
    match ability.repeat_until.clone() {
        None => resolve_chain_body(state, ability, events, depth),
        Some(RepeatContinuation::ControllerChoice) => {
            let initial_waiting_for = state.waiting_for.clone();
            resolve_chain_body(state, ability, events, depth)?;
            if state.waiting_for != initial_waiting_for {
                // Inner pause: stash so the drain re-sets the repeat prompt
                // after the iteration's player choice resolves.
                state.pending_repeat_until = Some(crate::types::game_state::PendingRepeatUntil {
                    ability: Box::new(ability.clone()),
                });
            } else {
                // CR 107.1c: after the iteration fully resolved, prompt the
                // controller to repeat the process or stop.
                state.waiting_for = WaitingFor::RepeatDecision {
                    player: ability.controller,
                    ability: Box::new(ability.clone()),
                };
            }
            Ok(())
        }
        Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand,
            stop_on_duplicate_exiled_names,
        }) => loop {
            let initial_waiting_for = state.waiting_for.clone();
            resolve_chain_body(state, ability, events, depth)?;
            if state.waiting_for != initial_waiting_for {
                state.pending_repeat_until = Some(crate::types::game_state::PendingRepeatUntil {
                    ability: Box::new(ability.clone()),
                });
                return Ok(());
            }
            if should_stop_repeat_until(
                state,
                ability,
                stop_on_put_to_hand,
                stop_on_duplicate_exiled_names,
            ) {
                return Ok(());
            }
        },
        // CR 608.2c: "[if <condition>,] repeat this process [once]" — re-follow
        // the whole chain while `condition` holds against the just-resolved
        // state, capped by `max_iterations` additional repeats. Iterates the
        // same `resolve_chain_body` as `UntilStopConditions`, stashing on an
        // inner pause so `drain_pending_repeat_until` resumes the loop.
        Some(RepeatContinuation::WhileCondition {
            condition,
            max_iterations,
        }) => {
            let mut remaining = max_iterations;
            loop {
                // CR 608.2c: each repeated process is a FRESH execution of the
                // instructions, so its "that card"/"those cards" tracked set must
                // not extend the prior iteration's. Reset the chain-local
                // tracked-set identity before every iteration so a producer→copy
                // chain (Sin: exile a card, then copy THAT card) binds only to the
                // current iteration's object — otherwise the set accumulates and
                // the copy multiplies (and the loop never terminates once the
                // accumulated set keeps the predicate true).
                state.chain_tracked_set_id = None;
                let initial_waiting_for = state.waiting_for.clone();
                resolve_chain_body(state, ability, events, depth)?;
                if state.waiting_for != initial_waiting_for {
                    // Inner pause: stash the loop ability with its remaining cap
                    // so the drain re-evaluates the condition after the choice.
                    let mut paused = ability.clone();
                    paused.repeat_until = Some(RepeatContinuation::WhileCondition {
                        condition: condition.clone(),
                        max_iterations: remaining,
                    });
                    state.pending_repeat_until =
                        Some(crate::types::game_state::PendingRepeatUntil {
                            ability: Box::new(paused),
                        });
                    return Ok(());
                }
                if !should_repeat_while_condition(state, ability, &condition, &mut remaining) {
                    return Ok(());
                }
            }
        }
    }
}

/// CR 608.2c: Loop-continuation predicate for `RepeatContinuation::WhileCondition`.
/// Returns `true` when another iteration must run: the game-state `condition`
/// holds against the just-resolved state AND the remaining additional-iteration
/// cap (if any) is not exhausted. When a cap is present it is decremented on each
/// `true` return so callers thread the running count by re-passing `remaining`.
fn should_repeat_while_condition(
    state: &GameState,
    ability: &ResolvedAbility,
    condition: &AbilityCondition,
    remaining: &mut Option<u32>,
) -> bool {
    if matches!(remaining, Some(0)) {
        return false;
    }
    if !evaluate_condition(condition, state, ability) {
        return false;
    }
    if let Some(n) = remaining.as_mut() {
        *n -= 1;
    }
    true
}

/// One full pass of an ability's resolution chain — the parent effect (with its
/// `repeat_for` count loop) and the entire `sub_ability` chain. This is one
/// "process" for the purposes of "repeat this process" (CR 608.2c). Extracted
/// from `resolve_ability_chain` so the `repeat_until` dispatch can drive it
/// iteratively. The `depth == 0` prelude (state-clearing, the resolution
/// counter, the BeginGame/Mulligan guards) runs once in `resolve_ability_chain`
/// and is intentionally NOT repeated per iteration.
fn resolve_chain_body(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<(), EffectError> {
    // CR 608.2c: Snapshot the `pending_continuation` *value* at chain-body
    // entry so the sub_ability guard further down can compare by IDENTITY, not
    // mere presence. An unrelated outer continuation (e.g. a `player_scope`
    // iteration's remaining-opponent queue) must NOT cause this chain's own
    // `sub_ability` link to be skipped — only a continuation that THIS effect's
    // resolver INSTALLED or REPLACED accounts for the sub. A bare `is_some()`
    // count cannot distinguish "outer present, effect installed nothing" from
    // "outer present, effect replaced it with its own" (e.g. `clash.rs` does a
    // REPLACE, not an append) — both leave `is_some()` true. Comparing the full
    // value catches the replace case correctly (issue #491).
    let pending_continuation_before = state.pending_continuation.clone();

    // CR 608.2c + CR 701.20b + CR 603.3d: A multi-target reveal-all producer whose
    // per-target referent (the revealed card) is consumed by later co-instructions
    // ("each lose … the card revealed by the other player", "each put the card they
    // revealed"). Instruction order (CR 608.2c) requires ALL reveals to complete
    // before ANY consumer runs — the reveal is therefore NOT interleaved with the
    // per-player consumer fan-out (unlike the whole-chain §5472 path below). Keys
    // on `effect_introduces_per_player_object_referent` (RevealTop only) +
    // `multi_target` + a >=2 Player-target guard, so it fires only for "target
    // players each reveal" cards (Parker Luck). `RevealTop.target_filter()` is now
    // `Some(Player)` (so the two players ARE selected), which the §5472 fan-out
    // below also keys on — the safety here is ORDERING, not filter disjointness:
    // this branch is placed BEFORE §5472 and returns early, so a multi_target
    // RevealTop never double-dispatches through the whole-chain §5472 path.
    if ability.multi_target.is_some()
        && effect_introduces_per_player_object_referent(&ability.effect)
        && ability
            .targets
            .iter()
            .filter(|t| matches!(t, TargetRef::Player(_)))
            .count()
            >= 2
    {
        // 1. Reveal-all: resolve ONLY the producer effect once with targets left
        //    wide. `reveal_top::resolve` reveals every targeted player's top card
        //    (§2.4a) and fills `state.last_revealed_ids` with the full set.
        resolve_effect(state, ability, events)?;

        // A bare "two target players each reveal" with no consumer is fully
        // resolved by the reveal-all above.
        let Some(sub_template) = ability.sub_ability.as_ref() else {
            return Ok(());
        };

        // 2. Fan out the sub-chain per chosen player in APNAP order (CR 101.4).
        let chosen_players: Vec<PlayerId> = ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
            .collect();
        let fanout_players: Vec<PlayerId> = crate::game::players::apnap_order(state)
            .into_iter()
            .filter(|pid| chosen_players.contains(pid))
            .collect();

        // Pre-build every player's narrowed sub-chain, binding each iteration's OWN
        // revealed card OWNER-KEYED (CR 108.3: a top-of-library reveal is in its
        // owner's library, so owner == revealer). `effect_context_object` drives
        // the "put the card they revealed" (`ChangeZone { ParentTarget }`, via the
        // inherited Object target) and is the by-exclusion anchor for the
        // OtherRevealedCard cross-loss. Snapshotting up front freezes each own card
        // before any lose/put mutates state (MV is zone-independent, CR 202.3).
        let mut narrowed_subs: Vec<ResolvedAbility> = Vec::with_capacity(fanout_players.len());
        for pid in &fanout_players {
            let own_id: Option<ObjectId> = state
                .last_revealed_ids
                .iter()
                .copied()
                .find(|id| state.objects.get(id).map(|obj| obj.owner) == Some(*pid));
            let own_snapshot = own_id.and_then(|id| {
                state.objects.get(&id).map(|obj| CostPaidObjectSnapshot {
                    object_id: id,
                    lki: obj.snapshot_for_mana_spent(),
                })
            });
            let mut sub = sub_template.as_ref().clone();
            // The directed life loss reads the `Player` target; the put reads the
            // inherited `Object` target. Absent an own card (empty library /
            // illegal target, CR 608.2b) only the `Player` target is present — the
            // put no-ops and the cross-loss fails closed to 0.
            let mut targets = vec![TargetRef::Player(*pid)];
            if let Some(id) = own_id {
                targets.push(TargetRef::Object(id));
            }
            sub.targets = targets;
            sub.multi_target = None;
            apply_parent_chain_context(&mut sub, ability, own_snapshot.as_ref(), state);
            narrowed_subs.push(sub);
        }

        // Resolve in APNAP order. On an interactive pause (defensive — the lose/put
        // pair does not itself pause), stash the untouched remainder as a
        // continuation, mirroring the §5472 pending-continuation stash pattern.
        let initial_waiting_for = state.waiting_for.clone();
        for (i, sub) in narrowed_subs.iter().enumerate() {
            resolve_ability_chain(state, sub, events, depth + 1)?;
            if state.waiting_for != initial_waiting_for {
                let mut tail: Option<Box<ResolvedAbility>> = None;
                for remaining in narrowed_subs[i + 1..].iter().rev() {
                    let mut remaining = remaining.clone();
                    if let Some(prev) = tail {
                        super::ability_utils::append_to_sub_chain(&mut remaining, *prev);
                    }
                    tail = Some(Box::new(remaining));
                }
                append_to_pending_continuation(state, tail);
                break;
            }
        }
        return Ok(());
    }

    // CR 603.3d + CR 601.2c: Multi-target-over-`Player` resolution fan-out.
    // An "any number of target players each <verb>" trigger/spell announces a
    // variable number of player targets when it goes on the stack (CR 601.2c,
    // reached for triggers via CR 603.3d). The chosen player set lands in
    // `ability.targets` as `TargetRef::Player` entries with `multi_target` set.
    // Single-player-recipient effect handlers (`Discard`, `Mill`, `LoseLife`)
    // resolve for only the FIRST `TargetRef::Player`, so without this branch a
    // selection of two players discards only one. This fan-out is the missing
    // iteration layer — a structural mirror of the `player_scope` loop above —
    // recursing once per chosen player with `targets` narrowed to that one
    // player and `multi_target` cleared.
    //
    // MUST run before the ConditionInstead "instead" swap below: swapping a
    // `Mill { target: Player }` parent for a `Mill { target: ParentTarget }`
    // rider (Court of Cunning — "each mill two … mills ten instead") clears
    // `multi_target` and changes the effect's target filter, which would
    // otherwise skip this branch and resolve only the first chosen player.
    if ability.multi_target.is_some()
        && effect_target_filter(&ability.effect) == Some(&TargetFilter::Player)
    {
        let chosen_players: Vec<PlayerId> = ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
            .collect();
        if chosen_players.len() != 1 {
            // CR 601.2c: "any number of target players" permits zero targets.
            // CR 608.2c: An ability resolving with zero chosen player targets
            // does nothing — emit `EffectResolved` and stop BEFORE
            // `resolve_effect`, whose `resolve_player_for_context_ref`
            // controller fallback would otherwise wrongly resolve for the
            // ability's controller. Exactly one chosen player falls through to
            // the unchanged single-recipient fast path; only zero or >=2 are
            // handled here.
            if chosen_players.is_empty() {
                events.push(GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::from(&ability.effect),
                    source_id: ability.source_id,
                });
                return Ok(());
            }
            // CR 101.4 + CR 608.2c: Resolve the effect once per chosen player in
            // APNAP order — each player's `<verb>` (and its `sub_ability`, e.g.
            // Tinybones' `Mill` + `LoseLife`) is one instruction applied to
            // multiple subjects, followed in turn order.
            let fanout_players: Vec<PlayerId> = crate::game::players::apnap_order(state)
                .into_iter()
                .filter(|pid| chosen_players.contains(pid))
                .collect();
            let initial_waiting_for = state.waiting_for.clone();
            for (i, pid) in fanout_players.iter().enumerate() {
                let mut narrowed = ability.clone();
                narrowed.targets = vec![TargetRef::Player(*pid)];
                narrowed.multi_target = None;
                resolve_ability_chain(state, &narrowed, events, depth + 1)?;

                // CR 608.2c: If an inner effect paused on a player choice (e.g.
                // `WaitingFor::DiscardChoice` when a targeted player must pick
                // which card to discard), stash the remaining players as a
                // continuation chain and break — they resume via
                // `drain_pending_continuation` after the choice resolves.
                if state.waiting_for != initial_waiting_for {
                    let remaining = &fanout_players[i + 1..];
                    let mut tail: Option<Box<ResolvedAbility>> = None;
                    for &remaining_pid in remaining.iter().rev() {
                        let mut remaining_narrowed = ability.clone();
                        remaining_narrowed.targets = vec![TargetRef::Player(remaining_pid)];
                        remaining_narrowed.multi_target = None;
                        if let Some(prev) = tail {
                            super::ability_utils::append_to_sub_chain(
                                &mut remaining_narrowed,
                                *prev,
                            );
                        }
                        tail = Some(Box::new(remaining_narrowed));
                    }
                    append_to_pending_continuation(state, tail);
                    break;
                }
            }
            return Ok(());
        }
    }

    // CR 608.2e: "Instead" kicker — check if a sub overrides the parent.
    // When condition is met, replace the current ability's effect with the sub's
    // effect, preserving the full resolution flow (tracked sets, continuations).
    let ability = if let Some(ref sub) = ability.sub_ability {
        // CR 608.2e: "Instead" kicker — swap parent effect with override sub's effect.
        let should_swap = if matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        ) {
            ability.context.additional_cost_paid
        } else if let Some(AbilityCondition::CastVariantPaidInstead { variant }) = sub.condition {
            // CR 608.2e + CR 702.49 + CR 702.190a: Read from GameObject, not SpellContext
            state
                .objects
                .get(&ability.source_id)
                .map(|obj| obj.cast_variant_paid == Some((variant, state.turn_number)))
                .unwrap_or(false)
        } else if let Some(AbilityCondition::TargetHasKeywordInstead { ref keyword }) =
            sub.condition
        {
            // CR 608.2e: Check if the first resolved object target has the keyword.
            ability
                .targets
                .iter()
                .find_map(|t| match t {
                    TargetRef::Object(id) => state.objects.get(id),
                    _ => None,
                })
                .is_some_and(|obj| obj.has_keyword(keyword))
        } else if let Some(AbilityCondition::ConditionInstead { ref inner }) = sub.condition {
            // CR 608.2c: General "instead" replacement — evaluate the wrapped condition.
            evaluate_condition(inner, state, ability)
        } else {
            false
        };
        if should_swap {
            // CR 608.2c + CR 608.2e: Single-authority swap helper preserves
            // every effect-shape field on the sub (player_scope, optional,
            // multi_target, repeat_for, …) and every runtime-context field on
            // the parent (controller, targets, chosen_x, …). See
            // `ability_utils::apply_instead_swap` for the full field map.
            // Issue #310: a hand-rolled clone here previously dropped
            // `sub.player_scope`.
            Cow::Owned(super::ability_utils::apply_instead_swap(ability, sub))
        } else {
            Cow::Borrowed(ability)
        }
    } else {
        Cow::Borrowed(ability)
    };
    let ability = ability.as_ref();

    // CR 608.2d (override) + CR 701.9b (analogous) + CR 109.4: A random
    // `Effect::Choose` ("choose a player at random") or `Effect::ChooseFromZone`
    // ("choose one of them at random") is resolved here, at the chain resolution
    // point where a mutable ability is available, so the game-selected value
    // lands on `chosen_players` / `targets` BEFORE the chain descends to a
    // dependent sub (Strax's reflexive Fight scoped to the chosen player; River
    // Song's Diary's `CastFromZone { target: ParentTarget }`). No interactive
    // `WaitingFor::NamedChoice` / `ChooseFromZoneChoice` is raised. This mirrors
    // the resolution-point handling of `TargetSelectionMode::Random` for targets.
    let random_choice_owned;
    let random_is_choose = matches!(
        &ability.effect,
        Effect::Choose { selection, .. }
            if matches!(selection, crate::types::ability::TargetSelectionMode::Random)
    );
    let random_is_choose_from_zone = matches!(
        &ability.effect,
        Effect::ChooseFromZone { selection, .. } if selection.is_random()
    );
    let (ability, random_choice_resolved) = if random_is_choose || random_is_choose_from_zone {
        let mut owned = ability.clone();
        if random_is_choose {
            choose::resolve_random_in_chain(state, &mut owned, events);
        } else {
            choose_from_zone::resolve_random_in_chain(state, &mut owned, events);
        }
        random_choice_owned = owned;
        (&random_choice_owned, true)
    } else {
        (ability, false)
    };

    if effect_depends_on_missing_chosen_player(ability) {
        state.cost_payment_failed_flag = true;
        if let Some(ref next) = ability.sub_ability {
            if next.sub_link == SubAbilityLink::SequentialSibling {
                let mut sibling = next.as_ref().clone();
                apply_parent_chain_context(&mut sibling, ability, None, state);
                resolve_ability_chain(state, &sibling, events, depth + 1)?;
            }
        }
        return Ok(());
    }

    if repeat_for_outermost_with_scope_or_unless(ability)
        && !has_member_driven_repeat_after_hydration(state, ability)
    {
        return drive_repeat_for_outermost(state, ability, events, depth);
    }

    // CR 608.2: player_scope iteration — when an ability has player_scope set,
    // execute the scoped instruction once per matching player. Runtime keeps
    // the scoped player as the acting `controller` for legacy effect handlers
    // while preserving `original_controller` so "you" quantities still read
    // the printed ability controller. The unscoped tail then resumes once
    // after the scoped loop, matching the printed instruction order.
    // EXCEPTION: `ChooseAndSacrificeRest` is a self-iterating effect — its
    // resolver walks the scoped player set itself (APNAP) and seeds the
    // `WaitingFor::CategoryChoice` continuation. It must receive `player_scope`
    // intact and must NOT be fanned out here, or every opponent's invocation
    // would re-sweep the whole table. See `choose_and_sacrifice_rest::resolve`.
    // EXCEPTION (same category): `EachPlayerCopyChosen` is likewise a
    // self-iterating APNAP effect — it seeds its own per-player continuation and
    // must receive `player_scope` intact rather than being fanned out here.
    let driver_scope = ability.player_scope.as_ref().filter(|_| {
        !matches!(
            ability.effect,
            Effect::ChooseAndSacrificeRest { .. } | Effect::EachPlayerCopyChosen { .. }
        )
    });
    if let Some(scope) = driver_scope {
        let scoped_events_before = events.len();
        let controller = ability.controller;
        // CR 101.4 + CR 800.4: Join Forces overrides the APNAP anchor with
        // "Starting with you"; otherwise this remains standard APNAP order.
        let apnap = crate::game::players::apnap_order_from(
            state,
            ability.starting_with.clone(),
            controller,
        );
        let matching_players: Vec<PlayerId> = match scope {
            PlayerFilter::AllExcept { .. } => {
                // CR 608.2c + CR 109.4 + CR 608.2h: the `AllExcept` anchor may be an
                // ability-target reference (ParentObjectTargetController), which the
                // generic `matches_player_scope` predicate cannot resolve (it carries
                // no `ResolvedAbility`). Route through the ability-aware
                // `speed_effects::players_for_filter`, then re-impose APNAP order by
                // intersecting against the apnap sequence.
                let set =
                    crate::game::effects::speed_effects::players_for_filter(state, scope, ability);
                apnap.into_iter().filter(|pid| set.contains(pid)).collect()
            }
            PlayerFilter::ChosenPlayer { index } => ability
                .chosen_players
                .get(*index as usize)
                .copied()
                .map(|chosen| apnap.into_iter().filter(|pid| *pid == chosen).collect())
                .unwrap_or_default(),
            _ => apnap
                .into_iter()
                .filter(|pid| {
                    matches_player_scope(state, *pid, scope, controller, ability.source_id)
                })
                .collect(),
        };
        let (scoped_template, after_scope) = split_player_scope_chain(ability, scope);
        let after_scope_needs_linked_exile = after_scope.as_ref().is_some_and(|tail| {
            crate::game::exile_links::ability_contains_linked_exile_consumer(tail)
        });

        let initial_waiting_for = state.waiting_for.clone();
        let mut paused = false;
        // CR 608.2e: each clause's equalization minimum is fixed when that
        // clause begins; the snapshot is per `player_scope` link, captured
        // before fan-out (the board is now exactly the clause's pre-clause
        // state) and cleared when the link completes — so clause N+1 re-enters
        // the driver with the field `None` and re-captures against the
        // post-clause-N board. See §8 of the Balance plan.
        capture_clause_minimum_snapshot(state, &scoped_template);
        for (i, pid) in matching_players.iter().enumerate() {
            let mut scoped = scoped_template.clone();
            // CR 608.2c + CR 101.3: Each scoped iteration is a fresh
            // sub-resolution of the scoped template — read the whole
            // instruction per iteration. The cost-payment-failed signal is
            // per-iteration; this is the missing fourth resumption boundary
            // alongside the three at engine_payment_choices.rs:30
            // (OptionalEffectChoice), :97 (OpponentMayChoice), and :661
            // (UnlessPay success). Without this, an earlier opponent's
            // mandatory failure leaks into a later opponent's
            // `IfCurrentScopeSucceeded` read for cards like Refurbished
            // Familiar and Aclazotz, Deepest Betrayal. Audit-2 verified
            // safety: no corpus card relies on cross-iteration carry-over
            // of this flag.
            state.cost_payment_failed_flag = false;
            scoped.set_original_controller_recursive(controller);
            // CR 608.2: The scoped player is the acting controller for the
            // WHOLE per-player chain, not just the top clause. A co-scoped
            // sub-clause kept in this iteration (Duskmantle Seer's "loses life
            // equal to that card's mana value, then puts it into their hand")
            // must resolve its implicit-controller recipient and any generic
            // handler against the iterating player — so rebind recursively. The
            // printed controller is preserved via `original_controller` above,
            // keeping "you" references stable (CR 109.5).
            scoped.set_controller_recursive(*pid);
            scoped.set_scoped_player_recursive(*pid);
            resolve_ability_chain(state, &scoped, events, depth + 1)?;

            // CR 608.2e: Break if inner effect entered a player-choice state —
            // remaining players resume after the choice resolves via continuation.
            if state.waiting_for != initial_waiting_for {
                if after_scope_needs_linked_exile {
                    mark_exile_choice_tracks_by_source(state, ability.source_id);
                }
                let remaining = &matching_players[i + 1..];
                let mut tail = after_scope.clone();
                // Build continuation chain for remaining players in APNAP order.
                // Each remaining player gets the scoped instruction only; the
                // unscoped tail runs once after the final scoped iteration.
                for &remaining_pid in remaining.iter().rev() {
                    let mut remaining_scoped = scoped_template.clone();
                    remaining_scoped.set_original_controller_recursive(controller);
                    // CR 608.2: mirror the in-loop recursive controller rebind so
                    // a co-scoped sub-clause resumed via continuation also acts as
                    // the iterating player.
                    remaining_scoped.set_controller_recursive(remaining_pid);
                    remaining_scoped.set_scoped_player_recursive(remaining_pid);
                    // CR 608.2c: each remaining player's clause is an INDEPENDENT
                    // following instruction, not a continuation of the prior
                    // player's. When the scoped template carries a conditional
                    // rider (e.g. Momentum Breaker's "each opponent who can't
                    // discards a card"), this clause gets appended after the
                    // first player's stashed rider; marking it `SequentialSibling`
                    // ensures it still resolves when that rider's condition is
                    // false (it ran for a player who DID perform the action), so
                    // the per-opponent fan-out is not truncated after the first.
                    remaining_scoped.sub_link = SubAbilityLink::SequentialSibling;
                    if let Some(prev) = tail {
                        super::ability_utils::append_to_sub_chain(&mut remaining_scoped, *prev);
                    }
                    tail = Some(Box::new(remaining_scoped));
                }
                // CR 608.2e: do NOT clear the clause snapshot here — the
                // remaining clause-N players resume via `pending_continuation`
                // as bare single-scoped effects (no `player_scope`, so they do
                // not re-enter this driver to re-capture) and must see the same
                // frozen extremum. The next `player_scope` link's
                // `capture_clause_minimum_snapshot` overwrites it; `apply()`
                // disposes of any residue once resolution ends.
                if tail.is_some() {
                    append_to_pending_continuation(state, tail);
                }
                paused = true;
                break;
            }
        }
        let scoped_events = &events[scoped_events_before..];
        let counts_by_player =
            previous_effect_counts_by_player_from_events(&scoped_template.effect, scoped_events);
        if !counts_by_player.is_empty() {
            state.last_effect_count = counts_by_player.values().copied().max();
            state.last_effect_amount = state.last_effect_count;
            state.last_effect_counts_by_player = counts_by_player;
        } else if let Some(amount) =
            previous_effect_amount_from_events(state, &scoped_template, scoped_events)
        {
            state.last_effect_amount = Some(amount);
            // CR 120.10: stamp the resolution-local excess channel alongside the
            // CR 120.6 total so a follow-up "if excess damage was dealt this way"
            // condition reads overkill-beyond-lethal.
            let excess =
                previous_effect_excess_amount_from_events(state, &scoped_template, scoped_events);
            state.last_effect_excess_amount = excess;
        }
        let affected_with_causes =
            if next_sub_needs_tracked_set(ability) || after_scope_needs_linked_exile {
                affected_objects_with_causes(
                    &scoped_template.effect,
                    scoped_events,
                    &scoped_template.targets,
                )
            } else {
                Vec::new()
            };
        let affected_ids: Vec<ObjectId> = affected_with_causes.iter().map(|(id, _)| *id).collect();
        if after_scope_needs_linked_exile {
            for id in &affected_ids {
                if state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == crate::types::zones::Zone::Exile)
                {
                    crate::game::exile_links::push_tracked_by_source(state, *id, ability.source_id);
                }
            }
        }
        // CR 608.2c: After a `player_scope: All` sacrifice clause completes,
        // publish the full scoped event slice so downstream "if you sacrificed
        // a permanent this way" / ZoneChangedThisWay gates see every player's
        // sacrifice — not only the last iteration's overwrite of
        // `last_zone_changed_ids`.
        let mut ids: Vec<ObjectId> = scoped_events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged { object_id, .. }
                | GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        ids.sort_unstable_by_key(|id| id.0);
        ids.dedup();
        state.last_zone_changed_ids = ids;
        if next_sub_needs_tracked_set(ability) {
            publish_tracked_set_with_causes(state, affected_with_causes);
        }
        if !paused {
            // CR 608.2e: this `player_scope` clause has completed. Clear its
            // frozen values before running any following instruction; if the
            // tail is another `player_scope` clause, that recursive entry will
            // capture its own fresh snapshot against the post-this-clause board.
            state.clause_minimum_snapshot = None;
            if let Some(after_scope) = after_scope {
                resolve_ability_chain(state, &after_scope, events, depth + 1)?;
            }
        }
        return Ok(());
    }

    // CR 608.2c + CR 608.2d + CR 109.4: Resolution-time target binding for a
    // `ControllerRef::ChosenPlayer`-scoped effect ("They put two +1/+1
    // counters on a creature they control"). The dependent effect's target
    // filter could not be resolved when the ability went on the stack —
    // `collect_target_slots` deliberately surfaced no slot for it because the
    // chosen player was unknown. Now, mid-resolution, the preceding
    // `Effect::Choose` has populated `chosen_players`, so the filter resolves
    // against the matching objects the chosen player controls:
    //   * 0 candidates  → the effect does nothing (CR 608.2c); fall through so
    //     the sub_ability chain (the next `Choose`) still resolves.
    //   * 1 candidate   → no choice exists; bind it directly.
    //   * 2+ candidates → CR 608.2d: the *chosen player* (not the ability's
    //     controller) chooses which object. Surface `ChooseFromZoneChoice`
    //     scoped to the chosen player and stash the dependent effect (with its
    //     `sub_ability` chain intact) as a continuation, mirroring the Bolster
    //     keyword-action pattern (`game/effects/bolster.rs`). The
    //     `ChooseFromZoneChoice` answer handler injects the picked object into
    //     `cont.chain.targets`; `drain_pending_continuation` then resumes the
    //     `PutCounter` and the trailing `Choose` clauses.
    if ability.targets.is_empty() && !ability.chosen_players.is_empty() {
        if let Some(filter) = ability.effect.target_filter() {
            if let Some(index) = crate::game::ability_utils::filter_chosen_player_index(filter) {
                if let Some(&chosen) = ability.chosen_players.get(index as usize) {
                    // Rewrite `ChosenPlayer → You` and enumerate against the
                    // chosen player so `find_legal_targets`' source-controller
                    // plumbing yields objects that player controls.
                    let enum_filter =
                        crate::game::ability_utils::rewrite_chosen_player_to_you(filter);
                    let mut legal = crate::game::targeting::find_legal_targets(
                        state,
                        &enum_filter,
                        chosen,
                        ability.source_id,
                    );
                    match legal.len() {
                        0 => {}
                        1 => {
                            let mut bound = ability.clone();
                            bound.targets = legal;
                            return resolve_ability_chain(state, &bound, events, depth);
                        }
                        _ => {
                            // CR 608.2d: the chosen player picks one object.
                            let candidates: Vec<ObjectId> = legal
                                .drain(..)
                                .filter_map(|t| match t {
                                    TargetRef::Object(id) => Some(id),
                                    TargetRef::Player(_) => None,
                                })
                                .collect();
                            if !candidates.is_empty() {
                                let mut cont = ability.clone();
                                cont.targets.clear();
                                state.pending_continuation =
                                    Some(PendingContinuation::new(Box::new(cont)));
                                state.waiting_for = WaitingFor::ChooseFromZoneChoice {
                                    player: chosen,
                                    cards: candidates,
                                    count: 1,
                                    up_to: false,
                                    constraint: None,
                                    source_id: ability.source_id,
                                };
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    // CR 608.2c: Evaluate top-level condition before emitting any optional or unless-pay
    // choice. This must run after player_scope binding so scoped abilities test
    // conditions relative to the scoped player.
    if let Some(ref condition) = ability.condition {
        if !evaluate_condition(condition, state, ability) {
            if let Some(ref else_branch) = ability.else_ability {
                let mut else_resolved = else_branch.as_ref().clone();
                if else_resolved.targets.is_empty() && !ability.targets.is_empty() {
                    else_resolved.targets = ability.targets.clone();
                }
                else_resolved.context = ability.context.clone();
                if try_begin_deferred_else_branch_target_selection(
                    state,
                    &mut else_resolved,
                    events,
                )? {
                    return Ok(());
                }
                resolve_ability_chain(state, &else_resolved, events, depth + 1)?;
            } else if let Some(ref sub) = ability.sub_ability {
                // CR 608.2c: A skipped `IfYouDo` head whose effect
                // did not happen must still hand off to a paired `Not(IfYouDo)`
                // continuation. The head's own condition gates only the head's
                // effect — the `sub_ability` is the next chain link with its
                // own condition. Springheart Nantuko's decline path resolves
                // here: `CopyTokenOf {IfYouDo}` is skipped (no pay → effect not
                // performed) and the chain descends to `Token(Insect)
                // {Not(IfYouDo)}`, which evaluates true and creates the Insect.
                // Restricted to performed-gate sub-conditions so a *dependent*
                // continuation (one whose own gate references the parent's
                // action, e.g. a `WhenYouDo` reflexive — Ezio's "When you do,
                // that player loses the game") is never run when its parent's
                // condition failed (that remains an early return).
                //
                // CR 608.2c: An UNCONDITIONAL `SequentialSibling` is the next
                // INDEPENDENT instruction "in the order written", not a
                // continuation of this node's action, so it resolves regardless
                // of this node's condition. The `is_none()` guard is what keeps
                // Ezio's gated reflexive blocked while letting a truly
                // independent sibling through. This covers per-opponent
                // `player_scope` continuations: when a scoped clause carries a
                // conditional rider (Momentum Breaker's "each opponent who can't
                // discards a card"), the remaining opponents' unconditional
                // sacrifice clauses are appended after the first opponent's
                // stashed rider as `SequentialSibling`s, and must still resolve
                // when that rider's condition is false. Mirrors the gated-sub
                // sibling escape hatch (the `next.sub_link == SequentialSibling`
                // branch below).
                if sub
                    .condition
                    .as_ref()
                    .is_some_and(condition_depends_on_effect_performed)
                    || (sub.sub_link == SubAbilityLink::SequentialSibling
                        && sub.condition.is_none())
                {
                    let mut sub_resolved = sub.as_ref().clone();
                    if sub_resolved.targets.is_empty() && !ability.targets.is_empty() {
                        sub_resolved.targets = ability.targets.clone();
                    }
                    sub_resolved.context = ability.context.clone();
                    resolve_ability_chain(state, &sub_resolved, events, depth + 1)?;
                }
            }
            return Ok(());
        }
        // CR 603.12: A `WhenYouDo` / `QuantityCheck` ability resumed from
        // `pending_continuation` (e.g. Inti's attack trigger after an
        // interactive `DiscardChoice`) carries its gate on `ability.condition`
        // itself, not as a parent's `sub_ability`. Mirror the reflexive target
        // selection path used for inline sub-chains.
        if matches!(
            condition,
            AbilityCondition::WhenYouDo | AbilityCondition::QuantityCheck { .. }
        ) && try_begin_reflexive_target_selection(state, ability, None, None, events, depth)?
        {
            return Ok(());
        }
    }

    // CR 608.2d + CR 101.4: "Any opponent may" / "Any player may" — prompt the
    // eligible players in APNAP order. The scope decides whether the controller
    // is included: AnyOpponent excludes them (unchanged); AnyPlayer (group
    // bargain / punisher) includes them in their correct APNAP slot.
    if ability.optional {
        if let Some(scope) = ability.optional_for {
            // Exhaustive match: there is no compiler exhaustiveness guard at the
            // other OpponentMayScope consumers, so this serves as the manual
            // guard. Adding a variant forces a decision here.
            let include_controller = match scope {
                OpponentMayScope::AnyOpponent => false,
                OpponentMayScope::AnyPlayer => true,
            };
            let description = ability.description.clone();
            // apnap_order returns ALL living players active-player-first
            // (CR 101.4), so the controller lands in its correct slot.
            let mut opponent_order: Vec<PlayerId> = crate::game::players::apnap_order(state)
                .into_iter()
                .filter(|p| include_controller || *p != ability.controller)
                .collect();
            if let Some(first) = opponent_order.first().copied() {
                let remaining = opponent_order.split_off(1);
                state.pending_optional_effect =
                    Some(Box::new(ability_with_event_context_targets(state, ability)));
                state.waiting_for = WaitingFor::OpponentMayChoice {
                    player: first,
                    source_id: ability.source_id,
                    description,
                    remaining,
                };
            }
            return Ok(());
        }
    }

    // CR 117.3a + CR 609.3: "You may" effects prompt the acting player before
    // execution. For subject-anchored optional effects ("its controller may
    // search their library" — Assassin's Trophy), the acting player is the
    // resolved subject (the target permanent's controller), NOT the caster.
    //
    // CR 608.2c + CR 608.2d: EXCEPTION — a `DistinctCounterKindsAmong` loop
    // (Bribe Taker) makes its "you may" apply PER ITERATED KIND, not once up
    // front. The card's ruling confirms the controller may decline one kind and
    // accept another. Suppress the single up-front gate here; the `repeat_for`
    // loop below fires its own per-iteration `OptionalEffectChoice` for each
    // counter kind (see the `kind_driven` optional path in the loop).
    // CR 603.12a: a repeated-optional-payment process (Hawkeye) offers its "you
    // may" PER iteration via `drive_repeated_optional_payment`, not once up
    // front — suppress the single gate here exactly as the kind/member-driven
    // loops do.
    if ability.optional
        && !has_kind_driven_repeat(ability)
        && !has_member_driven_repeat_after_hydration(state, ability)
        && !is_repeated_optional_payment(ability)
        && !optional_effect_is_infeasible(state, ability)
    {
        let description = ability.description.clone();
        let prompt_player = optional_prompt_player(state, ability);
        let may_trigger_key = ability
            .may_trigger_origin
            .map(|origin| MayTriggerAutoChoiceKey {
                player: prompt_player,
                source_id: ability.source_id,
                origin,
            });
        if let Some(key) = may_trigger_key {
            if let Some(choice) = state.may_trigger_auto_choice(&key) {
                resolve_optional_effect_decision(
                    state,
                    ability.clone(),
                    choice,
                    events,
                    depth + 1,
                )?;
                return Ok(());
            }
        }
        state.pending_optional_effect =
            Some(Box::new(ability_with_event_context_targets(state, ability)));
        // CR 608.2: capture the triggering event in lockstep with the stashed
        // ability while `current_trigger_event` is still live (we are inside
        // `execute_effect`). Restored when the optional decision resumes so an
        // optional ("may") trigger's effect resolves `TriggeringPlayer` and
        // other event-context refs exactly as a non-optional trigger would.
        state.pending_optional_trigger_event = state.current_trigger_event.clone();
        // CR 603.2c + CR 608.2: mirror the batched-trigger subject count so a
        // "you may" sub-ability of a batched trigger (Ur-Dragon's optional
        // permanent-from-hand sub-effect) resumes with the same
        // `EventContextAmount` the pre-pause resolution observed.
        state.pending_optional_trigger_match_count = state.current_trigger_match_count;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: prompt_player,
            source_id: ability.source_id,
            description,
            may_trigger_key,
        };
        return Ok(());
    }

    // CR 603.12a: a "you may pay {cost} up to N times. When you do, [reflexive]"
    // process drives its own per-iteration payment loop + once-after-loop
    // reflexive (Hawkeye, Master Marksman). The up-front gate is suppressed for
    // this class above; resolve it through the dedicated driver here, before the
    // generic `repeat_for` loop (which would re-resolve the reflexive per
    // payment).
    if is_repeated_optional_payment(ability) {
        return drive_repeated_optional_payment(state, ability);
    }

    // CR 118.12 + CR 118.12a: "Effect unless [player] pays {cost}" —
    // intercepted here for both tax triggers and counter-target-spell unless
    // costs. Post-fold, the cost is the unified `AbilityCost` taxonomy.
    if let Some(ref unless_pay) = ability.unless_pay {
        // CR 603.2 + CR 118.12a: Hydrate event-context targets before payer
        // resolution so trigger unless-costs ("that player ... unless they pay")
        // do not silently fall through when `ability.targets` is still empty
        // (issue #2361).
        let ability_for_unless = hydrate_event_context_targets(state, ability);
        // CR 118.12a: `resolve_unless_payers` yields the APNAP-ordered poll
        // list — one player for ordinary unless-costs, every player for
        // "unless any player pays ...". The first entry is prompted now; the
        // rest ride along in `WaitingFor::UnlessPayment.remaining`.
        let unless_payers =
            resolve_unless_payers(state, ability_for_unless.as_ref(), &unless_pay.payer);
        if let Some((&payer, remaining_payers)) = unless_payers.split_first() {
            // CR 118.4 + CR 107.3c: Resolve a dynamic-generic mana cost into a
            // fixed `Mana { cost }` BEFORE entering the prompt — the runtime
            // payment site only handles static AbilityCost variants.
            let resolved_cost = match &unless_pay.cost {
                AbilityCost::PerCounter {
                    counter,
                    target,
                    base,
                } => {
                    // CR 702.24a + CR 702.24b: Count counters on `target` at
                    // resolution time so multi-instance reads the post-tick
                    // total.
                    let n = match target {
                        TargetFilter::SelfRef => state
                            .objects
                            .get(&ability.source_id)
                            .map(|obj| obj.counters.get(counter).copied().unwrap_or(0))
                            .unwrap_or(0),
                        other => {
                            // CR 702.24a: No current mechanic constructs a
                            // non-SelfRef `PerCounter`. If one ever does,
                            // returning 0 routes through the CR 118.5 zero-
                            // cost short-circuit (the unless-effect proceeds
                            // without prompting), which is the least-
                            // surprising default until the target-resolution
                            // branch is implemented.
                            // CR 113.6b: TargetFilter resolution against game
                            // state belongs in `game/filter.rs`; wire it here
                            // when the second mechanic lands.
                            tracing::warn!(
                                "PerCounter resolution against non-SelfRef target {:?} \
                                 not yet implemented; defaulting to n=0",
                                other
                            );
                            0
                        }
                    };
                    expand_per_counter(base, n)
                }
                AbilityCost::ManaDynamic { quantity } => {
                    // CR 107.3a: thread ResolvedAbility.chosen_x so the announced
                    // X drives the unless-cost. The plain resolve_quantity path
                    // passes chosen_x=None, making a bare {X} unless-cost always
                    // resolve to 0 and wrongly short-circuit via CR 118.5.
                    let amount = crate::game::quantity::resolve_quantity_with_targets(
                        state, quantity, ability,
                    );
                    AbilityCost::Mana {
                        cost: ManaCost::generic(amount.max(0) as u32),
                    }
                }
                // CR 118.12 + CR 202.1: "unless you pay its mana cost" — materialize
                // the ability source's OWN printed mana cost at resolution time. The
                // cost is dynamic because the granting Aura can be attached to any
                // permanent (Pendrell Flux, Disruption Aura). An absent source or a
                // costless source (land, token, other permanent with no mana cost)
                // resolves to `ManaCost::NoCost`, which CR 118.6 / CR 202.1b define
                // as an UNPAYABLE cost; the dedicated unpayable branch below handles
                // it (kept distinct from the `{0}` "always payable" short-circuit).
                AbilityCost::Mana {
                    cost: ManaCost::SelfManaCost,
                } => {
                    let cost = state
                        .objects
                        .get(&ability.source_id)
                        .map(|obj| obj.mana_cost.clone())
                        .unwrap_or(ManaCost::NoCost);
                    AbilityCost::Mana { cost }
                }
                other => other.clone(),
            };
            // CR 118.5 + CR 118.12a: Zero-mana unless cost short-circuit.
            // Pre-fold (2026-05-09 audit) the counter and tax/trigger paths
            // had divergent behavior here:
            //   - The counter-specific resolver treated `{0}` as "the
            //     spell-controller paid; the spell survives" (per CR 118.5,
            //     "players can always pay 0"). This matches the player's
            //     real-world choice to always pay 0.
            //   - The generic tax-trigger path fell through and executed the
            //     effect anyway (no opt-out offered).
            // The fold preserves both behaviors verbatim to keep this batch
            // strictly architectural; harmonizing them is tracked separately.
            // CR 118.6 + CR 202.1b: A cost based on the mana cost of an object
            // that has no mana cost (lands, tokens, other costless permanents)
            // is UNPAYABLE — attempting to pay it is an illegal action. No
            // payment is offered and the unless-effect always resolves; even a
            // Counter is not prevented (its controller cannot pay). This is the
            // inverse of the `{0}` "players can always pay 0" branch below, and
            // the two must stay distinct: `ManaCost::NoCost != ManaCost::zero()`.
            if matches!(&resolved_cost, AbilityCost::Mana { cost } if *cost == ManaCost::NoCost) {
                // Unpayable: fall through to execute the effect unconditionally.
            } else if matches!(&resolved_cost, AbilityCost::Mana { cost } if *cost == ManaCost::zero())
            {
                if matches!(ability.effect, Effect::Counter { .. }) {
                    // Counter is prevented — spell survives.
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::Counter,
                        source_id: ability.source_id,
                    });
                    return Ok(());
                }
                // Non-counter unless-modified effects: pre-fold behavior was
                // to fall through and execute the effect.
            } else {
                let mut pending = ability.clone();
                pending.unless_pay = None;
                // CR 118.12a: A disjunctive unless-cost (`OneOf`) surfaces a
                // sub-cost choice first; the chosen single cost re-enters
                // `WaitingFor::UnlessPayment` via
                // `handle_unless_payment_choose_cost`. This keeps the
                // single-cost branch in `handle_unless_payment` unchanged.
                state.waiting_for = match resolved_cost {
                    AbilityCost::OneOf { costs } => WaitingFor::UnlessPaymentChooseCost {
                        player: payer,
                        costs,
                        pending_effect: Box::new(pending),
                        trigger_event: state.current_trigger_event.clone(),
                        effect_description: ability.description.clone(),
                        remaining_choices: vec![],
                        chosen: vec![],
                    },
                    // CR 702.24a + CR 118.12: A `Composite` of `OneOf`s is
                    // what `expand_per_counter` produces from a `OneOf` base
                    // cost at N ≥ 2 (e.g., Jötun Owl Keeper's "{W} or {U}"
                    // cumulative-upkeep cost with 2 age counters → 2
                    // independent disjunctive choices). Drive each choice
                    // sequentially via `UnlessPaymentChooseCost`, accumulate
                    // picks, and re-enter `UnlessPayment` with the resulting
                    // `Composite` of chosen sub-costs as a single payment.
                    // "Each choice is made separately for each age counter,
                    // then either the entire set of costs is paid, or none
                    // of them is paid."
                    AbilityCost::Composite { costs }
                        if !costs.is_empty()
                            && costs.iter().all(|c| matches!(c, AbilityCost::OneOf { .. })) =>
                    {
                        let mut queue: Vec<Vec<AbilityCost>> = costs
                            .into_iter()
                            .map(|c| match c {
                                AbilityCost::OneOf { costs } => costs,
                                _ => unreachable!("matched all-OneOf guard above"),
                            })
                            .collect();
                        let first = queue.remove(0);
                        WaitingFor::UnlessPaymentChooseCost {
                            player: payer,
                            costs: first,
                            pending_effect: Box::new(pending),
                            trigger_event: state.current_trigger_event.clone(),
                            effect_description: ability.description.clone(),
                            remaining_choices: queue,
                            chosen: vec![],
                        }
                    }
                    cost => WaitingFor::UnlessPayment {
                        player: payer,
                        cost,
                        pending_effect: Box::new(pending),
                        trigger_event: state.current_trigger_event.clone(),
                        effect_description: ability.description.clone(),
                        remaining: remaining_payers.to_vec(),
                    },
                };
                return Ok(());
            }
        }
    }

    // CR 603.7: Snapshot event count so we can detect objects moved by this effect.
    let events_before = events.len();

    // Skip no-op unimplemented/runtime-handled effects, and a random
    // `Effect::Choose` already resolved above by `resolve_random_in_chain`.
    if !random_choice_resolved
        && !matches!(
            ability.effect,
            Effect::Unimplemented { .. } | Effect::RuntimeHandled { .. }
        )
    {
        let hydrated = hydrate_event_context_targets(state, ability);
        let effective = hydrated.as_ref();

        // CR 608.2b: Validate SharesQuality group constraints before applying effects.
        // If targets don't share the required quality, skip the effect.
        let shares_quality_failed = if effective.targets.len() >= 2 {
            if let Some(target_filter) = effect_target_filter(&effective.effect) {
                let constraints = extract_shares_quality_props(target_filter);
                constraints.iter().any(|(quality, relation)| {
                    let shares =
                        filter::validate_shares_quality(state, &effective.targets, quality);
                    match relation {
                        SharedQualityRelation::Shares => !shares,
                        SharedQualityRelation::DoesNotShare => shares,
                    }
                })
            } else {
                false
            }
        } else {
            false
        };

        if shares_quality_failed {
            // Group constraint not met — emit EffectResolved but skip execution.
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
        } else {
            // CR 603.7 + CR 608.2c + CR 109.5: Per-iteration parent-target
            // rebinding. When the body references the iterated object via a
            // context ref (`ParentTarget` / `ParentTargetController`), each
            // iteration must bind to a distinct member so the per-iteration
            // subject is the i-th object — not always `effective.targets[0]`.
            //
            // Two member sources:
            //  * `TrackedSetSize` — a set populated by a prior chain effect
            //    (Winds of Abandon: each exiled creature's controller searches
            //    their own library).
            //  * `ObjectCount { filter }` — "For each [object], <verb> that
            //    object" with no prior set (Second Harvest copies each token you
            //    control; Cleansing destroys each land; Cut the Tethers bounces
            //    each token). Snapshot the matching objects at loop start (CR
            //    608.2) so objects created/affected mid-loop don't enter the
            //    iteration — e.g., the copies Second Harvest creates are not
            //    themselves copied.
            //
            // The `ObjectCount` member set is resolved against `effective` (the
            // event-context-resolved ability) and DRIVES the iteration count, so
            // count and bound objects come from one snapshot and cannot diverge —
            // including the empty case (0 members ⇒ 0 iterations). The
            // `TrackedSetSize` path keeps the existing quantity-driven count.
            let mut member_driven = false;
            let iter_tracked_members: Vec<crate::types::identifiers::ObjectId> =
                match &ability.repeat_for {
                    Some(QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize,
                    }) if effect_refs_parent_target(&effective.effect) => state
                        .chain_tracked_set_id
                        .and_then(|id| state.tracked_object_sets.get(&id).cloned())
                        .unwrap_or_default(),
                    Some(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    }) if effect_iterates_over_parent_target(&effective.effect) => {
                        member_driven = true;
                        // Same resolver as `QuantityRef::ObjectCount`'s count, on
                        // the same `effective` ability, so members and count match
                        // (including `OtherThanTriggerObject` handling).
                        let ctx = filter::FilterContext::from_ability(effective);
                        crate::game::quantity::object_count_matching_ids(
                            state,
                            filter,
                            &ctx,
                            effective.source_id,
                        )
                    }
                    _ => Vec::new(),
                };

            // CR 122.1 + CR 608.2c: A `repeat_for: DistinctCounterKindsAmong`
            // loop iterates once per distinct counter kind on filter-matched
            // permanents. Unlike `ObjectCount`, the branches reference `SelfRef`
            // (not `ParentTarget`), so `effect_iterates_over_parent_target` is
            // false and a separate snapshot arm is required. The kinds are
            // resolved here (sorted deterministically by `as_str`) so count and
            // per-iteration binding share one snapshot; each iteration's tagged
            // `ChooseOneOf` branch is rebound to `iterated_counter_kinds[i]`.
            let mut kind_driven = false;
            let iterated_counter_kinds: Vec<crate::types::counter::CounterType> =
                match &ability.repeat_for {
                    Some(QuantityExpr::Ref {
                        qty: QuantityRef::DistinctCounterKindsAmong { filter },
                    }) => {
                        kind_driven = true;
                        let ctx = filter::FilterContext::from_ability(effective);
                        crate::game::quantity::distinct_counter_kinds_among(state, filter, &ctx)
                    }
                    _ => Vec::new(),
                };

            // CR 609.3 + CR 608.2: Execute the effect N times when repeat_for is
            // set. A `member_driven` ObjectCount loop takes its count from the
            // snapshotted members (resolved against `effective`), keeping count and
            // bindings in lockstep even when the set is empty.
            // CR 107.3a: Variable("X") must resolve via resolve_quantity_with_targets
            // so that ability.chosen_x (the paid X value) is passed through. The
            // plain resolve_quantity path passes chosen_x=None, causing X to always
            // resolve to 0 and the loop to never execute (Torment of Hailfire bug).
            let base_iterations = if member_driven {
                iter_tracked_members.len()
            } else if kind_driven {
                // CR 122.1: count and per-iteration kind binding come from one
                // snapshot, so an empty controlled-counter set ⇒ 0 iterations ⇒
                // no prompt.
                iterated_counter_kinds.len()
            } else if let Some(ref qty) = ability.repeat_for {
                crate::game::quantity::resolve_quantity_with_targets(state, qty, ability).max(0)
                    as usize
            } else {
                1
            };

            // CR 707.10 + CR 614.1a: "copy an additional time" replacement
            // effects (Twinning Staff) increase how many copies a copy-a-spell
            // effect produces. Applied once here at the copy-count site because
            // copies are created through this `repeat_for` loop, not the
            // `ProposedEvent` replacement pipeline. The adjusted count flows into
            // `total_iterations` and the resume stash below, so each additional
            // copy runs the same per-copy retarget step as the base copies.
            //
            // `copy_count_status` guards against re-application: each per-copy
            // retarget pause re-stashes a single-iteration resume ability that the
            // drain driver feeds back through this code. Without the guard, every
            // resumed iteration would re-add the bonus and the loop would explode
            // into runaway copies (CR 614.5 — a replacement effect doesn't invoke
            // itself repeatedly; it gets only one opportunity to affect an event,
            // so the bonus applies to the copy event once, not per individual copy).
            let iterations = if matches!(ability.effect, Effect::CopySpell { .. })
                && ability.copy_count_status.is_pending()
            {
                copy_spell::copy_count_with_replacements(state, ability, base_iterations)
            } else {
                base_iterations
            };
            let replacement_added_copy_start = if matches!(ability.effect, Effect::CopySpell { .. })
                && iterations > base_iterations
            {
                Some(base_iterations)
            } else {
                None
            };

            let initial_waiting_for = state.waiting_for.clone();
            let mut iteration = 0usize;
            let repeated_full_chain =
                ability.repeat_for.is_some() && effective.sub_ability.is_some();
            while iteration < iterations {
                // Snapshot per-iteration ability with parent-target rebinding when
                // applicable. CR 109.5: the rebind is SINGLE-slot — every reachable
                // member-driven card has exactly ONE parent-ref object slot. Second
                // Harvest's copy source, and Asinine Antics' `attach_to: ParentTarget`
                // (whose `owner: Controller` is a player ref, not an object slot, and
                // whose `effective.targets` is empty) each push exactly one
                // `TargetRef::Object(member)`. The `Effect::Attach`-under-for-each
                // case (two sequential object slots) has zero reachable card
                // consumers and is deferred — see `effect_parent_ref_slots`.
                let mut iter_ability;
                let member = iter_tracked_members.get(iteration).copied();
                let is_replacement_added_copy =
                    replacement_added_copy_start.is_some_and(|start| iteration >= start);
                let iter_effective: &ResolvedAbility =
                    if member.is_some() || is_replacement_added_copy || kind_driven {
                        iter_ability = effective.clone();
                        if let Some(member) = member {
                            rebind_first_object_target(&mut iter_ability.targets, member);
                        }
                        // CR 122.1 + CR 608.2c: rebind this iteration's dynamic
                        // ChooseOneOf branch to the current counter kind.
                        if kind_driven {
                            rebind_iterated_counter_kind(
                                &mut iter_ability,
                                iterated_counter_kinds[iteration].clone(),
                            );
                        }
                        // CR 608.2c + CR 608.2d: per-iteration optional actions
                        // (Bribe Taker per counter kind; Doubling Chant per
                        // creature search) route through `resolve_ability_chain`
                        // so each iteration fires its own `OptionalEffectChoice`.
                        // Clear `repeat_for` on the clone so the inner chain does
                        // not re-enter this outer loop.
                        if kind_driven || (member_driven && iter_ability.optional) {
                            iter_ability.repeat_for = None;
                        }
                        if let (true, Effect::CopySpell { retarget, .. }) =
                            (is_replacement_added_copy, &mut iter_ability.effect)
                        {
                            *retarget = CopyRetargetPermission::MayChooseNewTargets;
                        }
                        &iter_ability
                    } else {
                        effective
                    };
                // CR 608.2d: A kind-driven or member-driven iteration whose action
                // is optional fires its per-iteration "you may" gate through the
                // full chain. All other iterations resolve the effect directly —
                // `resolve_effect` does not check `optional`, which is correct
                // because non-per-iteration loops apply their `optional` once up
                // front in `resolve_chain_body`.
                if repeated_full_chain {
                    let mut full_chain_iteration = iter_effective.clone();
                    full_chain_iteration.repeat_for = None;
                    full_chain_iteration.copy_count_status =
                        crate::types::ability::CopyCountStatus::Finalized;
                    resolve_ability_chain(state, &full_chain_iteration, events, depth.max(1))?;
                } else if (kind_driven || member_driven) && iter_effective.optional {
                    // CR 608.2c: pass a non-zero depth so the depth==0 prelude
                    // (chain-local state clearing, resolution counter) does not
                    // re-run mid-loop — this iteration continues the current
                    // resolution, mirroring the drain-path resume at depth 1.
                    let _ = resolve_ability_chain(state, iter_effective, events, depth.max(1));
                } else {
                    let _ = resolve_effect(state, iter_effective, events);
                }
                // CR 609.3 + CR 109.5: When the inner effect enters an
                // interactive WaitingFor (e.g. SearchChoice), stash the
                // remaining iterations so `drain_pending_continuation` can
                // resume the loop after the player choice (and its chained
                // sub-ability) complete. Without this, only the first
                // iteration would ever fire — the loop would break and the
                // remaining iterations would be silently dropped.
                if state.waiting_for != initial_waiting_for {
                    let next_iteration = iteration + 1;
                    if next_iteration < iterations {
                        // CR 609.3 + CR 109.5: Each resumed iteration must run
                        // through the FULL chain (parent effect + sub_ability)
                        // exactly the way iteration 0 just did. The drain path
                        // re-enters via `resolve_ability_chain`, which goes
                        // through the line-1461 sub_ability wiring and the
                        // line-1660 SearchChoice continuation stash. Without
                        // preserving `sub_ability` here, opponents picked
                        // during iterations 1+ would never have their chosen
                        // card placed onto the battlefield (Winds of Abandon).
                        //
                        // Clear `repeat_for` on the resumed copy so
                        // `resolve_ability_chain` treats each resumed call as
                        // a single iteration rather than re-entering this
                        // outer iteration loop (which would re-iterate from
                        // zero against `total_iterations`). The drain driver
                        // owns iteration accounting via `next_iteration`.
                        let mut resume_ability = effective.clone();
                        resume_ability.repeat_for = None;
                        // CR 614.5: the copy-count replacement bonus is already
                        // folded into `total_iterations`; mark the resume so the
                        // CopySpell count hook does not re-add it per resumed copy
                        // (a replacement effect gets only one opportunity to affect
                        // an event, so it must not re-fire on each resumed copy).
                        resume_ability.copy_count_status =
                            crate::types::ability::CopyCountStatus::Finalized;
                        state.pending_repeat_iteration =
                            Some(crate::types::game_state::PendingRepeatIteration {
                                ability: Box::new(resume_ability),
                                tracked_members: iter_tracked_members.clone(),
                                // CR 122.1 + CR 608.2c: carry the per-iteration
                                // counter kinds so each resumed iteration rebinds
                                // its dynamic branch (empty for non-kind loops).
                                iterated_counter_kinds: iterated_counter_kinds.clone(),
                                next_iteration,
                                total_iterations: iterations,
                            });
                    }
                    break;
                }
                iteration += 1;
            }
            if repeated_full_chain {
                return Ok(());
            }
        } // end shares_quality_failed else
    }

    // CR 609.3: Extract the numeric result emitted by this parent effect for
    // `QuantityRef::PreviousEffectAmount` / `EventContextAmount` in sub-abilities.
    // The event class is selected by the parent `Effect` so unrelated numeric
    // side effects from the same resolution are not mixed together: damage to a
    // battle removes defense counters and also deals damage, but "damage dealt
    // this way" must read only `DamageDealt`; Coalition Relic's "counter removed
    // this way" must read only `CounterRemoved`. Mirror the `player_scope` loop's
    // discard accounting so controller-only "discard your hand, then draw that
    // many" chains (Tolarian Winds) stamp `last_effect_count`.
    let parent_events = &events[events_before..];
    let counts_by_player =
        previous_effect_counts_by_player_from_events(&ability.effect, parent_events);
    if !counts_by_player.is_empty() {
        state.last_effect_count = counts_by_player.values().copied().max();
        state.last_effect_amount = state.last_effect_count;
        state.last_effect_counts_by_player = counts_by_player;
    } else if let Some(amount) = previous_effect_amount_from_events(state, ability, parent_events) {
        state.last_effect_amount = Some(amount);
        // CR 120.10: stamp the resolution-local excess channel alongside the
        // CR 120.6 total so a follow-up "if excess damage was dealt this way"
        // condition reads overkill-beyond-lethal.
        let excess = previous_effect_excess_amount_from_events(state, ability, parent_events);
        state.last_effect_excess_amount = excess;
    }

    // CR 608.2c: Populate last_zone_changed_ids for ZoneChangedThisWay condition evaluation.
    // Scans ZoneChanged events emitted by this effect, mirroring the forward_result pattern.
    state.last_zone_changed_ids = events[events_before..]
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    // CR 608.2c + CR 109.5: Accumulate player actions across the chain for
    // `PlayerFilter::PerformedActionThisWay`. This is distinct from
    // `last_zone_changed_ids`: "searched this way" keys off the player action
    // even when the search finds no card.
    for event in &events[events_before..] {
        if let GameEvent::PlayerPerformedAction { player_id, action } = event {
            state.player_actions_this_way.insert((*player_id, *action));
            state.player_actions_this_turn.push((*player_id, *action));
        }
    }

    // CR 603.7: Record the objects affected by this effect as a tracked set so
    // downstream sub-abilities can resolve "this way" references (pronouns,
    // `TrackedSetSize`, `TrackedSet` filters). The signal event depends on the
    // effect class:
    //   - ChangeZone / ChangeZoneAll / ExileTop → `ZoneChanged` filtered by
    //     destination zone (CR 903.9a: excludes commanders redirected to the
    //     command zone).
    //   - Destroy / DestroyAll → `CreatureDestroyed` (CR 701.8a). Emitted only
    //     when destruction actually completes — regeneration shields (CR 701.8c)
    //     and indestructible (CR 702.12b) creatures never produce the event,
    //     so the set correctly contains only creatures *destroyed this way*.
    //   - Counter-adding effects → `CounterAdded` (CR 122.1), so "those
    //     creatures" after a mass counter instruction means the permanents that
    //     actually received counters.
    if next_sub_needs_tracked_set(ability) {
        let affected_with_causes = affected_objects_with_causes(
            &ability.effect,
            &events[events_before..],
            &ability.targets,
        );
        publish_tracked_set_with_causes(state, affected_with_causes);
    }

    // ExileFromTopUntil handles its own sub_ability chain internally for both
    // `UntilCondition` arms — `NextMatches` injects the hit card as the
    // sub-ability's target; `CumulativeThreshold` runs the sub-chain with the
    // original target list intact so it can address the per-resolution exile
    // links via `TargetFilter::ExiledBySource`. Either way, skip the outer
    // chain to avoid double-execution.
    if matches!(ability.effect, Effect::ExileFromTopUntil { .. }) {
        return Ok(());
    }

    // CR 701.44d: `ExploreAll` is the single authority for its own sub_ability
    // chain. `explore::resolve_single_explorer` carries `ability.sub_ability`
    // onto the terminal explorer (and synthesizes the per-explorer `TrackedSet`
    // continuation between explorers). If the generic chain walker ALSO
    // processed the sub here, a paused explore (the nonland `DigChoice`) would
    // re-prepend the sub onto `pending_continuation` a SECOND time. For a
    // synthesized `ExploreAll { TrackedSet }` continuation that second prepend
    // chains it to itself, producing a self-renewing loop that re-explores the
    // same permanent every time the choice resolves — Hakbal of the Surging
    // Soul accrued unbounded +1/+1 counters this way. Mirror the
    // `ExileFromTopUntil` guard above and skip the outer chain.
    if matches!(ability.effect, Effect::ExploreAll { .. }) {
        return Ok(());
    }

    // CR 615.5: `PreventDamage` with a chained `ContinuationStep` sub-ability
    // installs the sub as the shield's `runtime_execute` continuation — it runs
    // once per fired damage prevention event (Gatta and Luzzu's "prevent that
    // damage and put that many +1/+1 counters on it"). The outer chain walker
    // must NOT also resolve such a sub inline, or the rider would fire twice
    // (once immediately when the shield is installed, and again from each
    // post-replacement continuation). The shield is the single authority for the
    // rider's execution lifecycle.
    //
    // CR 700.2d: A `SequentialSibling` sub is an INDEPENDENT instruction (a
    // separate chosen mode of a modal spell — Dromoka's Command mode 3's
    // `PutCounter`), not a rider. It is NOT installed as the shield's
    // `runtime_execute`, so the chain walker must fall through to the generic
    // sub resolution tail below and resolve it on its own target.
    if matches!(ability.effect, Effect::PreventDamage { .. }) {
        if let Some(sub) = ability.sub_ability.as_deref() {
            if sub.sub_link == SubAbilityLink::ContinuationStep {
                return Ok(());
            }
        }
    }

    // Extract moved objects for result forwarding when forward_result is set.
    // Used for "put onto the battlefield attached to [source]" patterns where the
    // moved card becomes the sub-ability's source and the original source becomes a target.
    let forwarded_objects: Vec<ObjectId> = if ability.forward_result {
        events[events_before..]
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    } else {
        vec![]
    };
    let effect_context_object =
        parent_referent_context_from_events(state, &events[events_before..]);
    let amassed_army_object = amassed_army_context_from_events(state, &events[events_before..]);

    // CR 608.2c: "[Mandatory action]. If you do, [rider]." — seed the
    // performed-flag for a mandatory parent whose action just occurred.
    //
    // The "if you do" gate (`EffectOutcome { OptionalEffectPerformed }`) means
    // "if the preceding instruction's event happened." For an *optional* parent
    // ("you may …") the flag is set when the controller accepts; for effects
    // that compute their own win/loss/selection outcome (coin flip, clash, dig —
    // see `effect_manages_own_outcome_flag`) the branch handlers set it; for an
    // unless-pay / "if a player does" alternative the payment flow sets it. But a
    // plain *mandatory* action (Sacrifice, Mill, Destroy, …) whose rider is a
    // SEPARATE SENTENCE ("Sacrifice it. If you do, create Marit Lage.") has no
    // such hook — and per CR 608.2c that sentence is the next printed
    // instruction, gating on whether the prior action happened. We restrict to
    // `SubAbilityLink::SequentialSibling` precisely so the within-clause
    // `ContinuationStep` alternatives that ride on a payment / opponent choice
    // (unless-pay, `IfAPlayerDoes`) are NOT affected. When the action was not
    // signalled as failed (`cost_payment_failed_flag`, e.g. nothing eligible to
    // sacrifice), set the flag on the parent's context so it propagates to the
    // sub via `apply_parent_chain_context` and survives the sub's own chain-body
    // condition re-check. Covers the whole mandatory-rider class, not one card.
    // NOTE: this seed fires for any `EffectOutcome`-gated `SequentialSibling`,
    // including a `CurrentScopeSucceeded` gate. Seeding `optional_effect_performed`
    // there is a harmless no-op: a `CurrentScopeSucceeded` rider resolves against
    // `!cost_payment_failed_flag` (not this flag), and that guard already requires
    // `!cost_payment_failed_flag` — the same condition gating this seed.
    let mandatory_rider_owned;
    let ability = if !ability.optional
        && !ability.context.optional_effect_performed
        && !state.cost_payment_failed_flag
        && mandatory_parent_effect_performed(&ability.effect, &events[events_before..])
        && !effect_manages_own_outcome_flag(&ability.effect)
        && ability.sub_ability.as_ref().is_some_and(|sub| {
            sub.sub_link == SubAbilityLink::SequentialSibling
                && sub
                    .condition
                    .as_ref()
                    .is_some_and(condition_depends_on_effect_performed)
        }) {
        let mut owned = ability.clone();
        owned.context.optional_effect_performed = true;
        mandatory_rider_owned = owned;
        &mandatory_rider_owned
    } else {
        ability
    };
    let amassed_army_owned;
    let ability = if let Some(snapshot) = &amassed_army_object {
        let mut owned = ability.clone();
        owned.set_amassed_army_object_recursive(snapshot.clone());
        amassed_army_owned = owned;
        &amassed_army_owned
    } else {
        ability
    };

    // CR 608.2c + CR 613.1: A chained sub-ability is the next instruction in the
    // same resolution (instructions are followed "in the order written"), so it
    // resolves AFTER the parent's effect and must read the object's CURRENT
    // characteristics, which the layer system (CR 613) determines. When the
    // parent effect mutated layer-affecting state (a +1/+1 counter via
    // `PutCounter`, a P/T-setting continuous effect, …) it only marked
    // `layers_dirty`; the derived `power`/`toughness` fields are not recomputed
    // until the next flush. Flush here, before the sub resolves its condition and
    // effect-quantity reads, so a sub like "add {R} equal to this creature's
    // power" (Molten-Core Maestro, issue #2384) sees the post-counter power
    // rather than the stale pre-counter snapshot. `flush_layers` is the single
    // authority and a no-op when nothing is dirty, so leaf chains and non-P/T
    // parents pay nothing.
    if ability.sub_ability.is_some() {
        crate::game::layers::flush_layers(state);
    }

    // CR 702.131b + CR 702.131d: If the sub-ability's condition is gated on
    // the city's blessing, re-evaluate the blessing now — before checking the
    // condition — so a permanent created by the parent effect (e.g. Ocelot
    // Pride's Cat token becoming the 10th permanent) is reflected in
    // `state.city_blessing`. This mirrors the SBA loop's "any time" semantics
    // without waiting for the next priority pass.
    if ability.sub_ability.as_ref().is_some_and(|sub| {
        sub.condition
            .as_ref()
            .is_some_and(condition_contains_city_blessing)
    }) {
        crate::game::sba::apply_city_blessing_if_triggered(state, events);
    }

    // Follow typed sub_ability chain, propagating parent targets when sub has none.
    // This allows sub-abilities like "its controller gains life" to access the object
    // targeted by the parent (e.g. the exiled creature in Swords to Plowshares).
    if let Some(ref sub) = ability.sub_ability {
        // CR 614.1a + CR 608.2c: CastFromZone consumes the Torrential/Kylox
        // graveyard-redirect rider by stamping the granted casting permission. Do
        // not also execute the parser's structural rider (`ChangeZone` /
        // `PutAtLibraryPosition` targeting `ParentTarget`) as an immediate move,
        // or the graveyard card leaves before the player can cast it. CastFromZone
        // recognizes any redirect destination (exile / library / hand); Counter
        // only ever carries the exile sub-ability rider (its library/hand redirect
        // rides `countered_spell_zone`) and consumes it during `counter::resolve`
        // (stack -> exile directly) — skip the follow-up move either way.
        if (matches!(&ability.effect, Effect::CastFromZone { .. })
            && cast_from_zone::graveyard_destination_rider(sub).is_some())
            || (matches!(&ability.effect, Effect::Counter { .. })
                && cast_from_zone::is_graveyard_exile_rider_subability(sub))
        {
            return Ok(());
        }

        // CR 614.1c + CR 122.1: CastFromZone consumes the "creature cast this way
        // enters with a [counter] counter on it" rider as permission metadata
        // (recorded on the granted `ExileWithAltCost`). Resolving the structural
        // `AddPendingETBCounters` rider here would read the (absent) SpellCast
        // trigger event of the granting ability, not the future graveyard cast —
        // so skip it (Osteomancer Adept, The Tomb of Aclazotz).
        if matches!(&ability.effect, Effect::CastFromZone { .. })
            && cast_from_zone::is_enters_with_counter_rider_subability(sub)
        {
            return Ok(());
        }

        // Check if the sub_ability has a condition that gates its execution.
        // Casting-time conditions are evaluated against the parent's SpellContext.
        if let Some(ref condition) = sub.condition {
            // CR 608.2e: "Instead" overrides are terminal — the Cow swap above either
            // replaced the parent's effect (condition met) or didn't (condition not met).
            // For kicker/ninjutsu/keyword-instead, the base has no continuation chain.
            // For ConditionInstead, the base chain (else_ability) must run when NOT swapped.
            if matches!(
                condition,
                AbilityCondition::AdditionalCostPaidInstead
                    | AbilityCondition::CastVariantPaidInstead { .. }
                    | AbilityCondition::TargetHasKeywordInstead { .. }
            ) {
                if let Some(ref base_chain) = sub.else_ability {
                    let mut resolved = base_chain.as_ref().clone();
                    if resolved.targets.is_empty() && !ability.targets.is_empty() {
                        resolved.targets = ability.targets.clone();
                    }
                    apply_parent_chain_context(
                        &mut resolved,
                        ability,
                        effect_context_object.as_ref(),
                        state,
                    );
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        debug_assert!(
                            state.pending_continuation.is_none(),
                            "pending_continuation overwritten before consumption — else_ability chain will be lost"
                        );
                        state.pending_continuation =
                            Some(PendingContinuation::new(Box::new(resolved)));
                    } else {
                        resolve_ability_chain(state, &resolved, events, depth + 1)?;
                    }
                }
                return Ok(());
            }
            if matches!(condition, AbilityCondition::ConditionInstead { .. }) {
                // CR 608.2c: Swap didn't fire (condition not met). The parent's own
                // effect has already executed; now run the base continuation chain
                // stored in else_ability (e.g., the "put into hand, then shuffle"
                // that follows the base SearchLibrary).
                if let Some(ref base_chain) = sub.else_ability {
                    let mut resolved = base_chain.as_ref().clone();
                    if resolved.targets.is_empty() && !ability.targets.is_empty() {
                        resolved.targets = ability.targets.clone();
                    }
                    apply_parent_chain_context(
                        &mut resolved,
                        ability,
                        effect_context_object.as_ref(),
                        state,
                    );
                    // If the parent effect entered an interactive state (e.g.,
                    // SearchChoice), stash the else chain as a continuation so it
                    // runs after the player responds — not immediately.
                    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                        debug_assert!(
                            state.pending_continuation.is_none(),
                            "pending_continuation overwritten before consumption — else_ability chain will be lost"
                        );
                        state.pending_continuation =
                            Some(PendingContinuation::new(Box::new(resolved)));
                    } else {
                        resolve_ability_chain(state, &resolved, events, depth + 1)?;
                    }
                } else if let Some(ref tail) = sub.sub_ability {
                    // CR 608.2c (read the whole text; follow instructions in the order
                    // written): a `SequentialSibling` clause printed AFTER the "instead"
                    // sentence is an INDEPENDENT instruction — the "instead" replaced only
                    // the prior sentence, so the tail runs in BOTH branches. The swap branch
                    // runs it as the swapped node's sub via the general sub path below
                    // (including the one-sided-fight source prepend at the swap block ~6844);
                    // mirror that delivery here for the not-swap branch so the tail is not
                    // silently dropped (Throw from the Saddle / Evil's Thrall / Take the
                    // Fall / That's Rough Buddy). This `else if` fires only when
                    // `else_ability` is None: a card carrying a distinct else-chain (From
                    // Father to Son: else = ChangeZone→Hand, a DISTINCT node from its
                    // Shuffle sibling) takes the else path above and never reaches here, so
                    // there is no double-run. Such a card's trailing sibling is deliberately
                    // not run in this branch (out of scope; the only corpus member is From
                    // Father to Son, whose from-hand shuffle is supplied by SearchLibrary's
                    // auto-shuffle).
                    // GUARDS: (1) `SequentialSibling` only — a `ContinuationStep` sub is part
                    // of the REPLACED clause and must NOT run when the swap didn't fire;
                    // (2) never run an `Unimplemented` tail — no speculative semantics
                    // (Increasing Vengeance's "copy with new targets").
                    if tail.sub_link == SubAbilityLink::SequentialSibling
                        && !matches!(tail.effect, Effect::Unimplemented { .. })
                    {
                        let mut resolved = tail.as_ref().clone();
                        // CR 120.1 + CR 115.10a + CR 601.2c: a one-sided-fight
                        // `DealDamage { damage_source: Target }` tail ("It deals damage equal
                        // to its power …") names the boosted creature (the base's chosen
                        // object) as the SOURCE; the tail carries only its fresh recipient
                        // slot, so prepend the base's object => targets = [source(own),
                        // recipient(foe)] — the exact contract the swap path's one-sided-fight
                        // block builds (CR 208.1: power read live at resolution). Otherwise
                        // (a target-less anaphoric tail — "Untap that creature." / "Draw a
                        // card.") inherit the base's targets, exactly as the else path does
                        // above.
                        if is_one_sided_fight_damage_sub(&tail.effect) && !tail.targets.is_empty() {
                            if let Some(source) = first_object_target(&ability.targets) {
                                if first_object_target(&resolved.targets) != Some(source) {
                                    resolved.targets.insert(0, TargetRef::Object(source));
                                }
                            }
                        } else if resolved.targets.is_empty() && !ability.targets.is_empty() {
                            resolved.targets = ability.targets.clone();
                        }
                        apply_parent_chain_context(
                            &mut resolved,
                            ability,
                            effect_context_object.as_ref(),
                            state,
                        );
                        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                            debug_assert!(
                                state.pending_continuation.is_none(),
                                "pending_continuation overwritten before consumption — instead-tail chain will be lost"
                            );
                            state.pending_continuation =
                                Some(PendingContinuation::new(Box::new(resolved)));
                        } else {
                            resolve_ability_chain(state, &resolved, events, depth + 1)?;
                        }
                    }
                }
                return Ok(());
            }

            // CR 608.2c + CR 603.12: When the parent effect suspended for a
            // player choice (e.g. an optional "you may" prompt, or the
            // `EffectZoneChoice` raised by `Effect::Sacrifice` of N permanents),
            // an `IfYouDo` / `IfAPlayerDoes` / `WhenYouDo` reflexive gate cannot
            // yet be evaluated — whether the effect was performed is not decided
            // until the player completes the choice. Eagerly evaluating it here
            // would read a stale `optional_effect_performed = false` and either
            // silently drop the conditional sub-ability (Ral, Monsoon Mage:
            // "you may exile Ral. If you do, return him transformed") or, for a
            // `WhenYouDo` reflexive, resolve its target selection immediately —
            // overwriting the parent's `EffectZoneChoice` with the reflexive's
            // `TriggerTargetSelection` and losing the sacrifice handler entirely
            // (issue #423: Grist's `[-2]` "Sacrifice a creature. When you do,
            // destroy …"). Defer instead: stash the sub WITH its condition so
            // `resolve_ability_chain`'s top-level condition check re-evaluates it
            // once the choice resolves and the flag is correct. `WhenYouDo` is
            // checked explicitly here (not folded into
            // `condition_depends_on_effect_performed`) so the condition-false
            // descent at the parent-skipped path is left unchanged.
            //
            // CR 608.2c: A `ZoneChangedThisWay` reflexive gate ("When you discard
            // a card this way, …" — Talion's Messenger, The Ancient One) reads
            // `last_zone_changed_ids`, which is empty while the parent's discard
            // pauses at `WaitingFor::DiscardChoice` (hand > 1). Evaluating it here
            // would read the empty ledger and drop the sub. Defer it on the same
            // path; the `DiscardChoice` resume populates `last_zone_changed_ids`
            // from the cards that reached the graveyard before draining, so the
            // re-evaluation at chain top sees the discarded objects.
            // CR 608.2c: a "that card / the revealed card"-referential destination
            // gate on a suspended `SearchLibrary` sub can't be evaluated until the
            // search injects the found card as the continuation target. Scoped to
            // `SearchChoice` specifically — the ONLY choice whose completion sets
            // `cont.chain.targets` to the result object; broadening to every
            // resolution choice would mis-defer e.g. a `Sacrifice`→`EffectZoneChoice`
            // →`TargetMatchesFilter`-gated sibling, whose completion leaves
            // `cont.chain.targets` empty (it uses the tracked-set/ParentTarget path).
            if waits_for_resolution_choice(&state.waiting_for)
                && (condition_depends_on_effect_performed(condition)
                    || condition_depends_on_zone_change_this_way(condition)
                    || matches!(condition, AbilityCondition::WhenYouDo)
                    || (matches!(state.waiting_for, WaitingFor::SearchChoice { .. })
                        && condition_depends_on_result_object(condition)))
            {
                let mut sub_clone = sub.as_ref().clone();
                if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
                    sub_clone.targets = ability.targets.clone();
                }
                apply_parent_chain_context(
                    &mut sub_clone,
                    ability,
                    effect_context_object.as_ref(),
                    state,
                );
                prepend_to_pending_continuation(state, sub_clone);
                return Ok(());
            }

            // CR 608.2c + CR 400.7j: When the parent effect wrote the
            // last-revealed set (a look/reveal/dig) but carries no targets of its
            // own, the gated sub references that revealed object both in its
            // condition ("if it has three or more colored mana symbols in its
            // mana cost") and in its effect ("add three mana in any combination
            // of its colors") — Omnath, Locus of All. CR 400.7j: an effect can
            // find an object it moved to a public zone, so the deep add-mana sub
            // still finds the revealed card after it is put into hand. Inject the
            // revealed ids as the parent's targets so BOTH the condition
            // evaluation and the performed-true sub-resolution (which inherits
            // the parent's targets via the early continuation/sibling paths) bind
            // to the revealed object.
            let injected_parent;
            let ability: &ResolvedAbility = if effect_writes_last_revealed_ids(&ability.effect)
                && !state.last_revealed_ids.is_empty()
                && ability.targets.is_empty()
            {
                let mut clone = ability.clone();
                clone.targets = inject_last_revealed_targets(state, ability, sub.as_ref());
                injected_parent = clone;
                &injected_parent
            } else {
                ability
            };

            let condition_met = evaluate_condition(condition, state, ability);
            if !condition_met {
                // CR 608.2c: Execute else branch if present ("Otherwise, [effect]")
                if let Some(ref else_branch) = sub.else_ability {
                    let mut else_resolved = else_branch.as_ref().clone();
                    // Inject revealed card IDs as targets for else branches following
                    // effects that write last_revealed_ids, so "Otherwise, put that
                    // card on the bottom of your library" knows which card to move.
                    if else_resolved.targets.is_empty()
                        && !state.last_revealed_ids.is_empty()
                        && effect_writes_last_revealed_ids(&ability.effect)
                    {
                        else_resolved.targets = state
                            .last_revealed_ids
                            .iter()
                            .map(|&id| TargetRef::Object(id))
                            .collect();
                    } else if else_resolved.targets.is_empty() && !ability.targets.is_empty() {
                        else_resolved.targets = ability.targets.clone();
                    }
                    apply_parent_chain_context(
                        &mut else_resolved,
                        ability,
                        effect_context_object.as_ref(),
                        state,
                    );
                    if try_begin_deferred_else_branch_target_selection(
                        state,
                        &mut else_resolved,
                        events,
                    )? {
                        return Ok(());
                    }
                    resolve_ability_chain(state, &else_resolved, events, depth + 1)?;
                } else if let Some(ref next) = sub.sub_ability {
                    // CR 608.2c: A separate-sentence sibling after the gated sub is
                    // the next independent instruction ("...in the order written")
                    // and resolves regardless of the gate's failure (Wernog clause 3
                    // "You investigate X times" after the per-opponent decline gate).
                    // Mirrors the optional-decline handler's SequentialSibling check.
                    // Predicate is `next.sub_link` (the sibling's link to the gated
                    // sub), NOT `sub.sub_link` (the gated sub's link to its parent =
                    // ContinuationStep).
                    //
                    // For multi-branch chains like Omnath (n=1, n=2, n=3), find the
                    // next SequentialSibling whose condition can actually resolve.
                    // A false no-op sibling is skipped, but once a live sibling is
                    // selected, its own chain is resolved by `resolve_ability_chain`;
                    // continuing this outer walk would double-resolve later siblings.
                    let mut current = Some(next);
                    while let Some(ref sibling) = current {
                        if sibling.sub_link == SubAbilityLink::SequentialSibling {
                            let mut sibling_resolved = sibling.as_ref().clone();
                            if sibling_resolved.targets.is_empty() && !ability.targets.is_empty() {
                                sibling_resolved.targets = ability.targets.clone();
                            }
                            apply_parent_chain_context(
                                &mut sibling_resolved,
                                ability,
                                effect_context_object.as_ref(),
                                state,
                            );
                            if sibling_resolved
                                .condition
                                .as_ref()
                                .is_some_and(|condition| {
                                    !evaluate_condition(condition, state, &sibling_resolved)
                                        && sibling_resolved.else_ability.is_none()
                                })
                            {
                                current = sibling.sub_ability.as_ref();
                                continue;
                            }
                            resolve_ability_chain(state, &sibling_resolved, events, depth + 1)?;
                            break;
                        }
                        current = sibling.sub_ability.as_ref();
                    }
                }
                return Ok(());
            }

            // CR 608.2c + CR 603.7: An `If you do` boundary (`EffectOutcome
            // { OptionalEffectPerformed }`) opens a new instruction clause —
            // "you may [do X]. If you do, [rider]." The rider falls into one of
            // two classes by what it does with the tracked-set channel:
            //
            //   * PRODUCER rider (e.g. Party Thrasher's `ExileTop`): the rider
            //     creates its OWN fresh set and nothing the gating action X
            //     affected may be named by a "those cards" / "one of them"
            //     reference inside it. The chain-scoped tracked-set identity
            //     MUST be reset here so the rider's producer starts clean.
            //     Without this, Party Thrasher's "you may discard a card. If
            //     you do, exile the top two cards…, then choose one of them"
            //     co-publishes the discarded card (now in the graveyard) with
            //     the two exiled cards, offering three to choose from (#1977).
            //
            //   * CONSUMER rider (e.g. God-Pharaoh's Gift's `CopyTokenOf {
            //     target: TrackedSet }`): the rider's "that card" anaphor
            //     CR 707.2a names the very card the gating exile published into
            //     `chain_tracked_set_id` THIS resolution. Resetting here would
            //     orphan it to the turn-global `latest_tracked_set_id` fallback,
            //     so a second same-turn resolution (whose first exile's set also
            //     persists) binds to the wrong card (#2350). Skip the reset for
            //     consumer riders so the anaphor stays chain-local.
            //
            // `effect_references_tracked_set` discriminates the two: it is true
            // exactly for consumer riders (any `TrackedSet` quantity/filter
            // position, incl. `CopyTokenOf { target }`), false for producers.
            if matches!(
                condition,
                AbilityCondition::EffectOutcome {
                    signal: EffectOutcomeSignal::OptionalEffectPerformed,
                }
            ) && !effect_references_tracked_set(&sub.effect)
            {
                state.chain_tracked_set_id = None;
            }

            // CR 603.12: Deferred reflexive target selection for inline sub-chains.
            if matches!(
                condition,
                AbilityCondition::WhenYouDo | AbilityCondition::QuantityCheck { .. }
            ) && try_begin_reflexive_target_selection(
                state,
                sub,
                Some(ability),
                effect_context_object.as_ref(),
                events,
                depth,
            )? {
                return Ok(());
            }
        }
        // If the effect resolver already set up a pending_continuation without
        // opening a choice (e.g. clash injects modified context for
        // optional_effect_performed), the sub_ability chain is already
        // accounted for — skip to avoid double execution.
        //
        // CR 608.2c: Only skip when THIS effect's resolution INSTALLED or
        // REPLACED the continuation. Compare by identity against the entry
        // snapshot:
        //   * `None` -> `Some(_)`  : effect installed a continuation -> skip.
        //   * `Some(a)` -> `Some(b)` where `a != b` : effect replaced the
        //     outer continuation with its own (clash injects a modified
        //     `optional_effect_performed` context this way) -> skip; clash's
        //     stashed continuation IS the `sub_ability`.
        //   * `Some(a)` -> `Some(a)` (unchanged) : the continuation belongs to
        //     an unrelated outer chain (e.g. a `player_scope` iteration's
        //     remaining-opponent queue). It does NOT account for this effect's
        //     own `sub_ability` — do NOT skip, or the sub is silently dropped
        //     (issue #491: the `LoseLife→Draw` decline body would lose its
        //     `Draw` while another opponent's iteration is queued).
        let continuation_installed_by_this_effect = state.pending_continuation.is_some()
            && state.pending_continuation != pending_continuation_before;
        if continuation_installed_by_this_effect && !waits_for_resolution_choice(&state.waiting_for)
        {
            return Ok(());
        }
        // CR 118.12 + CR 608.2c: a paused PayCost resolver installs the full
        // remaining-cost continuation itself so later sub-costs stay before
        // the original rider after the choice resolves.
        if continuation_installed_by_this_effect && matches!(ability.effect, Effect::PayCost { .. })
        {
            return Ok(());
        }
        // If resolve_effect just entered a player-choice state (Scry/Dig/Surveil),
        // save the sub-ability as a continuation to execute after the player responds,
        // rather than immediately processing it (which would bypass the UI).
        if waits_for_resolution_choice(&state.waiting_for) {
            let mut sub_clone = sub.as_ref().clone();
            if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
                sub_clone.targets = ability.targets.clone();
            }
            apply_parent_chain_context(
                &mut sub_clone,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            prepend_to_pending_continuation(state, sub_clone);
            return Ok(());
        }

        // CR 120.1 + CR 608.2c + CR 115.10a: one-sided-fight chain — the boost
        // head ("Target creature you control gets +N/+M …") chose the creature
        // that the trailing `DealDamage { damage_source = Target }` sub's "It"
        // anaphor names. The sub was assigned its OWN recipient slot in
        // declaration order, so its `targets` hold only the fresh opponent
        // recipient. Per CR 120.1 the object that deals the damage is the boosted
        // creature (the parent's chosen object target), and per CR 608.2c "It"/
        // "its power" refer to that same creature — not the recipient. Prepend
        // the parent's chosen object so the sub resolves with the contract the
        // `deal_damage` resolver and `quantity::resolve_object_pt`'s
        // one-sided-fight fallback expect: `targets = [source, recipient]`
        // (source = `targets[0]`, recipients = `targets[1..]`). Guarded on the
        // parent already carrying an object target and the source not already
        // being `targets[0]`, so it is a no-op for every other chain shape.
        if is_one_sided_fight_damage_sub(&sub.effect) && !sub.targets.is_empty() {
            if let Some(source) = first_object_target(&ability.targets) {
                if first_object_target(&sub.targets) != Some(source) {
                    let mut sub_with_source = sub.as_ref().clone();
                    sub_with_source.targets.insert(0, TargetRef::Object(source));
                    apply_parent_chain_context(
                        &mut sub_with_source,
                        ability,
                        effect_context_object.as_ref(),
                        state,
                    );
                    resolve_ability_chain(state, &sub_with_source, events, depth + 1)?;
                    return Ok(());
                }
            }
        }

        // CR 120.1 + CR 601.2c: multi-source-fight chain — the parent (the
        // `TargetOnly` source picker for the direct "up to N target creatures you
        // control each deal damage equal to their power …" form, or the prior
        // `SetTapState`/`Pump` sentence for the "They each …" back-reference)
        // chose the WHOLE source set. The trailing `DealDamage { damage_source =
        // EachTarget }` sub carries only its fresh recipient slot, so prepend
        // every parent object target ahead of it: the resolver then reads
        // `targets = [source_0, …, source_{n-1}, recipient]` and each source
        // deals its own power (CR 208.1 modifiable characteristic, CR 608.2 read
        // at resolution) to the recipient. Guarded on the parent carrying object
        // targets the sub does not already hold, so it is a no-op for any other
        // chain shape.
        if is_each_target_damage_sub(&sub.effect) {
            let parent_sources: Vec<TargetRef> = ability
                .targets
                .iter()
                .filter(|t| matches!(t, TargetRef::Object(_)))
                .cloned()
                .collect();
            if !parent_sources.is_empty() && parent_sources.iter().all(|s| !sub.targets.contains(s))
            {
                let mut sub_with_sources = sub.as_ref().clone();
                for (i, source) in parent_sources.into_iter().enumerate() {
                    sub_with_sources.targets.insert(i, source);
                }
                apply_parent_chain_context(
                    &mut sub_with_sources,
                    ability,
                    effect_context_object.as_ref(),
                    state,
                );
                resolve_ability_chain(state, &sub_with_sources, events, depth + 1)?;
                return Ok(());
            }
        }

        // Apply forward_result: moved object becomes sub's source.
        //
        // CR 303.4f: Aura entering by non-spell means — controller chooses the enchanted object.
        // CR 301.5b: Equipment entering attached via "put onto the battlefield attached to" wiring.
        // For the Attach shape (Armored Skyhunter, Quest for the Holy Relic),
        // the moved card is the attachment and the original source is the
        // host, so we additionally push the original source into the sub's
        // targets.
        //
        // CR 608.2c: For non-Attach shapes (Emperor of Bones' "It gains
        // haste. Sacrifice it." after a `ChangeZone` to Battlefield),
        // pushing the original source as a target would mis-bind any
        // downstream `ParentTarget` consumer — the delayed Sacrifice
        // would target Emperor itself instead of the just-returned
        // creature. Instead, when the sub has no targets of its own and
        // the parent ability has no targets to inherit (and the sub
        // isn't an implicit tracked-set consumer), prepend the moved
        // card as a target so `ParentTarget` consumers downstream
        // resolve to it.
        if !forwarded_objects.is_empty() {
            let mut sub_with_context = sub.as_ref().clone();
            // CR 707.10: `CopySpell { SelfRef }` copies the resolving spell
            // itself (Sevinne's Reclamation, Chain cycle). `forward_result`
            // rebinding `source_id` to the just-moved permanent would make
            // `copy_spell::resolve` look up the wrong stack entry after
            // `resolve_top` has popped the spell (issue #2860).
            if !copy_spell_self_ref_keeps_resolving_spell_source(sub) {
                sub_with_context.source_id = forwarded_objects[0];
                if matches!(
                    ability.effect,
                    Effect::Conjure {
                        destination: Zone::Battlefield,
                        ..
                    }
                ) && !effect_uses_implicit_tracked_set_targets(&sub.effect)
                {
                    // Conjure's propagated `ability.targets` carry the
                    // duplicate_of referent (Three Tree Battalion's
                    // battlefield-put creature), not the just-conjured object.
                    // Trailing ParentTarget anaphora ("the duplicate", "its
                    // base power…") must bind to the conjured card via the
                    // ZoneChanged event, mirroring ChangeZone forward_result
                    // wiring.
                    sub_with_context.targets = vec![TargetRef::Object(forwarded_objects[0])];
                } else if matches!(sub.effect, Effect::Attach { .. }) {
                    let attach_target_is_last_created = matches!(
                        &sub.effect,
                        Effect::Attach {
                            target: TargetFilter::LastCreated,
                            ..
                        }
                    );
                    if !attach_target_is_last_created
                        && !sub_with_context
                            .targets
                            .iter()
                            .any(|t| matches!(t, TargetRef::Object(id) if *id == ability.source_id))
                    {
                        sub_with_context
                            .targets
                            .push(TargetRef::Object(ability.source_id));
                    }
                } else if sub_with_context.targets.is_empty()
                    && !effect_uses_implicit_tracked_set_targets(&sub.effect)
                {
                    // CR 608.2c: ParentTarget consumers in a forward_result sub-chain
                    // need the moved object's id in `targets`, not just a rebound
                    // `source_id`. Goryo's Vengeance ("return target … creature …
                    // That creature gains haste. Exile it at the beginning of the
                    // next end step.") carries explicit cast-time targets on the
                    // parent `ChangeZone`; Emperor-of-Bones-style descriptors do
                    // not. Both shapes must snapshot the just-moved card for
                    // downstream ParentTarget / delayed-trigger registration.
                    if !ability.targets.is_empty() {
                        sub_with_context.targets = ability.targets.clone();
                    } else {
                        sub_with_context
                            .targets
                            .insert(0, TargetRef::Object(forwarded_objects[0]));
                    }
                }
            }
            apply_parent_chain_context(
                &mut sub_with_context,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && !state.last_revealed_ids.is_empty()
            && effect_writes_last_revealed_ids(&ability.effect)
            // CR 701.21a: Sacrifice resolves its own battlefield-scoped eligible
            // pool — injecting library card IDs from a look-only Dig (e.g.
            // Birthing Ritual) would route through effect_object_targets and
            // silently skip every card (Zone::Library ≠ Zone::Battlefield),
            // producing no PermanentSacrificed event and leaving
            // effect_context_object = None for the downstream PriorLook Dig.
            && !matches!(sub.effect, Effect::Sacrifice { .. })
        {
            // Inject revealed card IDs as targets for sub_abilities following
            // effects that write last_revealed_ids. Parallel to how
            // continuations inject chosen cards as targets.
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = inject_last_revealed_targets(state, ability, sub.as_ref());
            apply_parent_chain_context(
                &mut sub_with_targets,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && !state.last_zone_changed_ids.is_empty()
            && matches!(ability.effect, Effect::Draw { .. })
            && matches!(sub.effect, Effect::Reveal { .. })
        {
            // CR 701.20a: "Draw N cards and reveal them" — the pronoun refers to
            // the cards that just moved into the hand, not a separate library
            // reveal. Forward the draw's ZoneChanged objects as reveal targets
            // (Mad Wizard's Lair and the same draw-then-reveal class).
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = state
                .last_zone_changed_ids
                .iter()
                .map(|&id| TargetRef::Object(id))
                .collect();
            apply_parent_chain_context(
                &mut sub_with_targets,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && !state.last_zone_changed_ids.is_empty()
            && matches!(ability.effect, Effect::ExileTop { .. })
            && !effect_uses_implicit_tracked_set_targets(&sub.effect)
        {
            // CR 309.4c + CR 607.1: Forward exiled card IDs to sub-ability
            // (linked ability pair — second refers to cards exiled by the first).
            // Skipped when the sub explicitly references the chain-unified
            // tracked set via `TargetFilter::TrackedSet` (compound-exile grants
            // like Suspend Aggression must iterate the full set, not just the
            // ExileTop results).
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = state
                .last_zone_changed_ids
                .iter()
                .map(|&id| TargetRef::Object(id))
                .collect();
            apply_parent_chain_context(
                &mut sub_with_targets,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty() && effect_uses_implicit_tracked_set_targets(&sub.effect) {
            let mut sub_with_context = sub.as_ref().clone();
            apply_parent_chain_context(
                &mut sub_with_context,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        } else if sub.targets.is_empty() && !ability.targets.is_empty() {
            let mut sub_with_targets = sub.as_ref().clone();
            sub_with_targets.targets = ability.targets.clone();
            apply_parent_chain_context(
                &mut sub_with_targets,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_targets, events, depth + 1)?;
        } else if sub.targets.is_empty()
            && ability.targets.is_empty()
            && effect_refs_parent_target(&sub.effect)
            && effect_context_object.is_some()
        {
            // CR 608.2c + CR 400.7j (issue #2890): When neither the parent nor
            // the sub carries propagated `targets`, but the parent instruction
            // stamped a singular referent snapshot (exile/move/sacrifice), seed
            // the sub's target list so every ParentTarget* consumer — not only
            // `parent_target_controller` — can bind to the departed object.
            let mut sub_with_referent = sub.as_ref().clone();
            if let Some(snapshot) = &effect_context_object {
                sub_with_referent
                    .targets
                    .push(TargetRef::Object(snapshot.object_id));
            }
            apply_parent_chain_context(
                &mut sub_with_referent,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_referent, events, depth + 1)?;
        } else {
            // Propagate SpellContext so additional_cost_paid and other flags
            // survive through the chain (e.g., Gift delivery → spell effects
            // with "if the gift was promised" conditions).
            let mut sub_with_context = sub.as_ref().clone();
            apply_parent_chain_context(
                &mut sub_with_context,
                ability,
                effect_context_object.as_ref(),
                state,
            );
            resolve_ability_chain(state, &sub_with_context, events, depth + 1)?;
        }
    }

    Ok(())
}

fn effect_depends_on_missing_chosen_player(ability: &ResolvedAbility) -> bool {
    ability
        .effect
        .target_filter()
        .and_then(crate::game::ability_utils::filter_chosen_player_index)
        .is_some_and(|index| ability.chosen_players.get(index as usize).is_none())
}

/// CR 608.2c + CR 109.5: Spell-effect "if you sacrificed a [filter] this way"
/// (Deadly Brew, Rise of the Witch-king) when no activation-cost object is in
/// scope. Consults the chain tracked sacrifice set (or the scoped
/// `last_zone_changed_ids` snapshot) and requires a match sacrificed by the
/// printed controller.
fn controller_sacrificed_matching_this_way(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> bool {
    let controller = ability.original_controller.unwrap_or(ability.controller);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);

    let candidate_ids: Vec<ObjectId> = state
        .chain_tracked_set_id
        .and_then(|id| state.tracked_object_sets.get(&id).cloned())
        .filter(|ids| !ids.is_empty())
        .unwrap_or_else(|| state.last_zone_changed_ids.clone());

    candidate_ids.iter().any(|&id| {
        if let Some(lki) = state.lki_cache.get(&id) {
            lki.controller == controller
                && crate::game::filter::matches_target_filter_on_lki_snapshot(
                    state, id, lki, filter, &ctx,
                )
        } else if let Some(obj) = state.objects.get(&id) {
            obj.controller == controller
                && crate::game::filter::matches_target_filter(state, id, filter, &ctx)
        } else {
            false
        }
    })
}

/// CR 608.2c + CR 700.1: `RevealedHasCardType` riders (including `Not` for
/// nonland branches) must not evaluate when no card was revealed or moved this
/// way — negating a failed land match must not become true (issue #2871).
fn subject_dependent_type_condition_has_no_subject(
    condition: &AbilityCondition,
    state: &GameState,
) -> bool {
    match condition {
        AbilityCondition::RevealedHasCardType { .. } => state
            .last_revealed_ids
            .first()
            .or_else(|| state.last_zone_changed_ids.first())
            .is_none(),
        AbilityCondition::Not { condition } => {
            subject_dependent_type_condition_has_no_subject(condition, state)
        }
        _ => false,
    }
}

/// CR 608.2c: Evaluate a condition against the current game state and ability context.
/// Returns whether the condition is met. Handles all `AbilityCondition` variants as
/// pure boolean evaluators — callers are responsible for any terminal control flow
/// (e.g., "Instead" overrides that early-return in the sub-ability context).
pub(crate) fn evaluate_condition(
    condition: &AbilityCondition,
    state: &GameState,
    ability: &ResolvedAbility,
) -> bool {
    match condition {
        // CR 702.33d + CR 702.33f + CR 608.2c: Parameterized additional-cost
        // gating. The default shape (`variant: None`, `min_count: 1`) reads the
        // legacy single-bool flag used by Gift / Buyback / Bargain / Evidence /
        // plain "if it was kicked". Specific kicker variants and multi-kicker
        // counts read `kickers_paid` (populated by the casting flow, copied to
        // GameObject at cast resolution, and propagated back into the trigger's
        // resolved-ability context for ETB triggers).
        AbilityCondition::AdditionalCostPaid {
            subject,
            source,
            origin,
            origin_ordinal,
            variant,
            kicker_cost,
            min_count,
        } => match subject {
            // CR 113.7: Source-relative payments live in the resolving ability's
            // own SpellContext. `Anaphoric`/`Demonstrative` "it"/"that spell"
            // back-references resolve to the source's context here, mirroring the
            // trigger path (the legacy kicker/Gift/Buyback/Casualty/Replicate class).
            crate::types::ability::ObjectScope::Source
            | crate::types::ability::ObjectScope::Anaphoric
            | crate::types::ability::ObjectScope::Demonstrative => {
                if let Some(origin) = origin {
                    let count = origin_ordinal.map_or_else(
                        || ability.context.instance_payment_count(*origin),
                        |ordinal| {
                            ability
                                .context
                                .instance_payment_count_for_ordinal(*origin, ordinal)
                        },
                    );
                    count >= (*min_count).max(1)
                } else {
                    ability.context.additional_cost_paid_matches(
                        *source,
                        *variant,
                        kicker_cost.as_ref(),
                        *min_count,
                    )
                }
            }
            // CR 115.1 + CR 608.2c + CR 702.33d: Target-relative payments — "counter
            // target spell if it was kicked" (Ertai's Trickery): "it" anaphors to the
            // first object target (the countered spell), so we read that object's
            // cast-time payments stamped on its GameObject, not the source context.
            // Mirrors the trigger evaluation in `triggers.rs` precisely.
            crate::types::ability::ObjectScope::Target => {
                if kicker_cost.is_some() && variant.is_none() {
                    return false;
                }
                let Some(id) = ability.targets.iter().find_map(|t| match t {
                    crate::types::ability::TargetRef::Object(id) => Some(*id),
                    crate::types::ability::TargetRef::Player(_) => None,
                }) else {
                    return false;
                };
                let Some(obj) = state.objects.get(&id) else {
                    return false;
                };
                match variant {
                    Some(kicker) => obj.kickers_paid.contains(kicker),
                    None => {
                        let non_kicker_count = if let Some(origin) = origin {
                            origin_ordinal.map_or_else(
                                || obj.instance_payment_count(*origin),
                                |ordinal| obj.instance_payment_count_for_ordinal(*origin, ordinal),
                            )
                        } else if obj.additional_cost_payments.is_empty() {
                            obj.additional_cost_payment_count
                        } else {
                            obj.additional_cost_payments
                                .iter()
                                .map(|payment| payment.count)
                                .sum()
                        };
                        crate::types::ability::additional_cost_payment_count_matches(
                            *source,
                            non_kicker_count > 0 || !obj.kickers_paid.is_empty(),
                            obj.kickers_paid.len(),
                            non_kicker_count,
                            *min_count,
                        )
                    }
                }
            }
            // No additional-cost-read semantics exist for these scopes: a
            // recipient/event/cost-paid referent never carries the "if it was
            // kicked" casting-payment question, which is only ever asked of the
            // resolving spell/ability (Source) or the targeted spell (Target).
            crate::types::ability::ObjectScope::Recipient
            | crate::types::ability::ObjectScope::EventSource
            | crate::types::ability::ObjectScope::CostPaidObject
            | crate::types::ability::ObjectScope::OtherRevealedCard
            | crate::types::ability::ObjectScope::EventTarget
            | crate::types::ability::ObjectScope::AmassedArmy => false,
        },
        AbilityCondition::AlternativeManaCostPaid => ability.context.alternative_mana_cost_paid,
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        } => ability.context.optional_effect_performed && !state.cost_payment_failed_flag,
        // CR 101.3 + CR 608.2c: "For each opponent who can't ..." reads the
        // current player-scope iteration's mandatory-success bit. The flag is
        // reset per scope iteration and set by mandatory-impossible handlers
        // during that iteration.
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::CurrentScopeSucceeded,
        } => !state.cost_payment_failed_flag,
        // CR 608.2d: "if they guessed right/wrong" reads the just-resolved
        // `Effect::OpponentGuess` outcome (a choice made by a player during
        // resolution, processed in written order per CR 608.2c). CR 609.3: when
        // no guess happened (impossible commit / empty hand) `guess_outcome` is
        // `None`, so BOTH polarities are false and NEITHER rider fires.
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::Guessed { outcome },
        } => ability.context.guess_outcome == Some(*outcome),
        AbilityCondition::EventOutcomeWon => state
            .current_trigger_event
            .as_ref()
            .map_or(ability.context.optional_effect_performed, |event| {
                event_outcome_was_won_by_controller(event, ability.controller)
            }),
        // CR 603.12: A reflexive triggered ability ("when you do") triggers
        // "based on whether the trigger event or events occurred earlier during
        // the resolution" of the parent. For a cost-payment parent
        // (`Effect::PayCost`), an unpayable or declined cost is NOT a trigger
        // event occurrence, so the reflexive sub-ability must NOT fire — the
        // `PayCost` handler signals this via `cost_payment_failed_flag`
        // (mirrors `IfYouDo` above). For any non-cost parent (e.g. `BecomeCopy`
        // reflexives, copy/exile replacement sub-abilities) the "do" always
        // occurred, so the contract remains unconditionally true.
        AbilityCondition::WhenYouDo => {
            !(matches!(ability.effect, Effect::PayCost { .. }) && state.cost_payment_failed_flag)
        }
        // CR 603.4: "If you cast it from [zone]" — check cast origin.
        AbilityCondition::CastFromZone { zone } => ability.context.cast_from_zone == Some(*zone),
        // CR 608.2c: "If it's a [type] card" — check the revealed card's type.
        // CR 205.3m: Optional additional_filter checks extra properties like
        // "of the chosen type" (IsChosenCreatureType).
        // CR 700.1 + CR 406.6: When no reveal occurred but the parent effect
        // moved a card between zones (e.g. Currency Converter's
        // "Put a card exiled with ~ into its owner's graveyard. If it's a
        // land card, ..." — issue #1545), fall back to the just-moved card in
        // `last_zone_changed_ids`. The reveal-driven path still wins when both
        // trackers are populated, so existing reveal+rider cards keep their
        // pre-fix behavior.
        AbilityCondition::RevealedHasCardType {
            card_types,
            additional_filter,
            subtype_filter,
        } => {
            let subject_id = state
                .last_revealed_ids
                .first()
                .or_else(|| state.last_zone_changed_ids.first())
                .copied();
            let type_matches = subject_id
                .map(|id| {
                    if card_types.is_empty() {
                        additional_filter.is_some() || subtype_filter.is_some()
                    } else {
                        card_types.iter().any(|card_type| {
                            super::printed_cards::object_has_core_type(state, id, *card_type)
                        })
                    }
                })
                .unwrap_or(false);
            // CR 205.3m: Match the revealed card's subtype against the subtype filter.
            let subtype_matches = match subtype_filter.as_ref() {
                None => true,
                Some(filter) => subject_id.is_some_and(|id| {
                    crate::game::filter::matches_target_filter(
                        state,
                        id,
                        filter.as_ref(),
                        &crate::game::filter::FilterContext::from_ability(ability),
                    )
                }),
            };
            let filter_matches = match additional_filter {
                // CR 205.3m: "of the chosen type" — check the revealed card's subtype
                // against the source permanent's chosen creature type.
                Some(FilterProp::IsChosenCreatureType) => {
                    let source = state.objects.get(&ability.source_id);
                    let subject = subject_id.and_then(|id| state.objects.get(&id));
                    match (source, subject) {
                        (Some(src), Some(obj)) => {
                            src.chosen_creature_type().is_some_and(|chosen_type| {
                                obj.card_types
                                    .subtypes
                                    .iter()
                                    .any(|s| s.eq_ignore_ascii_case(chosen_type))
                            })
                        }
                        _ => false,
                    }
                }
                // CR 202.3 + CR 700.1: Generic property gates on the revealed card
                // (e.g. Kellan, Daring Traveler's "creature card with mana value 3
                // or less" → `FilterProp::Cmc`). Evaluate the property against the
                // revealed subject through the shared filter evaluator, exactly as
                // `subtype_filter` does above.
                Some(prop) => subject_id.is_some_and(|id| {
                    crate::game::filter::matches_target_filter(
                        state,
                        id,
                        &TargetFilter::Typed(crate::types::ability::TypedFilter {
                            type_filters: vec![],
                            controller: None,
                            properties: vec![prop.clone()],
                        }),
                        &crate::game::filter::FilterContext::from_ability(ability),
                    )
                }),
                None => true,
            };
            type_matches && subtype_matches && filter_matches
        }
        // CR 608.2c + CR 201.2: "if it shares a [quality] with [reference]" —
        // compare two anaphoric object references at resolution time.
        AbilityCondition::ObjectsShareQuality {
            subject,
            reference,
            quality,
        } => {
            let subject_id = crate::game::targeting::resolved_targets(ability, subject, state)
                .into_iter()
                .find_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    _ => None,
                });
            let reference_id = crate::game::targeting::resolved_targets(ability, reference, state)
                .into_iter()
                .find_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    _ => None,
                });
            match (subject_id, reference_id) {
                (Some(left), Some(right)) => {
                    crate::game::filter::objects_share_quality(state, left, right, quality)
                }
                _ => false,
            }
        }
        AbilityCondition::TargetSharesNameWithOtherExiledThisWay { target } => {
            crate::game::targeting::resolved_targets(ability, target, state)
                .into_iter()
                .find_map(|target_ref| match target_ref {
                    TargetRef::Object(id) => Some(id),
                    _ => None,
                })
                .is_some_and(|id| {
                    crate::game::exile_links::shares_name_with_other_exiled_by_source(
                        state,
                        ability.source_id,
                        id,
                    )
                })
        }
        // CR 400.7: source permanent entered the battlefield this turn.
        // For the "unless ~ entered this turn" sense, wrap with `Not`.
        AbilityCondition::SourceEnteredThisTurn => {
            eval_source_entered_this_turn(state, ability.source_id)
        }
        // CR 702.49 + CR 702.190a + CR 603.4: "if its sneak/ninjutsu cost was
        // paid" (Source) / CR 115.1 + CR 608.2c + CR 702.185a: "if that creature
        // was cast for its warp cost" (Target — Full Bore). `subject` selects
        // which object's `cast_variant_paid` marker is read, mirroring the
        // `AdditionalCostPaid { subject }` evaluator above.
        AbilityCondition::CastVariantPaid { variant, subject } => {
            // CR 113.7: Source-relative markers live on the resolving ability's
            // own source permanent. `Anaphoric`/`Demonstrative` "it"/"that spell"
            // back-references resolve to the source here, mirroring the
            // AdditionalCostPaid Source branch and the trigger path.
            let object_id = match subject {
                crate::types::ability::ObjectScope::Source
                | crate::types::ability::ObjectScope::Anaphoric
                | crate::types::ability::ObjectScope::Demonstrative => Some(ability.source_id),
                // CR 115.1 + CR 608.2c: Target-relative marker — "that creature"
                // anaphors to the first object target (the +3/+2 target permanent
                // on Full Bore), so read THAT object's stamped warp marker, not the
                // source's. CR 702.185a: a warp permanent is exiled at the
                // beginning of the next end step, so it can only be read on its
                // cast turn — the `== Some((variant, turn_number))` turn-bound
                // below is therefore sound for Warp. A FUTURE cross-turn
                // target-scoped "was cast for its X cost" consumer (a variant that
                // persists past its cast turn) must drop the turn component for
                // that variant.
                crate::types::ability::ObjectScope::Target => {
                    ability.targets.iter().find_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        TargetRef::Player(_) => None,
                    })
                }
                // No cast-variant-marker read semantics exist for these scopes:
                // the "was cast for its X cost" question is only ever asked of the
                // resolving permanent (Source) or the targeted permanent (Target).
                crate::types::ability::ObjectScope::Recipient
                | crate::types::ability::ObjectScope::EventSource
                | crate::types::ability::ObjectScope::CostPaidObject
                | crate::types::ability::ObjectScope::OtherRevealedCard
                | crate::types::ability::ObjectScope::EventTarget
                | crate::types::ability::ObjectScope::AmassedArmy => None,
            };
            object_id
                .and_then(|id| state.objects.get(&id))
                .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
                .unwrap_or(false)
        }
        // CR 608.2c: General quantity comparison on trigger/effect context.
        AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } => {
            // CR 608.2c: a conditional second effect — evaluate the quantity
            // comparison at resolution time. Thread the full `ability` so
            // target-relative scopes (e.g. `PlayerScope::Target`,
            // `ParentObjectTargetController`) resolve against `ability.targets`.
            let l =
                crate::game::quantity::resolve_quantity_for_ability_condition(state, lhs, ability);
            let r =
                crate::game::quantity::resolve_quantity_for_ability_condition(state, rhs, ability);
            comparator.evaluate(l, r)
        }
        AbilityCondition::PreviousEffectAmount {
            comparator,
            rhs,
            channel,
        } => {
            // CR 120.10 (Excess) reads the overkill-beyond-lethal channel;
            // CR 120.6 (Total) reads the total marked-damage channel.
            let l = match channel {
                crate::types::ability::DamageChannel::Excess => {
                    state.last_effect_excess_amount.unwrap_or(0)
                }
                crate::types::ability::DamageChannel::Total => {
                    state.last_effect_amount.unwrap_or(0)
                }
            };
            let r = crate::game::quantity::resolve_quantity(
                state,
                rhs,
                ability.controller,
                ability.source_id,
            );
            comparator.evaluate(l, r)
        }
        AbilityCondition::CastDuringPhase { phases } => ability
            .context
            .cast_phase
            .is_some_and(|cast_phase| phases.contains(&cast_phase)),
        AbilityCondition::CastTimingPermission { permission } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.cast_timing_permission == Some((*permission, state.turn_number)))
            .unwrap_or(false),
        // CR 601.2h + CR 608.2c: Spend-color riders read the resolving spell
        // object's recorded mana payment. Spell copies with no mana paid
        // naturally fail because their tally is empty.
        AbilityCondition::ManaColorSpent { color, minimum } => state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.colors_spent_to_cast.get(*color) >= *minimum),
        AbilityCondition::HasMaxSpeed => has_max_speed(state, ability.controller),
        // CR 103.1: True when the scoped player took the first turn of the
        // game. The parser only emits `ControllerRef::You` (Radiant Smite,
        // Cindercone Smite, Sylvan Smite — "if you weren't the starting
        // player"); `ScopedPlayer` resolves to the per-instruction acting
        // player. The starting player is fixed at game start.
        AbilityCondition::WasStartingPlayer { controller } => {
            let subject = match controller {
                ControllerRef::ScopedPlayer => ability.scoped_player.unwrap_or(ability.controller),
                _ => ability.controller,
            };
            state.current_starting_player == subject
        }
        // CR 702.185c: True when any player cast a spell using `variant` (e.g.
        // Warp) this turn. Plasma Bolt's Void clause is a spell-effect
        // intervening condition, so this `AbilityCondition` arm is the one
        // exercised at runtime.
        AbilityCondition::SpellCastWithVariantThisTurn { variant } => {
            crate::game::restrictions::spell_cast_with_variant_this_turn(state, variant)
        }
        AbilityCondition::IsMonarch => eval_is_monarch(state, ability.controller),
        // CR 726.3: The initiative is a player designation that effects can identify.
        AbilityCondition::IsInitiative => eval_is_initiative(state, ability.controller),
        // CR 702.131c: The city's blessing is a player designation that effects
        // can identify.
        AbilityCondition::HasCityBlessing => eval_has_city_blessing(state, ability.controller),
        // CR 701.54a: Ring-bearer designation on the ability source.
        AbilityCondition::IsRingBearer => crate::game::effects::ring::is_current_ring_bearer(
            state,
            ability.controller,
            ability.source_id,
        ),
        // "Instead" override conditions — return pure boolean value.
        // Terminal control flow (early return from resolve_ability_chain) is the caller's
        // responsibility in the sub-ability context.
        AbilityCondition::AdditionalCostPaidInstead => ability.context.additional_cost_paid,
        AbilityCondition::CastVariantPaidInstead { variant } => state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.cast_variant_paid == Some((*variant, state.turn_number)))
            .unwrap_or(false),
        AbilityCondition::TargetHasKeywordInstead { ref keyword } => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => state.objects.get(id),
                _ => None,
            })
            .is_some_and(|obj| obj.has_keyword(keyword)),
        // CR 400.7 + CR 608.2c: "if that creature was a [type]" — check target or its LKI.
        AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki,
            subject_slot,
        } => {
            // CR 608.2c: An explicit `subject_slot` tests a specific declared
            // chain slot (Malamet's condition reads slot 0, the you-control
            // fighter) — the current node's local `targets` were overwritten by
            // most-recent-only chain propagation, so resolve against the
            // flattened root chain instead of `targets.first()`.
            // CR 109.4 + CR 603.2: without a slot, "that creature" / "it" is the
            // ability's first object target, OR — for subject-based triggers that
            // carry no chosen target — the triggering event's subject object.
            // Mirror the `ParentTargetController` fallback (targeting.rs): when
            // `targets` has no object, resolve the anaphor against
            // `TriggeringSource` from the current trigger event.
            let target_id = if let Some(index) = subject_slot {
                match crate::game::targeting::resolve_parent_slot_from_root(state, ability, *index)
                {
                    Some(TargetRef::Object(id)) => Some(id),
                    _ => None,
                }
            } else {
                ability
                    .targets
                    .iter()
                    .find_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .or_else(|| {
                        crate::game::targeting::resolve_event_context_target(
                            state,
                            &TargetFilter::TriggeringSource,
                            ability.source_id,
                        )
                        .and_then(|t| match t {
                            TargetRef::Object(id) => Some(id),
                            TargetRef::Player(_) => None,
                        })
                    })
            };
            let matched = if let Some(id) = target_id {
                if *use_lki {
                    if let Some(GameEvent::ZoneChanged { record, .. }) =
                        state.current_trigger_event.as_ref()
                    {
                        if record.object_id == id {
                            return crate::game::filter::matches_target_filter_on_zone_change_record(
                                state,
                                record,
                                filter,
                                &crate::game::filter::FilterContext::from_ability(ability),
                            );
                        }
                    }
                    // CR 400.7: Check last-known information for past-tense conditions.
                    // Try LKI cache first, fall back to current state if object still exists.
                    if let Some(lki) = state.lki_cache.get(&id) {
                        crate::game::filter::matches_target_filter_on_lki_snapshot(
                            state,
                            id,
                            lki,
                            filter,
                            &crate::game::filter::FilterContext::from_ability(ability),
                        )
                    } else {
                        // Object still exists — check current state.
                        // CR 107.3a + CR 601.2b: ability-context filter evaluation.
                        crate::game::filter::matches_target_filter(
                            state,
                            id,
                            filter,
                            &crate::game::filter::FilterContext::from_ability(ability),
                        )
                    }
                } else {
                    // Check current state for present-tense conditions.
                    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
                    crate::game::filter::matches_target_filter(
                        state,
                        id,
                        filter,
                        &crate::game::filter::FilterContext::from_ability(ability),
                    )
                }
            } else {
                false
            };
            matched
        }
        // CR 608.2c + CR 603.2: "if it targets a [filter]" — check the triggering
        // spell's committed targets (Flurry on Shiko and Narset, Unified).
        AbilityCondition::TriggeringSpellTargetsFilter { filter } => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| match event {
                crate::types::events::GameEvent::SpellCast { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .is_some_and(|spell_id| {
                super::restrictions::triggering_spell_targets_filter(
                    state, spell_id, filter, spell_id,
                )
            }),
        // CR 608.2c: "If this creature/permanent is a [type]" — check source object.
        AbilityCondition::SourceMatchesFilter { filter } => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            crate::game::filter::matches_target_filter(
                state,
                ability.source_id,
                filter,
                &crate::game::filter::FilterContext::from_ability(ability),
            )
        }
        AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => state.current_trigger_event.as_ref().is_some_and(|event| {
            crate::game::filter::matches_zone_change_event_object_filter(
                state,
                event,
                *origin,
                *destination,
                filter,
                &crate::game::filter::FilterContext::from_ability(ability),
            )
        }),
        // CR 608.2c + CR 614.1d: "if you control a/no [filter]" — scan the battlefield
        // for any other permanent owned/controlled by the ability controller matching
        // `filter`. Excludes the source itself so a Soldier-typed land doesn't satisfy
        // its own "you control a Soldier" check. `filter` carries `ControllerRef::You`
        // pre-bound by the parser; FilterContext provides the source binding.
        AbilityCondition::ControllerControlsMatching { filter } => {
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            state.objects.values().any(|o| {
                o.zone == crate::types::zones::Zone::Battlefield
                    && o.id != ability.source_id
                    && crate::game::filter::matches_target_filter(state, o.id, filter, &ctx)
            })
        }
        // CR 601.2 + CR 608.2c: "if you controlled a [filter] as you cast this
        // spell" reads the casting snapshot, not the resolution-time battlefield.
        AbilityCondition::ControllerControlledMatchingAsCast { filter } => ability
            .context
            .controller_controlled_as_cast
            .iter()
            .any(|snapshot_filter| snapshot_filter == filter),
        // CR 608.2c: "If it's your turn" — check active player against the
        // scoped player during each-player iteration, otherwise the controller.
        AbilityCondition::IsYourTurn => {
            state.active_player == ability.scoped_player.unwrap_or(ability.controller)
        }
        // CR 500.8 + CR 506.1 + CR 608.2c: "if it's the first combat phase
        // of the turn" gates follow-up effects such as additional combats.
        AbilityCondition::FirstCombatPhaseOfTurn => state.combat_phases_started_this_turn == 1,
        // CR 500.8 + CR 513.1 + CR 608.2c: "if it's the first end step of the
        // turn" gates the additional-end-step follow-up (Y'shtola Rhul); only
        // the first end step schedules another, preventing an infinite loop.
        AbilityCondition::FirstEndStepOfTurn => state.end_steps_started_this_turn == 1,
        // CR 505.1 + CR 505.1a + CR 500.1 + CR 608.2c: "if it is[n't] your
        // [phase]" reads the live current phase at resolution (CR 608.2c). The
        // grouped "main phase" phrase carries both `PreCombatMain` and
        // `PostCombatMain` (CR 505.1/505.1a), so membership covers either. The
        // "your" possessive is composed separately via `IsYourTurn`.
        AbilityCondition::CurrentPhaseIs { phases } => phases.contains(&state.phase),
        // CR 608.2c: "If a [noun] was [verb]ed this way" — check if any zone-changed
        // object matches the type filter. For optional-targeting parents with no targets
        // chosen, last_zone_changed_ids is empty → returns false.
        AbilityCondition::ZoneChangedThisWay { filter } => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            state
                .last_zone_changed_ids
                .iter()
                .any(|&id| crate::game::filter::matches_target_filter(state, id, filter, &ctx))
        }
        AbilityCondition::CostPaidObjectMatchesFilter { filter } => {
            if let Some(snapshot) = &ability.cost_paid_object {
                crate::game::filter::matches_target_filter_on_lki_snapshot(
                    state,
                    snapshot.object_id,
                    &snapshot.lki,
                    filter,
                    &crate::game::filter::FilterContext::from_ability(ability),
                )
            } else {
                controller_sacrificed_matching_this_way(state, ability, filter)
            }
        }
        // CR 611.2b: "if this creature/permanent is tapped" — check source object.
        // For the untapped sense, wrap with `Not`. No battlefield zone guard
        // (ability conditions; zone constrained by functioning-abilities path).
        AbilityCondition::SourceIsTapped => eval_source_is_tapped(state, ability.source_id),
        // CR 301.5 + CR 303.4: "if this permanent is attached to a creature you
        // control" — check the source Aura/Equipment's host. False when the
        // source is unattached or its host isn't a creature controlled by the
        // ability's controller. Lets bestow triggers like Springheart Nantuko
        // skip their optional payment branch silently while still resolving
        // the fallback sub-ability.
        AbilityCondition::SourceAttachedToCreature => eval_source_attached_to_controlled_creature(
            state,
            ability.source_id,
            ability.controller,
        ),
        // CR 608.2c: General "instead" — delegate to the wrapped inner condition.
        // The "instead" semantics are handled by the swap/guard in resolve_ability_chain.
        AbilityCondition::ConditionInstead { inner } => evaluate_condition(inner, state, ability),
        // CR 601.2c + CR 115.1 + CR 608.2c: True iff the parent ability declared at
        // least one object target. Unlike `TargetMatchesFilter`, there is NO
        // `TriggeringSource` fallback — a declined variable-count target
        // (CR 601.2c: zero targets announced) reads false, so the lowering pass's
        // `And{[HasObjectTarget, …]}` wrapper suppresses a reflexive-target rider
        // whose anaphor ("that creature") has no antecedent.
        AbilityCondition::HasObjectTarget => ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(_))),
        // CR 608.2c: Compound condition — all inner conditions must be true.
        AbilityCondition::And { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(c, state, ability)),
        // CR 608.2c: Compound condition — at least one inner condition must be true.
        AbilityCondition::Or { conditions } => conditions
            .iter()
            .any(|c| evaluate_condition(c, state, ability)),
        // CR 608.2c: Logical negation — true when the inner condition is false.
        AbilityCondition::Not { condition } => {
            if subject_dependent_type_condition_has_no_subject(condition, state) {
                return false;
            }
            !evaluate_condition(condition, state, ability)
        }
        // CR 730.2a: True when it's neither day nor night (no designation set yet).
        AbilityCondition::DayNightIsNeither => state.day_night.is_none(),
        // CR 731.1: True when the game has the requested day/night designation.
        AbilityCondition::DayNightIs {
            state: DayNight::Day,
        } => state.day_night == Some(DayNight::Day),
        AbilityCondition::DayNightIs {
            state: DayNight::Night,
        } => state.day_night == Some(DayNight::Night),
        // CR 603.4: "if this is the [Nth] time this ability has resolved this turn".
        // The counter is bumped at the top of `resolve_ability_chain` (depth 0)
        // before this evaluator runs, so a freshly-incremented count of `n`
        // satisfies the condition for the Nth resolution. Abilities without an
        // `ability_index` stamp (synthesized triggers, activated abilities) never
        // increment the counter and therefore evaluate as `count == 0`, which
        // matches no `n >= 1` print.
        AbilityCondition::NthResolutionThisTurn { n } => {
            if let Some(idx) = ability.ability_index {
                let count = state
                    .ability_resolutions_this_turn
                    .get(&(ability.source_id, idx))
                    .copied()
                    .unwrap_or(0);
                count == *n
            } else {
                false
            }
        }
        AbilityCondition::SourceLacksKeyword { keyword } => state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| !obj.has_keyword(keyword)),
        // CR 101.3 + CR 109.5 + CR 608.2c: per-iteration scoped-player filter.
        // The decline-tail body for a cross-scope decline clause (parent
        // iterates a wider set than the decline-clause `PlayerFilter`) fires
        // only when the iterated player matches the decline scope. Outside a
        // `player_scope` iteration `scoped_player` is `None` and we fall back
        // to the ability's controller — the canonical
        // `ScopedPlayer`/`Controller` fallback semantics.
        AbilityCondition::ScopedPlayerMatches { filter } => {
            let candidate = ability.scoped_player.unwrap_or(ability.controller);
            scoped_player_matches_filter(state, ability, candidate, filter)
        }
    }
}

/// CR 101.3 + CR 109.5: Evaluate a `PlayerFilter` against a single per-iteration
/// candidate player. Mirrors the per-candidate predicates in
/// `quantity::resolve_player_count`, but applied to one already-bound iteration
/// player rather than a fold over all players. Set-valued / event-context
/// variants that have no single-player semantic outside an iteration loop fail
/// closed — `ScopedPlayerMatches` is only emitted by the parser for `All` /
/// `Opponent` / `Controller` today (the canonical decline-tail scopes), so the
/// fall-through is defensive rather than load-bearing.
fn scoped_player_matches_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    candidate: PlayerId,
    filter: &PlayerFilter,
) -> bool {
    // CR 109.5: During a `player_scope` iteration the driver rebinds
    // `ability.controller = scoped_player` (effects/mod.rs:2884) so that
    // body effects whose leaf target is `Controller` resolve to the
    // iterated player. The decline-clause scope filter, however, evaluates
    // "Opponent of whom" relative to the PRINTED controller — the canonical
    // idiom used by `quantity.rs` (see `original_controller.unwrap_or(controller)`
    // at quantity.rs:479,514,530,721). Without this fallback, `Opponent`
    // here would evaluate `candidate != iterated_player` and short-circuit
    // to false for every per-iteration call, silently dropping cross-scope
    // bodies (e.g. Liliana, Waker of the Dead [+1]).
    let controller = ability.original_controller.unwrap_or(ability.controller);
    match filter {
        PlayerFilter::Controller => candidate == controller,
        PlayerFilter::Opponent => candidate != controller,
        PlayerFilter::All => true,
        // CR 608.2c + CR 109.4: candidate matches unless it matches the anchor.
        // Recursive against the same printed-controller-relative predicate;
        // ability-target anchors fall through to the fail-closed arm below.
        PlayerFilter::AllExcept { exclude } => {
            !scoped_player_matches_filter(state, ability, candidate, exclude)
        }
        PlayerFilter::OpponentLostLife => {
            candidate != controller
                && state
                    .players
                    .iter()
                    .find(|p| p.id == candidate)
                    .is_some_and(|p| p.life_lost_this_turn > 0)
        }
        PlayerFilter::OpponentGainedLife => {
            candidate != controller
                && state
                    .players
                    .iter()
                    .find(|p| p.id == candidate)
                    .is_some_and(|p| p.life_gained_this_turn > 0)
        }
        // Set-valued / event-context / aggregate variants: not used by
        // decline-tail today. Fail closed (mirrors the
        // `TriggerCondition::DuringPlayersTurn` fallthrough pattern at
        // game/triggers.rs:3703-3723).
        PlayerFilter::DefendingPlayer
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentDealtDamage { .. }
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::TriggeringPlayer
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ChosenPlayer { .. }
        | PlayerFilter::ParentObjectTargetOwner
        | PlayerFilter::ControlsCount { .. }
        | PlayerFilter::PlayerAttribute { .. } => false,
    }
}

fn event_outcome_was_won_by_controller(event: &GameEvent, controller: PlayerId) -> bool {
    match event {
        // CR 701.30d: reuse the shared controller-relative clash-outcome
        // resolution so resolution-time "if you won" gating and trigger-matching
        // (`match_clash`) agree on who won.
        GameEvent::Clash {
            controller: clash_controller,
            opponent,
            result,
            ..
        } => {
            result.for_player(*clash_controller, *opponent, controller)
                == Some(crate::types::events::ClashResult::Won)
        }
        GameEvent::CoinFlipped { player_id, won } => *player_id == controller && *won,
        _ => false,
    }
}

/// Resolve the payer for an unless-pay modifier from the trigger event context.
/// `TriggeringPlayer` resolves to the player involved in the triggering event
/// (e.g., the opponent who cast a spell for Esper Sentinel).
fn resolve_unless_payer(
    state: &GameState,
    ability: &ResolvedAbility,
    payer: &TargetFilter,
) -> Option<crate::types::player::PlayerId> {
    match payer {
        TargetFilter::TriggeringPlayer => {
            state
                .current_trigger_event
                .as_ref()
                .and_then(|event| match event {
                    GameEvent::SpellCast { controller, .. } => Some(*controller),
                    GameEvent::PlayerPerformedAction { player_id, .. } => Some(*player_id),
                    _ => None,
                })
                // CR 702.21a: Fall back to broader event-context resolution
                // (handles `BecomesTarget` for ward, etc.) when the narrow
                // matches above don't apply.
                .or_else(|| {
                    crate::game::targeting::resolve_event_context_target(
                        state,
                        payer,
                        ability.source_id,
                    )
                    .and_then(|target| match target {
                        TargetRef::Player(p) => Some(p),
                        _ => None,
                    })
                })
        }
        TargetFilter::Controller => Some(state.active_player),
        // CR 702.21a + CR 603.7c: Ward firing on `BecomesTarget` produces a
        // counter ability whose unless-payer is the controller of the
        // offending spell — resolved via the trigger event's source object.
        TargetFilter::TriggeringSpellController => {
            crate::game::targeting::resolve_event_context_target(state, payer, ability.source_id)
                .and_then(|target| match target {
                    TargetRef::Player(p) => Some(p),
                    _ => None,
                })
        }
        // CR 118.12 + CR 608.2c: "Counter target spell unless its controller
        // pays {X}" (Mana Leak) — `ParentTargetController` reads the
        // controller of the targeted spell (the parent ability's first
        // target), which `resolve_effect_player_ref` looks up via the stack.
        TargetFilter::ParentTargetController | TargetFilter::ParentTargetOwner => {
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        }
        // CR 118.12a: "[Target player] loses N life unless they ..." —
        // the paying player is the chosen player target on the ability
        // (Tergrid's Lantern and the broader "target player unless they X"
        // punisher class). Delegates to `resolve_effect_player_ref`'s
        // `TargetFilter::Player` arm which scans `ability.targets` for the
        // first `TargetRef::Player`.
        TargetFilter::Player => {
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        }
        // CR 118.12a + CR 608.2f: "Each player/each opponent ... unless they pay" —
        // the payer is the player_scope iteration's scoped player, not a chosen
        // target. resolve_effect_player_ref maps ScopedPlayer -> ability.scoped_player
        // (bound per-iteration by the fan-out at effects/mod.rs:3015-3069).
        TargetFilter::ScopedPlayer => {
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        }
        // CR 115.1 + CR 118.12a: a payer DECLARED as a target inside the unless
        // clause ("unless target opponent/target player pays") resolves to the
        // player chosen at stack placement, read from `ability.targets` (same as
        // the anaphoric `Player` arm's resolution). `payer_is_declared_target` is
        // the single authority for the declared-target shape (shared with slot
        // creation in `ability_utils` and the resolver arm in `targeting`), so it
        // stays mutually exclusive with the `ChosenPlayer` `Typed` arm there.
        _ if crate::game::ability_utils::payer_is_declared_target(payer) => {
            crate::game::targeting::resolve_effect_player_ref(state, ability, payer)
        }
        _ => None,
    }
}

/// CR 118.12a: Resolve an `UnlessPayModifier.payer` to the ordered list of
/// players who may pay the unless-cost. Single-payer variants yield a
/// one-element list (or empty when unresolvable); `TargetFilter::AllPlayers`
/// ("unless any player pays ...") yields every player in APNAP order for the
/// sequential poll — the first to pay prevents the effect.
fn resolve_unless_payers(
    state: &GameState,
    ability: &ResolvedAbility,
    payer: &TargetFilter,
) -> Vec<crate::types::player::PlayerId> {
    match payer {
        TargetFilter::AllPlayers => crate::game::players::apnap_order(state),
        _ => resolve_unless_payer(state, ability, payer)
            .into_iter()
            .collect(),
    }
}

/// CR 702.24a: Expand `pay [base] for each counter on it` into the
/// concrete N-fold cost the player actually pays. N=0 short-circuits to
/// a zero mana cost (CR 118.5 — players can always pay 0). `OneOf`
/// unfolds into a `Composite` of N independent disjunctive choices
/// (CR 702.24a: each choice is made separately).
fn expand_per_counter(base: &AbilityCost, n: u32) -> AbilityCost {
    if n == 0 {
        return AbilityCost::Mana {
            cost: ManaCost::zero(),
        };
    }
    match base {
        AbilityCost::Mana { cost } => AbilityCost::Mana {
            cost: cost.scaled(n),
        },
        AbilityCost::PayLife { amount } => AbilityCost::PayLife {
            amount: amount.scaled_by(n),
        },
        AbilityCost::Sacrifice(cost) => {
            let requirement = match &cost.requirement {
                SacrificeRequirement::Count { count } => {
                    SacrificeRequirement::count(count.saturating_mul(n))
                }
                req => req.clone(),
            };
            AbilityCost::Sacrifice(SacrificeCost {
                target: cost.target.clone(),
                requirement,
            })
        }
        AbilityCost::OneOf { costs } => AbilityCost::Composite {
            costs: vec![
                AbilityCost::OneOf {
                    costs: costs.clone()
                };
                n as usize
            ],
        },
        AbilityCost::Composite { costs } => AbilityCost::Composite {
            costs: costs.iter().map(|c| expand_per_counter(c, n)).collect(),
        },
        // CR 702.24a: "If [cost] has choices … each choice is made separately for
        // each age counter." Scale the discard count by the age-counter multiplier
        // so N age counters demand N discards; filter/random/self_ref are unchanged
        // per-counter axes.
        AbilityCost::Discard {
            count,
            filter,
            selection,
            self_scope,
        } => AbilityCost::Discard {
            count: count.scaled_by(n),
            filter: filter.clone(),
            selection: *selection,
            self_scope: *self_scope,
        },
        // CR 702.24a: Thought Lash-style cumulative upkeep scales the number
        // of top-library cards exiled by the number of age counters.
        AbilityCost::Exile {
            count,
            zone: Some(Zone::Library),
            filter: None,
        } => AbilityCost::Exile {
            count: count.saturating_mul(n),
            zone: Some(Zone::Library),
            filter: None,
        },
        // YAGNI fallback: no current cumulative-upkeep card uses these
        // base variants. If a future mechanic does, the
        // Composite-of-N-copies expansion is semantically correct for
        // most cost shapes; refactor per-variant if needed.
        other => AbilityCost::Composite {
            costs: vec![other.clone(); n as usize],
        },
    }
}

/// CR 601.2f: "The next spell you cast this turn costs {N} less to cast."
/// Pushes a one-shot cost reduction entry consumed when the player casts their next spell.
fn resolve_reduce_next_spell_cost(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (amount, spell_filter) = match &ability.effect {
        Effect::ReduceNextSpellCost {
            amount,
            spell_filter,
        } => (*amount, spell_filter.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "ReduceNextSpellCost".to_string(),
            ))
        }
    };
    state
        .pending_spell_cost_reductions
        .push(crate::types::game_state::PendingSpellCostReduction {
            player: ability.controller,
            amount,
            spell_filter,
        });
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::ReduceNextSpellCost,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 601.2f: Register a pending next-spell modifier (keyword grant, uncounterability, flash).
/// Consumed when the player casts their next qualifying spell.
fn resolve_grant_next_spell_ability(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (modifier, player_scope, spell_filter) = match &ability.effect {
        Effect::GrantNextSpellAbility {
            modifier,
            player,
            spell_filter,
        } => (modifier.clone(), player.clone(), spell_filter.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "GrantNextSpellAbility".to_string(),
            ))
        }
    };
    // CR 115.1: "they cast / that player casts" (PlayerScope::Target) = the
    // player this ability targets — the mana-clause recipient on Bigger on the
    // Inside, inherited onto this SequentialSibling via chain target
    // propagation. CR 109.5: every other scope = the effect's controller
    // ("the next spell you cast"). Exhaustive over PlayerScope (no `_`), so any
    // future variant forces a maintainer to re-confirm its next-spell semantics.
    let player = match player_scope {
        PlayerScope::Target => ability.target_player(),
        PlayerScope::Controller
        | PlayerScope::ScopedPlayer
        | PlayerScope::Opponent { .. }
        | PlayerScope::AllPlayers { .. }
        | PlayerScope::RecipientController
        | PlayerScope::DefendingPlayer
        | PlayerScope::ParentObjectTargetController
        | PlayerScope::SourceChosenPlayer => ability.controller,
    };
    state
        .pending_next_spell_modifiers
        .push(crate::types::game_state::PendingNextSpellModifier {
            player,
            modifier,
            spell_filter,
            source_id: Some(ability.source_id),
        });
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::GrantNextSpellAbility,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 614.1c: Register pending ETB counters for the triggering creature spell.
/// Reads `current_trigger_event` (SpellCast) to identify the object, then adds
/// counters to `pending_etb_counters` so they are applied when the object enters
/// the battlefield.
fn resolve_add_pending_etb_counters(
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    let (counter_type, count) = match &ability.effect {
        Effect::AddPendingETBCounters {
            counter_type,
            count,
        } => (counter_type.clone(), count.clone()),
        _ => {
            return Err(crate::types::ability::EffectError::MissingParam(
                "AddPendingETBCounters".to_string(),
            ))
        }
    };

    // Resolve the count using existing quantity infrastructure
    let resolved_count = crate::game::quantity::resolve_quantity(
        state,
        &count,
        ability.controller,
        ability.source_id,
    ) as u32;

    // Extract the object_id from the triggering SpellCast event
    let object_id = state.current_trigger_event.as_ref().and_then(|e| match e {
        GameEvent::SpellCast { object_id, .. } => Some(*object_id),
        _ => None,
    });

    if let Some(oid) = object_id {
        state
            .pending_etb_counters
            .push((oid, counter_type, resolved_count));
    } else {
        tracing::warn!(
            "AddPendingETBCounters: no SpellCast trigger event found — counters not registered"
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::AddPendingETBCounters,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 611.2a + CR 205.1b: `AddPendingEntersModifications` is consumed only as
/// `CastFromZone` permission metadata (lifted in `cast_from_zone::grant_lingering_permissions`,
/// applied as a `TransientContinuousEffect` at cast finalization). It never
/// resolves standalone — a standalone resolve means the rider was mis-lowered
/// outside a `CastFromZone` chain, so fail loudly (warn) and no-op rather than
/// silently dropping the type grant, mirroring `resolve_add_pending_etb_counters`.
fn resolve_add_pending_enters_modifications(
    _state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), crate::types::ability::EffectError> {
    tracing::warn!(
        "AddPendingEntersModifications resolved standalone — only valid as \
         CastFromZone permission metadata; no-op"
    );
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::AddPendingEntersModifications,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::synthesis::synthesize_extort;
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, AggregateFunction, BounceSelection,
        CardPredicateChoice, CastingPermission, ChoiceValue, Chooser, ChosenAttribute, Comparator,
        ContinuousModification, ControllerRef, DelayedTriggerCondition, Duration, EffectScope,
        FilterProp, ManaSpendPermission, ObjectProperty, PermissionGrantee, PlayerFilter,
        PlayerScope, PtValue, QuantityExpr, QuantityRef, SpellContext, StaticDefinition,
        TapStateChange, TargetFilter, TargetRef, TypeFilter, TypedFilter, UnlessPayModifier,
        UntilCondition, ZoneOwner,
    };
    use crate::types::actions::GameAction;
    use crate::types::card::CardFace;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        AutoMayChoice, CastingVariant, ExileLink, ExileLinkKind, LKISnapshot, LinkedExileSnapshot,
        MayTriggerAutoChoiceKey, MayTriggerOrigin, StackEntry, StackEntryKind,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use crate::types::player::{PlayerCounterKind, PlayerId};
    use crate::types::statics::CastFrequency;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// CR 608.2c: a single tapped creature becomes the resolution's anaphoric
    /// referent, so a later "that creature's power" (Enlist) reads it.
    #[test]
    fn effect_parent_ref_slots_surfaces_context_ref_unattach_all_attachment() {
        // `Effect::UnattachAll` hides its `attachment` behind `target_filter()`
        // (which surfaces `target`), exactly like `Effect::Attach`. A context-ref
        // attachment — a delayed "when you lose control of this, unattach it"
        // trigger (Stolen Uniform, Ogre Geargrabber) — must be surfaced so the
        // per-source rebind finds it. Without the `UnattachAll` arm in
        // `effect_parent_ref_slots` the `_ => {}` fallback drops it: this asserts
        // the arm surfaces it (revert the arm → SelfRef absent → this fails).
        let effect = Effect::UnattachAll {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::Any,
        };
        let slots = effect_parent_ref_slots(&effect);
        assert!(
            slots.iter().any(|s| matches!(s, TargetFilter::SelfRef)),
            "context-ref UnattachAll attachment must surface as a parent-ref slot; got {slots:?}"
        );
        // Guard: a non-context-ref attachment must NOT be surfaced by the arm.
        let non_ctx = Effect::UnattachAll {
            attachment: TargetFilter::Any,
            target: TargetFilter::SelfRef,
        };
        assert!(
            !effect_parent_ref_slots(&non_ctx)
                .iter()
                .any(|s| matches!(s, TargetFilter::Any)),
            "non-context-ref UnattachAll attachment must not be surfaced"
        );
    }

    #[test]
    fn tapped_creature_is_captured_as_anaphoric_referent() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tapped".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }
        let events = vec![GameEvent::PermanentTapped {
            object_id: creature,
            caused_by: None,
        }];
        let referent = parent_referent_context_from_events(&state, &events)
            .expect("a single tapped creature must be captured as the anaphoric referent");
        assert_eq!(referent.object_id, creature);
        assert_eq!(
            referent.lki.power,
            Some(3),
            "the referent snapshot carries the tapped creature's power"
        );
    }

    /// A multi-creature tap has no singular "that creature" (mirrors the
    /// sacrifice/move guards).
    #[test]
    fn multiple_tapped_creatures_yield_no_singular_referent() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        for id in [a, b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let events = vec![
            GameEvent::PermanentTapped {
                object_id: a,
                caused_by: None,
            },
            GameEvent::PermanentTapped {
                object_id: b,
                caused_by: None,
            },
        ];
        assert!(
            parent_referent_context_from_events(&state, &events).is_none(),
            "two tapped creatures have no singular anaphoric referent"
        );
    }

    /// CR 707.10 + CR 608.2c: a spell copy put onto the stack can be an
    /// anaphoric referent only when the parent resolution produced exactly one
    /// copied stack object.
    #[test]
    fn stack_pushed_parent_referent_requires_singular_copy() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First Copy".to_string(),
            Zone::Stack,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second Copy".to_string(),
            Zone::Stack,
        );
        let single = [GameEvent::StackPushed { object_id: first }];
        let multiple = [
            GameEvent::StackPushed { object_id: first },
            GameEvent::StackPushed { object_id: second },
        ];

        assert_eq!(
            parent_referent_context_from_events(&state, &single).map(|snapshot| snapshot.object_id),
            Some(first),
            "one copied spell can feed ParentTarget"
        );
        assert!(
            parent_referent_context_from_events(&state, &multiple).is_none(),
            "multiple copied spells must not bind ParentTarget arbitrarily"
        );
    }

    /// CR 608.2c: duplicate events for the same tapped creature still describe
    /// one singular referent.
    #[test]
    fn duplicate_tap_events_for_same_creature_still_capture_single_referent() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tapped".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
            obj.power = Some(4);
            obj.toughness = Some(4);
        }
        let events = vec![
            GameEvent::PermanentTapped {
                object_id: creature,
                caused_by: None,
            },
            GameEvent::PermanentTapped {
                object_id: creature,
                caused_by: None,
            },
        ];
        let referent = parent_referent_context_from_events(&state, &events)
            .expect("duplicate tap events for one creature still have a singular referent");
        assert_eq!(referent.object_id, creature);
        assert_eq!(referent.lki.power, Some(4));
    }

    #[test]
    fn is_known_effect_rejects_unimplemented() {
        let known = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        };
        assert!(is_known_effect(&known));

        let unknown = Effect::Unimplemented {
            name: "Fateseal".to_string(),
            description: None,
        };
        assert!(!is_known_effect(&unknown));

        // RuntimeHandled is a known effect — it's handled by a dedicated engine path
        let runtime = Effect::RuntimeHandled {
            handler: crate::types::ability::RuntimeHandler::NinjutsuFamily,
        };
        assert!(is_known_effect(&runtime));
    }

    /// CR 508.6: "each player this creature attacked this turn" must bind to
    /// the source creature's own attacked-defender ledger, not the controller's
    /// aggregate "you attacked" set.
    #[test]
    fn source_attacked_this_turn_player_filter_is_per_creature() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let angel = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Angel of Destiny".to_string(),
            Zone::Battlefield,
        );
        let other_attacker = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Other Attacker".to_string(),
            Zone::Battlefield,
        );

        state
            .attacked_defenders_this_turn
            .entry(PlayerId(0))
            .or_default()
            .extend([PlayerId(1), PlayerId(2)]);
        state
            .creature_attacked_defenders_this_turn
            .entry(angel)
            .or_default()
            .insert(PlayerId(1));
        state
            .creature_attacked_defenders_this_turn
            .entry(other_attacker)
            .or_default()
            .insert(PlayerId(2));

        assert!(
            matches_player_scope(
                &state,
                PlayerId(2),
                &PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
                PlayerId(0),
                angel,
            ),
            "the controller aggregate should include every player any creature attacked",
        );
        assert!(
            !matches_player_scope(
                &state,
                PlayerId(2),
                &PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::Source,
                    scope: AttackScope::ThisTurn,
                },
                PlayerId(0),
                angel,
            ),
            "Angel must not affect a player attacked only by a different creature",
        );
        assert!(
            matches_player_scope(
                &state,
                PlayerId(1),
                &PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::Source,
                    scope: AttackScope::ThisTurn,
                },
                PlayerId(0),
                angel,
            ),
            "Angel must still affect the player it attacked",
        );
    }

    #[test]
    fn source_chosen_player_reads_live_source_then_lki() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Choice Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Player(PlayerId(1)));
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SourceChosenPlayer,
                damage_source: None,
                excess: None,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        assert_eq!(
            crate::game::game_object::source_chosen_player(&state, ability.source_id),
            Some(PlayerId(1))
        );

        state.objects.remove(&source);
        state.lki_cache.insert(
            source,
            LKISnapshot {
                name: "Choice Source".to_string(),
                power: None,
                toughness: None,
                base_power: None,
                base_toughness: None,
                mana_value: 4,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Artifact],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: vec![ChosenAttribute::Player(PlayerId(1))],
                counters: HashMap::new(),
                tapped: false,
                is_suspected: false,
            },
        );

        assert_eq!(
            crate::game::game_object::source_chosen_player(&state, ability.source_id),
            Some(PlayerId(1))
        );
    }

    /// CR 119.3 + CR 115.10: `effect_has_iteration_bound_recipient` must
    /// classify a `LoseLife` whose recipient is `ScopedPlayer` /
    /// `OriginalController` as iteration-bound — exactly like `Draw` / `Token`.
    /// This keeps the `LoseLife→Draw` decline-consequence edge inside the
    /// `player_scope` iteration (issue #491, Step 1b).
    #[test]
    fn lose_life_iteration_bound_recipient_classification() {
        let lose_scoped = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: Some(TargetFilter::ScopedPlayer),
        };
        assert!(
            effect_has_iteration_bound_recipient(&lose_scoped),
            "LoseLife → ScopedPlayer is iteration-bound"
        );

        let lose_original = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: Some(TargetFilter::OriginalController),
        };
        assert!(
            effect_has_iteration_bound_recipient(&lose_original),
            "LoseLife → OriginalController is iteration-bound"
        );

        let lose_none = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        };
        assert!(
            !effect_has_iteration_bound_recipient(&lose_none),
            "an undirected LoseLife (target: None) is NOT iteration-bound"
        );

        let lose_non_iteration = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: Some(TargetFilter::TriggeringPlayer),
        };
        assert!(
            !effect_has_iteration_bound_recipient(&lose_non_iteration),
            "LoseLife → a non-iteration filter is NOT iteration-bound"
        );

        // Regression: the existing Draw / Token arms are unchanged.
        let draw_scoped = Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ScopedPlayer,
        };
        assert!(effect_has_iteration_bound_recipient(&draw_scoped));
        let token_original = Effect::Token {
            name: "Treasure".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec!["Artifact".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::OriginalController,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };
        assert!(effect_has_iteration_bound_recipient(&token_original));
        // A non-recipient effect remains unclassified.
        let damage = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        };
        assert!(!effect_has_iteration_bound_recipient(&damage));
    }

    #[test]
    fn resolve_effect_returns_ok_for_unimplemented() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "NonExistentEffect".to_string(),
                description: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_effect(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    /// Telemetry `game_summary` surface: resolving an `Effect::Unimplemented`
    /// records the source card's oracle id in `unimplemented_oracle_ids`
    /// (name fallback for tokens/emblems with no `printed_ref`), and dedups on
    /// repeated resolution. Diagnostics plumbing only — no rules behavior.
    #[test]
    fn unimplemented_effect_records_source_in_game_summary() {
        let mut state = GameState::new_two_player(42);

        // Card object with a printed oracle id.
        let mut card = crate::game::game_object::GameObject::new(
            ObjectId(7),
            CardId(700),
            PlayerId(0),
            "Fancy Card".to_string(),
            Zone::Stack,
        );
        card.printed_ref = Some(crate::types::card::PrintedCardRef {
            oracle_id: "oracle-abc-123".to_string(),
            face_name: "Fancy Card".to_string(),
        });
        state.objects.insert(ObjectId(7), card);

        // Token object with no printed_ref — must fall back to the name.
        let token = crate::game::game_object::GameObject::new(
            ObjectId(8),
            CardId(0),
            PlayerId(0),
            "Goblin Token".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(8), token);

        let unimplemented = |source: ObjectId| {
            ResolvedAbility::new(
                Effect::unimplemented("SomeEffect", "some fragment"),
                vec![],
                source,
                PlayerId(0),
            )
        };
        let mut events = Vec::new();

        // Oracle-id path: resolves and records the oracle id.
        resolve_effect(&mut state, &unimplemented(ObjectId(7)), &mut events).unwrap();
        assert!(state.unimplemented_oracle_ids.contains("oracle-abc-123"));

        // Dedup: a second resolution of the same source adds no new entry.
        resolve_effect(&mut state, &unimplemented(ObjectId(7)), &mut events).unwrap();
        assert_eq!(state.unimplemented_oracle_ids.len(), 1);

        // Name-fallback path: a token with no printed_ref records its name.
        resolve_effect(&mut state, &unimplemented(ObjectId(8)), &mut events).unwrap();
        assert!(state.unimplemented_oracle_ids.contains("Goblin Token"));
        assert_eq!(state.unimplemented_oracle_ids.len(), 2);
    }

    fn optional_gain_life(
        source_id: ObjectId,
        controller: PlayerId,
        amount: i32,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        ability.optional = true;
        ability
    }

    #[test]
    fn repeat_for_tracked_set_marks_ability_as_referencing_tracked_set() {
        // Issue #740 (Seasoned Pyromancer): "for each nonland card discarded this
        // way, create a token" carries its loop count as `repeat_for: TrackedSetSize`
        // on the ResolvedAbility, NOT inside Effect. The publish predicate must
        // detect it so the forced-discard path (no WaitingFor pause) still publishes
        // the tracked set; without this check the token loop sees size 0.
        let mut ability = optional_gain_life(ObjectId(1), PlayerId(0), 1);
        ability.optional = false;
        // Control: a plain GainLife with no `repeat_for` references no tracked set.
        assert!(
            !ability_or_branch_references_tracked_set(&ability),
            "baseline ability must not reference a tracked set"
        );
        // With a tracked-set loop count, the predicate must return true.
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });
        assert!(
            ability_or_branch_references_tracked_set(&ability),
            "repeat_for: TrackedSetSize must mark the ability as referencing the tracked set"
        );
    }

    /// CR 118.1 + CR 603.12a: the repeated-optional-payment driver pays each
    /// iteration in line and reads the failure flag immediately, so it must admit
    /// ONLY a pure, static mana `PayCost` (Hawkeye's `{1}`). A PayCost whose cost
    /// PAUSES — unannounced-`{X}` mana (`WaitingFor::PayAmountChoice`) or an
    /// interactive `Discard` (`PaymentOutcome::Paused`) — or a per-object `scale`
    /// must fall through to the generic `repeat_for` path and stay honestly
    /// unimplemented; driving it would mis-count K and clobber the payment's own
    /// continuation.
    ///
    /// Revert discriminator: widening `is_synchronous_mana_pay_cost` back to a
    /// bare `matches!(effect, Effect::PayCost { .. })` makes the X-mana, Discard,
    /// and scaled-mana fixtures satisfy the predicate ⇒ the `!...` assertions fail.
    #[test]
    fn repeated_optional_payment_admits_only_synchronous_mana_cost() {
        use crate::types::ability::{CardSelectionMode, DiscardSelfScope};
        use crate::types::mana::ManaCostShard;

        fn repeated_optional_with_cost(
            cost: AbilityCost,
            scale: Option<QuantityExpr>,
        ) -> ResolvedAbility {
            let mut ability = ResolvedAbility::new(
                Effect::PayCost {
                    cost,
                    scale,
                    payer: TargetFilter::Controller,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            ability.optional = true;
            ability.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
            let mut reflexive = ResolvedAbility::new(
                Effect::GenericEffect {
                    static_abilities: vec![],
                    duration: None,
                    target: None,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            reflexive.condition = Some(AbilityCondition::WhenYouDo);
            ability.sub_ability = Some(Box::new(reflexive));
            ability
        }

        // Positive control: Hawkeye's `{1}` — pure static mana, synchronous.
        let mana = repeated_optional_with_cost(
            AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            None,
        );
        assert!(
            is_repeated_optional_payment(&mana),
            "pure {{1}} mana is the synchronous repeated-optional class"
        );

        // Negative: unannounced `{X}` mana pauses via `WaitingFor::PayAmountChoice`.
        let x_mana = repeated_optional_with_cost(
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::X],
                    generic: 0,
                },
            },
            None,
        );
        assert!(
            !is_repeated_optional_payment(&x_mana),
            "{{X}} mana prompts PayAmountChoice — must NOT be driven"
        );

        // Negative: interactive Discard pauses via `PaymentOutcome::Paused` — the
        // exact "you may discard a card up to N times" class the driver cannot run.
        let discard = repeated_optional_with_cost(
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::default(),
                self_scope: DiscardSelfScope::default(),
            },
            None,
        );
        assert!(
            !is_repeated_optional_payment(&discard),
            "interactive Discard pauses — must NOT be driven"
        );

        // Negative: a per-object `scale` is the resolution-only multiplier shape,
        // not the repeated-optional shape.
        let scaled = repeated_optional_with_cost(
            AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            Some(QuantityExpr::Fixed { value: 2 }),
        );
        assert!(
            !is_repeated_optional_payment(&scaled),
            "scaled mana is not the synchronous repeated-optional class"
        );
    }

    #[test]
    fn optional_trigger_prompt_includes_may_trigger_key() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 1);
        ability.set_may_trigger_origin_recursive(MayTriggerOrigin::Printed { trigger_index: 2 });

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        match state.waiting_for {
            WaitingFor::OptionalEffectChoice {
                may_trigger_key: Some(key),
                ..
            } => {
                assert_eq!(key.player, PlayerId(0));
                assert_eq!(key.source_id, source_id);
                assert_eq!(key.origin, MayTriggerOrigin::Printed { trigger_index: 2 });
            }
            other => panic!("expected keyed OptionalEffectChoice, got {other:?}"),
        }
    }

    #[test]
    fn optional_zone_choice_after_mill_can_return_milled_land() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Milled Land".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Milled Spell A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Milled Spell B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut return_land = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        return_land.optional = true;
        return_land.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
        ability.sub_ability = Some(Box::new(return_land));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        assert_eq!(state.players[0].graveyard.len(), 3);

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        let land = state.objects.get(&land_id).unwrap();
        assert_eq!(land.zone, Zone::Battlefield);
        assert!(land.tapped);
    }

    #[test]
    fn optional_triggering_source_zone_move_survives_optional_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tergrid".to_string(),
            Zone::Battlefield,
        );
        let victim_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sacrificed Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&victim_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.current_trigger_event = Some(crate::types::events::GameEvent::PermanentSacrificed {
            object_id: victim_id,
            player_id: PlayerId(1),
        });

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        state.current_trigger_event = None;

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        let victim = state.objects.get(&victim_id).unwrap();
        assert_eq!(victim.zone, Zone::Battlefield);
        assert_eq!(victim.controller, PlayerId(0));
        assert!(!state.players[1].graveyard.contains(&victim_id));
    }

    #[test]
    fn tergrid_style_sacrifice_trigger_reanimates_sacrificed_permanent() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tergrid".to_string(),
            Zone::Battlefield,
        );
        let victim_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sacrificed Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut trigger = crate::types::ability::TriggerDefinition::new(
            crate::types::triggers::TriggerMode::Sacrificed,
        )
        .optional();
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::Opponent),
            properties: vec![FilterProp::NonToken],
        }));
        trigger.execute = Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::TriggeringSource,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
            .optional(),
        ));
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let mut events = Vec::new();
        crate::game::sacrifice::sacrifice_permanent(
            &mut state,
            victim_id,
            PlayerId(1),
            &mut events,
        )
        .unwrap();
        crate::game::triggers::process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1);

        let mut resolution_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut resolution_events);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut resolution_events,
        )
        .unwrap();

        let victim = state.objects.get(&victim_id).unwrap();
        assert_eq!(victim.zone, Zone::Battlefield);
        assert_eq!(victim.controller, PlayerId(0));
        assert!(!state.players[1].graveyard.contains(&victim_id));
    }

    #[test]
    fn tergrid_style_trigger_reanimates_permanent_selected_for_sacrifice_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tergrid".to_string(),
            Zone::Battlefield,
        );
        let victim_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Chosen Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut trigger = crate::types::ability::TriggerDefinition::new(
            crate::types::triggers::TriggerMode::Sacrificed,
        )
        .optional();
        trigger.valid_card = Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::Opponent),
            properties: vec![FilterProp::NonToken],
        }));
        trigger.execute = Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::TriggeringSource,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
            .optional(),
        ));
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(1),
            cards: vec![victim_id],
            count: 1,
            min_count: 0,
            up_to: false,
            source_id: ObjectId(99),
            effect_kind: EffectKind::Sacrifice,
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
            enters_modified_if: None,
        };

        crate::game::engine::apply(
            &mut state,
            PlayerId(1),
            crate::types::actions::GameAction::SelectCards {
                cards: vec![victim_id],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);
        assert!(state.players[1].graveyard.contains(&victim_id));

        let mut resolution_events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut resolution_events);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        let accept_result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let victim = state.objects.get(&victim_id).unwrap();
        assert_eq!(victim.zone, Zone::Battlefield);
        assert_eq!(victim.controller, PlayerId(0));
        assert!(!state.players[1].graveyard.contains(&victim_id));

        let event_record = accept_result
            .events
            .iter()
            .find_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    record,
                    ..
                } if *object_id == victim_id => Some(record),
                _ => None,
            })
            .expect("Tergrid return should emit a battlefield-entry ZoneChanged event");
        assert_eq!(event_record.controller, PlayerId(0));
        assert_eq!(
            state
                .zone_changes_this_turn
                .iter()
                .rev()
                .find(|record| {
                    record.object_id == victim_id && record.to_zone == Zone::Battlefield
                })
                .map(|record| record.controller),
            Some(PlayerId(0))
        );
        assert_eq!(
            state
                .battlefield_entries_this_turn
                .iter()
                .rev()
                .find(|record| record.object_id == victim_id)
                .map(|record| record.controller),
            Some(PlayerId(0))
        );
    }

    #[test]
    fn saved_accept_for_may_trigger_resolves_without_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id,
            origin,
        };
        state.set_may_trigger_auto_choice(key, AutoMayChoice::Accept);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn saved_decline_for_may_trigger_resolves_without_prompt() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id,
            origin,
        };
        state.set_may_trigger_auto_choice(key, AutoMayChoice::Decline);
        let mut ability = optional_gain_life(source_id, PlayerId(0), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn saved_may_trigger_choice_is_scoped_to_prompt_player() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id,
                origin,
            },
            AutoMayChoice::Accept,
        );
        let mut ability = optional_gain_life(source_id, PlayerId(1), 3);
        ability.set_may_trigger_origin_recursive(origin);

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0).unwrap();

        match state.waiting_for {
            WaitingFor::OptionalEffectChoice {
                may_trigger_key: Some(key),
                ..
            } => assert_eq!(key.player, PlayerId(1)),
            other => panic!("expected prompt for player 1, got {other:?}"),
        }
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn may_trigger_origin_stamps_optional_sub_abilities() {
        let source_id = ObjectId(100);
        let origin = MayTriggerOrigin::Printed { trigger_index: 4 };
        let mut root = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        root.sub_ability = Some(Box::new(optional_gain_life(source_id, PlayerId(0), 1)));
        root.set_may_trigger_origin_recursive(origin);

        assert_eq!(root.may_trigger_origin, Some(origin));
        assert_eq!(
            root.sub_ability.as_ref().unwrap().may_trigger_origin,
            Some(origin)
        );
    }

    #[test]
    fn resolve_unless_payer_uses_player_action_event_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: crate::types::events::PlayerActionKind::SearchedLibrary,
        });
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        assert_eq!(
            resolve_unless_payer(&state, &ability, &TargetFilter::TriggeringPlayer),
            Some(PlayerId(1))
        );
    }

    #[test]
    fn resolve_unless_payer_uses_parent_target_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::ParentTargetController),
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        assert_eq!(
            resolve_unless_payer(&state, &ability, &TargetFilter::ParentTargetController),
            Some(PlayerId(1))
        );
    }

    // CR 118.12a + CR 608.2f: "each opponent ... unless they pay" — the payer
    // is the per-iteration scoped player bound by the fan-out, read through
    // `ScopedPlayer`, not a chosen target. Without this arm the resolver
    // returns `None` and the punisher fires unconditionally.
    #[test]
    fn resolve_unless_payer_scoped_player_reads_ability_scoped_player() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: Some(TargetFilter::ScopedPlayer),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // Simulate the fan-out binding the scoped opponent for this iteration.
        ability.scoped_player = Some(PlayerId(1));
        assert_eq!(
            resolve_unless_payer(&state, &ability, &TargetFilter::ScopedPlayer),
            Some(PlayerId(1))
        );
    }

    #[test]
    fn expand_per_counter_zero_returns_zero_mana() {
        let base = AbilityCost::Mana {
            cost: ManaCost::generic(5),
        };
        let expanded = expand_per_counter(&base, 0);
        assert!(matches!(expanded, AbilityCost::Mana { cost } if cost == ManaCost::zero()));
    }

    #[test]
    fn expand_per_counter_mana_scales() {
        let base = AbilityCost::Mana {
            cost: ManaCost::generic(2),
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        } = expanded
        else {
            panic!("expected Mana");
        };
        assert_eq!(generic, 6);
    }

    #[test]
    fn expand_per_counter_pay_life_multiplies() {
        let base = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::PayLife { amount } = expanded else {
            panic!("expected PayLife");
        };
        assert_eq!(amount, QuantityExpr::Fixed { value: 6 });
    }

    #[test]
    fn expand_per_counter_sacrifice_multiplies_count() {
        let base = AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1));
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Sacrifice(cost) = expanded else {
            panic!("expected Sacrifice");
        };
        assert_eq!(cost.requirement.fixed_count(), Some(3));
    }

    #[test]
    fn expand_per_counter_discard_scales_count() {
        // CR 702.24a: N age counters demand N discards; filter/random/self_ref
        // are unchanged per-counter axes.
        let base = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: Some(TargetFilter::SelfRef),
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Discard {
            count,
            filter,
            selection,
            self_scope,
        } = expanded
        else {
            panic!("expected Discard");
        };
        assert_eq!(count, QuantityExpr::Fixed { value: 3 });
        assert_eq!(filter, Some(TargetFilter::SelfRef));
        assert!(selection.is_chosen());
        assert!(self_scope.is_source_card());
    }

    #[test]
    fn expand_per_counter_top_library_exile_scales_count() {
        let base = AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Library),
            filter: None,
        };

        let expanded = expand_per_counter(&base, 3);

        assert_eq!(
            expanded,
            AbilityCost::Exile {
                count: 3,
                zone: Some(Zone::Library),
                filter: None,
            }
        );
    }

    #[test]
    fn expand_per_counter_one_of_unfolds_to_composite_of_one_ofs() {
        let base = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
            ],
        };
        let expanded = expand_per_counter(&base, 3);
        let AbilityCost::Composite { costs } = expanded else {
            panic!("expected Composite");
        };
        assert_eq!(costs.len(), 3);
        for c in &costs {
            match c {
                AbilityCost::OneOf { costs: inner } => {
                    assert_eq!(inner.len(), 2, "each OneOf must preserve both alternatives");
                    assert!(inner.iter().all(|sub| matches!(
                        sub,
                        AbilityCost::Mana {
                            cost: ManaCost::Cost { generic: 1, shards }
                        } if shards.is_empty()
                    )));
                }
                other => panic!("expected OneOf, got {other:?}"),
            }
        }
    }

    #[test]
    fn expand_per_counter_composite_recurses() {
        let base = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        };
        let expanded = expand_per_counter(&base, 2);
        let AbilityCost::Composite { costs } = expanded else {
            panic!("expected Composite");
        };
        assert_eq!(costs.len(), 2);
        assert!(matches!(
            costs[0],
            AbilityCost::Mana {
                cost: ManaCost::Cost { generic: 2, .. }
            }
        ));
        assert!(matches!(
            costs[1],
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        ));
    }

    /// CR 118.12a + CR 603.2: Triggered unless-costs on event-context player
    /// refs must hydrate targets before payer resolution (issue #2361).
    #[test]
    fn unless_pay_triggering_player_hydrates_before_prompt() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Esper Sentinel".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::TriggeringPlayer,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            payer: TargetFilter::TriggeringPlayer,
        });
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(99),
            controller: PlayerId(1),
            object_id: ObjectId(99),
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("unless-pay interceptor should arm a payment prompt");

        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, .. } => {
                assert_eq!(*player, PlayerId(1));
            }
            other => panic!("expected UnlessPayment for triggering player, got {other:?}"),
        }
    }

    /// CR 118.12a + CR 701.9: Wrench Mind — "unless they discard an artifact
    /// card" must surface `UnlessPayment` before the two-card discard (issue
    /// #2361).
    #[test]
    fn unless_pay_wrench_mind_discard_artifact_prompts_payment() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wrench Mind".to_string(),
            Zone::Stack,
        );
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("Hand{i}"),
                Zone::Hand,
            );
        }
        let artifact_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            ..Default::default()
        });
        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(artifact_filter),
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            },
            payer: TargetFilter::Player,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("unless-pay interceptor should arm a payment prompt");

        assert!(
            matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
            "expected UnlessPayment prompt, got {:?}",
            state.waiting_for
        );
    }

    /// CR 107.3a: a bare `{X}` unless-cost (`ManaDynamic` with `Variable("X")`)
    /// must resolve against the carrying ability's announced `chosen_x`. The
    /// interceptor folds the dynamic cost into a fixed `Mana` cost before
    /// arming `WaitingFor::UnlessPayment` — without threading `chosen_x` the
    /// value resolves to 0 and the unless prompt is wrongly skipped (CR 118.5).
    #[test]
    fn unless_pay_bare_x_threads_chosen_x_into_resolved_cost() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        ability.chosen_x = Some(3);
        ability.unless_pay = Some(crate::types::ability::UnlessPayModifier {
            cost: AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            payer: TargetFilter::ParentTargetController,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("unless-pay interceptor should arm a payment prompt");

        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(
                    *cost,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(3),
                    }
                );
            }
            other => panic!("expected WaitingFor::UnlessPayment, got {other:?}"),
        }
    }

    /// CR 702.24a + CR 702.24b: `AbilityCost::PerCounter` at the unless-payment
    /// entry point reads the current counter total on the trigger source and
    /// expands the base cost N-fold. With 3 age counters on the source and a
    /// base of `{2}`, the player is prompted to pay `{6}`.
    #[test]
    fn unless_pay_per_counter_expands_against_source_counter_total() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        // CR 702.24b: source carries 3 age counters at upkeep resolution.
        state
            .objects
            .get_mut(&source)
            .expect("source object exists")
            .counters
            .insert(CounterType::Age, 3);

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![TargetRef::Object(source)],
            source,
            PlayerId(0),
        );
        ability.unless_pay = Some(crate::types::ability::UnlessPayModifier {
            cost: AbilityCost::PerCounter {
                counter: CounterType::Age,
                target: TargetFilter::SelfRef,
                base: Box::new(AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                }),
            },
            payer: TargetFilter::Controller,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("unless-pay interceptor should arm a payment prompt");

        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(
                    *cost,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(6),
                    }
                );
            }
            other => panic!("expected WaitingFor::UnlessPayment, got {other:?}"),
        }
    }

    /// CR 118.12a: an "unless any player pays" effect (`TargetFilter::AllPlayers`
    /// payer) arms `WaitingFor::UnlessPayment` for the first player in APNAP
    /// order with every other player queued in `remaining` for the poll.
    #[test]
    fn unless_pay_any_player_arms_apnap_poll() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.unless_pay = Some(crate::types::ability::UnlessPayModifier {
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(2),
            },
            payer: TargetFilter::AllPlayers,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("unless-pay interceptor should arm the poll");

        match &state.waiting_for {
            WaitingFor::UnlessPayment {
                player, remaining, ..
            } => {
                // APNAP order from active player 0 → [P0, P1]: P0 prompted now,
                // P1 queued.
                assert_eq!(*player, PlayerId(0));
                assert_eq!(remaining, &vec![PlayerId(1)]);
            }
            other => panic!("expected WaitingFor::UnlessPayment, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ability_chain_single_effect() {
        let mut state = GameState::new_two_player(42);
        // Add a card in library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_with_typed_sub_ability() {
        let mut state = GameState::new_two_player(42);
        // Add cards to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        // Build a chain: DealDamage -> Draw using typed sub_ability
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        // Damage dealt to player 1
        assert_eq!(state.players[1].life, 18);
        // Controller drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    /// CR 608.2c: General building block for the Omnath, Locus of All decline
    /// shape — "you may X. If you do, A and B. If you don't, put IT into hand."
    /// The decline clause C is a `Not(OptionalEffectPerformed)` GRANDCHILD of the
    /// `IfYouDo` head (the accept body A;B is a two-instruction chain, so C is
    /// appended below them). On DECLINE only C must run; the accept-only
    /// instructions A and B must NOT. C moves the PARENT'S target object to hand
    /// via `ParentTarget` (exactly Omnath's "put it into your hand"), so this
    /// fixture is discriminating for BOTH failure modes: a finder-only fix runs
    /// the unconditioned B on decline (life changes), and a fix that resolves C
    /// detached WITHOUT inheriting the parent's targets leaves `ParentTarget`
    /// empty so the card never moves. Drives the real
    /// `resolve_optional_effect_decision` routing function.
    #[test]
    fn optional_decline_runs_only_nested_decline_clause_not_accept_body() {
        fn build_chain(card: ObjectId) -> ResolvedAbility {
            // C: decline clause — move the parent's target object to hand, gated
            // Not(IfYouDo). `ParentTarget` must resolve against the inherited
            // parent target (the card), as Omnath's "if you don't, put it into
            // your hand" does.
            let decline = ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Hand,
                    target: TargetFilter::ParentTarget,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
            .condition(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::EffectOutcome {
                    signal: EffectOutcomeSignal::OptionalEffectPerformed,
                }),
            });
            // B: second accept-body instruction — unconditioned GainLife(+3).
            let accept_b = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(decline);
            // A: IfYouDo head — GainLife(+2), gated OptionalEffectPerformed.
            let accept_a = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
            .condition(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed,
            })
            .sub_ability(accept_b);
            // Parent optional ability ("you may X") targeting the looked-at card;
            // neutral GainLife(0) effect.
            ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 0 },
                    player: TargetFilter::Controller,
                },
                vec![TargetRef::Object(card)],
                ObjectId(100),
                PlayerId(0),
            )
            .sub_ability(accept_a)
        }

        fn fresh_state() -> (GameState, ObjectId) {
            let mut state = GameState::new_two_player(42);
            let card = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Card".to_string(),
                Zone::Library,
            );
            (state, card)
        }

        // DECLINE: only C runs — the parent-target card moves to hand, life
        // UNCHANGED (A and B skipped). Card-in-hand proves C resolved AND bound
        // its `ParentTarget` to the inherited parent target; life-unchanged
        // proves the accept-only instructions did not run.
        {
            let (mut state, card) = fresh_state();
            let start_life = state.players[0].life;
            let mut events = Vec::new();
            resolve_optional_effect_decision(
                &mut state,
                build_chain(card),
                AutoMayChoice::Decline,
                &mut events,
                0,
            )
            .expect("decline resolves");
            assert!(
                state.players[0].hand.contains(&card),
                "decline runs C and binds ParentTarget (card moved to hand)"
            );
            assert_eq!(
                state.players[0].life, start_life,
                "decline must NOT run accept-only A/B (life unchanged)"
            );
        }

        // ACCEPT: A and B run (life +5), C skipped (card stays in library).
        {
            let (mut state, card) = fresh_state();
            let start_life = state.players[0].life;
            let mut events = Vec::new();
            resolve_optional_effect_decision(
                &mut state,
                build_chain(card),
                AutoMayChoice::Accept,
                &mut events,
                0,
            )
            .expect("accept resolves");
            assert_eq!(
                state.players[0].life,
                start_life + 5,
                "accept runs A(+2) and B(+3)"
            );
            assert!(
                !state.players[0].hand.contains(&card),
                "accept skips decline clause C (card stays in library)"
            );
        }
    }

    #[test]
    fn resolve_ability_chain_impossible_choice_does_not_wedge_chain() {
        // CR 609.3 (issue #3040): a `Choose` whose engine-enumerated option set
        // is empty is an impossible choice. It must resolve as a no-op so the
        // rest of the chain continues, instead of emitting an unsatisfiable
        // `WaitingFor::NamedChoice` that no `ChooseOption` can advance — which
        // would stash the dependent sub-ability forever and hang the game.
        use crate::types::ability::ChoiceType;

        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        // Empty `Keyword` option list → "choose an ability the target has" with
        // nothing to choose. The dependent Draw must still resolve.
        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::Keyword {
                    options: vec![],
                    count: 1,
                },
                persist: false,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(draw);
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "impossible choice must not leave the chain wedged on an empty NamedChoice"
        );
        // The dependent sub-ability resolved inline (no choice paused the chain).
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "the chain must continue past an impossible choice and draw the card"
        );
    }

    /// Regression (issue #1977, Party Thrasher): "you may discard a card. If you
    /// do, exile the top two cards of your library, then choose one of them."
    /// CR 608.2c + CR 603.7: the discard is a gating action behind the
    /// `If you do` boundary; its discarded card must NOT unify into the tracked
    /// set the rider's `ChooseFromZone` consumes. The choice must offer exactly
    /// the two exiled cards — never three (the discard plus the two exiled).
    #[test]
    fn if_you_do_gate_resets_tracked_set_so_choice_excludes_gating_discard() {
        let mut state = GameState::new_two_player(42);
        // One card in hand — discarded by the gating action (lands in graveyard).
        let discarded = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Discarded".to_string(),
            Zone::Hand,
        );
        // Two cards on top of library — exiled by the rider.
        let top_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Top A".to_string(),
            Zone::Library,
        );
        let top_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Top B".to_string(),
            Zone::Library,
        );

        // Rider: exile the top two cards, then choose one of them. The real
        // Party Thrasher chain continues from this choice into
        // GrantCastingPermission; the polluted tracked set first becomes
        // user-visible at this choice boundary.
        let choose = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let exile_top = ResolvedAbility::new(
            Effect::ExileTop {
                count: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
                face_down: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(choose)
        // The "If you do" boundary between the gating discard and the rider.
        .condition(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });

        // Gating action: discard the specific hand card (non-interactive path).
        let mut discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Object(discarded)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(exile_top);
        // "you may discard … If you do" — seed the performed flag so the gate passes.
        discard.context.optional_effect_performed = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &discard, &mut events, 0).unwrap();

        // The discarded card is in the graveyard; the two exiled cards are in exile.
        assert!(state.players[0].graveyard.contains(&discarded));
        assert!(state.exile.contains(&top_a) && state.exile.contains(&top_b));

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(
                    cards.len(),
                    2,
                    "choice must offer exactly the two exiled cards, not the discarded one too"
                );
                assert!(cards.contains(&top_a) && cards.contains(&top_b));
                assert!(
                    !cards.contains(&discarded),
                    "the gating discard must not be offered as a choice"
                );
            }
            other => panic!("Expected ChooseFromZoneChoice, got {other:?}"),
        }
    }

    /// CR 115.1d: With inherited object targets, `ParentTargetController` must
    /// not resolve to the trigger-event source's controller (issue #935).
    #[test]
    fn parent_target_controller_prefers_inherited_targets_over_trigger_source() {
        let mut state = GameState::new_two_player(42);
        let prey = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Prey".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Salamander Warrior".to_string(),
                power: PtValue::Fixed(4),
                toughness: PtValue::Fixed(3),
                types: vec!["Salamander".to_string(), "Warrior".to_string()],
                colors: vec![ManaColor::Blue],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::ParentTargetController,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(prey)],
            ObjectId(100),
            PlayerId(0),
        );
        let trigger_source = ObjectId(100);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: trigger_source,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                trigger_source,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        });

        assert_eq!(
            resolve_player_for_context_ref(&state, &ability, &TargetFilter::ParentTargetController,),
            PlayerId(1),
        );
    }

    /// CR 701.34 + CR 608.2c (issue #2890): Reality Shift — exile then manifest
    /// for the exiled creature's controller, including when the chained manifest
    /// sub inherits only `effect_context_object` and not parent targets.
    #[test]
    fn change_zone_exile_then_manifest_parent_target_controller_chain() {
        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        let opponent_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Top".to_string(),
            Zone::Library,
        );

        let manifest = ResolvedAbility::new(
            Effect::Manifest {
                target: TargetFilter::ParentTargetController,
                count: QuantityExpr::Fixed { value: 1 },
                profile: None,
                enters_under: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let exile = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(victim)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(manifest);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &exile, &mut events, 0).unwrap();

        assert_eq!(
            state.objects.get(&victim).map(|o| o.zone),
            Some(Zone::Exile)
        );
        let manifested = state.objects.get(&opponent_top).expect("manifested card");
        assert!(manifested.face_down);
        assert_eq!(manifested.zone, Zone::Battlefield);
        assert_eq!(manifested.controller, PlayerId(1));
    }

    #[test]
    fn damage_chain_controller_rider_ignores_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let rider = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(rider);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, 18);
        assert_eq!(state.players[1].life, 16);
    }

    #[test]
    fn counter_spell_damage_rider_hits_countered_spell_controller() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Target Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let damage = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::ParentTargetController,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackSpell,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![TargetRef::Object(spell)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(damage);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.players[1].life, 18);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == spell)));
    }

    #[test]
    fn sacrifice_effect_context_feeds_non_interactive_downstream_quantities() {
        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Four Power Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&victim).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
        }
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Library,
            );
        }

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::CostPaidObject,
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::CostPaidObject,
                    },
                },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(draw);
        let sacrifice = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(gain_life);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &sacrifice, &mut events, 0).unwrap();

        assert!(state.players[0].graveyard.contains(&victim));
        assert_eq!(state.players[0].life, 24);
        assert_eq!(state.players[0].hand.len(), 4);
    }

    #[test]
    fn moved_object_context_reads_single_zone_change_record() {
        let state = GameState::new_two_player(42);
        let moved = ObjectId(7);
        let record = ZoneChangeRecord {
            name: "Eight Mana Creature".to_string(),
            mana_value: 8,
            controller: PlayerId(0),
            owner: PlayerId(0),
            ..ZoneChangeRecord::test_minimal(moved, Some(Zone::Graveyard), Zone::Battlefield)
        };
        let events = vec![GameEvent::ZoneChanged {
            object_id: moved,
            from: Some(Zone::Graveyard),
            to: Zone::Battlefield,
            record: Box::new(record),
        }];

        let snapshot = parent_referent_context_from_events(&state, &events)
            .expect("single zone change to a public zone should provide context");

        assert_eq!(snapshot.object_id, moved);
        assert_eq!(snapshot.lki.mana_value, 8);
        assert_eq!(snapshot.lki.name, "Eight Mana Creature");
    }

    #[test]
    fn moved_object_context_ignores_ambiguous_multiple_zone_changes() {
        let state = GameState::new_two_player(42);
        let first = ObjectId(7);
        let second = ObjectId(8);
        let events = vec![
            GameEvent::ZoneChanged {
                object_id: first,
                from: Some(Zone::Graveyard),
                to: Zone::Battlefield,
                record: Box::new(ZoneChangeRecord::test_minimal(
                    first,
                    Some(Zone::Graveyard),
                    Zone::Battlefield,
                )),
            },
            GameEvent::ZoneChanged {
                object_id: second,
                from: Some(Zone::Graveyard),
                to: Zone::Battlefield,
                record: Box::new(ZoneChangeRecord::test_minimal(
                    second,
                    Some(Zone::Graveyard),
                    Zone::Battlefield,
                )),
            },
        ];

        assert!(parent_referent_context_from_events(&state, &events).is_none());
    }

    /// CR 608.2c + CR 701.20b: a single-card `CardsRevealed` event introduces
    /// an anaphoric referent (the revealed card).
    #[test]
    fn revealed_object_context_reads_single_revealed_card() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Three Mana Spell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&card).unwrap();
            obj.mana_cost = ManaCost::generic(3);
            obj.base_mana_cost = obj.mana_cost.clone();
        }
        let events = vec![GameEvent::CardsRevealed {
            player: PlayerId(0),
            card_ids: vec![card],
            card_names: vec!["Three Mana Spell".to_string()],
        }];

        let snapshot = revealed_object_context_from_events(&state, &events)
            .expect("a single-card reveal should provide a referent");
        assert_eq!(snapshot.object_id, card);
        assert_eq!(snapshot.lki.mana_value, 3);
        assert_eq!(snapshot.lki.name, "Three Mana Spell");
    }

    /// A multi-card reveal has no singular "it" → no referent.
    #[test]
    fn revealed_object_context_ignores_multi_card_reveal() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let b = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);
        let events = vec![GameEvent::CardsRevealed {
            player: PlayerId(0),
            card_ids: vec![a, b],
            card_names: vec!["A".into(), "B".into()],
        }];
        assert!(revealed_object_context_from_events(&state, &events).is_none());
    }

    /// CR 608.2c: a single object-targeted `DamageDealt` introduces a
    /// fight-back referent ("that creature deals damage equal to its power").
    #[test]
    fn damaged_object_context_reads_single_object_damage() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Five Power Creature".to_string(),
            Zone::Battlefield,
        );
        let events = vec![GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Object(creature),
            amount: 3,
            is_combat: false,
            excess: 0,
        }];

        let snapshot = damaged_object_context_from_events(&state, &events)
            .expect("a single object-targeted damage event should provide a referent");
        assert_eq!(snapshot.object_id, creature);
        assert_eq!(snapshot.lki.name, "Five Power Creature");
    }

    /// CR 608.2c: a multi-target damage parent (e.g. Living Inferno) has no
    /// singular "that creature" — the single-object guard declines it.
    #[test]
    fn damaged_object_context_ignores_multi_target_damage() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "A".into(),
            Zone::Battlefield,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "B".into(),
            Zone::Battlefield,
        );
        let events = vec![
            GameEvent::DamageDealt {
                source_id: ObjectId(99),
                target: TargetRef::Object(a),
                amount: 1,
                is_combat: false,
                excess: 0,
            },
            GameEvent::DamageDealt {
                source_id: ObjectId(99),
                target: TargetRef::Object(b),
                amount: 1,
                is_combat: false,
                excess: 0,
            },
        ];
        assert!(damaged_object_context_from_events(&state, &events).is_none());
        assert!(parent_referent_context_from_events(&state, &events).is_none());
    }

    /// CR 608.2c: player-targeted damage carries no "that creature" referent.
    #[test]
    fn damaged_object_context_ignores_player_damage() {
        let state = GameState::new_two_player(42);
        let events = vec![GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            excess: 0,
        }];
        assert!(damaged_object_context_from_events(&state, &events).is_none());
    }

    /// Two separate `CardsRevealed` events → ambiguous "it" → no referent.
    #[test]
    fn revealed_object_context_ignores_two_reveal_events() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(&mut state, CardId(1), PlayerId(0), "A".into(), Zone::Hand);
        let b = create_object(&mut state, CardId(2), PlayerId(0), "B".into(), Zone::Hand);
        let events = vec![
            GameEvent::CardsRevealed {
                player: PlayerId(0),
                card_ids: vec![a],
                card_names: vec!["A".into()],
            },
            GameEvent::CardsRevealed {
                player: PlayerId(0),
                card_ids: vec![b],
                card_names: vec!["B".into()],
            },
        ];
        assert!(revealed_object_context_from_events(&state, &events).is_none());
    }

    /// `parent_referent_context_from_events` prefers a sacrifice over a reveal
    /// when both event kinds are present (most-specific-first ordering).
    #[test]
    fn parent_referent_prefers_sacrifice_over_reveal() {
        let mut state = GameState::new_two_player(42);
        let revealed = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Revealed".into(),
            Zone::Hand,
        );
        let sacrificed = ObjectId(99);
        state.lki_cache.insert(
            sacrificed,
            crate::types::game_state::LKISnapshot {
                name: "Sacrificed".to_string(),
                power: Some(1),
                toughness: Some(1),
                base_power: Some(1),
                base_toughness: Some(1),
                mana_value: 5,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: Default::default(),
                tapped: false,
                is_suspected: false,
            },
        );
        let events = vec![
            GameEvent::CardsRevealed {
                player: PlayerId(0),
                card_ids: vec![revealed],
                card_names: vec!["Revealed".into()],
            },
            GameEvent::PermanentSacrificed {
                object_id: sacrificed,
                player_id: PlayerId(0),
            },
        ];
        let snapshot = parent_referent_context_from_events(&state, &events)
            .expect("sacrifice should provide a referent");
        assert_eq!(
            snapshot.object_id, sacrificed,
            "sacrifice must take priority over reveal"
        );
    }

    #[test]
    fn change_zone_then_lose_life_reads_moved_object_mana_value() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Eight Mana Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = ManaCost::generic(8);
            obj.base_mana_cost = obj.mana_cost.clone();
        }
        let lose_life = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: crate::types::ability::ObjectScope::CostPaidObject,
                    },
                },
                target: Some(TargetFilter::Controller),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let reanimate_shape = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(lose_life);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &reanimate_shape, &mut events, 0).unwrap();

        assert_eq!(state.objects[&creature].zone, Zone::Battlefield);
        assert_eq!(state.players[0].life, 12);
    }

    #[test]
    fn forward_result_non_attach_parent_target_binds_moved_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Returned Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        let sacrifice_moved = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut reanimate = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(sacrifice_moved);
        reanimate.forward_result = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reanimate, &mut events, 0).unwrap();

        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if *object_id == creature)),
            "parent ChangeZone must move the creature before forwarding it"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == creature
            )),
            "forward_result must bind ParentTarget to the moved creature"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == source
            )),
            "ParentTarget must not fall back to the source permanent"
        );
    }

    #[test]
    fn forward_result_with_parent_targets_binds_moved_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Goryo's Vengeance".to_string(),
            Zone::Stack,
        );
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Returned Legend".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        let haste_grant = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::ParentTarget)
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }])],
                duration: None,
                target: Some(TargetFilter::ParentTarget),
            },
            vec![],
            source,
            PlayerId(0),
        );
        let delayed_exile = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: Some(Zone::Battlefield),
                        destination: Zone::Exile,
                        target: TargetFilter::ParentTarget,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let haste_then_exile = haste_grant.sub_ability(delayed_exile);
        let mut reanimate = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(creature)],
            source,
            PlayerId(0),
        )
        .sub_ability(haste_then_exile);
        reanimate.forward_result = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reanimate, &mut events, 0).unwrap();

        assert_eq!(state.objects[&creature].zone, Zone::Battlefield);
        assert!(
            state.transient_continuous_effects.iter().any(|tce| {
                matches!(
                    tce.affected,
                    TargetFilter::SpecificObject { id } if id == creature
                ) && tce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword { keyword }
                            if matches!(keyword, Keyword::Haste)
                    )
                })
            }),
            "haste must attach to the returned creature when parent carried cast-time targets"
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(creature)],
            "delayed exile must snapshot the returned creature"
        );
    }

    #[test]
    fn forward_result_attach_to_last_created_uses_moved_object_as_attachment() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Ratonhnhake:ton".to_string(),
            Zone::Battlefield,
        );
        let equipment = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Assassin Gauntlet".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.base_card_types = obj.card_types.clone();
        }
        let token = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Assassin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&token)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.last_created_token_ids = vec![token];

        let attach_to_token = ResolvedAbility::new(
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut return_equipment = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Subtype("Equipment".to_string())],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(equipment)],
            source,
            PlayerId(0),
        )
        .sub_ability(attach_to_token);
        return_equipment.forward_result = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &return_equipment, &mut events, 0).unwrap();

        assert_eq!(state.objects[&equipment].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&equipment].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(token))
        );
        assert!(state.objects[&token].attachments.contains(&equipment));
        assert_eq!(state.objects[&source].attached_to, None);
        assert!(!state.objects[&equipment].attachments.contains(&source));
    }

    /// CR 608.2c + CR 400.7j + CR 608.2k: a non-targeted `ChangeZone` with 2+
    /// eligible objects raises `WaitingFor::EffectZoneChoice`. After the player
    /// picks a card, the `EffectZoneChoice` handler stamps parent-referent
    /// context onto the pending continuation so a later instruction (here a
    /// `LoseLife` rider reading
    /// `QuantityRef::ObjectManaValue { scope: CostPaidObject }`) sees
    /// the *chosen* object's mana value — not the first eligible card, and not
    /// a fallback of 0. This covers the `EffectZoneChoice` continuation stamp
    /// site, the sibling path to `change_zone_then_lose_life_reads_moved_object_mana_value`.
    ///
    /// `chosen_mv` is the mana value of the card the player selects; the test
    /// asserts the controller loses exactly that much life (starting from the
    /// `GameState::new_two_player` default of 20). A regression in the stamp
    /// would leave the `LoseLife` quantity unresolved (fallback 0) and life at 20.
    fn run_effect_zone_choice_lose_life_case(choose_mv8: bool) {
        let mut state = GameState::new_two_player(42);
        let mv8_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Eight Mana Creature".to_string(),
            Zone::Graveyard,
        );
        let mv3_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Three Mana Creature".to_string(),
            Zone::Graveyard,
        );
        for (id, mv) in [(mv8_creature, 8), (mv3_creature, 3)] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.mana_cost = ManaCost::generic(mv);
            obj.base_mana_cost = obj.mana_cost.clone();
        }

        let lose_life = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: crate::types::ability::ObjectScope::CostPaidObject,
                    },
                },
                target: Some(TargetFilter::Controller),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        // Empty `targets` + `optional_targeting: false` (the `new` default)
        // forces `resolve()` down the non-targeted resolution-time zone-scan
        // path; two eligible graveyard creatures raise `EffectZoneChoice`.
        let reanimate = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(lose_life);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reanimate, &mut events, 0).unwrap();

        let choice_player = match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                effect_kind: EffectKind::ChangeZone,
                zone: Zone::Graveyard,
                cards,
                ..
            } => {
                assert!(cards.contains(&mv8_creature));
                assert!(cards.contains(&mv3_creature));
                *player
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        };
        assert!(
            state.pending_continuation.is_some(),
            "LoseLife tail must be stashed as a pending continuation"
        );

        let chosen = if choose_mv8 {
            mv8_creature
        } else {
            mv3_creature
        };
        let unchosen = if choose_mv8 {
            mv3_creature
        } else {
            mv8_creature
        };

        crate::game::engine::apply(
            &mut state,
            choice_player,
            GameAction::SelectCards {
                cards: vec![chosen],
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&chosen].zone,
            Zone::Battlefield,
            "chosen card should have moved to the battlefield"
        );
        assert_eq!(
            state.objects[&unchosen].zone,
            Zone::Graveyard,
            "unchosen card should be untouched"
        );
        // Discriminating assertion: the continuation read the chosen card's MV.
        // A stamp regression would leave the quantity unresolved (0) and life at 20.
        let expected_life = if choose_mv8 { 20 - 8 } else { 20 - 3 };
        assert_eq!(
            state.players[0].life, expected_life,
            "controller should lose life equal to the chosen object's mana value"
        );
        assert!(
            state.pending_continuation.is_none(),
            "continuation should be drained after the choice resolves"
        );
    }

    #[test]
    fn effect_zone_choice_then_lose_life_reads_chosen_object_mana_value() {
        // Choosing the MV-8 creature -> lose 8 life (20 -> 12).
        run_effect_zone_choice_lose_life_case(true);
        // Choosing the MV-3 creature -> lose 3 life (20 -> 17): proves the
        // stamp tracks the actual choice, not the first eligible card.
        run_effect_zone_choice_lose_life_case(false);
    }

    fn bounce_then_draw_if_controller_matched_lki(
        permanent_controller: PlayerId,
    ) -> (GameState, Vec<GameEvent>) {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Permanent".to_string(),
            Zone::Battlefield,
        );
        let permanent_obj = state.objects.get_mut(&permanent).unwrap();
        permanent_obj.controller = permanent_controller;
        permanent_obj.card_types.core_types.push(CoreType::Artifact);

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            use_lki: true,
            subject_slot: None,
        });
        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(permanent)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(draw);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        (state, events)
    }

    #[test]
    fn bounce_followup_draws_when_caster_controlled_parent_target() {
        let (state, events) = bounce_then_draw_if_controller_matched_lki(PlayerId(0));

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[1].hand.len(), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    #[test]
    fn bounce_followup_skips_draw_when_opponent_controlled_parent_target() {
        let (state, events) = bounce_then_draw_if_controller_matched_lki(PlayerId(1));

        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    #[test]
    fn previous_effect_amount_for_damage_ignores_counter_side_effects() {
        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Siege".to_string(),
            Zone::Battlefield,
        );
        {
            let battle = state.objects.get_mut(&battle_id).unwrap();
            battle.card_types.core_types.push(CoreType::Battle);
            battle.defense = Some(5);
            battle.base_defense = Some(5);
            battle.counters.insert(CounterType::Defense, 5);
        }

        let sub = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(battle_id)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::CounterRemoved {
                    counter_type: CounterType::Defense,
                    count: 3,
                    ..
                }
            )),
            "damage to a battle must still emit the defense-counter side effect"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::DamageDealt { amount: 3, .. })),
            "damage event must still be present for PreviousEffectAmount"
        );
        assert_eq!(
            state.players[0].life, 23,
            "sub-ability must use the damage amount only, not damage + counters removed"
        );
    }

    #[test]
    fn resolve_ability_chain_condition_blocks_optional_prompt() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayLife {
                    amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        })
        .sub_ability(ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        ));
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        assert!(state.pending_optional_effect.is_none());
        assert!(events.is_empty());
    }

    /// Issue #418 (Guide of Souls): the `WhenYouDo` reflexive sub-ability of a
    /// `PayCost` parent must NOT run when the embedded `{E}{E}{E}` cost is
    /// unpayable. CR 603.12: a reflexive trigger fires based on whether the
    /// trigger event (the cost payment) actually occurred.
    #[test]
    fn when_you_do_skipped_when_embedded_energy_cost_unpayable() {
        let mut state = GameState::new_two_player(42);
        // Controller has insufficient energy to pay {E}{E}{E}.
        state.players[0].energy = 0;

        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let sub = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                // `None` (not `SelfRef`) so the counter resolves against the
                // explicit chosen target rather than short-circuiting to the
                // ability's source object.
                target: TargetFilter::None,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::WhenYouDo);

        let ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.cost_payment_failed_flag,
            "an unpayable {{E}}{{E}}{{E}} cost must set cost_payment_failed_flag"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CounterAdded { .. })),
            "WhenYouDo reflexive sub-ability must NOT run when the embedded cost was unpayable"
        );
    }

    /// Issue #418 (Guide of Souls): when the embedded `{E}{E}{E}` cost IS paid,
    /// the `WhenYouDo` reflexive sub-ability runs and energy is deducted.
    #[test]
    fn when_you_do_runs_when_embedded_energy_cost_paid() {
        let mut state = GameState::new_two_player(42);
        // Controller has exactly enough energy to pay {E}{E}{E}.
        state.players[0].energy = 3;

        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let sub = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                // `None` (not `SelfRef`) so the counter resolves against the
                // explicit chosen target rather than short-circuiting to the
                // ability's source object.
                target: TargetFilter::None,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::WhenYouDo);

        let ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            !state.cost_payment_failed_flag,
            "a fully-paid {{E}}{{E}}{{E}} cost must not set cost_payment_failed_flag"
        );
        assert_eq!(
            state.players[0].energy, 0,
            "paying {{E}}{{E}}{{E}} must deduct all 3 energy"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::EnergyChanged { delta: -3, .. })),
            "energy payment must emit EnergyChanged delta -3"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::CounterAdded {
                    counter_type: CounterType::Plus1Plus1,
                    count: 2,
                    ..
                }
            )),
            "WhenYouDo reflexive sub-ability must run when the embedded cost was paid"
        );
    }

    /// Issue #418 negative control: `WhenYouDo` attached to a non-`PayCost`
    /// parent (e.g. a `BecomeCopy` reflexive) must STILL fire even when
    /// `cost_payment_failed_flag` is stale-`true` from an earlier resolution.
    /// The flag is not reset at `resolve_ability_chain` entry, so the gate
    /// must be scoped to `Effect::PayCost` parents only — proving the
    /// parent-effect-type gate protects non-cost reflexives.
    #[test]
    fn when_you_do_non_paycost_parent_fires_despite_stale_cost_failed_flag() {
        let mut state = GameState::new_two_player(42);
        // Simulate a previous resolution that left the flag set.
        state.cost_payment_failed_flag = true;

        // A non-cost, non-optional parent: a `BecomeCopy` reflexive parent.
        let become_copy_parent = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::SelfRef,
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert!(
            evaluate_condition(&AbilityCondition::WhenYouDo, &state, &become_copy_parent),
            "WhenYouDo on a non-PayCost parent must fire even with a stale \
             cost_payment_failed_flag — the gate is scoped to Effect::PayCost parents"
        );

        // Sanity check: the same flag DOES gate a PayCost parent.
        let pay_cost_parent = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        assert!(
            !evaluate_condition(&AbilityCondition::WhenYouDo, &state, &pay_cost_parent),
            "WhenYouDo on a PayCost parent must be suppressed when cost_payment_failed_flag is set"
        );
    }

    #[test]
    fn chain_depth_exceeds_limit_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, 21);
        assert_eq!(result, Err(EffectError::ChainTooDeep));
    }

    #[test]
    fn tracked_set_recorded_for_delayed_trigger() {
        let mut state = GameState::new_two_player(42);

        // Create 2 objects on the battlefield to be exiled
        let obj1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        let obj2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Creature B".to_string(),
            Zone::Battlefield,
        );

        // Build chain: ChangeZone(exile) -> CreateDelayedTrigger(uses_tracked_set: true)
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::TrackedSet {
                            id: TrackedSetId(0),
                        },
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                )),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(obj1), TargetRef::Object(obj2)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        let result = resolve_ability_chain(&mut state, &ability, &mut events, 0);
        assert!(result.is_ok());

        // Tracked set should contain both exiled objects
        assert_eq!(state.tracked_object_sets.len(), 1);
        let set = state.tracked_object_sets.values().next().unwrap();
        assert!(set.contains(&obj1));
        assert!(set.contains(&obj2));

        // Delayed trigger should have been created
        assert_eq!(state.delayed_triggers.len(), 1);
    }

    #[test]
    fn no_tracked_set_without_flag() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );

        // Same chain but uses_tracked_set: false
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(obj)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.tracked_object_sets.is_empty(),
            "Should NOT record tracked set when uses_tracked_set is false"
        );
    }

    /// CR 608.2c building-block regression: a synchronous chain of
    /// `ChangeZoneAll(Battlefield → Hand)` followed by
    /// `Token { count: Ref(TrackedSetSize) }` produces one token per object
    /// moved by the parent. CR 608.2c (verified via `grep '^608.2c'
    /// docs/MagicCompRules.txt`) covers "instructions in the order written"
    /// — the per-instruction-set basis for "this way" referencing. Covers the
    /// "Return all <X> to their owners' hands. If you do, create N Treasure
    /// tokens, where N is the number of permanents returned this way" pattern
    /// (Item 1 of the design doc) at the primitive level — the parser arm is
    /// deferred until a real card surfaces.
    ///
    /// Asserts both the K-object case and the K=0 case so the
    /// `chain_tracked_set_id` plumbing can be trusted by future callers.
    #[test]
    fn change_zone_all_battlefield_to_hand_publishes_chain_for_tracked_set_size_cr_609_3() {
        fn run_with_count(k: usize) -> (Vec<ObjectId>, GameState) {
            let mut state = GameState::new_two_player(42);
            let mut moving_ids = Vec::with_capacity(k);
            for i in 0..k {
                moving_ids.push(create_object(
                    &mut state,
                    CardId(100 + i as u64),
                    PlayerId(0),
                    format!("Equipment {i}"),
                    Zone::Battlefield,
                ));
            }
            let initial_battlefield = state.battlefield.len();

            let token_sub = ResolvedAbility::new(
                Effect::Token {
                    name: "Treasure".to_string(),
                    power: PtValue::Fixed(0),
                    toughness: PtValue::Fixed(0),
                    types: vec!["Artifact".to_string(), "Treasure".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize,
                    },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            );
            let ability = ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Hand,
                    target: TargetFilter::Any,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )
            .sub_ability(token_sub);

            let mut events = Vec::new();
            resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

            // Sanity: the K originals left the battlefield, the new tokens entered.
            // Battlefield count = initial - K (originals returned) + K (tokens minted).
            assert_eq!(state.battlefield.len(), initial_battlefield);
            (moving_ids, state)
        }

        // Three objects → three Treasure tokens; originals are in Hand.
        let (moved, state) = run_with_count(3);
        for id in &moved {
            assert_eq!(state.objects[id].zone, Zone::Hand);
        }
        let treasures: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.name == "Treasure")
            .collect();
        assert_eq!(
            treasures.len(),
            3,
            "TrackedSetSize must equal the number of permanents moved by the parent ChangeZoneAll"
        );

        // Zero objects → zero Treasure tokens (no spurious creation when chain is empty).
        let (_, state0) = run_with_count(0);
        let treasures0 = state0
            .battlefield
            .iter()
            .filter_map(|id| state0.objects.get(id))
            .filter(|o| o.name == "Treasure")
            .count();
        assert_eq!(treasures0, 0, "Empty chain must mint zero tokens");
    }

    /// CR 613.1b + CR 608.2c: a mass gain-control effect publishes the objects
    /// actually gained so a downstream "those creatures" continuation can act on
    /// that exact set (Call for Aid / Mob Rule class). This exercises both the
    /// `GainControlAll` publication arm and the `SetTapState(All)` tracked-set
    /// consumer path; without either, the creatures remain tapped.
    #[test]
    fn gain_control_all_publishes_tracked_set_for_those_creatures_tail() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Call for Aid".to_string(),
            Zone::Stack,
        );

        let make_tapped_creature = |state: &mut GameState, card_id: u64, name: &str| -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.tapped = true;
            id
        };
        let creature_a = make_tapped_creature(&mut state, 2, "Stolen A");
        let creature_b = make_tapped_creature(&mut state, 3, "Stolen B");
        let noncreature = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Ignored Relic".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&noncreature).unwrap().tapped = true;

        let untap_those_creatures = ResolvedAbility::new(
            Effect::SetTapState {
                scope: EffectScope::All,
                state: TapStateChange::Untap,
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                    caused_by: None,
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let gain_control = ResolvedAbility::new(
            Effect::GainControlAll {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        )
        .sub_ability(untap_those_creatures);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &gain_control, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        for id in [creature_a, creature_b] {
            let obj = state.objects.get(&id).unwrap();
            assert_eq!(
                obj.controller,
                PlayerId(0),
                "gained creature should be controlled by P0"
            );
            assert!(
                !obj.tapped,
                "gained creature should be untapped by the tracked-set tail"
            );
        }
        assert!(
            state.objects.get(&noncreature).unwrap().tapped,
            "non-gained object must not be included in the tracked set"
        );
    }

    /// CR 608.2c + CR 701.15a + CR 122.1: when a counter instruction is
    /// followed by "goad each creature that had counters put on it this way",
    /// the countered objects publish as the tracked set consumed by GoadAll.
    #[test]
    fn put_counter_publishes_tracked_set_for_goad_all_tail() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Agitator Ant".to_string(),
            Zone::Battlefield,
        );
        let countered = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Countered Creature".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Other Creature".to_string(),
            Zone::Battlefield,
        );
        for id in [countered, other] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        let goad_countered_this_way = ResolvedAbility::new(
            Effect::GoadAll {
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                    caused_by: None,
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let put_counter = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![TargetRef::Object(countered)],
            source,
            PlayerId(0),
        )
        .sub_ability(goad_countered_this_way);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &put_counter, &mut events, 0).unwrap();

        assert!(state.objects[&countered].goaded_by.contains(&PlayerId(0)));
        assert!(
            !state.objects[&other].goaded_by.contains(&PlayerId(0)),
            "only the creature that received counters this way should be goaded"
        );
    }

    /// CR 701.20b + CR 608.2c: `RevealTop` publishes `CardsRevealed`, not
    /// `ZoneChanged`. Without a dedicated arm in `affected_objects_from_events`,
    /// the tracked-set publish is empty and `ChooseFromZone` falls back to a
    /// stale graveyard set from an earlier resolution (issue #1374).
    #[test]
    fn reveal_top_publishes_cards_revealed_for_tracked_set() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top Card".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second Card".to_string(),
            Zone::Library,
        );
        let events = vec![GameEvent::CardsRevealed {
            player: PlayerId(0),
            card_ids: vec![top, second],
            card_names: vec!["Top Card".to_string(), "Second Card".to_string()],
        }];
        let effect = Effect::RevealTop {
            player: TargetFilter::Controller,
            count: 2,
        };

        assert_eq!(
            affected_objects_from_events(&effect, &events, &[]),
            vec![top, second]
        );
    }

    /// CR 701.20a + CR 608.2f: an Indomitable Creativity-style
    /// `RevealUntil(kept_destination=Exile)` publishes only the kept card as
    /// the chain tracked set. Without the `RevealUntil` destination filter in
    /// `affected_objects_from_events`, both the kept card and miss pile zone
    /// changes would be published.
    #[test]
    fn reveal_until_exile_kept_publishes_only_kept_card_for_tracked_set() {
        let miss = ObjectId(2);
        let hit = ObjectId(3);
        let events = vec![
            GameEvent::ZoneChanged {
                object_id: miss,
                from: Some(Zone::Library),
                to: Zone::Library,
                record: Box::new(ZoneChangeRecord::test_minimal(
                    miss,
                    Some(Zone::Library),
                    Zone::Library,
                )),
            },
            GameEvent::ZoneChanged {
                object_id: hit,
                from: Some(Zone::Library),
                to: Zone::Exile,
                record: Box::new(ZoneChangeRecord::test_minimal(
                    hit,
                    Some(Zone::Library),
                    Zone::Exile,
                )),
            },
        ];
        let effect = Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            matched_disposition: crate::types::ability::RevealUntilDisposition::KeepEach,
            kept_destination: Zone::Exile,
            rest_destination: Zone::Library,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        };

        assert_eq!(
            affected_objects_from_events(&effect, &events, &[]),
            vec![hit]
        );
    }

    /// CR 608.2c + CR 400.7: a mass-destroy parent must publish the destroyed
    /// set so a token-count follow-up can count only the filtered subset.
    #[test]
    fn destroy_all_chain_counts_filtered_tracked_set_for_token_followup() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Ceaseless Conflict".to_string(),
            Zone::Graveyard,
        );
        let controller_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Creature".to_string(),
            Zone::Battlefield,
        );
        let controller_token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Controller Token".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for id in [controller_creature, controller_token, opponent_creature] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }
        state.objects.get_mut(&controller_token).unwrap().is_token = true;

        let token_sub = ResolvedAbility::new(
            Effect::Token {
                name: "Spirit".to_string(),
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(2),
                types: vec!["Creature".to_string(), "Spirit".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::FilteredTrackedSetSize {
                        filter: Box::new(TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .properties(vec![FilterProp::NonToken]),
                        )),
                        caused_by: None,
                    },
                },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(token_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        for id in [controller_creature, controller_token, opponent_creature] {
            assert_eq!(state.objects[&id].zone, Zone::Graveyard);
        }
        let spirits = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.name == "Spirit")
            .count();
        assert_eq!(
            spirits, 1,
            "only the controller's nontoken destroyed creature should be counted"
        );
    }

    #[test]
    fn put_counter_all_publishes_countered_objects_for_tracked_set_followup() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Elspeth".to_string(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Soldier A".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Soldier B".to_string(),
            Zone::Battlefield,
        );
        let opponent = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Soldier".to_string(),
            Zone::Battlefield,
        );
        for id in [first, second, opponent] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let flying = StaticDefinition::continuous()
            .affected(TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        let followup = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![flying],
                duration: Some(Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                }),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(followup);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        for id in [first, second] {
            assert_eq!(
                state.objects[&id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied(),
                Some(1)
            );
            assert!(state
                .transient_continuous_effects
                .iter()
                .any(|effect| effect.affected == TargetFilter::SpecificObject { id }));
        }
        assert!(!state.objects[&opponent]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        assert!(!state
            .transient_continuous_effects
            .iter()
            .any(|effect| effect.affected == TargetFilter::SpecificObject { id: opponent }));
    }

    fn haste_last_created_followup(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::LastCreated)
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }])],
                duration: Some(Duration::Permanent),
                target: Some(TargetFilter::LastCreated),
            },
            vec![],
            source,
            PlayerId(0),
        )
    }

    fn outer_target_only_granted_static_followup(source: ObjectId) -> ResolvedAbility {
        let granted_static = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![ContinuousModification::AddPower { value: 1 }]);
        ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(TypedFilter::creature()))
                    .modifications(vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(granted_static),
                    }])],
                duration: Some(Duration::Permanent),
                target: Some(TargetFilter::LastCreated),
            },
            vec![],
            source,
            PlayerId(0),
        )
    }

    /// CR 608.2c + CR 707.2 + CR 611.2c: Helm of the Host's follow-up
    /// "That token gains haste" binds to the copy token created by the parent
    /// instruction, not the equipped creature that served as the copy source.
    #[test]
    fn copy_token_chain_grants_haste_to_created_token_not_copy_source() {
        let mut state = GameState::new_two_player(42);
        let equipped = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Equipped Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&equipped).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        let helm = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Helm of the Host".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&helm).unwrap().attached_to = Some(equipped.into());

        let copy = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(equipped)],
            helm,
            PlayerId(0),
        )
        .sub_ability(haste_last_created_followup(helm));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &copy, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        let token = state.last_created_token_ids[0];
        assert!(
            state.objects[&token].has_keyword(&Keyword::Haste),
            "created copy token must gain haste"
        );
        assert!(
            !state.objects[&equipped].has_keyword(&Keyword::Haste),
            "copied source creature must not receive the token's haste grant"
        );
        assert!(state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id: token }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        }
                    )
                })
        }));
        assert!(!state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id: equipped }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        }
                    )
                })
        }));
    }

    /// CR 608.2c + CR 707.2 + CR 611.2c: A `LastCreated` copy-token follow-up
    /// applies to every token just created by the parent copy instruction while
    /// keeping the copied source objects distinct from the produced-token set.
    #[test]
    fn copy_token_chain_grants_haste_to_all_created_tokens_not_sources() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Copy Spell".to_string(),
            Zone::Stack,
        );
        let mut copied_sources = Vec::new();
        for (card_id, name) in [(1, "First Creature"), (2, "Second Creature")] {
            let id = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            copied_sources.push(id);
        }

        let copy = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            copied_sources
                .iter()
                .copied()
                .map(TargetRef::Object)
                .collect(),
            source,
            PlayerId(0),
        )
        .sub_ability(haste_last_created_followup(source));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &copy, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(state.last_created_token_ids.len(), 2);
        for token in &state.last_created_token_ids {
            assert!(
                state.objects[token].has_keyword(&Keyword::Haste),
                "created token {token:?} must gain haste"
            );
        }
        for copied_source in copied_sources {
            assert!(
                !state.objects[&copied_source].has_keyword(&Keyword::Haste),
                "copy source {copied_source:?} must not receive the token grant"
            );
        }
    }

    /// CR 608.2c + CR 611.2c: If a token follow-up grants the token a static
    /// ability whose own `affected` filter names another object set, the outer
    /// `GenericEffect.target` is the only token-recipient slot. Runtime must
    /// bind that outer `LastCreated` target to the copy token, not to the copied
    /// source carried by the parent effect's targets.
    #[test]
    fn copy_token_chain_outer_target_only_grant_binds_created_token_not_source() {
        let mut state = GameState::new_two_player(42);
        let copied_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copied Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&copied_source).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Stack,
        );
        let copy = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(copied_source)],
            source,
            PlayerId(0),
        )
        .sub_ability(outer_target_only_granted_static_followup(source));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &copy, &mut events, 0).unwrap();

        let token = state.last_created_token_ids[0];
        assert!(state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id: token }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::GrantStaticAbility { .. }
                    )
                })
        }));
        assert!(!state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id: copied_source }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::GrantStaticAbility { .. }
                    )
                })
        }));
    }

    /// CR 608.2c: If a copy-token parent creates no tokens, it clears the
    /// `LastCreated` ledger before the follow-up resolves so a stale token id
    /// from an earlier resolution cannot receive the new grant.
    #[test]
    fn copy_token_chain_no_created_tokens_clears_stale_last_created_before_followup() {
        let mut state = GameState::new_two_player(42);
        let stale = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Old Token".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&stale).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
            obj.is_token = true;
        }
        state.last_created_token_ids = vec![stale];

        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Stack,
        );
        let copy = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Typed(TypedFilter::creature()),
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(haste_last_created_followup(source));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &copy, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert!(state.last_created_token_ids.is_empty());
        assert!(
            !state.objects[&stale].has_keyword(&Keyword::Haste),
            "stale last-created token must not receive a grant from a no-op copy"
        );
        assert!(!state
            .transient_continuous_effects
            .iter()
            .any(|effect| { effect.affected == TargetFilter::SpecificObject { id: stale } }));
    }

    #[test]
    fn empty_targets_record_empty_tracked_set_for_downstream_context() {
        let mut state = GameState::new_two_player(42);

        // Chain with uses_tracked_set: true but no targets — nothing to exile
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::TrackedSet {
                            id: TrackedSetId(0),
                        },
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                )),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![], // no targets
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(delayed);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.tracked_object_sets.len(), 1);
        assert!(state
            .tracked_object_sets
            .get(&TrackedSetId(1))
            .is_some_and(|objects| objects.is_empty()));
    }

    /// CR 608.2c + CR 701.26a: a tapped-object set published by `Tap` must
    /// bind a downstream filtered "each of those <type>" counter effect.
    #[test]
    fn tap_chain_publishes_filtered_tracked_set_for_counter_followup() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Urge to Feed".to_string(),
            Zone::Graveyard,
        );
        let vampire = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vampire".to_string(),
            Zone::Battlefield,
        );
        let other_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Battlefield,
        );

        state
            .objects
            .get_mut(&vampire)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&vampire).unwrap().card_types.subtypes = vec!["Vampire".to_string()];
        state
            .objects
            .get_mut(&other_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        state.objects.get_mut(&land).unwrap().card_types.subtypes = vec!["Swamp".to_string()];

        let counter = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter: Box::new(TargetFilter::Typed(
                        TypedFilter::creature().subtype("Vampire".to_string()),
                    )),
                    caused_by: None,
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature().subtype("Vampire".to_string())),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![TargetRef::Object(vampire)],
            source,
            PlayerId(0),
        )
        .sub_ability(counter);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(state.objects[&vampire].tapped);
        assert_eq!(
            state.objects[&vampire]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
        for id in [other_creature, land] {
            assert_eq!(
                state.objects[&id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied(),
                None,
                "only the tapped Vampire should receive the counter"
            );
        }
    }

    #[test]
    fn airbend_chain_exiles_all_creatures_when_no_target_is_chosen() {
        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Creature B".to_string(),
            Zone::Battlefield,
        );
        for creature in [creature_a, creature_b] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::ParentTarget),
                            },
                        ],
                    },
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                        cost: ManaCost::generic(2),
                        cast_transformed: false,
                        constraint: None,
                        granted_to: None,
                        resolution_cleanup: None,
                        duration: None,

                        graveyard_replacement: None,
                        enters_with_counter: None,
                        enters_with_modifications: Vec::new(),
                        mana_spend_permission: None,
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: Default::default(),
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        for creature in [creature_a, creature_b] {
            let obj = state.objects.get(&creature).unwrap();
            assert_eq!(obj.zone, Zone::Exile);
            assert!(obj.casting_permissions.iter().any(|permission| matches!(
                permission,
                crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. }
                    if *cost == ManaCost::generic(2)
            )));
        }
    }

    #[test]
    fn airbend_chain_preserves_chosen_target_and_exiles_other_creatures() {
        let mut state = GameState::new_two_player(42);
        let chosen = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Chosen".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Other".to_string(),
            Zone::Battlefield,
        );
        for creature in [chosen, other] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
            vec![TargetRef::Object(chosen)],
            ObjectId(901),
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::ParentTarget),
                            },
                        ],
                    },
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                },
                vec![],
                ObjectId(901),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                        cost: ManaCost::generic(2),
                        cast_transformed: false,
                        constraint: None,
                        granted_to: None,
                        resolution_cleanup: None,
                        duration: None,

                        graveyard_replacement: None,
                        enters_with_counter: None,
                        enters_with_modifications: Vec::new(),
                        mana_spend_permission: None,
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: Default::default(),
                },
                vec![],
                ObjectId(901),
                PlayerId(0),
            )),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.objects.get(&chosen).unwrap().zone, Zone::Battlefield);
        let other_obj = state.objects.get(&other).unwrap();
        assert_eq!(other_obj.zone, Zone::Exile);
        assert!(other_obj
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. }
                    if *cost == ManaCost::generic(2)
            )));
    }

    #[test]
    fn tracked_set_sentinel_does_not_reuse_prior_non_empty_set_when_current_move_is_empty() {
        let mut state = GameState::new_two_player(42);
        let stale = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Stale".to_string(),
            Zone::Exile,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![stale]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            ObjectId(902),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                    cost: ManaCost::generic(2),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: None,
                    resolution_cleanup: None,
                    duration: None,

                    graveyard_replacement: None,
                    enters_with_counter: None,
                    enters_with_modifications: Vec::new(),
                    mana_spend_permission: None,
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: Default::default(),
            },
            vec![],
            ObjectId(902),
            PlayerId(0),
        ));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(state
            .tracked_object_sets
            .get(&TrackedSetId(2))
            .is_some_and(|objects| objects.is_empty()));
        assert!(state
            .objects
            .get(&stale)
            .is_some_and(|obj| obj.casting_permissions.is_empty()));
    }

    #[test]
    fn override_instead_condition_met_swaps_effect() {
        // CR 608.2e: When AdditionalCostPaidInstead condition is met,
        // the sub's effect replaces the parent's effect.
        let mut state = GameState::new_two_player(42);

        // Sub: deal 5 damage (override) with AdditionalCostPaidInstead
        let sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::AdditionalCostPaidInstead);

        // Parent: deal 2 damage — should be REPLACED by the sub
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .context(SpellContext {
            additional_cost_paid: true,
            ..Default::default()
        })
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the override effect (5 damage) should have fired, not the parent (2 damage)
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage from override, not 2 from parent"
        );
    }

    #[test]
    fn override_instead_condition_not_met_runs_parent() {
        // CR 608.2e: When AdditionalCostPaidInstead condition is NOT met,
        // the parent runs normally and the override sub is skipped.
        let mut state = GameState::new_two_player(42);

        let sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::AdditionalCostPaidInstead);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .context(SpellContext::default())
        .sub_ability(sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the parent effect (2 damage) should have fired
        assert_eq!(
            state.players[1].life, 18,
            "Expected 2 damage from parent, override should be skipped"
        );
    }

    #[test]
    fn condition_instead_swaps_when_met() {
        // CR 608.2c: ConditionInstead wraps a general condition with "instead" swap
        // semantics. When the inner condition is met, the sub's effect replaces the
        // parent's. The sub's chain continues after the swap.
        let mut state = GameState::new_two_player(42);

        // Instead sub: deal 5 damage (replaces parent when condition is met)
        let instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::IsYourTurn),
        });

        // Parent: deal 2 damage — should be replaced
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // IsYourTurn is true (player 0 is active), so the swap fires: 5 damage
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage from instead override"
        );
    }

    #[test]
    fn condition_instead_runs_base_chain_when_not_met() {
        // CR 608.2c: When ConditionInstead condition is NOT met, the parent effect
        // runs and the base continuation chain (else_ability) executes after it.
        let mut state = GameState::new_two_player(42);
        // Give player 0 cards to draw
        for i in 0..3 {
            crate::game::zones::create_object(
                &mut state,
                CardId(i + 50),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Base continuation: draw 1 card (stored in else_ability)
        let base_chain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        // Instead sub: deal 5 damage (with its own chain: draw 2)
        let instead_chain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            // negated: true → NOT your turn → condition NOT met (it IS our turn)
            inner: Box::new(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
        })
        .sub_ability(instead_chain);
        instead_sub.else_ability = Some(Box::new(base_chain));

        // Parent: deal 2 damage — should execute (condition not met)
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // IsYourTurn negated=true → NOT met → parent runs (2 damage) + base chain (draw 1)
        assert_eq!(
            state.players[1].life, 18,
            "Expected 2 damage from parent (condition not met)"
        );
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "Expected 1 card drawn from base continuation chain"
        );
    }

    /// Drive the not-swap `ConditionInstead` tail-runner (the new `else if`
    /// branch) and return P0's hand size. The override carries a false condition
    /// (`Not{IsYourTurn}` while it IS P0's turn) so the swap does NOT fire; its
    /// `else_ability` is None so the tail-runner is the live path.
    fn run_condition_instead_not_swap_tail(
        link: SubAbilityLink,
        unimplemented_tail: bool,
    ) -> usize {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            crate::game::zones::create_object(
                &mut state,
                CardId(i + 70),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let tail_effect = if unimplemented_tail {
            Effect::Unimplemented {
                name: "speculative".to_string(),
                description: None,
            }
        } else {
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }
        };
        let mut tail = ResolvedAbility::new(tail_effect, vec![], ObjectId(100), PlayerId(0));
        tail.sub_link = link;

        let mut instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
        });
        instead_sub.sub_ability = Some(Box::new(tail));
        instead_sub.else_ability = None;

        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(instead_sub));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        state.players[0].hand.len()
    }

    /// CR 608.2c — building-block guard for the not-swap `ConditionInstead`
    /// tail-runner. A `SequentialSibling` tail printed AFTER the override is an
    /// INDEPENDENT instruction and MUST run in the not-swap branch; a
    /// `ContinuationStep` sub is part of the REPLACED clause and MUST NOT run when
    /// the swap did not fire; an `Unimplemented` tail MUST never run (no
    /// speculative semantics). Each arm flips a single gate in the new branch.
    #[test]
    fn condition_instead_not_swap_tail_runner_honors_gates() {
        // Positive control — a SequentialSibling tail runs (the Draw fires).
        assert_eq!(
            run_condition_instead_not_swap_tail(SubAbilityLink::SequentialSibling, false),
            1,
            "SequentialSibling tail must run in the not-swap branch (Draw fires). \
             Removing the new else-if branch makes this 0."
        );
        // Negative (row 9) — a ContinuationStep sub must NOT run.
        assert_eq!(
            run_condition_instead_not_swap_tail(SubAbilityLink::ContinuationStep, false),
            0,
            "ContinuationStep sub belongs to the REPLACED clause and must NOT run when \
             the swap did not fire. Loosening the `sub_link == SequentialSibling` gate \
             makes this 1."
        );
        // Negative (row 8) — an Unimplemented SequentialSibling tail must NOT run
        // (and must not panic).
        assert_eq!(
            run_condition_instead_not_swap_tail(SubAbilityLink::SequentialSibling, true),
            0,
            "Unimplemented tail must never run. Removing the `!Unimplemented` guard \
             would attempt speculative semantics."
        );
    }

    #[test]
    fn repeat_until_controller_choice_prompts_each_iteration() {
        // CR 107.1c: a "you may repeat this process" loop resolves one
        // iteration, prompts the controller via `WaitingFor::RepeatDecision`,
        // and repeats on accept. Accept twice then decline → 3 resolutions.
        let mut state = GameState::new_two_player(42);
        let start_life = state.players[0].life;

        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_until = Some(RepeatContinuation::ControllerChoice);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // First iteration resolved; the controller is now prompted.
        assert!(
            matches!(state.waiting_for, WaitingFor::RepeatDecision { .. }),
            "expected RepeatDecision prompt, got {:?}",
            state.waiting_for,
        );
        assert_eq!(state.players[0].life, start_life + 1);

        // Accept twice, then decline.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::RepeatDecision { .. }
        ));
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::RepeatDecision { .. }
        ));
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::RepeatDecision { .. }),
            "declining ends the loop",
        );
        assert_eq!(
            state.players[0].life,
            start_life + 3,
            "initial iteration + 2 accepted = 3 resolutions, each gaining 1 life",
        );
    }

    #[test]
    fn repeat_until_paused_resume_resets_prompt_after_inner_choice() {
        // CR 107.1c: when a `ControllerChoice` iteration pauses on an inner
        // player choice, `pending_repeat_until` is stashed and
        // `drain_pending_continuation` re-sets `WaitingFor::RepeatDecision`
        // once the choice drains.
        let mut state = GameState::new_two_player(42);

        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_until = Some(RepeatContinuation::ControllerChoice);

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.pending_repeat_until = Some(crate::types::game_state::PendingRepeatUntil {
            ability: Box::new(ability),
        });

        let mut events = Vec::new();
        drain_pending_continuation(&mut state, &mut events);

        assert!(
            state.pending_repeat_until.is_none(),
            "the resume slot must be consumed by the drain",
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::RepeatDecision { .. }),
            "the drain re-sets the repeat prompt, got {:?}",
            state.waiting_for,
        );
    }

    #[test]
    fn repeat_for_draws_multiple_cards() {
        // CR 609.3: repeat_for = Fixed(3) with Draw(1) should draw 3 cards
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            crate::game::zones::create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Fixed { value: 3 });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            3,
            "repeat_for=3 with Draw(1) should draw 3 cards"
        );
    }

    /// CR 609.3 + CR 701.34a: Engine e2e for Expand the Sphere's swallowed
    /// proliferate sub-ability (swallowed-clause plan unit 7e).
    ///
    /// The parser threads `repeat_for: Difference { Ref(TrackedSetSize),
    /// Fixed(2) }` onto the Proliferate sub-ability — proven separately by the
    /// `oracle_effect` parser test
    /// `expand_the_sphere_difference_repeat_threads_onto_proliferate_sub`.
    /// This helper proves the RUNTIME half: `fold_compose` resolves that
    /// `Difference` against the Dig-published tracked set and the proliferate
    /// loop honors the resulting count. Returns the +1/+1 counter total on the
    /// sole proliferate-eligible creature after resolution.
    fn run_expand_the_sphere_proliferate(lands_put: usize) -> u32 {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);

        // One creature with a single +1/+1 counter — the only
        // proliferate-eligible permanent, so each iteration is observable as
        // exactly one added counter.
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Bearer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        // Simulate Expand the Sphere's Dig parent having published
        // `lands_put` land objects as the chain tracked set (STEP 0 baseline:
        // a choice-Dig publishes its kept cards). `QuantityRef::TrackedSetSize`
        // reads this set's length.
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        let tracked: Vec<ObjectId> = (0..lands_put).map(|i| ObjectId(2000 + i as u64)).collect();
        state.tracked_object_sets.insert(set_id, tracked);
        state.chain_tracked_set_id = Some(set_id);

        // The Proliferate sub-ability exactly as plan unit 7e's parser emits
        // it: `repeat_for = Difference { Ref(TrackedSetSize), Fixed(2) }`.
        let mut ability =
            ResolvedAbility::new(Effect::Proliferate, vec![], ObjectId(100), PlayerId(0));
        ability.repeat_for = Some(QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            }),
            right: Box::new(QuantityExpr::Fixed { value: 2 }),
        });

        let mut events = Vec::new();
        // Depth 1: the proliferate sub-ability runs inside Expand the Sphere's
        // outer chain (the Dig parent publishes the tracked set first).
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // CR 701.34a: drive each iteration's ProliferateChoice by selecting the
        // counter-bearing creature; the repeat loop resumes via drain.
        let mut guard = 0;
        while let WaitingFor::ProliferateChoice { player, .. } = state.waiting_for.clone() {
            apply(
                &mut state,
                player,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature)],
                },
            )
            .unwrap();
            guard += 1;
            assert!(guard < 10, "proliferate loop failed to terminate");
        }

        *state.objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0)
    }

    #[test]
    fn expand_the_sphere_zero_lands_proliferates_twice() {
        // 0 lands put this way → difference |0 - 2| = 2 → proliferate twice →
        // the creature's +1/+1 counter total goes 1 → 2 → 3.
        assert_eq!(run_expand_the_sphere_proliferate(0), 3);
    }

    #[test]
    fn expand_the_sphere_two_lands_proliferates_zero_times() {
        // CR 609.3: 2 lands put this way → difference |2 - 2| = 0 →
        // proliferate zero times → the counter total stays at 1.
        assert_eq!(run_expand_the_sphere_proliferate(2), 1);
    }

    /// CR 603.7 + CR 109.5 + CR 701.23a: Winds of Abandon-shape — per-iteration
    /// parent-target rebinding for `repeat_for: TrackedSetSize` over a
    /// `ParentTargetController` search. Two creatures controlled by *different*
    /// opponents (P1 and P2) are exiled. Without the per-iteration rebind both
    /// iterations would prompt the same player; with the rebind, the FIRST
    /// iteration must prompt the controller of the FIRST tracked-set member
    /// specifically (not just "some opponent"), proving the rebind is the only
    /// mechanism that places the per-iteration creature as the parent.
    ///
    /// Critical: `ability.targets` starts EMPTY. Without the rebind path the
    /// SearchLibrary resolver would fall through to `ability.controller`
    /// (the caster, P0) rather than to either creature's controller, so the
    /// assertion below is reachable only through the rebind.
    #[test]
    fn repeat_for_rebinds_parent_target_to_tracked_set_member_per_iteration() {
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::format::FormatConfig;

        // 3-player game so each tracked-set member can have a distinct
        // controller — proves the rebind picks per-iteration, not "any opponent".
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        // Seed P1's and P2's libraries with basic lands so the search finds
        // matching cards in each opponent's library.
        for (lib_owner, card_id, name) in [
            (PlayerId(1), CardId(60), "Forest"),
            (PlayerId(2), CardId(61), "Plains"),
        ] {
            let land = create_object(
                &mut state,
                card_id,
                lib_owner,
                name.to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        // Publish a chain-scoped tracked set listing both creatures in order.
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        // ability.targets is EMPTY: the only way for SearchLibrary's
        // ParentTargetController to resolve to any opponent is via the
        // per-iteration rebind populating targets[0] with the i-th member.
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9000),
            PlayerId(0), // caster is P0
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        // Depth=1 simulates being inside a larger chain (Winds of Abandon's
        // outer chain publishes the tracked set in its first sub-ability).
        // Calling at depth=0 would clear `chain_tracked_set_id` per CR 603.7's
        // chain-local reset, defeating the test's setup.
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // First iteration must prompt P1 — controller of `creature_a`, the
        // FIRST tracked-set member. If the rebind didn't run, this would
        // resolve via `ability.controller` (P0) — which is not an
        // opponent — and the SearchLibrary would never set
        // WaitingFor::SearchChoice for P1 specifically.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "first iteration must prompt the controller of the FIRST tracked-set member (P1, not P2 or P0)"
                );
            }
            other => panic!("expected SearchChoice, got {:?}", other),
        }

        // The remaining iteration must be stashed in `pending_repeat_iteration`
        // so subsequent SearchChoice resolutions resume the loop.
        let pending = state
            .pending_repeat_iteration
            .as_ref()
            .expect("second iteration must be stashed for resumption");
        assert_eq!(pending.next_iteration, 1);
        assert_eq!(pending.total_iterations, 2);
        assert_eq!(pending.tracked_members, vec![creature_a, creature_b]);
    }

    /// Issue #687 + CR 707.2 + CR 608.2: "For each token you control, create a
    /// token that's a copy of that permanent" (Second Harvest) copies each
    /// DISTINCT token you control — not the spell itself. The copy source is
    /// `ParentTarget` and the loop is `repeat_for: ObjectCount`; each iteration
    /// must rebind `ParentTarget` to the i-th controlled token. Before the
    /// ObjectCount rebind, `ParentTarget` was unbound and `CopyTokenOf` fell back
    /// to the source object, producing degenerate copies of Second Harvest.
    #[test]
    fn second_harvest_copies_each_controlled_token_not_the_source() {
        let mut state = GameState::new_two_player(42);

        let bear = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let wolf = create_object(
            &mut state,
            CardId(51),
            PlayerId(0),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        for id in [bear, wolf] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.is_token = true;
            obj.controller = PlayerId(0);
            obj.card_types.core_types = vec![CoreType::Creature];
        }

        // The resolving Second Harvest spell — the (wrong) fallback copy source
        // the bug produced copies of.
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Second Harvest".to_string(),
            Zone::Stack,
        );

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "For each token you control, create a token that's a copy of that permanent.",
            AbilityKind::Spell,
        );
        let ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Count tokens on the battlefield by name. `last_created_token_ids` is
        // overwritten per `CopyTokenOf` call, so inspect the battlefield directly.
        // CR 608.2: the set is snapshotted before any copy enters, so exactly one
        // copy is made per pre-existing controlled token (the new copies are not
        // themselves copied) — leaving 2 Bears + 2 Wolves (each original plus its
        // copy) and, critically, zero copies of Second Harvest itself.
        let mut token_names: Vec<String> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.is_token)
            .map(|obj| obj.name.clone())
            .collect();
        token_names.sort();
        assert_eq!(
            token_names,
            vec![
                "Bear".to_string(),
                "Bear".to_string(),
                "Wolf".to_string(),
                "Wolf".to_string()
            ],
            "each controlled token gets one copy; nothing copies Second Harvest itself"
        );

        // CR 111.2: every resulting token (originals and copies) is under the
        // caster's control — the copies don't leak to another player.
        for obj in state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.is_token)
        {
            assert_eq!(obj.controller, PlayerId(0), "copies enter under the caster");
        }
    }

    /// CR 609.3 + CR 109.5: End-to-end iteration resumption — overloaded Winds
    /// of Abandon shape across two distinct opponent controllers. After the
    /// FIRST iteration's SearchChoice is resolved (P1 picks a basic land), the
    /// loop must resume and prompt the SECOND opponent (P2) for their own
    /// search. Without the `pending_repeat_iteration` infrastructure, only
    /// the first iteration would ever fire.
    #[test]
    fn repeat_for_resumes_iteration_after_search_choice_resolves() {
        use crate::game::engine::apply;
        use crate::types::ability::SearchSelectionConstraint;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        // Seed each opponent's library with one basic land.
        let p1_forest = create_object(
            &mut state,
            CardId(60),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p1_forest).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        let p2_plains = create_object(
            &mut state,
            CardId(61),
            PlayerId(2),
            "Plains".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p2_plains).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        // Depth=1: simulate being inside Winds of Abandon's outer chain (the
        // tracked set is published by the parent sub-ability before this
        // iteration loop runs). See sibling test for rationale.
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Iteration 0: P1 prompted.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(*player, PlayerId(1)),
            other => panic!("expected SearchChoice for P1, got {:?}", other),
        }

        // P1 picks the Forest. After resolving, the loop must resume and
        // prompt P2 for the second iteration.
        apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards {
                cards: vec![p1_forest],
            },
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(2),
                "second iteration must prompt the controller of the SECOND tracked-set member (P2). \
                 Without iteration resumption, only the first iteration would ever fire."
            ),
            other => panic!(
                "expected SearchChoice for P2 after P1 resolves, got {:?}. \
                 This indicates the repeat_for loop did not resume.",
                other
            ),
        }

        // P2 picks the Plains; the loop should now complete with no further
        // pending iteration.
        apply(
            &mut state,
            PlayerId(2),
            GameAction::SelectCards {
                cards: vec![p2_plains],
            },
        )
        .unwrap();

        assert!(
            state.pending_repeat_iteration.is_none(),
            "loop must clear pending_repeat_iteration after final iteration completes"
        );
    }

    #[test]
    fn effect_zone_choice_publishes_chosen_cards_for_plotted_continuation() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Make Your Own Luck".to_string(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "First Card".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second Card".to_string(),
            Zone::Library,
        );
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![first, second]);
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![first, second],
            count: 1,
            min_count: 0,
            up_to: false,
            source_id: source,
            effect_kind: EffectKind::ChangeZone,
            zone: Zone::Library,
            destination: Some(Zone::Exile),
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
            enters_modified_if: None,
        };
        state.pending_continuation =
            Some(PendingContinuation::new(Box::new(ResolvedAbility::new(
                Effect::GrantCastingPermission {
                    permission: CastingPermission::Plotted { turn_plotted: 0 },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: PermissionGrantee::ObjectOwner,
                },
                vec![],
                source,
                PlayerId(0),
            ))));
        let mut events = Vec::new();

        let _outcome = crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                cards: vec![first, second],
                count: 1,
                min_count: 0,
                up_to: false,
                source_id: source,
                effect_kind: EffectKind::ChangeZone,
                zone: Zone::Library,
                destination: Some(Zone::Exile),
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
                enters_modified_if: None,
            },
            GameAction::SelectCards {
                cards: vec![second],
            },
            &mut events,
        )
        .unwrap();

        assert_eq!(state.objects[&second].zone, Zone::Exile);
        assert_eq!(
            state.objects[&second].casting_permissions,
            vec![CastingPermission::Plotted { turn_plotted: 1 }]
        );
        assert!(state.objects[&first].casting_permissions.is_empty());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::BecomesPlotted {
                object_id,
                player_id: PlayerId(0)
            } if *object_id == second
        )));
    }

    /// CR 609.3 + CR 109.5 + CR 701.23i: End-to-end Winds of Abandon shape —
    /// the resumed iteration MUST run its full sub_ability chain
    /// (put-onto-battlefield + shuffle), not just the SearchLibrary effect.
    /// Without preserving `sub_ability` on the resumed `pending_repeat_iteration`,
    /// the FIRST opponent's chosen card lands on the battlefield (iteration 0
    /// goes through the line-1660 SearchChoice continuation wiring), but the
    /// SECOND opponent's chosen card is silently lost — the resume path
    /// previously called `resolve_effect` directly and never wired the
    /// continuation. This test asserts BOTH opponents' cards land on the
    /// battlefield AND the search emits a Shuffle for each iteration.
    #[test]
    fn repeat_for_resumed_iteration_runs_full_sub_ability_chain() {
        use crate::game::engine::apply;
        use crate::types::ability::{EffectKind, SearchSelectionConstraint};
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let creature_a = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let creature_b = create_object(
            &mut state,
            CardId(51),
            PlayerId(2),
            "Wolf".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&creature_a).unwrap().controller = PlayerId(1);
        state.objects.get_mut(&creature_b).unwrap().controller = PlayerId(2);

        let p1_forest = create_object(
            &mut state,
            CardId(60),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p1_forest).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        let p2_plains = create_object(
            &mut state,
            CardId(61),
            PlayerId(2),
            "Plains".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&p2_plains).unwrap();
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Land];
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }

        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(set_id, vec![creature_a, creature_b]);
        state.chain_tracked_set_id = Some(set_id);

        // Build the full Winds of Abandon sub-chain:
        //   SearchLibrary (repeat_for=TrackedSetSize, target_player=ParentTargetController)
        //     -> ChangeZone (Library -> Battlefield, enter_tapped=true)
        //       -> Shuffle (target=ParentTargetController)
        let shuffle = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::ParentTargetController,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        );
        let put = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(shuffle);
        let mut search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(put);
        search.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut all_events: Vec<GameEvent> = Vec::new();
        // depth=1 to preserve the chain-scoped tracked set we published above.
        resolve_ability_chain(&mut state, &search, &mut all_events, 1).unwrap();

        // Iteration 0: P1 prompted; P1 picks the Forest.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(*player, PlayerId(1)),
            other => panic!("expected SearchChoice for P1, got {:?}", other),
        }
        let r1 = apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards {
                cards: vec![p1_forest],
            },
        )
        .unwrap();
        all_events.extend(r1.events);

        // Iteration 1: P2 prompted; P2 picks the Plains.
        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(2),
                "iteration 1 must prompt P2 — controller of the SECOND tracked-set member"
            ),
            other => panic!("expected SearchChoice for P2, got {:?}", other),
        }
        let r2 = apply(
            &mut state,
            PlayerId(2),
            GameAction::SelectCards {
                cards: vec![p2_plains],
            },
        )
        .unwrap();
        all_events.extend(r2.events);

        // Both chosen lands MUST be on the battlefield. This is the regression
        // that the resumed-iteration `sub_ability` preservation guards against
        // — without it, p2_plains would still be in P2's library.
        let forest_zone = state.objects.get(&p1_forest).unwrap().zone;
        let plains_zone = state.objects.get(&p2_plains).unwrap().zone;
        assert_eq!(
            forest_zone,
            Zone::Battlefield,
            "P1's chosen Forest must be on the battlefield (iteration 0's sub_ability)"
        );
        assert_eq!(
            plains_zone,
            Zone::Battlefield,
            "P2's chosen Plains must be on the battlefield — failure means iteration 1's \
             sub_ability (put-onto-battlefield) was dropped on the resume path."
        );

        // Both controllers must own their respective lands on their side.
        assert_eq!(
            state.objects.get(&p1_forest).unwrap().controller,
            PlayerId(1),
            "Forest controller is P1"
        );
        assert_eq!(
            state.objects.get(&p2_plains).unwrap().controller,
            PlayerId(2),
            "Plains controller is P2"
        );

        // The Shuffle sub_ability must have run for each iteration. Each
        // Shuffle resolution emits an EffectResolved { kind: Shuffle } event.
        let shuffle_count = all_events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Shuffle,
                        ..
                    }
                )
            })
            .count();
        assert!(
            shuffle_count >= 2,
            "expected at least 2 Shuffle resolutions (one per iteration), got {}. \
             Failure means iteration 1's Shuffle sub_ability was dropped on the resume path.",
            shuffle_count
        );

        assert!(
            state.pending_repeat_iteration.is_none(),
            "loop must clear pending_repeat_iteration after final iteration completes"
        );
    }

    /// CR 608.2c + CR 109.5 + CR 701.23a: Ghost Quarter shape — after a land
    /// is destroyed, "its controller may search their library..." binds the
    /// search and the Library -> Battlefield continuation to the destroyed
    /// land's controller, not the ability controller. `ChangeZone { target:
    /// Any }` is the continuation sentinel for the card selected by
    /// SearchLibrary, so the selected object target must flow through the
    /// pending continuation instead of scanning any player's library.
    #[test]
    fn parent_target_controller_search_puts_chosen_card_onto_that_players_battlefield() {
        use crate::game::engine::apply;
        use crate::types::ability::{EffectKind, SearchSelectionConstraint};

        let mut state = GameState::new_two_player(42);

        let destroyed_land = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Destroyed Land".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&destroyed_land).unwrap().controller = PlayerId(1);

        let p0_basic = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Caster Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&p0_basic)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        state
            .objects
            .get_mut(&p0_basic)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Basic);

        let p1_basic = create_object(
            &mut state,
            CardId(61),
            PlayerId(1),
            "Opponent Plains".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&p1_basic)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Land];
        state
            .objects
            .get_mut(&p1_basic)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Basic);

        let shuffle = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::ParentTargetController,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        );
        let put = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(shuffle);
        let search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![TargetRef::Object(destroyed_land)],
            ObjectId(9000),
            PlayerId(0),
        )
        .sub_ability(put);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &search, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, cards, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "destroyed land's controller must receive the search prompt"
                );
                assert_eq!(
                    cards,
                    &vec![p1_basic],
                    "search must inspect the destroyed land controller's library, not the caster's"
                );
            }
            other => panic!("expected SearchChoice for destroyed land controller, got {other:?}"),
        }

        let result = apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards {
                cards: vec![p1_basic],
            },
        )
        .unwrap();
        events.extend(result.events);

        assert_eq!(
            state.objects.get(&p1_basic).unwrap().zone,
            Zone::Battlefield,
            "chosen basic land must enter the battlefield"
        );
        assert_eq!(
            state.objects.get(&p1_basic).unwrap().controller,
            PlayerId(1),
            "chosen basic land must remain under its owner's control"
        );
        assert_eq!(
            state.objects.get(&p0_basic).unwrap().zone,
            Zone::Library,
            "caster's library must not be searched by the ParentTargetController continuation"
        );
        assert!(state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(1)));
        assert!(!state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(0)));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "shuffle continuation must resolve for the searching player"
        );
    }

    /// CR 609.3 + CR 109.5: Direct unit test of the synchronous-continuation
    /// re-stash predicate inside `drain_pending_repeat_iteration`. Constructs
    /// a multi-iteration resume whose iterations install a `pending_continuation`
    /// without changing `waiting_for`, then verifies the drain detects the
    /// continuation transition and re-stashes the remaining iterations rather
    /// than letting them be silently dropped.
    ///
    /// Strategy: use `ConditionInstead` with `else_ability` set. When the
    /// instead condition is NOT met, line 1486-1487 of `resolve_ability_chain`
    /// stashes the `else_ability` chain into `pending_continuation` whenever
    /// `waiting_for != Priority`. We pre-set `waiting_for` to a non-Priority
    /// state so the stash fires synchronously without any waiting_for change,
    /// directly exercising the new `installed_continuation` predicate.
    #[test]
    fn drain_pending_repeat_iteration_restashes_on_synchronous_continuation() {
        use crate::types::ability::AbilityCondition;
        use crate::types::game_state::PendingRepeatIteration;

        let mut state = GameState::new_two_player(42);
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Pre-seed waiting_for to a non-Priority state so the
        // `ConditionInstead` else-branch stash path fires synchronously
        // (line 1486 requires `waiting_for != Priority`). The drain's
        // `entered_choice` predicate compares against this initial value, so
        // the same waiting_for at end-of-iteration registers as "no
        // transition" — only `installed_continuation` can fire the re-stash.
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: vec![],
            count: 0,
            reveal: false,
            up_to: true,
            allows_partial_find: false,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
            split: None,
        };

        // Build a Draw ability (synchronous, no waiting_for change) with a
        // sub_ability whose condition is `ConditionInstead` carrying an
        // `else_ability`. When the inner condition evaluates to false, the
        // else branch is stashed synchronously into pending_continuation
        // via line 1486.
        let else_branch = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            // Pick a condition that evaluates to false in this state so the
            // swap does NOT fire and the else branch stash path runs.
            inner: Box::new(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
        });
        sub.else_ability = Some(Box::new(else_branch));

        let mut iter_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        iter_ability.repeat_for = None;

        // Stage a resume of 3 iterations starting at iteration 1.
        state.pending_repeat_iteration = Some(PendingRepeatIteration {
            ability: Box::new(iter_ability),
            tracked_members: vec![],
            iterated_counter_kinds: vec![],
            next_iteration: 1,
            total_iterations: 3,
        });

        let mut events = Vec::new();
        super::drain_pending_repeat_iteration(&mut state, &mut events);

        // Iteration 1 ran: parent Draw fired (1 card), then the
        // ConditionInstead sub stashed its else_ability into
        // pending_continuation synchronously (no waiting_for change). The
        // drain's `installed_continuation` predicate must observe this
        // transition and re-stash iteration 2 for the next drain pass.
        assert!(
            state.pending_continuation.is_some(),
            "iteration 1 must have installed a synchronous pending_continuation \
             (else_ability of ConditionInstead)"
        );
        let pending = state.pending_repeat_iteration.as_ref().expect(
            "iteration 2 must be re-stashed — without the synchronous-continuation \
             predicate, this would be None and iteration 2 would be silently dropped",
        );
        assert_eq!(
            pending.next_iteration, 2,
            "re-stash must advance to iteration 2"
        );
        assert_eq!(pending.total_iterations, 3);

        // Exactly one iteration's worth of effects fired before the break:
        // iteration 1's parent Draw (1 card). The else_ability chain has not
        // run yet — it is stashed in pending_continuation, awaiting the next
        // drain_pending_continuation call.
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "only iteration 1's parent Draw should have fired before the re-stash break"
        );
    }

    /// CR 603.7 + CR 608.2c: Regression — when `repeat_for` is set but the
    /// effect does NOT use a parent-target reference (e.g. plain Draw), the
    /// per-iteration rebind logic must NOT touch `ability.targets`. Guards
    /// against the new rebind path leaking into unrelated `repeat_for`
    /// callers.
    #[test]
    fn repeat_for_does_not_rebind_when_effect_lacks_parent_ref() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        // Publish a tracked set with a pretend object so the rebind path could
        // misfire if the gate were too loose.
        let dummy = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Dummy".to_string(),
            Zone::Battlefield,
        );
        let set_id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state.tracked_object_sets.insert(set_id, vec![dummy]);
        state.chain_tracked_set_id = Some(set_id);

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // 1 tracked-set member → 1 iteration → 1 card drawn. Targets remain empty
        // (no spurious Object rebind).
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_player_scope_opponent_discard() {
        let mut state = GameState::new_two_player(42);
        // Put a card in opponent's hand for discard
        create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Card C".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0), // controller
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Opponent (PlayerId(1)) should have discarded
        assert!(
            state.players[1].hand.is_empty(),
            "opponent should have discarded their card"
        );
    }

    // Repro for Discord #781 (Wheel of Fortune): "Each player discards their hand,
    // then draws seven cards." EVERY player must discard their own hand and draw 7
    // — not just the spell's controller. Mirrors the parsed AST exactly:
    // Discard{player_scope: All, count: HandSize(Controller)} with a chained
    // Draw{player_scope: All, count: 7}. If the opponent fails to discard, they
    // would end with 2 (kept) + 7 (drawn) = 9 cards, not 7.
    #[test]
    fn wheel_of_fortune_each_player_discards_hand_then_draws_seven() {
        let mut state = GameState::new_two_player(42);

        // Each player: 2 cards in hand, 8 in library (enough to draw 7).
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("P0 Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(1),
                format!("P1 Hand {i}"),
                Zone::Hand,
            );
        }
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(300 + i),
                PlayerId(0),
                format!("P0 Lib {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(400 + i),
                PlayerId(1),
                format!("P1 Lib {i}"),
                Zone::Library,
            );
        }

        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 7 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);

        let mut wheel = ResolvedAbility::new(
            Effect::Discard {
                // Post-fix AST: "their hand" under an each-player scope binds to
                // the iterated player (ScopedPlayer), not the caster (#781).
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        wheel.player_scope = Some(PlayerFilter::All);
        wheel.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &wheel, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            7,
            "controller discards their hand then draws 7"
        );
        assert_eq!(
            state.players[1].hand.len(),
            7,
            "OPPONENT must also discard their hand then draw 7 (#781)"
        );
    }

    /// Tolarian Winds class: controller discards their hand, then draws that many
    /// (EventContextAmount). Without stamping `last_effect_count` after a
    /// non-`player_scope` Discard, the chained Draw resolves to 0.
    #[test]
    fn discard_hand_then_draw_event_context_amount_without_player_scope() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(20 + i),
                PlayerId(0),
                format!("Lib {i}"),
                Zone::Library,
            );
        }

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut winds = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        winds.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &winds, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            3,
            "controller should draw as many cards as they discarded"
        );
    }

    #[test]
    fn evelyn_chain_exiles_each_players_top_card_with_collection_counter_and_permission() {
        let mut state = GameState::new_two_player(42);
        let evelyn = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Evelyn, the Covetous".to_string(),
            Zone::Battlefield,
        );
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Top".to_string(),
            Zone::Library,
        );

        let grant = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    granted_to: PlayerId(0),
                    frequency: CastFrequency::OncePerTurn,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: PermissionGrantee::AbilityController,
            },
            vec![],
            evelyn,
            PlayerId(0),
        );
        let mut put_counter = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Generic("collection".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
            },
            vec![],
            evelyn,
            PlayerId(0),
        );
        put_counter.sub_ability = Some(Box::new(grant));
        let mut exile = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            evelyn,
            PlayerId(0),
        );
        exile.player_scope = Some(PlayerFilter::All);
        exile.sub_ability = Some(Box::new(put_counter));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &exile, &mut events, 0).unwrap();

        for exiled in [p0_top, p1_top] {
            let obj = state.objects.get(&exiled).unwrap();
            assert_eq!(obj.zone, Zone::Exile);
            assert_eq!(
                obj.counters
                    .get(&CounterType::Generic("collection".to_string())),
                Some(&1)
            );
            assert!(obj.casting_permissions.iter().any(|permission| {
                matches!(
                    permission,
                    CastingPermission::PlayFromExile {
                        granted_to: PlayerId(0),
                        frequency: CastFrequency::OncePerTurn,
                        source_id: Some(source),
                        mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                        ..
                    } if *source == evelyn
                )
            }));
        }
        assert!(
            state.exile_links.is_empty(),
            "tracked-set PlayFromExile permission must not create source exile links"
        );
    }

    #[test]
    fn player_scope_exile_links_for_exiled_by_source_tail_without_tracked_set() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Linked Exile Source".to_string(),
            Zone::Battlefield,
        );
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Top".to_string(),
            Zone::Library,
        );

        let move_exiled = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::ExiledBySource,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut exile = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        exile.player_scope = Some(PlayerFilter::All);
        exile.sub_ability = Some(Box::new(move_exiled));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &exile, &mut events, 0).unwrap();

        assert_eq!(state.objects[&p0_top].zone, Zone::Graveyard);
        assert_eq!(state.objects[&p1_top].zone, Zone::Graveyard);
        assert!(
            state.exile_links.is_empty(),
            "ExiledBySource tail must consume the temporary source links"
        );
    }

    #[test]
    fn player_scope_exile_until_links_for_exiled_by_source_tail() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Linked Exile Until Source".to_string(),
            Zone::Battlefield,
        );
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Top".to_string(),
            Zone::Library,
        );

        let move_exiled = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::ExiledBySource,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut exile_until = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: TargetFilter::Any,
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        exile_until.player_scope = Some(PlayerFilter::All);
        exile_until.sub_ability = Some(Box::new(move_exiled));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &exile_until, &mut events, 0).unwrap();

        assert_eq!(state.objects[&p0_top].zone, Zone::Graveyard);
        assert_eq!(state.objects[&p1_top].zone, Zone::Graveyard);
        assert!(
            state.exile_links.is_empty(),
            "ExileFromTopUntil ExiledBySource tail must consume the temporary source links"
        );
    }

    #[test]
    fn player_scope_interactive_exile_tracks_for_exiled_by_source_tail() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Interactive Linked Exile Source".to_string(),
            Zone::Battlefield,
        );
        let p0_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Grave A".to_string(),
            Zone::Graveyard,
        );
        let p0_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "P0 Grave B".to_string(),
            Zone::Graveyard,
        );
        let p1_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "P1 Grave A".to_string(),
            Zone::Graveyard,
        );
        let p1_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "P1 Grave B".to_string(),
            Zone::Graveyard,
        );

        let move_exiled = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::ExiledBySource,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut exile = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        exile.player_scope = Some(PlayerFilter::All);
        exile.sub_ability = Some(Box::new(move_exiled));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &exile, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                destination: Some(Zone::Exile),
                track_exiled_by_source,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert!(cards.contains(&p0_a));
                assert!(cards.contains(&p0_b));
                assert!(*track_exiled_by_source);
            }
            other => panic!("expected first EffectZoneChoice, got {other:?}"),
        }

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![p0_a] },
        )
        .unwrap();

        assert_eq!(state.objects[&p0_a].zone, Zone::Exile);
        assert!(state.exile_links.iter().any(|link| {
            link.exiled_id == p0_a
                && link.source_id == source
                && matches!(link.kind, ExileLinkKind::TrackedBySource)
        }));
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                destination: Some(Zone::Exile),
                track_exiled_by_source,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(cards.contains(&p1_a));
                assert!(cards.contains(&p1_b));
                assert!(*track_exiled_by_source);
            }
            other => panic!("expected second EffectZoneChoice, got {other:?}"),
        }

        crate::game::engine::apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectCards { cards: vec![p1_a] },
        )
        .unwrap();

        assert_eq!(state.objects[&p0_a].zone, Zone::Graveyard);
        assert_eq!(state.objects[&p1_a].zone, Zone::Graveyard);
        assert_eq!(state.objects[&p0_b].zone, Zone::Graveyard);
        assert_eq!(state.objects[&p1_b].zone, Zone::Graveyard);
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn resolve_ability_chain_player_scope_opponent_sacrifice_uses_scoped_controller() {
        let mut state = GameState::new_two_player(42);
        let own = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Own Creature".to_string(),
            Zone::Battlefield,
        );
        let opp_a = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opp Creature A".to_string(),
            Zone::Battlefield,
        );
        let opp_b = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Opp Creature B".to_string(),
            Zone::Battlefield,
        );
        let opp_c = create_object(
            &mut state,
            CardId(22),
            PlayerId(1),
            "Opp Creature C".to_string(),
            Zone::Battlefield,
        );
        let opp_d = create_object(
            &mut state,
            CardId(23),
            PlayerId(1),
            "Opp Creature D".to_string(),
            Zone::Battlefield,
        );
        for id in [own, opp_a, opp_b, opp_c, opp_d] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                count: QuantityExpr::Fixed { value: 3 },
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(cards.len(), 4);
                assert!(cards.contains(&opp_a));
                assert!(cards.contains(&opp_b));
                assert!(cards.contains(&opp_c));
                assert!(cards.contains(&opp_d));
                assert!(!cards.contains(&own));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn player_scope_runs_unscoped_tail_once_after_scoped_iterations() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(20),
            PlayerId(2),
            "P2 Card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "P0 Draw".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "P0 Extra".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[1].hand.len(), 0);
        assert_eq!(state.players[2].hand.len(), 0);
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "unscoped tail must resolve once after all opponent iterations"
        );
    }

    #[test]
    fn player_scope_opponent_counter_then_unscoped_draw() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "P0 Draw".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::GivePlayerCounter {
                counter_kind: PlayerCounterKind::Poison,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].poison_counters, 0);
        assert_eq!(state.players[1].poison_counters, 1);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[1].hand.len(), 0);
    }

    #[test]
    fn resolve_ability_chain_player_scope_all_draw() {
        let mut state = GameState::new_two_player(42);
        // Add a card in each player's library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0), // controller
        );
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Both players should have drawn a card
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "controller should have drawn a card"
        );
        assert_eq!(
            state.players[1].hand.len(),
            1,
            "opponent should have drawn a card"
        );
    }

    #[test]
    fn player_scope_discard_then_dark_deal_draws_per_players_discard_count_minus_one() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("P0 Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(20 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(30 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }
        create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "P1 Hand".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                    offset: -1,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        ability.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[1].hand.len(), 0);
        assert_eq!(state.players[0].graveyard.len(), 3);
        assert_eq!(state.players[1].graveyard.len(), 1);
    }

    #[test]
    fn player_scope_discard_then_windfall_draws_greatest_discard_count() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(50 + i),
                PlayerId(0),
                format!("P0 Hand {i}"),
                Zone::Hand,
            );
            create_object(
                &mut state,
                CardId(60 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(70 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }
        create_object(
            &mut state,
            CardId(80),
            PlayerId(1),
            "P1 Hand".to_string(),
            Zone::Hand,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(101),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(101),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        ability.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 3);
        assert_eq!(state.players[1].hand.len(), 3);
        assert_eq!(state.players[0].graveyard.len(), 3);
        assert_eq!(state.players[1].graveyard.len(), 1);
    }

    /// CR 608.2c + CR 118.12 + CR 701.9: Read the Runes — draw X, then for
    /// each card drawn discard unless you sacrifice; declining the sacrifice
    /// performs the discard.
    #[test]
    fn read_the_runes_declining_sacrifice_discards_per_card_drawn() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(300 + i),
                PlayerId(0),
                format!("Library {i}"),
                Zone::Library,
            );
        }
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(400 + i),
                PlayerId(0),
                format!("Hand Card {i}"),
                Zone::Hand,
            );
        }
        let source = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Read the Runes".to_string(),
            Zone::Stack,
        );

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Draw X cards. For each card drawn this way, discard a card unless you sacrifice a permanent.",
            AbilityKind::Spell,
        );
        let mut ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        ability.set_chosen_x_recursive(2);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut unless_prompts = 0;
        while matches!(
            state.waiting_for,
            WaitingFor::UnlessPayment {
                player: PlayerId(0),
                ..
            }
        ) {
            unless_prompts += 1;
            apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();
            let discard_pick = match &state.waiting_for {
                WaitingFor::DiscardChoice { cards, .. } => Some(cards[0]),
                _ => None,
            };
            if let Some(pick) = discard_pick {
                apply_as_current(&mut state, GameAction::SelectCards { cards: vec![pick] })
                    .unwrap();
            }
        }

        assert_eq!(
            unless_prompts, 2,
            "X=2 must produce two unless-sacrifice prompts"
        );
        assert_eq!(
            state.players[0].hand.len(),
            2,
            "draw two then discard two must net the starting hand size"
        );
        assert_eq!(state.players[0].graveyard.len(), 2);
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::UnlessPayment { .. } | WaitingFor::DiscardChoice { .. }
            ),
            "no extra unless/discard prompts after both iterations"
        );
    }

    /// CR 608.2c — building-block discriminator for the per-player reveal-anaphora
    /// chain (issue #1534, Duskmantle Seer). `split_player_scope_chain` must keep a
    /// co-scoped sub-clause that consumes the reveal's per-player object referent
    /// ("loses life equal to that card's mana value", "puts it into their hand")
    /// INSIDE the scoped template — and strip its redundant `player_scope` so it
    /// resolves once per player in the same iteration — while a co-scoped sub-clause
    /// that reads a CROSS-PLAYER aggregate (Windfall's `PreviousEffectAmount`)
    /// detaches as the post-all-iterations tail.
    #[test]
    fn split_player_scope_keeps_reveal_anaphora_chain_but_detaches_aggregate() {
        use crate::types::ability::ObjectScope;

        let make_reveal = || {
            ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: 1,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            )
        };

        // Reveal → LoseLife(that card's MV) → ChangeZone(it → Hand), all scope=All.
        let mut anaphoric = make_reveal();
        anaphoric.player_scope = Some(PlayerFilter::All);
        let mut lose = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Demonstrative,
                    },
                },
                target: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        lose.player_scope = Some(PlayerFilter::All);
        let mut put = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Hand,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        put.player_scope = Some(PlayerFilter::All);
        lose.sub_ability = Some(Box::new(put));
        anaphoric.sub_ability = Some(Box::new(lose));

        let (scoped, tail) = split_player_scope_chain(&anaphoric, &PlayerFilter::All);
        assert!(
            tail.is_none(),
            "the whole reveal-anaphora chain stays inside the scoped template"
        );
        let lose_in_scope = scoped
            .sub_ability
            .as_ref()
            .expect("LoseLife stays attached");
        assert!(
            matches!(lose_in_scope.effect, Effect::LoseLife { .. }),
            "LoseLife is the first kept sub-clause"
        );
        assert!(
            lose_in_scope.player_scope.is_none(),
            "the kept LoseLife's redundant player_scope is cleared so it does not re-loop"
        );
        let put_in_scope = lose_in_scope
            .sub_ability
            .as_ref()
            .expect("ChangeZone stays attached after LoseLife");
        assert!(
            put_in_scope.player_scope.is_none(),
            "the kept ChangeZone's redundant player_scope is cleared too"
        );

        // Reveal -> Draw(that card's MV), all scope=All. This is a latent
        // sibling of the Duskmantle shape: no current card prints this exact
        // per-player draw form, but it consumes the same per-iteration
        // demonstrative quantity and must stay in the scoped template.
        let mut draw_anaphoric = make_reveal();
        draw_anaphoric.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Demonstrative,
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        draw_anaphoric.sub_ability = Some(Box::new(draw));

        let (scoped_draw, tail_draw) =
            split_player_scope_chain(&draw_anaphoric, &PlayerFilter::All);
        assert!(
            tail_draw.is_none(),
            "an anaphoric Draw quantity stays inside the reveal iteration"
        );
        let draw_in_scope = scoped_draw
            .sub_ability
            .as_ref()
            .expect("Draw stays attached");
        assert!(
            matches!(draw_in_scope.effect, Effect::Draw { .. }),
            "Draw is the kept sub-clause"
        );
        assert!(
            draw_in_scope.player_scope.is_none(),
            "the kept Draw's redundant player_scope is cleared"
        );

        // Reveal -> PutCounter(that card's MV counters), all scope=All. The
        // target is deliberately non-anaphoric here; the quantity alone must be
        // enough to keep the consumer in the scoped iteration.
        let mut counter_anaphoric = make_reveal();
        counter_anaphoric.player_scope = Some(PlayerFilter::All);
        let mut put_counter = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Demonstrative,
                    },
                },
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        put_counter.player_scope = Some(PlayerFilter::All);
        counter_anaphoric.sub_ability = Some(Box::new(put_counter));

        let (scoped_counter, tail_counter) =
            split_player_scope_chain(&counter_anaphoric, &PlayerFilter::All);
        assert!(
            tail_counter.is_none(),
            "an anaphoric PutCounter quantity stays inside the reveal iteration"
        );
        let counter_in_scope = scoped_counter
            .sub_ability
            .as_ref()
            .expect("PutCounter stays attached");
        assert!(
            matches!(counter_in_scope.effect, Effect::PutCounter { .. }),
            "PutCounter is the kept sub-clause"
        );
        assert!(
            counter_in_scope.player_scope.is_none(),
            "the kept PutCounter's redundant player_scope is cleared"
        );

        // Reveal → Draw(PreviousEffectAmount): cross-player aggregate, scope=All.
        let mut aggregate = make_reveal();
        aggregate.player_scope = Some(PlayerFilter::All);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::All);
        aggregate.sub_ability = Some(Box::new(draw));

        let (scoped_agg, tail_agg) = split_player_scope_chain(&aggregate, &PlayerFilter::All);
        assert!(
            scoped_agg.sub_ability.is_none(),
            "the aggregate Draw is NOT merged into the reveal iteration"
        );
        let detached = tail_agg.expect("the aggregate Draw detaches as the unscoped tail");
        assert_eq!(
            detached.player_scope,
            Some(PlayerFilter::All),
            "the detached Draw keeps its own player_scope so it fans out post-all-reveals"
        );
    }

    #[test]
    fn player_scope_preserves_controller_for_you_quantities() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life_gained_this_turn = 3;
        state.players[1].life_gained_this_turn = 0;

        for i in 0..5 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                format!("Opponent Card {i}"),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "opponent should mill based on the original controller's life gained"
        );
        assert_eq!(state.players[1].library.len(), 2);
    }

    #[test]
    fn resolve_ability_chain_evaluates_condition_per_scoped_player() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Card B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        });
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 1);
    }

    #[test]
    fn quantity_condition_uses_original_controller_during_player_scope() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Condition Source".to_string(),
            Zone::Battlefield,
        );
        let controller_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Creature".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for (id, toughness) in [(controller_creature, 40), (opponent_creature, 1)] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(toughness);
            obj.base_toughness = Some(toughness);
        }

        let condition = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Toughness,
                    filter: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 40 },
        };
        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Controller),
            },
            vec![],
            source,
            PlayerId(1),
        )
        .condition(condition);
        ability.original_controller = Some(PlayerId(0));
        ability.scoped_player = Some(PlayerId(1));

        assert!(
            evaluate_condition(ability.condition.as_ref().unwrap(), &state, &ability),
            "the condition must count P0's creatures, not the scoped opponent's"
        );
    }

    #[test]
    fn resolve_ability_chain_gates_on_source_mana_color_spent() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Spend Check".to_string(),
            Zone::Stack,
        );
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .condition(AbilityCondition::ManaColorSpent {
            color: ManaColor::Black,
            minimum: 1,
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.players[0].hand.len(), 0);

        state
            .objects
            .get_mut(&source)
            .unwrap()
            .colors_spent_to_cast
            .add(ManaColor::Black, 1);
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn player_scope_zone_changed_this_way_filters_by_owner() {
        let mut state = GameState::new_two_player(42);

        // Create objects owned by Player 0 in graveyard (simulating milled cards)
        let obj_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Milled A".to_string(),
            Zone::Graveyard,
        );
        let obj_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Milled B".to_string(),
            Zone::Graveyard,
        );

        // Simulate that these objects were zone-changed by the preceding effect
        state.last_zone_changed_ids = vec![obj_a, obj_b];

        // Add library cards so Draw has something to draw
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::ZoneChangedThisWay);

        let mut events = Vec::new();
        // Use depth=1 to simulate sub_ability execution — depth=0 clears last_zone_changed_ids
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Only Player 0 owned the zone-changed objects, so only they draw
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "player 0 should have drawn (owned zone-changed objects)"
        );
        assert!(
            state.players[1].hand.is_empty(),
            "player 1 should NOT have drawn (no owned zone-changed objects)"
        );
    }

    #[test]
    fn player_scope_zone_changed_this_way_includes_both_when_both_own() {
        let mut state = GameState::new_two_player(42);

        // Objects owned by different players
        let obj_p0 = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "P0 Card".to_string(),
            Zone::Graveyard,
        );
        let obj_p1 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Graveyard,
        );

        state.last_zone_changed_ids = vec![obj_p0, obj_p1];

        // Library cards for both
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::ZoneChangedThisWay);

        let mut events = Vec::new();
        // Use depth=1 to simulate sub_ability execution — depth=0 clears last_zone_changed_ids
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        // Both players owned zone-changed objects, so both draw
        assert_eq!(state.players[0].hand.len(), 1, "player 0 should have drawn");
        assert_eq!(state.players[1].hand.len(), 1, "player 1 should have drawn");
    }

    /// CR 608.2c + CR 701.38: `PlayerFilter::VotedFor { choice_index }`
    /// matches only players whose ballot in `state.last_vote_ballots`
    /// has the recorded choice index. Mirrors the ZoneChangedThisWay shape.
    #[test]
    fn voted_for_filter_matches_only_recorded_choosers() {
        let mut state = GameState::new_two_player(42);

        // Library cards for both players so Draw has something to do.
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        // Seed the ballot ledger: P0 voted for choice 0, P1 voted for choice 1.
        state.last_vote_ballots = crate::im::Vector::new();
        state.last_vote_ballots.push_back((PlayerId(0), 0));
        state.last_vote_ballots.push_back((PlayerId(1), 1));

        // Effect with player_scope = VotedFor { 0 } — only P0 should resolve.
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::VotedFor { choice_index: 0 });

        let mut events = Vec::new();
        // depth=1 so the chain entry doesn't clear last_vote_ballots.
        resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

        assert_eq!(state.players[0].hand.len(), 1, "P0 voted for 0 — draws");
        assert_eq!(
            state.players[1].hand.len(),
            0,
            "P1 voted for 1 — does NOT draw under VotedFor {{ 0 }}"
        );
    }

    /// CR 608.2c + CR 701.38: `last_vote_ballots` is cleared at chain depth 0,
    /// so a `VotedFor` filter evaluated in a fresh top-level resolution
    /// matches no players (no ballots recorded yet).
    #[test]
    fn voted_for_filter_clears_at_chain_boundary() {
        let mut state = GameState::new_two_player(42);

        // Library cards so Draw can resolve.
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Lib B".to_string(),
            Zone::Library,
        );

        // Seed ballot ledger from a "previous" vote.
        state.last_vote_ballots = crate::im::Vector::new();
        state.last_vote_ballots.push_back((PlayerId(0), 0));
        state.last_vote_ballots.push_back((PlayerId(1), 0));

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::VotedFor { choice_index: 0 });

        let mut events = Vec::new();
        // depth=0 — top-level resolution clears last_vote_ballots before the
        // player_scope expansion runs, so no player matches.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            0,
            "P0 should not draw — ledger cleared at chain depth 0"
        );
        assert_eq!(
            state.players[1].hand.len(),
            0,
            "P1 should not draw — ledger cleared at chain depth 0"
        );
    }

    /// CR 608.2c + CR 109.5: "for each opponent who searched their library
    /// this way" relies on `player_actions_this_way` accumulating across
    /// player_scope iterations.
    #[test]
    fn player_actions_this_way_accumulates_across_player_scope_iterations() {
        use crate::types::format::FormatConfig;

        // 3-player game: P0 (controller), P1, P2 (opponents).
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        // Search with player_scope: Opponent. Empty libraries still emit the
        // SearchedLibrary action, which is exactly what "searched this way"
        // means; no ZoneChanged event is required.
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        assert!(
            state
                .player_actions_this_way
                .contains(&(PlayerId(1), action)),
            "P1 searched and must be recorded in player_actions_this_way"
        );
        assert!(
            state
                .player_actions_this_turn
                .contains(&(PlayerId(1), action)),
            "P1 searched and must be recorded in player_actions_this_turn"
        );
        assert!(
            state
                .player_actions_this_way
                .contains(&(PlayerId(2), action)),
            "P2 searched and must be recorded in player_actions_this_way"
        );
        assert!(
            state
                .player_actions_this_turn
                .contains(&(PlayerId(2), action)),
            "P2 searched and must be recorded in player_actions_this_turn"
        );
    }

    /// CR 608.2c + CR 109.5: `PerformedActionThisWay` resolves to the count
    /// of matching players in the chain-local action accumulator.
    #[test]
    fn performed_action_this_way_player_count_excludes_controller() {
        use crate::game::quantity::resolve_quantity;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);

        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        state.player_actions_this_way.insert((PlayerId(0), action));
        state.player_actions_this_way.insert((PlayerId(1), action));

        let qty = QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: crate::types::ability::PlayerRelation::Opponent,
                    action,
                },
            },
        };
        let count = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));

        // Only P1 counts: P0 is the controller (excluded), P2 is not in the
        // accumulator (declined the offer).
        assert_eq!(
            count, 1,
            "PerformedActionThisWay must exclude controller and count only \
             opponents who performed the action"
        );

        state.player_actions_this_way.insert((PlayerId(2), action));
        let count_all = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));
        assert_eq!(
            count_all, 2,
            "All opponents in accumulator → count of opponents (excl controller) is 2"
        );

        state.player_actions_this_way.remove(&(PlayerId(1), action));
        state.player_actions_this_way.remove(&(PlayerId(2), action));
        let count_none = resolve_quantity(&state, &qty, PlayerId(0), ObjectId(0));
        assert_eq!(
            count_none, 0,
            "No opponents in accumulator → count is 0 (controller is excluded)"
        );
    }

    #[test]
    fn per_opponent_cant_discard_consequence_draws_for_zero_count_players() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(2),
            "Discard Me".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Draw Me".to_string(),
            Zone::Library,
        );

        let mut discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        discard.player_scope = Some(PlayerFilter::Opponent);
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::OriginalController,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        draw.player_scope = Some(PlayerFilter::Opponent);
        draw.condition = Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::LT,
            rhs: QuantityExpr::Fixed { value: 1 },
        });
        discard.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &discard, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            1,
            "P0 draws once for P1, the opponent who could not discard"
        );
        assert_eq!(
            state.players[2].hand.len(),
            0,
            "P2 discarded their only card and should not add a draw"
        );
    }

    /// CR 608.2c + CR 109.5: `player_actions_this_way` clears at depth=0
    /// chain entry — does NOT leak across unrelated top-level resolutions.
    #[test]
    fn player_actions_this_way_clears_at_top_level_chain_entry() {
        let mut state = GameState::new_two_player(42);

        // Pre-pollute the accumulator with stale state from a "previous"
        // resolution.
        let action = crate::types::events::PlayerActionKind::SearchedLibrary;
        state.player_actions_this_way.insert((PlayerId(0), action));
        state.player_actions_this_way.insert((PlayerId(1), action));

        // Add one library card so Draw has something to draw — Draw itself
        // emits no ZoneChanged events (cards moving from library to hand DO
        // emit ZoneChanged, so use a no-zone-change effect to make the test
        // assertion cleaner).
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Lib".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Stale state (SearchedLibrary) must be cleared at depth=0 entry.
        // Shuffle now emits PlayerPerformedAction { ShuffledLibrary } which is
        // correctly accumulated "this way" — only the pre-polluted stale
        // actions must be gone.
        let stale_action = crate::types::events::PlayerActionKind::SearchedLibrary;
        assert!(
            !state
                .player_actions_this_way
                .contains(&(PlayerId(0), stale_action)),
            "depth=0 chain entry must clear stale player_actions_this_way; \
             leaking across top-level resolutions would cause spurious counts \
             in 'opponent who [verbed] this way' references on subsequent spells"
        );
        assert!(
            !state
                .player_actions_this_way
                .contains(&(PlayerId(1), stale_action)),
            "stale P1 SearchedLibrary must also be cleared"
        );
        // The shuffle action itself IS expected in the accumulator — it
        // happened "this way" during the current resolution.
        let shuffle_action = crate::types::events::PlayerActionKind::ShuffledLibrary;
        assert!(
            state
                .player_actions_this_way
                .contains(&(PlayerId(0), shuffle_action)),
            "ShuffledLibrary from current resolution must be accumulated"
        );
    }

    #[test]
    fn player_scope_owners_of_cards_exiled_by_source_creates_owner_sized_tokens() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Battlefield,
        );

        for (card_id, owner, mv) in [
            (101, PlayerId(0), 2u32),
            (102, PlayerId(0), 3),
            (103, PlayerId(1), 4),
        ] {
            let exiled = create_object(
                &mut state,
                CardId(card_id),
                owner,
                format!("Exiled {card_id}"),
                Zone::Exile,
            );
            state.objects.get_mut(&exiled).unwrap().mana_cost = ManaCost::generic(mv);
            state.exile_links.push(ExileLink {
                source_id: source,
                exiled_id: exiled,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                types: vec!["Creature".to_string(), "Illusion".to_string()],
                colors: vec![ManaColor::Blue],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .map(|object| {
                (
                    object.owner,
                    object.controller,
                    object.power,
                    object.toughness,
                )
            })
            .collect();
        created.sort_by_key(|entry| entry.0);

        assert_eq!(
            created,
            vec![
                (PlayerId(0), PlayerId(0), Some(5), Some(5)),
                (PlayerId(1), PlayerId(1), Some(4), Some(4)),
            ]
        );
    }

    #[test]
    fn player_scope_owners_of_cards_exiled_by_source_uses_ltb_snapshot() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Graveyard,
        );
        let exiled_a = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Exiled 101".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_a).unwrap().mana_cost = ManaCost::generic(2);
        let exiled_b = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Exiled 102".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_b).unwrap().mana_cost = ManaCost::generic(3);
        let exiled_c = create_object(
            &mut state,
            CardId(103),
            PlayerId(1),
            "Exiled 103".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled_c).unwrap().mana_cost = ManaCost::generic(4);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: source,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord {
                linked_exile_snapshot: vec![
                    LinkedExileSnapshot {
                        exiled_id: exiled_a,
                        owner: PlayerId(0),
                        mana_value: 2,
                    },
                    LinkedExileSnapshot {
                        exiled_id: exiled_b,
                        owner: PlayerId(0),
                        mana_value: 3,
                    },
                    LinkedExileSnapshot {
                        exiled_id: exiled_c,
                        owner: PlayerId(1),
                        mana_value: 4,
                    },
                ],
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        });

        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Illusion".to_string(),
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate {
                        function: crate::types::ability::AggregateFunction::Sum,
                        property: crate::types::ability::ObjectProperty::ManaValue,
                        filter: TargetFilter::And {
                            filters: vec![
                                TargetFilter::ExiledBySource,
                                TargetFilter::Typed(TypedFilter::default().properties(vec![
                                    FilterProp::Owned {
                                        controller: ControllerRef::You,
                                    },
                                ])),
                            ],
                        },
                    },
                }),
                types: vec!["Creature".to_string(), "Illusion".to_string()],
                colors: vec![ManaColor::Blue],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::OwnersOfCardsExiledBySource);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let mut created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .map(|object| {
                (
                    object.owner,
                    object.controller,
                    object.power,
                    object.toughness,
                )
            })
            .collect();
        created.sort_by_key(|entry| entry.0);

        assert_eq!(
            created,
            vec![
                (PlayerId(0), PlayerId(0), Some(5), Some(5)),
                (PlayerId(1), PlayerId(1), Some(4), Some(4)),
            ]
        );
    }

    /// CR 702.33d + CR 702.33f + CR 608.2c: Default-shape `AdditionalCostPaid`
    /// (variant=None, min_count=1) reads `additional_cost_paid` so legacy
    /// Gift / Buyback / Bargain / Evidence / plain "if it was kicked" gating
    /// stays correct.
    #[test]
    fn additional_cost_paid_default_shape_reads_legacy_bool() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let cond = AbilityCondition::additional_cost_paid_any();
        assert!(!evaluate_condition(&cond, &state, &ability));
        ability.context.additional_cost_paid = true;
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    /// CR 701.30d + CR 608.2c: "if you won" on a triggered clash ability reads
    /// the triggering clash result for the ability controller, not the unrelated
    /// `IfYouDo` flag used by optional costs in the same effect chain.
    #[test]
    fn event_outcome_won_reads_clash_result_for_ability_controller() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.context.optional_effect_performed = false;
        let cond = AbilityCondition::EventOutcomeWon;

        state.current_trigger_event = Some(GameEvent::Clash {
            controller: PlayerId(1),
            opponent: PlayerId(0),
            controller_mana_value: Some(1),
            opponent_mana_value: Some(3),
            result: crate::types::events::ClashResult::Lost,
        });
        assert!(evaluate_condition(&cond, &state, &ability));

        state.current_trigger_event = Some(GameEvent::Clash {
            controller: PlayerId(1),
            opponent: PlayerId(0),
            controller_mana_value: Some(3),
            opponent_mana_value: Some(1),
            result: crate::types::events::ClashResult::Won,
        });
        assert!(!evaluate_condition(&cond, &state, &ability));

        state.current_trigger_event = None;
        ability.context.optional_effect_performed = true;
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    /// CR 701.30b: "Clash with an opponent" lets the clashing player CHOOSE the
    /// opponent. With two or more opponents the engine must pause on
    /// `ClashChooseOpponent` (offering every opponent) instead of silently
    /// clashing with the first opponent in seat order.
    #[test]
    fn clash_with_multiple_opponents_prompts_for_opponent_choice() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let ability = ResolvedAbility::new(Effect::Clash, vec![], ObjectId(1), PlayerId(0));

        let mut events = Vec::new();
        clash::resolve(&mut state, &ability, &mut events).expect("clash resolves");

        match &state.waiting_for {
            WaitingFor::ClashChooseOpponent {
                player, candidates, ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert!(
                    candidates.contains(&PlayerId(1)) && candidates.contains(&PlayerId(2)),
                    "both opponents must be offered, got {candidates:?}"
                );
            }
            other => panic!("expected ClashChooseOpponent, got {other:?}"),
        }

        // CR 701.30b: the controller's chosen opponent — not the first in seat
        // order — is the one that clashes.
        let mut clash_events = Vec::new();
        clash::perform_clash(&mut state, &ability, PlayerId(2), &mut clash_events)
            .expect("clash performs against the chosen opponent");
        assert!(
            clash_events.iter().any(|e| matches!(
                e,
                GameEvent::Clash {
                    opponent: PlayerId(2),
                    ..
                }
            )),
            "clash must be against the chosen opponent PlayerId(2)"
        );
    }

    /// CR 701.30b: With a single opponent there is no decision to make, so a
    /// two-player clash proceeds without a `ClashChooseOpponent` prompt.
    #[test]
    fn clash_with_single_opponent_needs_no_choice() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::Clash, vec![], ObjectId(1), PlayerId(0));

        let mut events = Vec::new();
        clash::resolve(&mut state, &ability, &mut events).expect("clash resolves");
        assert!(
            !matches!(state.waiting_for, WaitingFor::ClashChooseOpponent { .. }),
            "two-player clash must not prompt for an opponent choice"
        );
    }

    /// CR 701.30b: If no opponent exists, there is no legal player to choose
    /// for "clash with an opponent"; the effect is a no-op rather than
    /// manufacturing `PlayerId(1)`.
    #[test]
    fn clash_with_no_opponents_does_not_default_to_invalid_player() {
        let mut state = GameState::new(FormatConfig::standard(), 1, 42);
        let ability = ResolvedAbility::new(Effect::Clash, vec![], ObjectId(1), PlayerId(0));

        let mut events = Vec::new();
        clash::resolve(&mut state, &ability, &mut events).expect("clash no-op succeeds");

        assert!(
            !matches!(state.waiting_for, WaitingFor::ClashChooseOpponent { .. }),
            "no-op clash must not prompt when there are no candidates"
        );
        assert!(
            !events.iter().any(|e| matches!(e, GameEvent::Clash { .. })),
            "no-op clash must not emit a Clash event with a fabricated opponent"
        );
    }

    /// CR 702.33f: variant gating reads `kickers_paid` membership. Mirrors
    /// Ana Battlemage's per-kicker triggers.
    #[test]
    fn additional_cost_paid_variant_reads_kickers_paid() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let first = AbilityCondition::additional_cost_paid_kicker(
            crate::types::ability::KickerVariant::First,
        );
        let second = AbilityCondition::additional_cost_paid_kicker(
            crate::types::ability::KickerVariant::Second,
        );
        assert!(!evaluate_condition(&first, &state, &ability));
        assert!(!evaluate_condition(&second, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::First);
        assert!(evaluate_condition(&first, &state, &ability));
        assert!(!evaluate_condition(&second, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::Second);
        assert!(evaluate_condition(&first, &state, &ability));
        assert!(evaluate_condition(&second, &state, &ability));
    }

    /// CR 702.33b/c: `min_count >= 2` reads `kickers_paid.len()`. Mirrors
    /// Archangel of Wrath's "kicked twice" trigger.
    #[test]
    fn additional_cost_paid_min_count_reads_kickers_paid_len() {
        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let twice = AbilityCondition::additional_cost_paid_n_times(2);
        assert!(!evaluate_condition(&twice, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::First);
        assert!(!evaluate_condition(&twice, &state, &ability));
        ability
            .context
            .kickers_paid
            .push(crate::types::ability::KickerVariant::Second);
        assert!(evaluate_condition(&twice, &state, &ability));
    }

    #[test]
    fn zone_change_object_ability_condition_checks_current_trigger_event_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(210),
            PlayerId(0),
            "Observer".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(211),
            PlayerId(0),
            "Countered Entry".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&entering)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(crate::types::game_state::ZoneChangeRecord {
                core_types: vec![CoreType::Creature],
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    entering,
                    Some(Zone::Hand),
                    Zone::Battlefield,
                )
            }),
        });

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let condition = AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::Counters {
                    counters: crate::types::counter::CounterMatch::Any,
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                },
            ])),
        };

        assert!(evaluate_condition(&condition, &state, &ability));
    }

    #[test]
    fn evaluate_condition_and_all_true() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(false=not negated), IsYourTurn(false=not negated)]) — both true
        let cond = AbilityCondition::And {
            conditions: vec![AbilityCondition::IsYourTurn, AbilityCondition::IsYourTurn],
        };
        assert!(evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn evaluate_condition_first_combat_phase_checks_turn_counter() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        state.combat_phases_started_this_turn = 0;
        assert!(!evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));

        state.combat_phases_started_this_turn = 1;
        assert!(evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));

        state.combat_phases_started_this_turn = 2;
        assert!(!evaluate_condition(
            &AbilityCondition::FirstCombatPhaseOfTurn,
            &state,
            &ability,
        ));
    }

    #[test]
    fn evaluate_condition_current_phase_is_your_main_phase_dose_gate() {
        // CR 505.1 + CR 505.1a + CR 102.1: Dose of Dawnglow's blight gate is
        // Not(And([CurrentPhaseIs{[PreCombatMain, PostCombatMain]}, IsYourTurn])).
        // "if it isn't your main phase" is true exactly when the live phase is
        // NOT one of the controller's two main phases.
        let mut state = GameState::new_two_player(42);
        // The ability's controller is PlayerId(0), so "your" = PlayerId(0)'s turn.
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let dose_gate = AbilityCondition::Not {
            condition: Box::new(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::CurrentPhaseIs {
                        phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
                    },
                    AbilityCondition::IsYourTurn,
                ],
            }),
        };

        // (1) Your precombat main → inner And true → Not false (blight suppressed).
        state.active_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        assert!(!evaluate_condition(&dose_gate, &state, &ability));

        // (2) CR 505.1a: your postcombat main also satisfies the grouped "main
        // phase" set → inner And true → Not false (blight suppressed).
        state.phase = Phase::PostCombatMain;
        assert!(!evaluate_condition(&dose_gate, &state, &ability));

        // (3) Your end step → not a main phase → Not true (blight fires).
        state.phase = Phase::End;
        assert!(evaluate_condition(&dose_gate, &state, &ability));

        // (4) CR 102.1: opponent's precombat main (active = PlayerId(1)) →
        // IsYourTurn false → inner And false → Not true (blight fires). Proves
        // the IsYourTurn conjunct is load-bearing: a main phase that isn't YOURS
        // still counts as "isn't your main phase".
        state.active_player = PlayerId(1);
        state.phase = Phase::PreCombatMain;
        assert!(evaluate_condition(&dose_gate, &state, &ability));
    }

    #[test]
    fn evaluate_condition_bare_current_phase_is_reads_live_phase() {
        // CR 500.1 + CR 608.2c: bare CurrentPhaseIs membership against the live
        // resolution-time phase, independent of whose turn it is.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let cond = AbilityCondition::CurrentPhaseIs {
            phases: vec![Phase::PreCombatMain],
        };
        state.phase = Phase::PreCombatMain;
        assert!(evaluate_condition(&cond, &state, &ability));
        state.phase = Phase::PostCombatMain;
        assert!(!evaluate_condition(&cond, &state, &ability));
        state.phase = Phase::End;
        assert!(!evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn evaluate_condition_is_monarch_checks_ability_controller() {
        let mut state = GameState::new_two_player(42);
        state.monarch = Some(PlayerId(0));
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(evaluate_condition(
            &AbilityCondition::IsMonarch,
            &state,
            &ability
        ));

        let opponent_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(2),
            PlayerId(1),
        );
        assert!(!evaluate_condition(
            &AbilityCondition::IsMonarch,
            &state,
            &opponent_ability
        ));
    }

    #[test]
    fn evaluate_condition_city_blessing_checks_ability_controller() {
        let mut state = GameState::new_two_player(42);
        state.city_blessing.insert(PlayerId(0));
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        assert!(evaluate_condition(
            &AbilityCondition::HasCityBlessing,
            &state,
            &ability,
        ));

        let opponent_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(2),
            PlayerId(1),
        );
        assert!(!evaluate_condition(
            &AbilityCondition::HasCityBlessing,
            &state,
            &opponent_ability,
        ));
    }

    /// CR 115.1 + CR 608.2c + CR 702.33d: `AdditionalCostPaid { subject }` reads
    /// the *target spell's* `kickers_paid` under `ObjectScope::Target`, and the
    /// resolving ability's own (empty) context under `ObjectScope::Source`. This
    /// is the building-block guard behind Ertai's Trickery: the Target path must
    /// see the target's kicker state while the Source path stays blind to it,
    /// proving the two scopes evaluate independently.
    #[test]
    fn evaluate_condition_additional_cost_paid_target_reads_target_object() {
        let mut state = GameState::new_two_player(42);

        // Target spell object whose kicker WAS paid at cast time.
        let mut kicked = crate::game::game_object::GameObject::new(
            ObjectId(7),
            CardId(700),
            PlayerId(0),
            "Kickable Brute".to_string(),
            Zone::Stack,
        );
        kicked
            .kickers_paid
            .push(crate::types::ability::KickerVariant::First);
        state.objects.insert(ObjectId(7), kicked);

        // Target spell object whose kicker was NOT paid.
        let unkicked = crate::game::game_object::GameObject::new(
            ObjectId(8),
            CardId(700),
            PlayerId(0),
            "Kickable Brute".to_string(),
            Zone::Stack,
        );
        state.objects.insert(ObjectId(8), unkicked);

        let target_condition = AbilityCondition::additional_cost_paid_target();

        // ObjectScope::Target reads the FIRST object target's kicker state.
        let counter_kicked = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![TargetRef::Object(ObjectId(7))],
            ObjectId(99),
            PlayerId(1),
        );
        assert!(
            evaluate_condition(&target_condition, &state, &counter_kicked),
            "Target subject must see the kicked target's kickers_paid"
        );

        let counter_unkicked = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![TargetRef::Object(ObjectId(8))],
            ObjectId(99),
            PlayerId(1),
        );
        assert!(
            !evaluate_condition(&target_condition, &state, &counter_unkicked),
            "Target subject must report false when the target was not kicked"
        );

        // The same ability under ObjectScope::Source reads its OWN empty context,
        // not the target object — proving the two paths are independent.
        let source_condition = AbilityCondition::AdditionalCostPaid {
            subject: crate::types::ability::ObjectScope::Source,
            source: crate::types::ability::AdditionalCostPaymentSource::Any,
            origin: None,
            origin_ordinal: None,
            variant: None,
            kicker_cost: None,
            min_count: 1,
        };
        assert!(
            !evaluate_condition(&source_condition, &state, &counter_kicked),
            "Source subject must ignore the target's kicker and read the (empty) own context"
        );
    }

    /// Walk a parsed ability tree and return the condition attached to the first
    /// sub/else ability whose effect matches `want`. Used by the S07 gift-card
    /// resolver tests to drive the REAL parsed condition (not a hand-built one)
    /// through `evaluate_condition`.
    fn s07_find_gated_condition(
        defs: &[AbilityDefinition],
        want: fn(&Effect) -> bool,
    ) -> Option<AbilityCondition> {
        fn walk(d: &AbilityDefinition, want: fn(&Effect) -> bool) -> Option<AbilityCondition> {
            if want(&d.effect) {
                if let Some(c) = &d.condition {
                    return Some(c.clone());
                }
            }
            if let Some(s) = &d.sub_ability {
                if let Some(c) = walk(s, want) {
                    return Some(c);
                }
            }
            if let Some(s) = &d.else_ability {
                if let Some(c) = walk(s, want) {
                    return Some(c);
                }
            }
            None
        }
        defs.iter().find_map(|d| walk(d, want))
    }

    /// T6 (s25 site 2) — CR 603.7c + CR 608.2c: `filter_refs_parent_target`
    /// treats `ParentTargetSlot` like the broad `ParentTarget` anaphor so a
    /// slot-referencing delayed effect snapshots the parent targets at creation
    /// (the index is honored at firing by `effect_object_targets`). Pre-fix the
    /// missing arm fell to `_ => false`, so the snapshot was skipped and the
    /// delayed effect no-opped — reverting the arm flips the first two asserts.
    #[test]
    fn filter_refs_parent_target_detects_parent_target_slot() {
        assert!(filter_refs_parent_target(&TargetFilter::ParentTargetSlot {
            index: 0
        }));
        // Recursion must find a slot ref nested inside a composite filter.
        assert!(filter_refs_parent_target(&TargetFilter::Or {
            filters: vec![
                TargetFilter::Any,
                TargetFilter::ParentTargetSlot { index: 2 },
            ],
        }));
        // Negative control: a plain filter is not a parent-target reference.
        assert!(!filter_refs_parent_target(&TargetFilter::Any));
    }

    /// CR 702.174b + CR 608.2c: Longstalk Brawl — the +1/+1 counter is gated on
    /// "if the gift was promised", parsed onto the `PutCounter` sub_ability as
    /// `additional_cost_paid_any()`. Drives the REAL parsed condition through
    /// `evaluate_condition`: it fires only when the spell's additional cost
    /// (gift promise) was paid. Reverting the gate recognizer drops the
    /// condition (`None`) and `s07_find_gated_condition` returns `None`, failing
    /// the `.expect`.
    #[test]
    fn s07_longstalk_brawl_counter_gated_on_gift_promised() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Choose target creature you control and target creature you don't control. \
             Put a +1/+1 counter on the creature you control if the gift was promised. \
             Then those creatures fight each other.",
            "Longstalk Brawl",
            &["Gift".to_string()],
            &["Sorcery".to_string()],
            &[],
        );
        let cond = s07_find_gated_condition(&parsed.abilities, |e| {
            matches!(e, Effect::PutCounter { .. })
        })
        .expect("Longstalk's counter must carry the gift-promised condition");

        let state = GameState::new_two_player(1);
        let mut ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        ability.context.additional_cost_paid = true;
        assert!(
            evaluate_condition(&cond, &state, &ability),
            "gift promised → counter clause fires"
        );
        ability.context.additional_cost_paid = false;
        assert!(
            !evaluate_condition(&cond, &state, &ability),
            "gift not promised → counter clause does not fire"
        );
    }

    /// CR 702.174b + CR 205.4 + CR 608.2c: Coiling Rebirth — the copy token is
    /// gated on "if the gift was promised and that creature isn't legendary",
    /// parsed onto the `CopyTokenOf` sub_ability as
    /// `And([AdditionalCostPaid, Not(TargetMatchesFilter{Legendary})])`. Drives
    /// the REAL parsed conjunction through `evaluate_condition` against a
    /// legendary vs. nonlegendary FIRST target. Reverting the conjunction
    /// fallback or the "that creature isn't legendary" recognizer drops the
    /// condition, failing the `.expect` or the polarity assertions.
    #[test]
    fn s07_coiling_rebirth_token_gated_on_gift_and_nonlegendary() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Return target creature card from your graveyard to the battlefield. \
             Then if the gift was promised and that creature isn't legendary, create a \
             token that's a copy of that creature, except it's 1/1.",
            "Coiling Rebirth",
            &["Gift".to_string()],
            &["Sorcery".to_string()],
            &[],
        );
        let cond = s07_find_gated_condition(&parsed.abilities, |e| {
            matches!(e, Effect::CopyTokenOf { .. })
        })
        .expect("Coiling's token must carry the gift+nonlegendary condition");

        let mut state = GameState::new_two_player(1);
        // Nonlegendary target (id 7) and legendary target (id 8).
        let nonleg = crate::game::game_object::GameObject::new(
            ObjectId(7),
            CardId(700),
            PlayerId(0),
            "Ordinary Beast".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(7), nonleg);
        let mut legendary = crate::game::game_object::GameObject::new(
            ObjectId(8),
            CardId(701),
            PlayerId(0),
            "Legendary Hero".to_string(),
            Zone::Battlefield,
        );
        legendary
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Legendary);
        state.objects.insert(ObjectId(8), legendary);

        let build = |target: ObjectId, paid: bool| {
            let mut ability = ResolvedAbility::new(
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                    countered_spell_zone: None,
                },
                vec![TargetRef::Object(target)],
                ObjectId(99),
                PlayerId(0),
            );
            ability.context.additional_cost_paid = paid;
            ability
        };

        assert!(
            evaluate_condition(&cond, &state, &build(ObjectId(7), true)),
            "gift promised + nonlegendary returned creature → token created"
        );
        assert!(
            !evaluate_condition(&cond, &state, &build(ObjectId(8), true)),
            "gift promised + LEGENDARY returned creature → no token"
        );
        assert!(
            !evaluate_condition(&cond, &state, &build(ObjectId(7), false)),
            "gift NOT promised → no token even for a nonlegendary creature"
        );
    }

    /// CR 115.1 + CR 608.2c + CR 702.185a: `CastVariantPaid { subject }` reads the
    /// *target permanent's* `cast_variant_paid` marker under `ObjectScope::Target`
    /// (Full Bore's "if that creature was cast for its warp cost") and the
    /// resolving ability's own source under `ObjectScope::Source`. The
    /// discriminating fixture: the SOURCE (the Full Bore spell, ObjectId 99) was
    /// NOT warp-cast — only the TARGET creature was — so a Source-scope read
    /// returns the WRONG answer and only the Target branch is correct. Reverting
    /// the evaluator's Target branch to read the source flips the first assertion
    /// to false.
    #[test]
    fn evaluate_condition_cast_variant_paid_target_reads_target_object() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 7;

        // Target creature that WAS warp-cast this turn.
        let mut warped = crate::game::game_object::GameObject::new(
            ObjectId(7),
            CardId(700),
            PlayerId(0),
            "Warped Brute".to_string(),
            Zone::Battlefield,
        );
        warped.cast_variant_paid = Some((crate::types::ability::CastVariantPaid::Warp, 7));
        state.objects.insert(ObjectId(7), warped);

        // Target creature that was hard-cast — no warp marker.
        let hard_cast = crate::game::game_object::GameObject::new(
            ObjectId(8),
            CardId(700),
            PlayerId(0),
            "Hard-cast Brute".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(8), hard_cast);

        // Source spell (Full Bore) — never warp-cast, no marker.
        let full_bore = crate::game::game_object::GameObject::new(
            ObjectId(99),
            CardId(999),
            PlayerId(0),
            "Full Bore".to_string(),
            Zone::Stack,
        );
        state.objects.insert(ObjectId(99), full_bore);

        let target_condition = AbilityCondition::CastVariantPaid {
            variant: crate::types::ability::CastVariantPaid::Warp,
            subject: crate::types::ability::ObjectScope::Target,
        };

        // Target subject reads the +3/+2 target's marker — true for the warp-cast
        // creature even though the source spell (ObjectId 99) was NOT warp-cast.
        let on_warped = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 0 },
                player: TargetFilter::Controller,
            },
            vec![TargetRef::Object(ObjectId(7))],
            ObjectId(99),
            PlayerId(0),
        );
        assert!(
            evaluate_condition(&target_condition, &state, &on_warped),
            "Target subject must read the warp-cast target's marker (true) even though \
             the source spell was not warp-cast"
        );

        let on_hard = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 0 },
                player: TargetFilter::Controller,
            },
            vec![TargetRef::Object(ObjectId(8))],
            ObjectId(99),
            PlayerId(0),
        );
        assert!(
            !evaluate_condition(&target_condition, &state, &on_hard),
            "Target subject must report false when the target was not warp-cast"
        );

        // Source subject reads the source spell (ObjectId 99) — never warp-cast —
        // so it is false even while targeting the warp-cast creature. This is the
        // proof that reading the source is the WRONG answer for Full Bore.
        let source_condition = AbilityCondition::CastVariantPaid {
            variant: crate::types::ability::CastVariantPaid::Warp,
            subject: crate::types::ability::ObjectScope::Source,
        };
        assert!(
            !evaluate_condition(&source_condition, &state, &on_warped),
            "Source subject must read the source spell's (empty) marker, not the target's"
        );
    }

    /// CR 608.2c + CR 700.1: Currency Converter — "Put a card exiled with this
    /// artifact into its owner's graveyard. If it's a land card, create a
    /// Treasure token. If it's a nonland card, create a 2/2 black Rogue
    /// creature token." (issue #1545)
    ///
    /// The parser lowers "If it's a [type] card" to `RevealedHasCardType`, but
    /// the parent effect here is a `ChangeZone` (Exile -> Graveyard), not a
    /// reveal. With no preceding reveal, `last_revealed_ids` is empty and the
    /// pre-fix evaluator returns `false` for both branches, so neither the
    /// Treasure nor the Rogue token is ever created.
    ///
    /// CR 406.6: This shape (linked-exile consumer + conditional rider on the
    /// moved card's type) is a class: Splinter Twin-style "if it's a creature
    /// card", reanimate-conditional riders, and any future
    /// "Put a card exiled with ~ into [zone]. If it's a [type] card, ..."
    /// printing all benefit from the fallback to `last_zone_changed_ids`.
    #[test]
    fn revealed_has_card_type_falls_back_to_last_zone_changed_when_no_reveal() {
        let mut state = GameState::new_two_player(42);
        let land_card = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&land_card).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.base_card_types = obj.card_types.clone();
        }
        let nonland_card = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Goblin Guide".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&nonland_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        // Currency Converter style sub-ability: parent ChangeZone moved a card,
        // sub clause reads "If it's a land card, ...". No reveal happened.
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let land_cond = AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Land],
            additional_filter: None,
            subtype_filter: None,
        };

        // Empty trackers — no reveal, no zone change: condition is false.
        assert!(state.last_revealed_ids.is_empty());
        assert!(state.last_zone_changed_ids.is_empty());
        assert!(!evaluate_condition(&land_cond, &state, &ability));

        // Parent ChangeZone moved the land card. The land branch of the
        // "If it's a [type] card" rider must fire.
        state.last_zone_changed_ids.push(land_card);
        assert!(
            evaluate_condition(&land_cond, &state, &ability),
            "land-card branch must fire when parent ChangeZone moved a land",
        );

        // Nonland branch must NOT fire on the same moved land card. Equivalent
        // to the parsed `Not { RevealedHasCardType { Land } }` rider that gates
        // Currency Converter's Rogue token.
        let nonland_cond = AbilityCondition::Not {
            condition: Box::new(land_cond.clone()),
        };
        assert!(!evaluate_condition(&nonland_cond, &state, &ability));

        // Swap: parent ChangeZone moved a nonland card instead.
        state.last_zone_changed_ids.clear();
        state.last_zone_changed_ids.push(nonland_card);
        assert!(
            !evaluate_condition(&land_cond, &state, &ability),
            "land-card branch must NOT fire when parent ChangeZone moved a nonland",
        );
        assert!(
            evaluate_condition(&nonland_cond, &state, &ability),
            "nonland-card branch must fire when parent ChangeZone moved a nonland",
        );

        // Issue #2871: with no reveal and no parent zone change, the nonland
        // `Not { RevealedHasCardType { Land } }` rider must NOT fire.
        state.last_zone_changed_ids.clear();
        state.last_revealed_ids.clear();
        assert!(!evaluate_condition(&nonland_cond, &state, &ability));

        // CR 700.1 + CR 701.20: A real reveal still wins over the zone-change
        // fallback so existing reveal-driven cards (Goblin Guide, dig effects)
        // are not regressed by the fallback path.
        state.last_zone_changed_ids.clear();
        state.last_zone_changed_ids.push(nonland_card);
        state.last_revealed_ids.push(land_card);
        assert!(
            evaluate_condition(&land_cond, &state, &ability),
            "reveal must take precedence over the zone-change fallback",
        );
    }

    #[test]
    fn revealed_has_card_type_accepts_chosen_land_nonland_property_without_type_list() {
        let mut state = GameState::new_two_player(42);
        let land_card = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Island".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&land_card).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.base_card_types = obj.card_types.clone();
        }
        let nonland_card = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Ornithopter".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&nonland_card).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.base_card_types = obj.card_types.clone();
        }
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let condition = AbilityCondition::RevealedHasCardType {
            card_types: Vec::new(),
            additional_filter: Some(FilterProp::MatchesLastChosenCardPredicate),
            subtype_filter: None,
        };

        state.last_named_choice = Some(ChoiceValue::CardPredicate(CardPredicateChoice::Land));
        state.last_revealed_ids.push(land_card);
        assert!(evaluate_condition(&condition, &state, &ability));

        state.last_revealed_ids.clear();
        state.last_revealed_ids.push(nonland_card);
        assert!(!evaluate_condition(&condition, &state, &ability));

        state.last_named_choice = Some(ChoiceValue::CardPredicate(CardPredicateChoice::Nonland));
        assert!(evaluate_condition(&condition, &state, &ability));
    }

    #[test]
    fn revealed_has_card_type_chain_uses_card_moved_by_parent_change_zone() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Currency Converter".to_string(),
            Zone::Battlefield,
        );
        let land_card = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&land_card).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.base_card_types = obj.card_types.clone();
        }
        state.exile_links.push(ExileLink {
            source_id: source,
            exiled_id: land_card,
            kind: ExileLinkKind::TrackedBySource,
        });

        let token_rider = ResolvedAbility::new(
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        )
        .condition(AbilityCondition::RevealedHasCardType {
            card_types: vec![CoreType::Land],
            additional_filter: None,
            subtype_filter: None,
        });
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Graveyard,
                target: TargetFilter::ExiledBySource,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(token_rider);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.objects[&land_card].zone, Zone::Graveyard);
        assert_eq!(
            state
                .objects
                .values()
                .filter(|obj| obj.name == "Treasure" && obj.zone == Zone::Battlefield)
                .count(),
            1,
            "land-card rider must create a Treasure after the parent ChangeZone moved a land",
        );
    }

    #[test]
    fn evaluate_cast_during_main_phase_checks_cast_phase_context() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = Phase::BeginCombat;
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.context.cast_phase = Some(Phase::PreCombatMain);

        assert!(evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));

        ability.context.cast_phase = Some(Phase::BeginCombat);
        assert!(!evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));

        ability.context.cast_phase = Some(Phase::PostCombatMain);
        state.active_player = PlayerId(1);
        assert!(evaluate_condition(
            &AbilityCondition::CastDuringPhase {
                phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
            },
            &state,
            &ability
        ));
    }

    #[test]
    fn evaluate_controller_controlled_as_cast_reads_spell_context_snapshot() {
        let state = GameState::new_two_player(42);
        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Faerie".to_string())
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Battlefield,
                }]),
        );
        let condition = AbilityCondition::ControllerControlledMatchingAsCast {
            filter: filter.clone(),
        };
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .context(SpellContext {
            controller_controlled_as_cast: vec![filter],
            ..SpellContext::default()
        });

        assert!(evaluate_condition(&condition, &state, &ability));

        ability.context.controller_controlled_as_cast.clear();
        assert!(!evaluate_condition(&condition, &state, &ability));
    }

    #[test]
    fn evaluate_condition_and_one_false() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // And([IsYourTurn(true), IsYourTurn(false)]) — one is "not your turn" which is false
        let cond = AbilityCondition::And {
            conditions: vec![
                AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::IsYourTurn),
                },
                AbilityCondition::IsYourTurn,
            ],
        };
        assert!(!evaluate_condition(&cond, &state, &ability));
    }

    #[test]
    fn condition_instead_swap_clears_parent_condition() {
        // CR 608.2c: When ConditionInstead fires, the parent's condition should be
        // cleared — "instead" replaces the entire clause.
        let mut state = GameState::new_two_player(42);

        // Instead sub: deal 5 damage (replaces parent when condition is met)
        let instead_sub = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::IsYourTurn),
        });

        // Parent: deal 2 damage with a condition that would normally block it.
        // IsYourTurn(negated=true) = "not your turn" = false for active player.
        // Without the swap clearing it, the parent condition would block resolution.
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        })
        .sub_ability(instead_sub);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The swap fires (IsYourTurn is true), clearing the parent's "not your turn"
        // condition. The overridden ability deals 5 damage.
        assert_eq!(
            state.players[1].life, 15,
            "Expected 5 damage — swap should clear parent condition"
        );
    }

    /// CR 603.7 + CR 608.2c: Compound zone-changing effects in one resolution
    /// chain coalesce into a single tracked set. Shape modeled on Suspend
    /// Aggression: "Exile target permanent AND exile the top card ... For
    /// each of those cards, its owner may play it." The two exile steps must
    /// produce ONE set so the downstream GrantCastingPermission sees both
    /// objects, not just the most recent exile.
    #[test]
    fn compound_zone_change_chain_unifies_tracked_set() {
        use crate::types::ability::PermissionGrantee;
        let mut state = GameState::new_two_player(42);
        let permanent = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let lib_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        state.players[0].library.push_back(lib_card);

        // Grandchild: grant PlayFromExile to the tracked set. Forces every
        // zone-changing ancestor to publish (transitive descendant check).
        let grant = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        // Sub-ability: ExileTop 1 card from controller's library.
        let exile_top = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(grant);

        // Parent: Exile target permanent. ChangeZone{target=Any} moves
        // `permanent` to exile via the explicit TargetRef::Object(permanent).
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(permanent)],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(exile_top);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Exactly ONE tracked set — unified — containing both exiled objects.
        assert_eq!(
            state.tracked_object_sets.len(),
            1,
            "chain should publish into a single tracked set, got {}",
            state.tracked_object_sets.len()
        );
        let set = state.tracked_object_sets.values().next().unwrap();
        assert!(
            set.contains(&permanent),
            "unified set must contain the exiled permanent"
        );
        assert!(
            set.contains(&lib_card),
            "unified set must contain the exiled library card"
        );

        // Both objects received the grant, bound to their respective owners.
        for (id, owner) in [(permanent, PlayerId(1)), (lib_card, PlayerId(0))] {
            let obj = &state.objects[&id];
            assert_eq!(
                obj.casting_permissions.len(),
                1,
                "object {id:?} should have one PlayFromExile grant"
            );
            match obj.casting_permissions[0] {
                CastingPermission::PlayFromExile { granted_to, .. } => {
                    assert_eq!(
                        granted_to, owner,
                        "ObjectOwner grantee should bind to each object's owner"
                    );
                }
                _ => panic!("expected PlayFromExile"),
            }
        }
    }

    #[test]
    fn random_graveyard_exile_chain_grants_play_from_exile_permission() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            crate::types::identifiers::CardId(100),
            PlayerId(0),
            "Advanced Reconstruction".to_string(),
            Zone::Battlefield,
        );
        let grave_card = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Previously Stolen Card".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&grave_card).unwrap().controller = PlayerId(1);

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Mill a card, then exile a card from your graveyard at random. You may play the exiled card this turn.",
            AbilityKind::Spell,
        );
        let resolved =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::TargetSelection { .. }),
            "random graveyard exile must not prompt for target selection"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "random graveyard exile must not prompt for a zone choice"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "play-from-exile permission must be granted without an optional-effect prompt"
        );

        assert_eq!(state.objects[&grave_card].zone, Zone::Exile);

        let permissions = &state.objects[&grave_card].casting_permissions;
        assert!(
            permissions.iter().any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    granted_to: PlayerId(0),
                    ..
                }
            )),
            "randomly exiled card should get PlayFromExile for player 0, got {:?}",
            permissions
        );
    }

    /// CR 400.7i + CR 603.7: Issue #1549 — ExileTop(3) chained to
    /// `GrantCastingPermission { PlayFromExile, TrackedSet }` must attach
    /// exactly one permission per exiled card (no double-grant).
    #[test]
    fn exile_top_three_impulse_grant_applies_once_per_card() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "The Legend of Roku".to_string(),
            Zone::Battlefield,
        );
        let mut exiled_ids = Vec::new();
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Lib Card {i}"),
                Zone::Library,
            );
            exiled_ids.push(id);
        }

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Exile the top three cards of your library. Until the end of your next turn, you may play those cards.",
            AbilityKind::Spell,
        );
        fn count_grant_subs(ability: &AbilityDefinition) -> usize {
            let mut n = matches!(
                ability.effect.as_ref(),
                Effect::GrantCastingPermission { .. }
            ) as usize;
            if let Some(sub) = &ability.sub_ability {
                n += count_grant_subs(sub);
            }
            n
        }
        assert_eq!(
            count_grant_subs(&def),
            1,
            "parsed chain must contain exactly one GrantCastingPermission, got tree {:?}",
            def.effect
        );
        assert!(
            def.repeat_for.is_none(),
            "impulse exile-top chain must not carry repeat_for, got {:?}",
            def.repeat_for
        );
        let resolved =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();

        let exile_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { object_id, to, .. } if *to == Zone::Exile => {
                    Some(*object_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            exile_events, exiled_ids,
            "expected one exile ZoneChanged per library card"
        );

        let tracked: Vec<_> = state
            .tracked_object_sets
            .values()
            .flatten()
            .copied()
            .collect();
        for id in exiled_ids {
            let obj = &state.objects[&id];
            assert_eq!(
                obj.zone,
                Zone::Exile,
                "card {id:?} should be exiled; tracked={tracked:?}"
            );
            assert_eq!(
                obj.casting_permissions.len(),
                1,
                "card {id:?} should receive exactly one PlayFromExile grant, got {:?}",
                obj.casting_permissions
            );
        }
    }

    // CR 603.4: Runtime tests for `AbilityCondition::NthResolutionThisTurn`.

    /// Build a minimal `ResolvedAbility` with a stamped `ability_index` for
    /// nth-resolution tracking tests.
    fn nth_test_ability(source_id: ObjectId, idx: usize) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.ability_index = Some(idx);
        ability
    }

    /// Issue #1595 — Nissa, Resurgent Animist. A chained `SequentialSibling`
    /// sub-ability gated on `NthResolutionThisTurn{2}` must fire on the SECOND
    /// resolution this turn. Before the fix, sub-abilities carried no
    /// `ability_index`, so the gate evaluated false forever and the second-
    /// resolution half never happened (the reported "only did the first portion
    /// again and added the mana"). Drives the real `resolve_ability_chain`
    /// descent (which routes through `apply_parent_chain_context`).
    #[test]
    fn nth_resolution_gates_sequential_sibling_subability() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);

        // Sub: gain 100 life, gated on the 2nd resolution (SequentialSibling).
        let mut sub = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 100 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        sub.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 2 });
        sub.sub_link = SubAbilityLink::SequentialSibling;
        // The sub is built WITHOUT an ability_index, exactly as the trigger
        // pipeline produces it (only the top-level trigger gets a stamp).
        assert!(sub.ability_index.is_none());

        // Top-level: gain 1 life ALWAYS (the "add mana" analogue), index stamped.
        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.ability_index = Some(0);

        let start = state.players[0].life;
        let mut events = Vec::new();

        // Resolution 1: only the top-level fires (+1); gated sub must NOT fire.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.players[0].life,
            start + 1,
            "1st resolution: only the top-level (+1) should fire"
        );

        // Resolution 2: top-level (+1) AND the gated sub (+100) both fire.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.players[0].life,
            start + 1 + 1 + 100,
            "2nd resolution: top-level (+1) AND gated sub (+100) must BOTH fire"
        );

        // Counter must read exactly 2 — the propagated index must not have
        // caused the sub to bump the counter a second time per resolution.
        assert_eq!(
            state.ability_resolutions_this_turn[&(source_id, 0)],
            2,
            "counter must be bumped exactly once per top-level resolution"
        );
    }

    /// Test Omnath-style chain: three SequentialSibling sub-abilities gated on
    /// n=1, n=2, n=3. Each resolution should fire exactly one branch.
    #[test]
    fn nth_resolution_omnath_three_branch_chain() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);

        // Branch 3: lose 4 life (as damage proxy), gated on n=3 (SequentialSibling).
        let mut branch3 = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 4 },
                target: Some(TargetFilter::Controller),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch3.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 3 });
        branch3.sub_link = SubAbilityLink::SequentialSibling;
        assert!(branch3.ability_index.is_none());

        // Branch 2: lose 2 life (as mana proxy), gated on n=2 (SequentialSibling).
        let mut branch2 = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::Controller),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch2.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 2 });
        branch2.sub_link = SubAbilityLink::SequentialSibling;
        branch2.sub_ability = Some(Box::new(branch3));
        assert!(branch2.ability_index.is_none());

        // Branch 1: gain 4 life, gated on n=1 (SequentialSibling).
        let mut branch1 = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch1.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 1 });
        branch1.sub_link = SubAbilityLink::SequentialSibling;
        branch1.sub_ability = Some(Box::new(branch2));
        assert!(branch1.ability_index.is_none());

        // Top-level: gain 1 life (no-op proxy), chains to the three branches.
        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(branch1);
        ability.ability_index = Some(0);

        let start_life = state.players[0].life;
        let mut events = Vec::new();

        // Resolution 1: only n=1 branch should fire (+4 life).
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.players[0].life,
            start_life + 1 + 4,
            "1st resolution: top-level (+1) and n=1 branch (+4) should fire"
        );

        // Resolution 2: only n=2 branch should fire (lose 2 life).
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.players[0].life,
            start_life + 1 + 4 + 1 - 2,
            "2nd resolution: top-level (+1) and n=2 branch (-2) should fire"
        );

        // Resolution 3: only n=3 branch should fire (lose 4 life).
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.players[0].life,
            start_life + 1 + 4 + 1 - 2 + 1 - 4,
            "3rd resolution: top-level (+1) and n=3 branch (-4) should fire"
        );

        // Counter must be exactly 3.
        assert_eq!(
            state.ability_resolutions_this_turn[&(source_id, 0)],
            3,
            "counter must be bumped exactly once per top-level resolution"
        );
    }

    #[test]
    fn sequential_sibling_failure_walk_resolves_selected_chain_once() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);

        // Branch 3 is also true on the first resolution. It should resolve once
        // as branch 2's child, not a second time from the failure-path sibling walk.
        let mut branch3 = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch3.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 1 });
        branch3.sub_link = SubAbilityLink::SequentialSibling;

        let mut branch2 = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch2.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 1 });
        branch2.sub_link = SubAbilityLink::SequentialSibling;
        branch2.sub_ability = Some(Box::new(branch3));

        let mut branch1 = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 100 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        branch1.condition = Some(AbilityCondition::NthResolutionThisTurn { n: 2 });
        branch1.sub_link = SubAbilityLink::SequentialSibling;
        branch1.sub_ability = Some(Box::new(branch2));

        let mut ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(branch1);
        ability.ability_index = Some(0);

        let start_life = state.players[0].life;
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].life,
            start_life + 1 + 2 + 4,
            "branch 3 must not be double-resolved by the failure-path sibling walk"
        );
    }

    #[test]
    fn nth_resolution_increments_per_resolution_and_gates_correctly() {
        // Three sequential resolutions of the same printed ability — only the
        // matching n-th resolution evaluates true; others evaluate false.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = nth_test_ability(source_id, 0);
        let mut events = Vec::new();

        // Initial state: counter is 0; n=1 should be false BEFORE any resolution.
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "before any resolution, n=1 must not match (counter is 0)"
        );

        // Resolution 1 — counter becomes 1.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            *state
                .ability_resolutions_this_turn
                .get(&(source_id, 0))
                .expect("counter must be present"),
            1,
            "first resolution must produce count=1"
        );
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "n=1 must evaluate true after first resolution"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate false after only one resolution"
        );

        // Resolution 2 — counter becomes 2.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.ability_resolutions_this_turn[&(source_id, 0)],
            2,
            "second resolution must produce count=2"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "n=1 must evaluate false after second resolution"
        );
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate true after second resolution"
        );

        // Resolution 3 — counter becomes 3.
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 3 },
                &state,
                &ability
            ),
            "n=3 must evaluate true after third resolution"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 2 },
                &state,
                &ability
            ),
            "n=2 must evaluate false after third resolution"
        );
    }

    #[test]
    fn nth_resolution_counter_is_per_source() {
        // Two distinct Omnaths must track resolutions independently — each
        // has its own (source_id, ability_index) key.
        let mut state = GameState::new_two_player(42);
        let omnath_a = ObjectId(10);
        let omnath_b = ObjectId(20);
        let ability_a = nth_test_ability(omnath_a, 0);
        let ability_b = nth_test_ability(omnath_b, 0);
        let mut events = Vec::new();

        // Resolve A twice, B once.
        resolve_ability_chain(&mut state, &ability_a, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_a, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_b, &mut events, 0).unwrap();

        assert_eq!(state.ability_resolutions_this_turn[&(omnath_a, 0)], 2);
        assert_eq!(state.ability_resolutions_this_turn[&(omnath_b, 0)], 1);
    }

    #[test]
    fn nth_resolution_counter_is_per_ability_index() {
        // Same source, two distinct printed abilities (different ability_index)
        // — counters are tracked separately. Mirrors a card with multiple
        // triggered abilities each gated on its own nth-resolution count.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability_idx_0 = nth_test_ability(source_id, 0);
        let ability_idx_1 = nth_test_ability(source_id, 1);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability_idx_0, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_idx_0, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability_idx_1, &mut events, 0).unwrap();

        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 0)], 2);
        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 1)], 1);
    }

    #[test]
    fn nth_resolution_counter_resets_on_turn_start() {
        // CR 514 + CR 603.4: Per-turn counter clears at start of next turn.
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = nth_test_ability(source_id, 0);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(state.ability_resolutions_this_turn[&(source_id, 0)], 2);

        // Simulate turn boundary by invoking start_next_turn, which is the
        // canonical reset site for per-turn counters in the engine.
        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);

        assert!(
            state.ability_resolutions_this_turn.is_empty(),
            "ability_resolutions_this_turn must be cleared at turn start, got {:?}",
            state.ability_resolutions_this_turn
        );
    }

    #[test]
    fn nth_resolution_no_index_does_not_increment_or_match() {
        // Synthesized abilities (prowess, firebending) lack an ability_index.
        // They must NOT bump the counter and NthResolutionThisTurn must
        // evaluate false against them (count is implicitly 0 / no key).
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(1);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        // ability.ability_index is None by default.
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.ability_resolutions_this_turn.is_empty(),
            "abilities without ability_index must not register in counter"
        );
        assert!(
            !evaluate_condition(
                &AbilityCondition::NthResolutionThisTurn { n: 1 },
                &state,
                &ability
            ),
            "NthResolutionThisTurn must evaluate false when ability lacks an index"
        );
    }

    /// Abandon Attachments: "You may discard a card. If you do, draw two cards."
    /// Regression test for #81: after discarding, the IfYouDo draw-2 sub-ability
    /// must fire via the continuation chain. The bug was that context propagation
    /// or cost_payment_failed_flag was blocking the IfYouDo condition.
    #[test]
    fn optional_discard_if_you_do_draw_fires_after_choice() {
        let mut state = GameState::new_two_player(42);

        // Card in hand to discard
        let _hand_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build Abandon Attachments: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        // Step 1: resolve_ability_chain → OptionalEffectChoice
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Step 2: Accept optional effect → forced discard (1 card in hand, count=1)
        // then IfYouDo sub-ability should fire drawing 2 cards.
        let waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        // Forced discard (hand_size == count) skips DiscardChoice, resolves inline.
        // After: discard 1 (-1), draw 2 (+2) = 2 cards in hand.
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after,
            2,
            "After discarding 1 and drawing 2, hand should have 2 cards, got {}. \
             IfYouDo sub-ability likely did not fire. waiting_for={:?}, Events: {:?}",
            hand_after,
            waiting,
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::EffectResolved { .. }
                        | GameEvent::Discarded { .. }
                        | GameEvent::CardsDrawn { .. }
                ))
                .collect::<Vec<_>>()
        );
    }

    /// Abandon Attachments #81: interactive discard (player has 2+ cards) → IfYouDo draw 2.
    /// When the discard requires player interaction (DiscardChoice), the sub-ability
    /// must be stashed as a continuation and fire after the player selects a card.
    #[test]
    fn optional_discard_if_you_do_draw_fires_after_interactive_choice() {
        let mut state = GameState::new_two_player(42);

        // Two cards in hand so the discard is interactive (player must choose 1 of 2)
        let hand_card_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder A".to_string(),
            Zone::Hand,
        );
        let _hand_card_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Fodder B".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        // Step 1: resolve_ability_chain → OptionalEffectChoice
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Step 2: Accept optional effect → DiscardChoice (2 cards, must choose 1)
        let waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(waiting, WaitingFor::DiscardChoice { .. }),
            "Expected DiscardChoice after accepting optional with 2 cards in hand, got {:?}",
            waiting
        );

        // Verify the sub-ability was stashed as a pending continuation
        assert!(
            state.pending_continuation.is_some(),
            "IfYouDo sub-ability should be stashed as pending_continuation during DiscardChoice"
        );

        // Step 3: Select one card to discard
        let wf = state.waiting_for.clone();
        let _result = crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            wf,
            crate::types::GameAction::SelectCards {
                cards: vec![hand_card_a],
            },
            &mut events,
        )
        .unwrap();

        // After: started with 2 in hand, discarded 1 (-1), drew 2 (+2) = 3 cards in hand.
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after, 3,
            "After discarding 1 of 2 and drawing 2, hand should have 3 cards, got {}. \
             IfYouDo sub-ability likely did not fire.",
            hand_after,
        );
    }

    /// Issue #1972: Extort must prompt before draining, then pay {W/B}, drain each
    /// opponent, and gain life equal to the total life lost.
    #[test]
    fn issue_1972_extort_optional_accept_drains_all_opponents_and_gains() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Extort);
        synthesize_extort(&mut face);
        let execute = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::SpellCast)
                    && matches!(t.execute.as_deref().map(|e| e.optional), Some(true))
            })
            .and_then(|t| t.execute.as_deref())
            .expect("synthesized extort trigger");

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source_id = ObjectId(100);
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::White,
            ObjectId(200),
            false,
            Vec::new(),
        ));
        let resolved = build_resolved_from_def(execute, source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "extort must prompt before draining, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            (state.players[1].life, state.players[2].life),
            (20, 20),
            "opponents must not lose life before the may-pay decision"
        );

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].life, 22);
        assert_eq!(state.players[1].life, 19);
        assert_eq!(state.players[2].life, 19);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
    }

    /// Accept extort with no {W/B} available — drain must not run (CR 702.101a).
    #[test]
    fn issue_1972_extort_accept_without_payable_mana_does_not_drain() {
        let mut face = CardFace::default();
        face.keywords.push(Keyword::Extort);
        synthesize_extort(&mut face);
        let execute = face
            .triggers
            .iter()
            .find(|t| {
                matches!(t.mode, TriggerMode::SpellCast)
                    && matches!(t.execute.as_deref().map(|e| e.optional), Some(true))
            })
            .and_then(|t| t.execute.as_deref())
            .expect("synthesized extort trigger");

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source_id = ObjectId(100);
        assert!(
            state.players[0].mana_pool.mana.is_empty(),
            "controller must have no mana to pay W/B"
        );
        let resolved = build_resolved_from_def(execute, source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "extort must prompt before draining, got {:?}",
            state.waiting_for
        );

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        assert!(
            state.cost_payment_failed_flag,
            "PayCost with no W/B must set cost_payment_failed_flag"
        );
        assert_eq!(
            (
                state.players[0].life,
                state.players[1].life,
                state.players[2].life
            ),
            (20, 20, 20),
            "accepting without payable mana must not drain opponents or grant life"
        );
    }

    #[test]
    fn optional_resolution_pay_ability_cost_if_you_do_draws_after_composite_payment() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Ability Source".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );
        state.players[0].life = 20;
        state.players[0]
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(200),
                false,
                Vec::new(),
            ));

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: crate::types::ability::AbilityCost::Composite {
                    costs: vec![
                        crate::types::ability::AbilityCost::Mana {
                            cost: ManaCost::generic(1),
                        },
                        crate::types::ability::AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(draw);
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].library.len(), 0);
    }

    #[test]
    fn optional_resolution_pay_mana_if_you_do_creates_token() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Myrsmith".to_string(),
            Zone::Battlefield,
        );
        state.players[0]
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(200),
                false,
                Vec::new(),
            ));

        let token = ResolvedAbility::new(
            Effect::Token {
                name: "Myr".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec![
                    "Artifact".to_string(),
                    "Creature".to_string(),
                    "Myr".to_string(),
                ],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )
        .sub_ability(token);
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));

        crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert!(
            events.iter().any(
                |event| matches!(event, GameEvent::TokenCreated { name, .. } if name == "Myr")
            ),
            "accepted optional mana payment must create the reflexive Myr token"
        );
    }

    /// Abandon Attachments #81: stale cost_payment_failed_flag from a previous resolution
    /// must not block the IfYouDo condition. The flag should be cleared when accepting
    /// an optional effect.
    #[test]
    fn optional_discard_if_you_do_not_blocked_by_stale_flag() {
        let mut state = GameState::new_two_player(42);

        // Simulate a previous resolution that left the flag set
        state.cost_payment_failed_flag = true;

        // Card in hand to discard
        create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Hand,
        );

        // Cards in library to draw
        create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Draw A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw B".to_string(),
            Zone::Library,
        );

        // Build: optional Discard 1 → sub: Draw 2 (IfYouDo)
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());

        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        ability.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Accept → forced discard (1 card, count=1) → should draw 2
        let _waiting = crate::game::engine_payment_choices::handle_optional_effect_choice(
            &mut state,
            true,
            &mut events,
        )
        .unwrap();

        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after, 2,
            "Stale cost_payment_failed_flag should be cleared by handle_optional_effect_choice. \
             Hand should have 2 cards (discard 1 + draw 2), got {}.",
            hand_after,
        );
    }

    // CR 603.7: publish_fresh_tracked_set always allocates a strictly-greater
    // id and rebinds chain_tracked_set_id — never extends an ancestor set.
    #[test]
    fn publish_fresh_tracked_set_never_extends_ancestor() {
        let mut state = GameState::new_two_player(42);
        // Simulate an ancestor publish that set the chain set.
        let ancestor = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(ancestor, vec![ObjectId(7)]);
        state.chain_tracked_set_id = Some(ancestor);

        // A fresh publish must NOT extend the ancestor — it allocates anew.
        let fresh = publish_fresh_tracked_set(&mut state, vec![ObjectId(1), ObjectId(2)]);
        assert!(
            fresh.0 > ancestor.0,
            "fresh id {} must be strictly greater than ancestor {}",
            fresh.0,
            ancestor.0
        );
        assert_eq!(
            state.chain_tracked_set_id,
            Some(fresh),
            "chain_tracked_set_id must rebind to the fresh set"
        );
        assert_eq!(
            state.tracked_object_sets.get(&ancestor),
            Some(&vec![ObjectId(7)]),
            "ancestor set must be untouched"
        );
        assert_eq!(
            state.tracked_object_sets.get(&fresh),
            Some(&vec![ObjectId(1), ObjectId(2)]),
            "fresh set must hold exactly the published ids"
        );
    }

    // CR 118.5: An empty selection yields an empty fresh tracked set (size 0).
    #[test]
    fn publish_fresh_tracked_set_empty_selection() {
        let mut state = GameState::new_two_player(42);
        let fresh = publish_fresh_tracked_set(&mut state, vec![]);
        assert_eq!(
            state.tracked_object_sets.get(&fresh),
            Some(&vec![]),
            "empty selection produces an empty fresh set"
        );
    }

    /// CR 608.2e + CR 701.9a: Building-block test for issue #456. A
    /// `player_scope: Opponent` `Discard` with a `Draw { Ref(TrackedSetSize) }`
    /// tail must accumulate every opponent's discarded card into ONE chain
    /// tracked set across the per-opponent interactive `DiscardChoice` pauses,
    /// so the trailing `Draw` reads the union (count == 3 for 3 opponents).
    #[test]
    fn discard_choice_publishes_tracked_set_across_continuation() {
        let mut state = GameState::new(FormatConfig::commander(), 4, 42);

        // P0's library: cards for the trailing Draw to draw.
        for i in 0..6 {
            create_object(
                &mut state,
                CardId(900 + i),
                PlayerId(0),
                format!("Lib {i}"),
                Zone::Library,
            );
        }
        // Each opponent holds 2 hand cards so its discard is interactive
        // (hand > count → DiscardChoice).
        for opp in 1..4u8 {
            for c in 0..2u64 {
                create_object(
                    &mut state,
                    CardId(u64::from(opp) * 100 + c),
                    PlayerId(opp),
                    format!("P{opp} card {c}"),
                    Zone::Hand,
                );
            }
        }

        // Syphon-Mind-shaped ability: each opponent discards one card, then the
        // controller draws one card per card discarded this way.
        let mut ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        )));

        let p0_hand_before = state.players[0].hand.len();

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Resolve each opponent's interactive discard via the real pipeline.
        let mut select_actions = 0;
        while let WaitingFor::DiscardChoice { player, cards, .. } = &state.waiting_for {
            let pick = vec![cards[0]];
            let _ = *player;
            crate::game::engine::apply_as_current(
                &mut state,
                GameAction::SelectCards { cards: pick },
            )
            .unwrap();
            select_actions += 1;
            assert!(select_actions <= 3, "must not loop past 3 opponents");
        }

        assert_eq!(select_actions, 3, "three opponents each discard once");
        // The trailing Draw reads TrackedSetSize == 3 (one card per opponent
        // discard, accumulated across the continuation pauses).
        assert_eq!(
            state.players[0].hand.len() - p0_hand_before,
            3,
            "controller draws one card per card discarded this way (3 total)"
        );
    }

    /// GH #582 — CR 104.2b + CR 107.3i + CR 608.2c + CR 700.5: Thassa's Oracle
    /// runtime discriminator. The synthesized AST gates `Effect::WinTheGame`
    /// with `AbilityCondition::QuantityCheck { lhs: Devotion{[Blue]}, comparator: GE, rhs:
    /// ZoneCardCount{Library, Controller} }`. This test drives that condition
    /// through the production `evaluate_condition` against two library/devotion
    /// fixtures (WIN: library=1, devotion=2; NO-WIN: library=50, devotion=2)
    /// and asserts the boolean outcome.
    ///
    /// **Three-way reversion discriminator (plan Step 6):**
    /// 1. Revert Step 1 (drop `parse_quantity_quantity_comparison`): the
    ///    parser cannot lower the trailing "if X >= number of cards" clause,
    ///    so the WinTheGame sub_ability ships with `condition = None` and
    ///    fires unconditionally — the NO-WIN scenario regresses (spurious
    ///    win at library=50).
    /// 2. Revert Step 1b (drop the forward-fill pass in
    ///    `compute_sentence_where_x`): sentence 3 receives `None` binding so
    ///    LHS stays `Variable("X")`; `resolve_quantity` returns 0; `0 >= 1`
    ///    is false — the WIN scenario regresses (no win at library=1).
    /// 3. Revert Step 3 (drop `apply_where_x_ability_condition` and the
    ///    `def.condition` walker block): forward-fill produced the binding,
    ///    but the condition walker never substitutes; LHS stays
    ///    `Variable("X")` → identical failure mode to (2). WIN regresses.
    ///
    /// The synthesis test in `parser::oracle::tests` already proves the AST
    /// shape; this test proves the runtime evaluator agrees with the AST.
    #[test]
    fn thassas_oracle_win_condition_runtime_discriminator() {
        use crate::types::ability::{
            Comparator, DevotionColors, Effect, QuantityExpr, QuantityRef, ResolvedAbility, ZoneRef,
        };
        use crate::types::mana::ManaCostShard;

        // Two-permanent {U}{U} fixture: devotion-to-blue = 2.
        // Mirrors `devotion_counts_matching_shards` in game/devotion.rs.
        let build_state = |library_size: usize| -> GameState {
            let mut state = GameState::new_two_player(42);
            let id = create_object(
                &mut state,
                CardId(7000),
                PlayerId(0),
                "Devotion Source".to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            };
            // Reset library to the exact size we want.
            state.players[0].library.clear();
            for i in 0..library_size {
                let lib_id = create_object(
                    &mut state,
                    CardId(8000 + i as u64),
                    PlayerId(0),
                    format!("Library Card {i}"),
                    Zone::Library,
                );
                let _ = lib_id;
            }
            state
        };

        let condition = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Devotion {
                    colors: DevotionColors::Fixed(vec![ManaColor::Blue]),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Library,
                    card_types: Vec::new(),
                    scope: crate::types::ability::CountScope::Controller,
                    filter: None,
                },
            },
        };

        // WIN scenario: library = 1, devotion = 2 → 2 >= 1 → true.
        let state_win = build_state(1);
        let ability_win = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(9001),
            PlayerId(0),
        );
        assert!(
            evaluate_condition(&condition, &state_win, &ability_win),
            "WIN scenario: devotion=2, library=1 should satisfy GE",
        );

        // NO-WIN scenario: library = 50, devotion = 2 → 2 >= 50 → false.
        let state_no_win = build_state(50);
        let ability_no_win = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(9002),
            PlayerId(0),
        );
        assert!(
            !evaluate_condition(&condition, &state_no_win, &ability_no_win),
            "NO-WIN scenario: devotion=2, library=50 must NOT satisfy GE",
        );
    }

    /// CR 122.1: Non-interactive proof that
    /// `repeat_for: DistinctCounterKindsAmong` drives the iteration count. A
    /// plain `PutCounter` (no ChooseOneOf, no "you may") runs once per distinct
    /// counter kind among controlled permanents.
    #[test]
    fn distinct_counter_kinds_among_drives_repeat_for_count() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);

        // Source permanent (Bribe-Taker-like) — counters land here.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Two distinct counter kinds among controlled permanents: P1P1 + Lore.
        let perm_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_a).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }
        let perm_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_b).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.counters.insert(CounterType::Lore, 1);
        }

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });

        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong { filter },
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // 2 distinct kinds → loop ran twice → source has 2 +1/+1 counters
        // (the source's own counters do not change the controlled-permanent set
        // mid-loop because P1P1 is already present on perm_a — the kind set is
        // snapshotted at loop entry regardless).
        assert_eq!(
            state
                .objects
                .get(&source)
                .unwrap()
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "loop must run once per distinct counter kind (2)"
        );
    }

    /// CR 122.1 + CR 608.2c + CR 608.2d + CR 109.4 (T1): Drive Bribe Taker's
    /// interactive for-each-kind choice end-to-end, PROVING per-kind
    /// optionality. The controller DECLINES the first kind's "you may" (no
    /// counter for that kind) and the loop must still ADVANCE to a prompt for
    /// the second kind, which is ACCEPTED. This is discriminating: under the old
    /// single up-front gate, declining would have skipped ALL kinds and accepting
    /// would have forced a counter on EVERY kind — neither matches the card's
    /// ruling that each kind is independently optional.
    ///
    /// Also the H1 discriminator — without the `drain_pending_continuation` call
    /// in the ChooseBranch handler, only the first prompted kind would advance.
    #[test]
    fn bribe_taker_for_each_kind_interactive_choice_runtime() {
        use crate::game::engine::apply;
        use crate::types::ability::{IterationKindBinding, TypeFilter, TypedFilter};
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bribe Taker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Controller perm A: +1/+1 counter. Perm B: Lore counter.
        let perm_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_a).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }
        let perm_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm_b).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.counters.insert(CounterType::Lore, 1);
        }
        // OPPONENT perm with Stun counter — MUST be excluded (CR 109.4): if it
        // leaked in, there would be 3 prompts, not 2.
        let opp = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opp".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&opp).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Stun, 1);
        }

        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });

        // Build the Bribe Taker ability: optional ChooseOneOf, fixed (+1/+1) +
        // dynamic (RebindToIteratedKind) branches, driven by
        // repeat_for: DistinctCounterKindsAmong.
        let fixed_branch = AbilityDefinition {
            ..AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            )
        };
        let dynamic_branch = AbilityDefinition {
            iteration_kind_binding: Some(IterationKindBinding::RebindToIteratedKind),
            ..AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            )
        };
        let mut ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: crate::types::ability::PlayerFilter::Controller,
                branches: vec![fixed_branch, dynamic_branch],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.optional = true;
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong { filter },
        });

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // (d) — non-empty set: the FIRST kind's per-iteration "you may" gate must
        // fire. The deterministic sorted order is [P1P1, Lore]; iteration 0 is the
        // P1P1 kind. Under per-kind optionality this is an OptionalEffectChoice
        // (the decline path is only reachable when the gate is per-iteration).
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "iteration 0 must fire its own per-kind 'you may' gate (got {:?})",
            state.waiting_for
        );

        // Iteration 0 (P1P1 kind): DECLINE the "you may". No counter is placed
        // for this kind, and the loop must ADVANCE to the next kind's gate — this
        // is the discriminating step: a single up-front gate would have skipped
        // ALL kinds here.
        let r = apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();
        events.extend(r.events);

        // (a) Per-kind decline + H1: after declining iteration 0, the loop must
        // ADVANCE to iteration 1's (Lore) own "you may" gate — not return to
        // Priority and not skip the remaining kind.
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "after declining kind 0, kind 1 (Lore) must fire its own 'you may' gate \
             (got {:?})",
            state.waiting_for
        );

        // Iteration 1 (Lore kind): ACCEPT the "you may", then choose the DYNAMIC
        // branch (index 1). (b) This must place a LORE counter, proving the
        // rebind binds the iterated kind.
        let r = apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
        events.extend(r.events);
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "accepting kind 1 must surface the ChooseOneOf branch prompt, got {:?}",
            state.waiting_for
        );
        let r = apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 1 },
        )
        .unwrap();
        events.extend(r.events);

        // Loop complete: back to Priority.
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "loop must complete after the last kind, got {:?}",
            state.waiting_for
        );

        let src = state.objects.get(&source).unwrap();
        // Iteration 0 was DECLINED: NO +1/+1 counter was placed for that kind.
        assert_eq!(
            src.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "declining the P1P1 kind must place NO +1/+1 counter (per-kind 'may')"
        );
        // (b) Iteration 1 was ACCEPTED, dynamic branch on the Lore iteration: 1
        // LORE counter, proving rebind binds the iterated kind.
        assert_eq!(
            src.counters.get(&CounterType::Lore).copied().unwrap_or(0),
            1,
            "accepting the Lore kind's dynamic branch must place a LORE counter (rebind)"
        );
    }

    /// CR 122.1 (T1d): empty controlled-counter set → 0 iterations → no prompt.
    #[test]
    fn bribe_taker_empty_counter_set_no_prompt() {
        use crate::types::ability::{IterationKindBinding, TypeFilter, TypedFilter};
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bribe Taker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // No counters anywhere the controller controls.
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Permanent],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });
        let dynamic_branch = AbilityDefinition {
            iteration_kind_binding: Some(IterationKindBinding::RebindToIteratedKind),
            ..AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            )
        };
        let mut ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: crate::types::ability::PlayerFilter::Controller,
                branches: vec![dynamic_branch],
            },
            vec![],
            source,
            PlayerId(0),
        );
        // CR 608.2c + CR 608.2d: `optional = true` mirrors the real card. With
        // per-kind optionality, the "you may" gate fires INSIDE the `repeat_for`
        // loop per iterated kind — so an empty controlled-counter set yields 0
        // iterations, 0 gates, and no prompt. The up-front single gate in
        // `resolve_chain_body` is suppressed for `DistinctCounterKindsAmong`
        // loops (see `has_kind_driven_repeat`), so it cannot leak a stray prompt
        // on the empty set.
        ability.optional = true;
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong { filter },
        });

        let initial_waiting = state.waiting_for.clone();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        // Zero iterations → no prompt installed → waiting_for unchanged.
        assert_eq!(
            state.waiting_for, initial_waiting,
            "empty counter set must not prompt"
        );
    }

    /// CR 122.1 + CR 608.2d: Dramatist's Puppet / Quarry Hauler end-to-end — a
    /// `TargetOnly` parent plus a per-kind add-OR-remove `ChooseOneOf` driven by
    /// `repeat_for: DistinctCounterKindsAmong { ParentTarget }` over the chosen
    /// permanent. Discriminating: the kind set must resolve against the chosen
    /// TARGET (not the battlefield), and the remove branch must rebind to the
    /// iterated kind. Two kinds on the target; the deterministic sort by
    /// `as_str` is ["P1P1", "lore"], so iteration 0 (Plus1Plus1) ADDS and
    /// iteration 1 (Lore) REMOVES.
    #[test]
    fn targeted_counter_adjust_add_and_remove_per_kind() {
        use crate::game::engine::apply;
        use crate::types::ability::{IterationKindBinding, PlayerFilter};
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quarry Hauler".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // The chosen target permanent carries TWO distinct counter kinds.
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
            obj.counters.insert(CounterType::Lore, 1);
        }

        let add_branch = AbilityDefinition {
            iteration_kind_binding: Some(IterationKindBinding::RebindToIteratedKind),
            ..AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ParentTarget,
                },
            )
        };
        let remove_branch = AbilityDefinition {
            iteration_kind_binding: Some(IterationKindBinding::RebindToIteratedKind),
            ..AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RemoveCounter {
                    counter_type: Some(CounterType::Plus1Plus1),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ParentTarget,
                },
            )
        };

        let mut choice_sub = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![add_branch, remove_branch],
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        choice_sub.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong {
                filter: TargetFilter::ParentTarget,
            },
        });

        let mut ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Permanent],
                    controller: None,
                    properties: vec![],
                }),
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(choice_sub));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Iteration 0 (Plus1Plus1 — sorts first): surface the branch choice, ADD.
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "iteration 0 must surface the per-kind add/remove choice, got {:?}",
            state.waiting_for
        );
        let r = apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 }, // add
        )
        .unwrap();
        events.extend(r.events);

        // Iteration 1 (Lore): surface again, then REMOVE.
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "iteration 1 must surface its own per-kind choice, got {:?}",
            state.waiting_for
        );
        let r = apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 1 }, // remove
        )
        .unwrap();
        events.extend(r.events);

        let obj = state.objects.get(&target).unwrap();
        // Plus1Plus1: started at 2, ADD one of THAT kind → 3 (rebind binds P1P1).
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            3,
            "adding the P1P1 iteration must add a P1P1 counter (rebind)"
        );
        // Lore: started at 1, REMOVE one of THAT kind → 0 (rebind binds Lore).
        assert_eq!(
            obj.counters.get(&CounterType::Lore).copied().unwrap_or(0),
            0,
            "removing the Lore iteration must remove a LORE counter (rebind)"
        );
    }

    /// CR 402.1 / 119.1 / 119.3 / 122.1f / 404.1: `candidate_player_scalar` reads each
    /// scalar `QuantityRef` attribute directly off the candidate
    /// `Player`, and returns `None` for any non-scalar `QuantityRef` (failing
    /// the predicate closed). Exercises the building block across its full input
    /// range, not a single card.
    #[test]
    fn candidate_player_scalar_reads_each_attribute() {
        let mut state = GameState::new(FormatConfig::commander(), 3, 42);
        // Give the player distinguishable scalar values so the wrong attribute
        // can never accidentally pass.
        {
            let p = &mut state.players[1];
            p.life = 17;
            p.life_lost_this_turn = 3;
            p.poison_counters = 4;
            p.hand.push_back(ObjectId(1));
            p.hand.push_back(ObjectId(2));
            p.hand.push_back(ObjectId(3));
            p.graveyard.push_back(ObjectId(4));
            p.graveyard.push_back(ObjectId(5));
            p.player_counter(&PlayerCounterKind::Experience); // no-op read
            p.add_player_counters(&PlayerCounterKind::Experience, 6);
        }
        let p = &state.players[1];

        // CR 402.1: hand size reads p.hand.len(), ignoring the inert PlayerScope.
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::HandSize {
                    player: PlayerScope::Controller
                }
            ),
            Some(3)
        );
        // CR 810.9a: life total now needs game state (team fold), so the
        // STATELESS reader returns None and falls the predicate closed.
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer
                }
            ),
            None
        );
        // CR 119.1 + CR 810.9a: the STATEFUL reader returns the team total —
        // off-team (Commander) this is the candidate's own life (17).
        assert_eq!(
            candidate_player_scalar_with_state(
                &state,
                p,
                PlayerId(0),
                &QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer
                }
            ),
            Some(17)
        );
        // CR 119.3: life lost this turn reads p.life_lost_this_turn.
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::ScopedPlayer
                }
            ),
            Some(3)
        );
        // CR 404.1: graveyard size reads p.graveyard.len().
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::GraveyardSize {
                    player: PlayerScope::Controller
                }
            ),
            Some(2)
        );
        // CR 122.1f: poison reads the dedicated poison_counters field.
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Poison,
                    scope: crate::types::ability::CountScope::ScopedPlayer,
                }
            ),
            Some(4)
        );
        // CR 122.1: a generic player counter reads the player_counters map.
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Experience,
                    scope: crate::types::ability::CountScope::ScopedPlayer,
                }
            ),
            Some(6)
        );
        // Non-scalar QuantityRef → None (parser invariant; fails the predicate
        // closed rather than reading a controller-scoped quantity off a
        // candidate).
        assert_eq!(
            candidate_player_scalar(p, &QuantityRef::Variable { name: "X".into() }),
            None
        );
        assert_eq!(
            candidate_player_scalar(
                p,
                &QuantityRef::ObjectCount {
                    filter: TargetFilter::Any
                }
            ),
            None
        );
    }

    /// Issue #2405: Broken Bond — optional hand→battlefield land put must not
    /// consume the land-drop counter (CR 305.4).
    #[test]
    fn issue_2405_broken_bond_land_put_does_not_consume_land_drop() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Destroy target artifact or enchantment. You may put a land card from your hand onto the battlefield.",
            AbilityKind::Spell,
        );
        let sub = def.sub_ability.as_ref().expect("land put sub");

        let mut state = GameState::new_two_player(42);
        state.lands_played_this_turn = 1;
        state.players[0].lands_played_this_turn = 1;
        let _played_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let hand_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Island".to_string(),
            Zone::Hand,
        );
        for id in [_played_land, hand_land] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Land];
        }
        let mut ability =
            crate::game::ability_utils::build_resolved_from_def(sub, ObjectId(999), PlayerId(0));
        ability.optional = false;
        ability.context.optional_effect_performed = true;
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert_eq!(
            state.lands_played_this_turn, 1,
            "effect-driven land put must not consume land drop"
        );
        assert!(
            state.battlefield.contains(&hand_land),
            "land must reach battlefield, waiting_for={:?}",
            state.waiting_for
        );
    }

    /// Issue #2403: Sin, Spira's Punishment — random exile must publish a
    /// tracked set so the chained `CopyTokenOf` creates a tapped copy.
    #[test]
    fn issue_2403_sin_spira_random_exile_copy_token_from_tracked_set() {
        let mut state = GameState::new_two_player(42);

        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_card).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = obj.card_types.clone();
            obj.base_name = "Grizzly Bears".to_string();
        }
        state.players[0].graveyard.push_back(gy_card);

        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Sin, Spira's Punishment".to_string(),
            Zone::Stack,
        );

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Exile a permanent card from your graveyard at random, then create a tapped token that's a copy of that card.",
            AbilityKind::Spell,
        );
        let mut ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        ability.target_selection_mode = crate::types::ability::TargetSelectionMode::Random;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).expect("copy token created");
        assert!(token.is_token);
        assert!(token.tapped);
        assert_eq!(token.name, "Grizzly Bears");
        assert_eq!(state.objects[&gy_card].zone, Zone::Exile);
    }

    /// Issue #2400: Doubling Chant — `repeat_for: ObjectCount` over controlled
    /// creatures must drive the `SearchLibrary` loop when the search filter uses
    /// `SameNameAsParentTarget`, not silently produce zero iterations.
    #[test]
    fn issue_2400_doubling_chant_member_driven_search_iterations() {
        let mut state = GameState::new_two_player(42);

        for (card_id, name) in [(CardId(1), "Bear"), (CardId(2), "Elephant")] {
            let id = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
        }
        let library_bear = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&library_bear)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let library_elephant = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Elephant".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&library_elephant)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let library_bear_noncreature = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );

        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Doubling Chant".to_string(),
            Zone::Stack,
        );

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "For each creature you control, you may search your library for a creature card with the same name as that creature. Put those cards onto the battlefield, then shuffle.",
            AbilityKind::Spell,
        );
        let ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        let pending = state
            .pending_repeat_iteration
            .as_ref()
            .expect("first optional prompt must stash the remaining iteration");
        assert_eq!(
            pending.total_iterations, 2,
            "two controlled creatures imply two search iterations"
        );
        assert_eq!(pending.next_iteration, 1);
        assert_eq!(pending.tracked_members.len(), 2);

        let first_member = pending.tracked_members[0];
        let first_name = state.objects.get(&first_member).unwrap().name.clone();
        let expected_first_card = match first_name.as_str() {
            "Bear" => library_bear,
            "Elephant" => library_elephant,
            other => panic!("unexpected iterated creature {other}"),
        };
        let other_card = if expected_first_card == library_bear {
            library_elephant
        } else {
            library_bear
        };

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let WaitingFor::SearchChoice { cards, count, .. } = &state.waiting_for else {
            panic!(
                "accepting first per-creature optional search must enter SearchChoice, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*count, 1);
        assert!(
            cards.contains(&expected_first_card),
            "SearchChoice must offer same-named creature card {expected_first_card:?} for {first_name}"
        );
        assert!(
            !cards.contains(&other_card),
            "SearchChoice must not offer a creature with the other iterated name"
        );
        assert!(
            !cards.contains(&library_bear_noncreature),
            "SearchChoice must still require a creature card"
        );
    }

    #[test]
    fn dichotomancy_member_driven_search_uses_target_players_library() {
        let mut state = GameState::new_two_player(42);

        let battlefield_relic = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battlefield_relic).unwrap();
            obj.controller = PlayerId(1);
            obj.tapped = true;
            obj.card_types.core_types = vec![crate::types::card_type::CoreType::Artifact];
        }

        let caster_library_relic = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Library,
        );
        let opponent_library_relic = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Relic".to_string(),
            Zone::Library,
        );

        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Dichotomancy".to_string(),
            Zone::Stack,
        );

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "For each tapped nonland permanent target opponent controls, search that player's library for a card with the same name as that permanent and put it onto the battlefield under your control. Then that player shuffles.",
            AbilityKind::Spell,
        );
        let mut ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "Dichotomancy must enter a search choice for the target player's library, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*player, PlayerId(0), "the caster searches that library");
        assert_eq!(*count, 1);
        assert!(
            cards.contains(&opponent_library_relic),
            "search must offer same-named card from the target opponent's library"
        );
        assert!(
            !cards.contains(&caster_library_relic),
            "search must not offer same-named card from the caster's library"
        );
    }

    /// CR 707.10 + CR 608.2c (issue #1370): the reflexive "if you don't copy a
    /// spell this way" gate keys on whether a copy was actually made. A
    /// `CopySpell` parent counts as "performed" iff it pushed a copy onto the
    /// stack (`StackPushed`); when the source can't be copied no such event
    /// fires, so the negated rider (Shiko's draw) must run. This is
    /// the building-block contract the full-pipeline `shiko_*` integration tests
    /// exercise end to end — asserted here directly so the class is covered
    /// independent of the parser/scenario wiring.
    #[test]
    fn copy_spell_performed_tracks_stack_pushed_event() {
        let copy = Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: crate::types::ability::CopyRetargetPermission::MayChooseNewTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        };

        // A copy was put on the stack → the effect was performed → the negated
        // "if you don't copy" rider must NOT fire.
        let made = [GameEvent::StackPushed {
            object_id: ObjectId(7),
        }];
        assert!(
            mandatory_parent_effect_performed(&copy, &made),
            "a CopySpell that pushed a copy onto the stack is 'performed'"
        );

        // No copy was made (e.g. the source can't be copied) → not performed →
        // the negated rider fires.
        let not_made = [GameEvent::EffectResolved {
            kind: EffectKind::CopySpell,
            source_id: ObjectId(7),
        }];
        assert!(
            !mandatory_parent_effect_performed(&copy, &not_made),
            "a CopySpell that made no copy is NOT 'performed' — the draw rider must run"
        );
    }

    /// CR 110.2 + CR 608.2c (issue #1335): Kain's "that player gains control of
    /// Kain. If they do, …" gates the rider on whether control actually
    /// transferred. `GiveControl` counts as performed only when
    /// `ControllerChanged` or a `GiveControl` `EffectResolved` is emitted.
    #[test]
    fn give_control_performed_tracks_controller_changed_event() {
        let give = Effect::GiveControl {
            target: TargetFilter::SelfRef,
            recipient: TargetFilter::TriggeringPlayer,
        };

        let transferred = [GameEvent::ControllerChanged {
            object_id: ObjectId(1),
            old_controller: PlayerId(0),
            new_controller: PlayerId(1),
        }];
        assert!(
            mandatory_parent_effect_performed(&give, &transferred),
            "GiveControl that changed controllers is 'performed'"
        );

        let not_transferred: [GameEvent; 0] = [];
        assert!(
            !mandatory_parent_effect_performed(&give, &not_transferred),
            "GiveControl that failed must not seed the if-they-do rider"
        );
    }

    /// CR 702.131b + CR 702.131d (#2873): Ocelot Pride's race. The parent
    /// `Effect::Token` pushes the controller from 9 to 10 permanents *during*
    /// resolution. The sub-ability is gated on `HasCityBlessing`. Without the
    /// eager re-evaluation in `resolve_chain_body`, `state.city_blessing` is
    /// still empty when the sub-ability condition is checked (it would only be
    /// updated by the SBA loop at the next priority pass), so the sub-ability's
    /// token would NOT be created. With the fix, the blessing is granted before
    /// the gate fires, so BOTH tokens exist.
    ///
    /// Discriminating assertion: `state.battlefield.len() == 11` (9 + 2 tokens)
    /// and `state.city_blessing.contains(PlayerId(0))`. Reverting the fix makes
    /// the sub-ability gate read a false `HasCityBlessing`, dropping the second
    /// token (battlefield 10, not 11).
    #[test]
    fn city_blessing_race_grants_sub_ability_token_same_resolution() {
        let mut state = GameState::new_two_player(42);

        let mut ascend_permanent = None;
        for i in 0..9u64 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Permanent {i}"),
                Zone::Battlefield,
            );
            if i == 0 {
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                obj.base_card_types = obj.card_types.clone();
                obj.base_power = Some(1);
                obj.base_toughness = Some(1);
                obj.power = Some(1);
                obj.toughness = Some(1);
                obj.keywords.push(Keyword::Ascend);
                obj.static_definitions.push(
                    StaticDefinition::continuous()
                        .condition(crate::types::ability::StaticCondition::HasCityBlessing)
                        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
                        .modifications(vec![
                            ContinuousModification::AddPower { value: 1 },
                            ContinuousModification::AddToughness { value: 1 },
                        ]),
                );
                ascend_permanent = Some(id);
            }
        }
        crate::game::layers::flush_layers(&mut state);
        assert!(
            !state.city_blessing.contains(&PlayerId(0)),
            "precondition: no city's blessing at 9 permanents"
        );
        assert_eq!(
            state
                .objects
                .get(&ascend_permanent.unwrap())
                .and_then(|obj| obj.power),
            Some(1),
            "the city's-blessing-gated continuous effect must be inactive before the grant"
        );

        let make_cat = || Effect::Token {
            name: "Cat".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string(), "Cat".to_string()],
            colors: vec![ManaColor::White],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };

        let mut sub = ResolvedAbility::new(make_cat(), vec![], ObjectId(1000), PlayerId(0));
        sub.condition = Some(AbilityCondition::HasCityBlessing);

        let mut parent = ResolvedAbility::new(make_cat(), vec![], ObjectId(1000), PlayerId(0));
        parent.sub_ability = Some(Box::new(sub));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &parent, &mut events, 0).unwrap();

        assert!(
            state.city_blessing.contains(&PlayerId(0)),
            "the parent token made the 10th permanent, so the blessing must be granted \
             before the sub-ability gate fires"
        );
        assert_eq!(
            state.battlefield.len(),
            11,
            "9 starting permanents + parent Cat + sub-ability Cat = 11; a missing \
             city's-blessing re-evaluation would drop the sub-ability token (10)"
        );
        assert_eq!(
            state
                .objects
                .get(&ascend_permanent.unwrap())
                .and_then(|obj| obj.power),
            Some(2),
            "CR 702.131d: continuous effects gated on the city's blessing must be \
             reapplied before the sub-ability condition/effect continues"
        );
    }

    #[test]
    fn condition_contains_city_blessing_recurses_through_condition_instead() {
        let condition = AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::HasCityBlessing),
        };

        assert!(
            condition_contains_city_blessing(&condition),
            "city's-blessing gated continuations wrapped in ConditionInstead must run the \
             mid-chain blessing check before the condition is evaluated"
        );
    }

    /// CR 601.2a + CR 608.2c (issue #1162): Expressive Iteration looks at
    /// three cards, keeps one in hand, then must still reach the bottom/exile
    /// tail on the other looked-at cards.
    #[test]
    fn expressive_iteration_dig_chain_reaches_library_bottom_and_exile() {
        use crate::game::engine;
        use crate::types::ability::CastingPermission;
        use crate::types::ability::Duration;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Expressive Iteration".to_string(),
            Zone::Stack,
        );
        let card_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let card_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        let card_c = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".to_string(),
            Zone::Library,
        );
        state.players[0].library = vec![card_a, card_b, card_c].into();

        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Look at the top three cards of your library. Put one of them into your hand, put one of them on the bottom of your library, and exile one of them. You may play the exiled card this turn.",
            AbilityKind::Spell,
        );
        let ability =
            crate::game::ability_utils::build_resolved_from_def(&def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "expected initial dig choice, got {:?}",
            state.waiting_for
        );

        engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![card_a],
            },
        )
        .unwrap();

        let tracked: Vec<_> = state
            .tracked_object_sets
            .get(
                &state
                    .chain_tracked_set_id
                    .expect("dig tail must publish a tracked set"),
            )
            .expect("tracked set must exist")
            .clone();
        assert_eq!(
            tracked,
            vec![card_b, card_c],
            "dig must publish only the unkept looked-at cards"
        );

        let WaitingFor::EffectZoneChoice {
            cards: eligible,
            effect_kind,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected bottom-of-library choice after keeping to hand, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            effect_kind,
            crate::types::ability::EffectKind::PutAtLibraryPosition
        );
        assert_eq!(
            eligible,
            vec![card_b, card_c],
            "bottom choice must be among unkept library cards"
        );

        engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![card_b],
            },
        )
        .unwrap();

        assert_eq!(state.objects[&card_a].zone, Zone::Hand);
        assert_eq!(state.objects[&card_b].zone, Zone::Library);
        assert_eq!(state.objects[&card_c].zone, Zone::Exile);
        assert!(
            state.players[0].library.back() == Some(&card_b),
            "card B must be on the bottom of the library"
        );
        assert!(
            !state.objects[&card_b]
                .casting_permissions
                .iter()
                .any(|p| matches!(p, CastingPermission::PlayFromExile { .. })),
            "bottomed card must not receive play permission"
        );
        assert!(
            state.objects[&card_c]
                .casting_permissions
                .iter()
                .any(|p| matches!(
                    p,
                    CastingPermission::PlayFromExile {
                        duration: Duration::UntilEndOfTurn,
                        granted_to: PlayerId(0),
                        ..
                    }
                )),
            "exiled card must receive play-this-turn permission"
        );
    }

    /// CR 122.1 + CR 701.10e + CR 608.2c: Turtle Van's attack trigger — "put a
    /// +1/+1 counter on target creature that crewed it this turn. Then if that
    /// creature is a Mutant, Ninja, or Turtle, double the number of +1/+1 counters
    /// on it." Drives the REAL parser (`parse_oracle_text`) and the REAL chain
    /// resolver (`resolve_ability_chain`) with the crewing creature as the chosen
    /// target.
    ///
    /// Helper returns the crewing creature's final +1/+1 counter total after the
    /// chain resolves. `subtype` selects whether the conditional doubling fires.
    fn run_turtle_van_chain(subtype: &str, starting_counters: u32) -> u32 {
        use crate::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(11);
        // The crewing creature targeted by the trigger.
        let crewer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Crewer".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&crewer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push(subtype.to_string());
            obj.power = Some(2);
            obj.toughness = Some(2);
            if starting_counters > 0 {
                obj.counters
                    .insert(CounterType::Plus1Plus1, starting_counters);
            }
        }
        // The Vehicle is the ability source.
        let vehicle = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Turtle Van".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vehicle)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let parsed = parse_oracle_text(
            "Whenever this Vehicle attacks, put a +1/+1 counter on target creature that crewed it this turn. Then if that creature is a Mutant, Ninja, or Turtle, double the number of +1/+1 counters on it.\nCrew 1",
            "Turtle Van",
            &[],
            &["Artifact".to_string()],
            &["Vehicle".to_string()],
        );
        let trigger = parsed
            .triggers
            .first()
            .expect("Turtle Van must parse an attack trigger");
        let execute = trigger
            .execute
            .as_deref()
            .expect("attack trigger must carry an execute ability");

        let ability = crate::game::ability_utils::build_resolved_from_def_with_targets(
            execute,
            vehicle,
            PlayerId(0),
            vec![TargetRef::Object(crewer)],
        );

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        state.objects[&crewer]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    #[test]
    fn turtle_van_doubles_counters_on_matching_crewer() {
        // Turtle crewer starting with 2 counters: PutCounter → 3, then double → 6.
        // 6 is distinct from the no-double result (3) AND a no-op (2), so reverting
        // either the condition wiring or the MultiplyCounter→ParentTarget rewrite
        // flips this assertion.
        assert_eq!(
            run_turtle_van_chain("Turtle", 2),
            6,
            "Turtle crewer: 2 + 1 = 3, doubled to 6"
        );
        // Ninja and Mutant must match the same subtype disjunction.
        assert_eq!(
            run_turtle_van_chain("Ninja", 0),
            2,
            "Ninja: 0 + 1 = 1, doubled to 2"
        );
        assert_eq!(
            run_turtle_van_chain("Mutant", 1),
            4,
            "Mutant: 1 + 1 = 2, doubled to 4"
        );
    }

    #[test]
    fn turtle_van_does_not_double_counters_on_nonmatching_crewer() {
        // A Wizard is none of Mutant/Ninja/Turtle: the conditional doubling must
        // NOT fire. Only the PutCounter applies: 2 + 1 = 3 (no double to 6).
        assert_eq!(
            run_turtle_van_chain("Wizard", 2),
            3,
            "non-matching crewer: only the +1/+1 counter is added, no doubling"
        );
    }

    /// Issue #2019 — Kiora-style look-then-cast must filter chain-injected targets
    /// through the CastFromZone filter, not grant every looked card.
    #[test]
    fn look_then_cast_chain_filters_injected_targets_by_cast_filter() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::types::ability::{CardPlayMode, CastFromZoneDriver, DigSource, TypeFilter};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Kiora".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Library Creature".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Library Instant".to_string(),
            Zone::Library,
        );
        let top_instant = state.players[0].library[0];
        let second_creature = state.players[0].library[1];
        state
            .objects
            .get_mut(&top_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state
            .objects
            .get_mut(&second_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let cast_sub = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CastFromZone {
                target: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::ExiledBySource,
                    ],
                },
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::LingeringPermission,
                mana_spend_permission: None,
            },
        );
        let dig_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 2 },
                destination: None,
                keep_count: Some(0),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
        )
        .sub_ability(cast_sub);

        let ability = build_resolved_from_def(&dig_def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("look-then-cast chain must resolve");

        let instant_obj = state.objects.get(&top_instant).expect("instant");
        assert_eq!(
            instant_obj.zone,
            Zone::Exile,
            "legal instant must be exiled for cast grant"
        );
        assert!(
            instant_obj.casting_permissions.iter().any(|p| matches!(
                p,
                CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
            )),
            "legal instant must receive a free cast permission"
        );

        let creature_obj = state.objects.get(&second_creature).expect("creature");
        assert_eq!(
            creature_obj.zone,
            Zone::Library,
            "illegal looked creature must stay in library"
        );
        assert!(
            creature_obj.casting_permissions.is_empty(),
            "illegal looked creature must not receive a cast permission"
        );
    }

    /// CR 102.2 + CR 603.2 (issue #4361): `PlayerFilter::OpponentOfTriggeringPlayer`
    /// matches every player who is an opponent of the CASTER (the triggering
    /// player), independent of the effect controller. With a SpellCast event by
    /// B: A matches, B does not. With no trigger event: neither matches
    /// (caster anchor undefined → fail closed).
    #[test]
    fn opponent_of_triggering_player_matches_casters_opponents() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Heartwood Storyteller".to_string(),
            Zone::Battlefield,
        );

        // B (PlayerId(1)) is the caster; the effect controller is A (PlayerId(0)).
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(9000),
            controller: PlayerId(1),
            object_id: ObjectId(9000),
        });

        assert!(
            matches_player_scope(
                &state,
                PlayerId(0),
                &PlayerFilter::OpponentOfTriggeringPlayer,
                PlayerId(0),
                source,
            ),
            "A is an opponent of the caster B and must match (even as the controller)"
        );
        assert!(
            !matches_player_scope(
                &state,
                PlayerId(1),
                &PlayerFilter::OpponentOfTriggeringPlayer,
                PlayerId(0),
                source,
            ),
            "B (the caster) is not its own opponent"
        );

        // No trigger event in scope → fail closed (neither matches).
        state.current_trigger_event = None;
        for pid in [PlayerId(0), PlayerId(1)] {
            assert!(
                !matches_player_scope(
                    &state,
                    pid,
                    &PlayerFilter::OpponentOfTriggeringPlayer,
                    PlayerId(0),
                    source,
                ),
                "without a trigger event the caster anchor is undefined; {pid:?} must not match"
            );
        }
    }

    /// CR 102.3 (issue #4361): in Two-Headed Giant, the caster's TEAMMATE is not
    /// an opponent, so `PlayerFilter::OpponentOfTriggeringPlayer` must exclude
    /// them. With a 2HG topology (seats 0,1 = team 0; seats 2,3 = team 1) and a
    /// SpellCast by seat 0, only the two members of the OTHER team match — not
    /// the caster and not the caster's teammate (seat 1). A `p.id != caster`
    /// predicate would wrongly include the teammate; `players::is_opponent` does
    /// not.
    #[test]
    fn opponent_of_triggering_player_excludes_two_headed_giant_teammate() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Heartwood Storyteller".to_string(),
            Zone::Battlefield,
        );

        // Seat 0 casts the spell (the triggering player).
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(9000),
            controller: PlayerId(0),
            object_id: ObjectId(9000),
        });

        // The caster (seat 0) and its teammate (seat 1) are NOT opponents.
        for (pid, who) in [(PlayerId(0), "caster"), (PlayerId(1), "caster's teammate")] {
            assert!(
                !matches_player_scope(
                    &state,
                    pid,
                    &PlayerFilter::OpponentOfTriggeringPlayer,
                    PlayerId(0),
                    source,
                ),
                "2HG: the {who} ({pid:?}) is not an opponent of the caster and must not match"
            );
        }

        // The opposing team (seats 2 and 3) are both opponents and must match.
        for pid in [PlayerId(2), PlayerId(3)] {
            assert!(
                matches_player_scope(
                    &state,
                    pid,
                    &PlayerFilter::OpponentOfTriggeringPlayer,
                    PlayerId(0),
                    source,
                ),
                "2HG: opposing-team member {pid:?} is an opponent of the caster and must match"
            );
        }
    }
}
