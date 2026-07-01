use crate::types::ability::{
    is_variable_remove_counter_cost_count, AbilityCondition, AbilityCost, AbilityDefinition,
    AbilityKind, AdditionalCost, CardPlayMode, CastTimingPermission, CastingPermission, ChoiceType,
    ContinuousModification, CostObjectCount, CostPaidObjectSnapshot, CounterCostSelection,
    Duration, Effect, FilterProp, GameRestriction, ModalSelectionCondition, ObjectScope,
    PlayerFilter, PlayerScope, ProhibitedActivity, QuantityExpr, QuantityRef, ResolvedAbility,
    RestrictionExpiry, RestrictionPlayerScope, StaticCondition, StaticDefinition,
    TapCreaturesRequirement, TargetFilter, TargetRef,
};
use crate::types::actions::AlternativeCastDecision;
use crate::types::card::LayoutKind;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActivationResidual, CastOfferKind, CastPaymentMode, CastingVariant, CastingVariantChoiceOption,
    ConvokeMode, CostResume, GameState, NextSpellModifier, PayCostKind, PendingCast,
    SneakPlacement, SpellCastRecord, SpellCostSource, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind};
use crate::types::mana::{
    ManaColor, ManaCost, ManaCostShard, ManaSpellGrant, PaymentContext, SpecialAction, SpellMeta,
};
use crate::types::player::PlayerId;
use crate::types::statics::{
    ActivationExemption, AdditionalCostTaxAction, CastFreeOrigin, CastFrequency,
    CastingProhibitionCondition, CostModifyMode, ExileCardPool, ExileCastCost, ExileCastTiming,
    ProhibitionScope, StaticMode,
};
use crate::types::zones::{ExileCostSourceZone, Zone};

use std::collections::HashSet;

use super::ability_utils::{
    ability_target_legality_needs_chosen_x, assign_targets_in_chain, auto_select_targets,
    auto_select_targets_for_ability, begin_target_selection, begin_target_selection_for_ability,
    build_resolved_from_def, build_target_slots, compute_unavailable_modes,
    filter_references_target_player, flatten_targets_in_chain,
    has_legal_target_assignment_for_ability, kicker_instead_spell_has_legal_targets,
    modal_choice_for_player, simple_legal_target_assignment_exists_for_ability,
    target_constraints_from_modal,
};
use super::casting_costs::{self, check_additional_cost_or_pay};
use super::engine::EngineError;
use super::functioning_abilities::active_static_definitions;
use super::game_object::{GameObject, PreparedState, PrototypeFormState};
use super::mana_payment;
use super::priority;
use super::quantity::resolve_quantity;
use super::restrictions;
use super::speed::effective_speed;
use super::splice;
use super::stack;
use super::targeting;

const FORETELL_SPECIAL_ACTION_COST: u32 = 2;

fn runtime_granted_cycling_abilities(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<AbilityDefinition> {
    let Some(obj) = state.objects.get(&source_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Hand {
        return Vec::new();
    }

    crate::game::off_zone_characteristics::effective_off_zone_keywords(state, source_id)
        .into_iter()
        .filter(|keyword| {
            matches!(keyword, Keyword::Cycling(_) | Keyword::Typecycling { .. })
                && !obj.base_keywords.iter().any(|printed| printed == keyword)
        })
        .filter_map(|keyword| crate::database::synthesis::cycling_ability_for_keyword(&keyword))
        .collect()
}

/// CR 604.1 (seam 4: activated-ability-on-grant): synthesize graveyard activated
/// abilities (Encore, Scavenge) for keywords granted to a graveyard card by a
/// static. The `AddKeyword` layer seam installs only the keyword + triggers, so a
/// granted graveyard activated keyword carries no activatable ability without
/// this on-the-fly synthesis. Mirrors `runtime_granted_cycling_abilities`: only
/// keywords present in the *effective* (granted-inclusive) set but NOT printed on
/// the card are synthesized, so a printed Encore/Scavenge ability (already in
/// `obj.abilities`) is never double-counted.
fn runtime_granted_graveyard_activated_abilities(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<AbilityDefinition> {
    let Some(obj) = state.objects.get(&source_id) else {
        return Vec::new();
    };
    if obj.zone != Zone::Graveyard {
        return Vec::new();
    }

    crate::game::off_zone_characteristics::effective_off_zone_keywords(state, source_id)
        .into_iter()
        .filter(|keyword| !obj.base_keywords.iter().any(|printed| printed == keyword))
        .filter_map(|keyword| {
            // CR 702.128a / CR 702.129a / CR 702.141a: Embalm / Eternalize / Encore
            // granted with a self-referential cost ("equal to its mana cost" or
            // "where X is its mana value") carry `ManaCost::SelfManaCost` or
            // `ManaCost::SelfManaValue`; concretize before synthesizing the
            // activated ability (the activated-ability payment path would
            // otherwise treat those placeholders as free).
            let keyword = super::keywords::resolve_self_cost_graveyard_activated_keyword(
                state, source_id, &keyword,
            );
            crate::database::synthesis::graveyard_activated_ability_for_keyword(&keyword).or_else(
                || {
                    crate::database::embalm_eternalize::embalm_eternalize_ability_for_keyword(
                        &keyword,
                    )
                },
            )
        })
        .collect()
}

/// CR 702.170f + CR 702.170a: synthesize the plot special action as a runtime-
/// granted activated ability on the *authorized top card* of a player's library
/// (Fblthp, Lost on the Range). CR 702.170f authorizes plot to function from a
/// zone other than hand (here the library) and to exile from that zone; the
/// nonland eligibility is Fblthp's printed L4 scope (NOT a CR 702.170f clause),
/// enforced by the delegated `top_of_library_plot_source` predicate. Returns
/// `vec![]` for every object that is not the current authorized top card, so no
/// non-top library card can ever carry a plot ability. Mirrors
/// `runtime_granted_graveyard_activated_abilities`.
///
/// `activation_zone = Some(Zone::Library)` (set by `build_plot_activation`) is a
/// first-of-its-kind value. It is safe ONLY because this ability is present
/// exclusively on the positional top card — `top_of_library_plot_source`
/// re-derives `library.front()` each call, so the activation gate's
/// `obj.zone == Library` check passes just that one card. A future change that
/// grants an ability with `activation_zone = Library` by a NON-positional path
/// would authorize every library card; do not copy this value blindly.
fn runtime_granted_top_of_library_plot_abilities(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<AbilityDefinition> {
    let Some(obj) = state.objects.get(&source_id) else {
        return Vec::new();
    };
    // Cheap zone guard before the battlefield scan: plot-from-library functions
    // only in the Library zone (CR 702.170f).
    if obj.zone != Zone::Library {
        return Vec::new();
    }
    // CR 702.170d: the plot grant belongs to the library's owner — the player
    // who may later cast the plotted card. Delegate authorization to the
    // single-authority predicate; it must return exactly this top card.
    let player = obj.owner;
    let Some((top_id, _src_id)) = top_of_library_plot_source(state, player) else {
        return Vec::new();
    };
    if top_id != source_id {
        return Vec::new();
    }
    // CR 702.170a: plot cost = the card's mana cost, computed live from the top
    // card (not stored on the static). CR 702.170f: the ability functions in,
    // and exiles from, the Library zone. `build_plot_activation` is the single
    // authority for the cost/effect shape (shared verbatim with hand-Plot).
    vec![crate::database::synthesis::build_plot_activation(
        obj.mana_cost.clone(),
        Zone::Library,
        Zone::Library,
    )]
}

pub fn activated_ability_definitions(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<(usize, AbilityDefinition)> {
    let Some(obj) = state.objects.get(&source_id) else {
        return Vec::new();
    };
    let printed_len = obj.abilities.len();
    let mut abilities: Vec<(usize, AbilityDefinition)> =
        obj.abilities.iter().cloned().enumerate().collect();
    abilities.extend(
        runtime_granted_cycling_abilities(state, source_id)
            .into_iter()
            .chain(runtime_granted_graveyard_activated_abilities(
                state, source_id,
            ))
            // CR 702.170f: plot-from-library (Fblthp) chained LAST — must use
            // the identical append order in `activation_ability_definition` so
            // the `ability_index` stays consistent between enumeration and
            // activation. Empty for every object except the authorized top card.
            .chain(runtime_granted_top_of_library_plot_abilities(
                state, source_id,
            ))
            .enumerate()
            .map(|(offset, ability)| (printed_len + offset, ability)),
    );
    abilities
}

fn activation_ability_definition(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<AbilityDefinition> {
    let obj = state.objects.get(&source_id)?;
    let mut ability = if let Some(ability) = obj.abilities.get(ability_index) {
        ability.clone()
    } else {
        let offset = ability_index.checked_sub(obj.abilities.len())?;
        // Must match the append order in `activated_ability_definitions`: printed
        // abilities first, then runtime-granted cycling, then runtime-granted
        // graveyard activated (Encore/Scavenge), then runtime-granted
        // plot-from-library (Fblthp). Identical order is REQUIRED for
        // `ability_index` consistency.
        runtime_granted_cycling_abilities(state, source_id)
            .into_iter()
            .chain(runtime_granted_graveyard_activated_abilities(
                state, source_id,
            ))
            .chain(runtime_granted_top_of_library_plot_abilities(
                state, source_id,
            ))
            .nth(offset)?
    };
    if let Some(ref cost) = ability.cost {
        ability.cost = Some(super::keywords::resolve_self_mana_in_ability_cost(
            state, source_id, cost,
        ));
    }
    if matches!(ability.effect.as_ref(), Effect::Encore) {
        if let Some(ref mut cost) = ability.cost {
            super::keywords::concretize_encore_mana_value_in_ability_cost(state, source_id, cost);
        }
    }
    Some(ability)
}

pub(crate) fn variable_speed_payment_range(cost: &AbilityCost, max_speed: u8) -> Option<(u8, u8)> {
    match cost {
        AbilityCost::PaySpeed {
            amount:
                QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable { .. },
                },
        } => Some((0, max_speed)),
        AbilityCost::Composite { costs } => costs
            .iter()
            .find_map(|sub_cost| variable_speed_payment_range(sub_cost, max_speed)),
        _ => None,
    }
}

pub(crate) fn begin_variable_speed_payment(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    resolved: ResolvedAbility,
    cost: AbilityCost,
    ability_index: usize,
) -> WaitingFor {
    let max_speed = effective_speed(state, player);
    let (min, max) = variable_speed_payment_range(&cost, max_speed).unwrap_or((0, max_speed));
    let mut pending = PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
    pending.activation_cost = Some(cost);
    pending.activation_ability_index = Some(ability_index);
    state.pending_cast = Some(Box::new(pending));
    WaitingFor::NamedChoice {
        player,
        options: (min..=max).map(|value| value.to_string()).collect(),
        choice_type: ChoiceType::NumberRange { min, max },
        source_id: None,
    }
}

/// CR 107.3a + CR 118.3: X in an activation/additional cost is chosen as part
/// of activating or casting, bounded by the resources available to pay fully.
pub(crate) fn sacrifice_cost_bounds(count: u32, eligible_len: usize) -> (usize, usize) {
    if count == u32::MAX {
        (0, eligible_len)
    } else {
        let exact = count as usize;
        (exact, exact)
    }
}

pub(crate) fn sacrifice_cost_bounds_with_chosen_x(
    count: u32,
    eligible_len: usize,
    chosen_x: Option<u32>,
) -> (usize, usize) {
    if count == u32::MAX {
        if let Some(value) = chosen_x {
            let exact = value as usize;
            return (exact, exact);
        }
    }
    sacrifice_cost_bounds(count, eligible_len)
}

/// Emit `BecomesTarget` and `CrimeCommitted` events for each target.
///
/// Called whenever targets are locked in for a spell or ability. CR 700.13:
/// Targeting an opponent, their permanent, or a card in their graveyard is a crime.
pub(crate) fn emit_targeting_events(
    state: &GameState,
    targets: &[TargetRef],
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let mut crime_committed = false;
    for target in targets {
        match target {
            TargetRef::Object(obj_id) => {
                events.push(GameEvent::BecomesTarget {
                    target: TargetRef::Object(*obj_id),
                    source_id,
                });
                if !crime_committed {
                    if let Some(obj) = state.objects.get(obj_id) {
                        if obj.controller != controller && obj.owner != controller {
                            crime_committed = true;
                        }
                    }
                }
            }
            TargetRef::Player(pid) => {
                events.push(GameEvent::BecomesTarget {
                    target: TargetRef::Player(*pid),
                    source_id,
                });
                if !crime_committed && *pid != controller {
                    crime_committed = true;
                }
            }
        }
    }
    if crime_committed {
        events.push(GameEvent::CrimeCommitted {
            player_id: controller,
        });
    }
}

/// Controls which checks are applied during spell preparation.
///
/// `Actual` is the full rules-correct path used when a player declares a cast.
/// `Display` suppresses situational restrictions (timing, prohibitions, per-turn
/// cast limits, color identity) while preserving the full cost-computation pipeline
/// so the UI can show the effective mana cost the engine would charge without
/// gating on whether the player can legally cast right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CastingMode {
    Actual,
    Display,
}

#[derive(Debug, Clone)]
struct PreparedSpellCast {
    object_id: ObjectId,
    card_id: CardId,
    /// The spell's ability definition. `None` for permanent spells with no
    /// spell-level effect (creatures, artifacts, etc.).
    ability_def: Option<AbilityDefinition>,
    mana_cost: crate::types::mana::ManaCost,
    /// CR 601.2f: The tax-inclusive base cost captured BEFORE any cost
    /// reductions/increases or {X} concretization. Threaded onto
    /// `PendingCast.base_cost` so the full cost can be recomputed from scratch
    /// for any chosen X with floors applied LAST.
    base_mana_cost: crate::types::mana::ManaCost,
    modal: Option<crate::types::ability::ModalChoice>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    /// CR 601.2a: Zone the card was in before announcement (hand / command /
    /// graveyard / exile). Threaded onto `PendingCast.origin_zone` so that
    /// CancelCast (CR 601.2i) can return the object to its origin zone.
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
}

pub(crate) fn combined_spell_ability_def(
    obj: &crate::game::game_object::GameObject,
) -> Option<AbilityDefinition> {
    let mut spell_abilities = obj
        .abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell);
    let mut combined = spell_abilities.next()?.clone();

    if obj.modal.is_some() {
        return Some(combined);
    }

    for spell_ability in spell_abilities {
        append_to_ability_def_sub_chain(&mut combined, spell_ability.clone());
    }

    Some(combined)
}

fn append_to_ability_def_sub_chain(ability: &mut AbilityDefinition, next: AbilityDefinition) {
    let mut node = ability;
    while node.sub_ability.is_some() {
        node = node
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above");
    }
    node.sub_ability = Some(Box::new(next));
}

/// CR 101.2 + CR 601.2a: Temporary restrictions can limit which zones affected
/// players may cast spells from.
fn restriction_scope_matches_player(
    source_controller: Option<PlayerId>,
    affected_players: &RestrictionPlayerScope,
    caster: PlayerId,
) -> bool {
    // CR 101.2: Restriction scope defines who is affected by the prohibition.
    match affected_players {
        RestrictionPlayerScope::AllPlayers => true,
        RestrictionPlayerScope::SpecificPlayer(player) => *player == caster,
        RestrictionPlayerScope::TargetedPlayer => {
            debug_assert!(
                false,
                "TargetedPlayer should be resolved by add_restriction"
            );
            false
        }
        RestrictionPlayerScope::ParentTargetedPlayer => {
            debug_assert!(
                false,
                "ParentTargetedPlayer should be resolved by add_restriction"
            );
            false
        }
        RestrictionPlayerScope::DefendingPlayer => {
            // CR 508.5a: resolved to `SpecificPlayer` by `add_restriction` when
            // the restriction is created. An unresolved scope here means the
            // source was not attacking, so it restricts no one.
            debug_assert!(
                false,
                "DefendingPlayer should be resolved by add_restriction"
            );
            false
        }
        RestrictionPlayerScope::ScopedPlayer => {
            // CR 109.5: resolved to `SpecificPlayer` by `add_restriction` when
            // the restriction is created, so an unresolved scope here is a bug.
            debug_assert!(false, "ScopedPlayer should be resolved by add_restriction");
            false
        }
        RestrictionPlayerScope::OpponentsOfSourceController => {
            source_controller.is_some_and(|controller| controller != caster)
        }
    }
}

/// CR 601.2a: Build the spell-record projection used by prohibition filters.
fn spell_record_for_restrictions(spell_obj: &super::game_object::GameObject) -> SpellCastRecord {
    SpellCastRecord {
        name: spell_obj.name.clone(),
        core_types: spell_obj.card_types.core_types.clone(),
        supertypes: spell_obj.card_types.supertypes.clone(),
        subtypes: spell_obj.card_types.subtypes.clone(),
        keywords: spell_obj.keywords.clone(),
        colors: spell_obj.color.clone(),
        // CR 202.3e: While on the stack, X equals the announced value, not 0.
        mana_value: spell_obj
            .mana_cost
            .mana_value_with_x(spell_obj.zone, spell_obj.cost_x_paid),
        has_x_in_cost: super::casting_costs::cost_has_x(&spell_obj.mana_cost),
        from_zone: spell_obj.zone,
        cast_variant: crate::types::game_state::CastingVariant::Normal,
        was_kicked: !spell_obj.kickers_paid.is_empty(),
    }
}

fn is_blocked_by_cast_only_from_zones(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    caster: PlayerId,
) -> bool {
    state
        .restrictions
        .iter()
        .any(|restriction| match restriction {
            GameRestriction::ProhibitActivity {
                source,
                affected_players,
                activity: ProhibitedActivity::CastOnlyFromZones { allowed_zones },
                ..
            } => {
                let source_controller = state
                    .objects
                    .get(source)
                    .map(|source_obj| source_obj.controller);
                let caster_affected =
                    restriction_scope_matches_player(source_controller, affected_players, caster);
                caster_affected && !allowed_zones.contains(&obj.zone)
            }
            _ => false,
        })
}

/// CR 101.2: Check if a CantCastSpells restriction prevents the given player
/// from casting any spells. E.g., Silence: "Your opponents can't cast spells this turn."
fn is_blocked_by_cant_cast_spells(
    state: &GameState,
    caster: PlayerId,
    spell_obj: Option<&super::game_object::GameObject>,
) -> bool {
    // CR 702.50b: a player who controls a resolved Epic spell can't cast spells
    // for the rest of the game. Activated/triggered abilities and spell copies
    // are unaffected — neither routes through this cast-legality gate.
    if super::effects::epic::is_epic_locked(state, caster) {
        return true;
    }

    let spell_record = spell_obj.map(spell_record_for_restrictions);

    state.restrictions.iter().any(|restriction| {
        let GameRestriction::ProhibitActivity {
            source,
            affected_players,
            expiry,
            activity: ProhibitedActivity::CastSpells { spell_filter },
        } = restriction
        else {
            return false;
        };
        // CR 514.2 + CR 500.7: a still-pre-armed `UntilEndOfNextTurnOf` cast ban
        // ("Each opponent can't cast … during that player's next turn" — Sphinx's
        // Decree / Azor, fanned out per opponent) is not yet in force; it takes
        // effect only once the restricted player's untap step converts it to
        // `EndOfTurn` (turns.rs). Mirror the activate-abilities gate so the ban
        // does not leak onto the creating turn.
        if matches!(expiry, RestrictionExpiry::UntilEndOfNextTurnOf { .. }) {
            return false;
        }
        let source_controller = state
            .objects
            .get(source)
            .map(|source_obj| source_obj.controller);
        let caster_affected =
            restriction_scope_matches_player(source_controller, affected_players, caster);

        // CR 101.2: Once scope matches, filter-matching spells are prohibited.
        caster_affected
            && match spell_filter {
                Some(filter) => spell_record.as_ref().is_some_and(|record| {
                    super::filter::spell_record_matches_filter(
                        record,
                        filter,
                        source_controller.unwrap_or(caster),
                        &state.all_creature_types,
                    )
                }),
                None => true,
            }
    })
}

/// CR 602.5 + CR 605.1a: Temporary game restrictions can prohibit activating
/// abilities, optionally exempting mana abilities via the single classifier.
fn is_blocked_by_cant_activate_abilities(
    state: &GameState,
    caster: PlayerId,
    activating_ability: &AbilityDefinition,
) -> bool {
    state.restrictions.iter().any(|restriction| {
        let GameRestriction::ProhibitActivity {
            source,
            affected_players,
            expiry,
            activity:
                ProhibitedActivity::ActivateAbilities {
                    exemption,
                    only_tag,
                },
        } = restriction
        else {
            return false;
        };
        // CR 514.2 + CR 500.7: A `UntilEndOfNextTurnOf` prohibition (Kang's "during
        // that turn, power-up abilities can't be activated") is created PRE-ARMED and
        // only takes force during the granted extra turn. It stays dormant on the
        // creating turn until that player's next untap step CONVERTS it to
        // `EndOfTurn` (turns.rs). While still pre-armed it is not yet in force, so it
        // must not block activations on the creation turn — the expiry variant is the
        // single source of truth shared with the untap-step arming.
        if matches!(expiry, RestrictionExpiry::UntilEndOfNextTurnOf { .. }) {
            return false;
        }
        let source_controller = state
            .objects
            .get(source)
            .map(|source_obj| source_obj.controller);
        let caster_affected =
            restriction_scope_matches_player(source_controller, affected_players, caster);
        if !caster_affected {
            return false;
        }
        // CR 101.2 + CR 602.5: A tag-scoped prohibition (Kang → power-up) applies
        // only to abilities carrying that keyword tag; every other activation is
        // still legal. `None` prohibits all activations (legacy behavior).
        if let Some(required_tag) = only_tag {
            if activating_ability.ability_tag != Some(*required_tag) {
                return false;
            }
        }
        match exemption {
            ActivationExemption::None => true,
            ActivationExemption::ManaAbilities => {
                // CR 605.1a: Mana abilities are exempt from this prohibition.
                !super::mana_abilities::is_mana_ability(activating_ability)
            }
        }
    })
}

/// Oathbreaker RC: true when `player` has their Oathbreaker on the battlefield
/// under their control. Used to gate signature-spell casting from the command zone.
fn oathbreaker_on_battlefield(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state
            .objects
            .get(id)
            .is_some_and(|obj| obj.is_commander && obj.owner == player && obj.controller == player)
    })
}

pub fn spell_objects_available_to_cast(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    let mut objects: Vec<ObjectId> = player_data.hand.iter().copied().collect();
    if state.format_config.command_zone {
        let ob_in_play = oathbreaker_on_battlefield(state, player);
        objects.extend(
            state
                .objects
                .values()
                .filter(|obj| {
                    obj.owner == player
                        && obj.zone == Zone::Command
                        && (obj.is_commander || (obj.is_signature_spell() && ob_in_play))
                })
                .map(|obj| obj.id),
        );
    }

    // CR 715.3d + CR 400.7i: Cards in exile with casting permissions are
    // castable by their owner, except PlayFromExile binds to the player the
    // resolving effect granted the permission to. CR 305.1 land exclusion lives
    // in `exile_object_castable_by_permission`.
    objects.extend(state.exile.iter().copied().filter(|&obj_id| {
        state
            .objects
            .get(&obj_id)
            .is_some_and(|obj| exile_object_castable_by_permission(state, obj, player))
    }));

    // CR 601.2a + CR 611.2a: Opponent's exiled cards with an alt-cost
    // permission are castable only when that same permission authorizes this
    // player and the current cast constraints.
    objects.extend(state.exile.iter().copied().filter(|&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner != player
                && obj.casting_permissions.iter().any(|permission| {
                    exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
                })
        })
    }));

    objects.extend(graveyard_spell_objects_available_to_cast(
        state,
        player,
        &player_data.graveyard,
    ));

    // CR 601.2a + CR 113.6b + CR 118.9: Cards in exile castable via a
    // `StaticMode::ExileCastPermission` static from a battlefield permanent
    // (Maralen, Fae Ascendant). Restricted to cards exiled "with" the source
    // *this turn* (per the per-turn rolling list); the static's `affected`
    // filter further constrains the eligible cards (type, mana value, etc.).
    // CR 117.1c: per-turn frequency is enforced inside the helper, not by
    // active-player gating, so the same logic covers the rare case of an
    // `Unlimited` printing on either player's turn.
    let exile_permission_ids: HashSet<ObjectId> =
        exile_objects_castable_by_permission(state, player)
            .iter()
            .map(|(obj_id, _source_id, _freq)| *obj_id)
            .collect();
    objects.extend(exile_permission_ids);

    // CR 401.5 + CR 118.9 + CR 601.2a: Top card of library castable via a
    // `TopOfLibraryCastPermission` static (Realmwalker, Future Sight, Bolas's
    // Citadel, Magus of the Future, etc.). Filter is re-evaluated each call
    // because the top changes between priority windows. The card itself stays
    // in `Zone::Library` until `finalize_cast` performs the standard zone-
    // change to `Zone::Stack` — there is NO exile step (CR 601.2a:
    // "moves that card from where it is to the stack").
    if let Some((top_id, _src, _freq, _alt)) =
        top_of_library_permission_source(state, player, Some(CardPlayMode::Cast))
    {
        // Only non-land cards reach the cast path; lands flow through the
        // play-land action (`top_of_library_land_playable_by_permission`).
        if state.objects.get(&top_id).is_some_and(|o| {
            !o.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        }) {
            objects.push(top_id);
        }
    }

    objects
        .into_iter()
        .filter(|obj_id| {
            state.objects.get(obj_id).is_some_and(|obj| {
                !is_blocked_by_cast_only_from_zones(state, obj, player)
                    && !is_blocked_by_cant_cast_spells(state, player, Some(obj))
            })
        })
        .collect()
}

fn graveyard_spell_objects_available_to_cast(
    state: &GameState,
    player: PlayerId,
    graveyard: &im::Vector<ObjectId>,
) -> Vec<ObjectId> {
    let permission_sources = if state.active_player == player {
        graveyard_permission_sources(state, player, Some(CardPlayMode::Cast))
    } else {
        Vec::new()
    };
    let mut keyword_objects = Vec::new();
    let mut permission_objects = Vec::new();
    let mut timed_permission_objects = Vec::new();
    let mut play_from_exile_objects = Vec::new();

    for &obj_id in graveyard {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.owner != player {
            continue;
        }

        // CR 701.17d: A mill effect that grants permission to play "that card"
        // attaches an object-tagged `PlayFromExile` to the milled card in the
        // graveyard (Ark of Hunger, Tablet of Discovery). The permission is
        // consultable from the graveyard exactly as from exile; only non-land
        // cards reach the cast path (CR 305.1 — milled lands are played via
        // `graveyard_lands_playable_by_permission`).
        if play_from_exile_object_in_cast_path(obj)
            && play_from_exile_permission_source(state, obj, player, state.turn_number).is_some()
        {
            play_from_exile_objects.push(obj_id);
        }

        // CR 702.34 / CR 702.81 / CR 702.127 / CR 702.138 / CR 702.180:
        // Cards in graveyard with graveyard-cast keywords. Escape and Retrace
        // must have enough eligible non-mana additional-cost material available.
        if has_effective_graveyard_cast_keyword(state, obj_id, obj)
            && (has_harmonize_keyword(state, obj_id)
                || has_flashback_keyword(state, obj_id)
                || has_aftermath_keyword(state, obj_id)
                || has_disturb_keyword(state, obj_id)
                || retrace_has_discardable_land(state, player, obj_id)
                || jumpstart_has_discardable_card(state, player, obj_id)
                || can_pay_escape_additional_cost(state, player, obj_id)
                // CR 702.187b: Mayhem is eligible only while the card was
                // discarded this turn.
                || (was_discarded_this_turn(state, obj_id)
                    && super::keywords::effective_mayhem_cost(state, obj_id).is_some()))
        {
            keyword_objects.push(obj_id);
        }

        // CR 601.2a + CR 604.3: Cards in graveyard castable via static
        // permission from a battlefield permanent (Lurrus, Karador, etc.).
        // CR 117.1c: "Each of your turns" — only during controller's turn.
        if graveyard_object_castable_by_permission_sources(
            state,
            player,
            obj_id,
            obj,
            &permission_sources,
        ) {
            permission_objects.push(obj_id);
        }

        // CR 601.2a + CR 611.2a: Graveyard objects with a timed
        // `ExileWithAltCost` grant from `CastFromZone` (Emry class).
        if has_graveyard_timed_alt_cost_permission(state, obj, player) {
            timed_permission_objects.push(obj_id);
        }
    }

    let mut objects = keyword_objects;
    objects.extend(permission_objects);
    objects.extend(timed_permission_objects);
    objects.extend(play_from_exile_objects);
    objects
}

fn graveyard_object_castable_by_permission_sources(
    state: &GameState,
    player: PlayerId,
    obj_id: ObjectId,
    obj: &crate::game::game_object::GameObject,
    sources: &[GraveyardPermissionSource<'_>],
) -> bool {
    if obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return false;
    }

    sources.iter().any(|source| {
        // CR 604.2 + CR 110.4: Per-source frequency slot check; for
        // `OncePerTurnPerPermanentType` this is per-(source, permanent-type),
        // so the per-object check must happen inside the object loop.
        frequency_slot_available(state, source.source_id, obj_id, source.frequency) && {
            let ctx =
                super::filter::FilterContext::from_source_with_controller(source.source_id, player);
            super::filter::matches_target_filter(state, obj_id, source.filter, &ctx)
        }
    })
}

/// CR 702.138a + CR 601.2f-h: Check that the player can pay escape's additional
/// (exile) cost. Delegates the whole residual `AbilityCost` to the single
/// affordability authority `AbilityCost::is_payable` — its Composite arm requires
/// ALL sub-costs payable and routes each `Exile` sub-cost (the graveyard clause
/// and the battlefield "Exile a land you control" clause on Lunar Hatchling)
/// through the same `exile_cost_effective_zone` + `eligible_exile_cost_objects`
/// functions the payment arm uses, so the pre-check and payment-time eligibility
/// match by construction. Returns `false` for an unparsed/placeholder escape
/// (no residual), correctly gating it out of legal actions.
fn can_pay_escape_additional_cost(
    state: &GameState,
    player: PlayerId,
    escape_obj_id: ObjectId,
) -> bool {
    let Some((_, residual)) = super::keywords::effective_escape_data(state, escape_obj_id) else {
        return false;
    };
    residual.is_payable(state, player, escape_obj_id)
}

/// CR 702.180: Check if an object has the Harmonize keyword. Off-zone-aware so a
/// granted graveyard harmonize (Songcrafter Mage) is recognized, mirroring
/// `has_flashback_keyword`.
fn has_harmonize_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Harmonize)
}

/// CR 702.34: Check if an object has the Flashback keyword.
fn has_flashback_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Flashback)
}

/// CR 702.187b: Mayhem may be used only "as long as you discarded this card
/// this turn." The mark is stamped on the graveyard object at discard time and
/// auto-expires when the turn advances, so a simple equality against the
/// current turn number is the gate.
fn was_discarded_this_turn(state: &GameState, object_id: ObjectId) -> bool {
    state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.discarded_turn)
        == Some(state.turn_number)
}

/// CR 702.81: Check if an object has the Retrace keyword.
fn has_retrace_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Retrace)
}

/// CR 702.81a: Retrace requires discarding a land card as an additional cost.
fn retrace_has_discardable_land(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    has_retrace_keyword(state, object_id)
        && casting_costs::can_pay_retrace_additional_cost(state, player, object_id)
}

/// CR 702.127: Check if an object has the Aftermath keyword.
fn has_aftermath_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Aftermath)
}

/// CR 702.133: Check if an object has the Jump-start keyword.
fn has_jumpstart_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::JumpStart)
}

/// CR 702.133a: Jump-start's graveyard-cast permission applies only "if the
/// resulting spell is an instant or sorcery spell." The keyword is printed only
/// on instants/sorceries, but an exotic keyword-grant could place it on another
/// card type, so the type is checked explicitly rather than assumed implicit.
fn jumpstart_castable_from_graveyard(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        obj.zone == Zone::Graveyard
            && has_jumpstart_keyword(state, object_id)
            && obj.card_types.core_types.iter().any(|ct| {
                matches!(
                    ct,
                    crate::types::card_type::CoreType::Instant
                        | crate::types::card_type::CoreType::Sorcery
                )
            })
    })
}

/// CR 702.133a: Jump-start requires discarding a card (any card — `filter: None`,
/// unlike Retrace's land filter) as an additional cost, so it is only castable
/// with at least one card in hand and an instant/sorcery in the graveyard.
fn jumpstart_has_discardable_card(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    jumpstart_castable_from_graveyard(state, object_id)
        && casting_costs::can_pay_jumpstart_additional_cost(state, player, object_id)
}

/// CR 702.146: Check if an object has the Disturb keyword.
fn has_disturb_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Disturb)
}

/// CR 702.137a: Spectacle's gate — whether any opponent of `caster` lost life
/// this turn. Mirrors the existing `LifeLostThisTurn`/"an opponent lost life"
/// predicate (see `game/quantity.rs`) so no new state tracking is introduced.
fn an_opponent_lost_life_this_turn(state: &GameState, caster: PlayerId) -> bool {
    state
        .players
        .iter()
        .any(|p| p.id != caster && p.life_lost_this_turn > 0)
}

/// CR 702.76a: Prowl's gate — whether `player` controlled a creature that dealt
/// combat damage to a player this turn while having one of `object_id`'s
/// creature types. The per-turn creature-type ledger
/// (`creature_types_dealt_combat_damage_this_turn`) is snapshot at damage time.
/// Single authority shared by the candidate path and the normal-vs-prowl
/// alternative-cast choice so both agree on legality.
fn prowl_damage_ledger_satisfied(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    state
        .creature_types_dealt_combat_damage_this_turn
        .iter()
        .any(|(controller, creature_type)| {
            *controller == player
                && obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|spell_type| spell_type == creature_type)
        })
}

/// CR 702.143d: the single authority for "any foretell cost it has" — reads
/// the printed `Keyword::Foretell` cost off an object. Shared between the
/// foretell special action (`handle_foretell`) and the effect-driven "becomes
/// foretold" grant (`effects::grant_permission`).
pub(crate) fn foretell_cost(obj: &crate::game::game_object::GameObject) -> Option<ManaCost> {
    obj.keywords.iter().find_map(|keyword| match keyword {
        Keyword::Foretell(cost) => Some(cost.clone()),
        _ => None,
    })
}

fn can_pay_special_action_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    cost: &ManaCost,
) -> bool {
    let mut simulated = state.clone();
    pay_unless_cost(&mut simulated, player, cost, &mut Vec::new()).is_ok()
}

/// CR 702.143a-b: A player may foretell a card from hand any time they have
/// priority during their turn by paying {2}. This is a special action and does
/// not use the stack.
pub fn can_foretell_card(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    if state.active_player != player {
        return false;
    }

    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.owner != player || obj.zone != Zone::Hand || foretell_cost(obj).is_none() {
        return false;
    }

    let cost = ManaCost::generic(FORETELL_SPECIAL_ACTION_COST);
    can_pay_special_action_cost_after_auto_tap(state, player, &cost)
}

/// CR 702.143a-b: Pay {2}, exile the hand card, mark it foretold in exile, and
/// grant the later-turn foretell-cost casting permission.
pub fn handle_foretell(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Foretell is legal only during your turn".to_string(),
        ));
    }

    let foretell_cost = {
        let obj = state
            .objects
            .get(&object_id)
            .ok_or_else(|| EngineError::InvalidAction("Card not found".to_string()))?;
        if obj.card_id != card_id || obj.owner != player || obj.zone != Zone::Hand {
            return Err(EngineError::InvalidAction(
                "Card is not in your hand".to_string(),
            ));
        }
        foretell_cost(obj).ok_or_else(|| {
            EngineError::ActionNotAllowed("Card does not have foretell".to_string())
        })?
    };

    pay_unless_cost(
        state,
        player,
        &ManaCost::generic(FORETELL_SPECIAL_ACTION_COST),
        events,
    )?;
    super::zones::move_to_zone(state, object_id, Zone::Exile, events);
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.foretold = true;
        obj.face_down = true;
        obj.casting_permissions.push(CastingPermission::Foretold {
            cost: foretell_cost,
            turn_foretold: state.turn_number,
        });
    }
    events.push(GameEvent::Foretold {
        player_id: player,
        object_id,
    });

    Ok(WaitingFor::Priority { player })
}

// CR 702.34 (Flashback) / CR 702.81 (Retrace) / CR 702.127 (Aftermath) /
// CR 702.133 (Jump-start) / CR 702.138 (Escape) / CR 702.146 (Disturb) /
// CR 702.180 (Harmonize): graveyard-cast alternative permissions. Sneak
// (CR 702.190a) is a HAND-cast alt-cost and is deliberately NOT listed here —
// including it would misclassify graveyard objects with a granted Sneak as
// castable from the graveyard, which the rules do not permit.
fn has_effective_graveyard_cast_keyword(
    state: &GameState,
    object_id: ObjectId,
    // Retained for call-site symmetry with the surrounding graveyard scan; all
    // keyword checks below are now off-zone-aware and key on `object_id` only.
    _obj: &crate::game::game_object::GameObject,
) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Escape)
        || has_retrace_keyword(state, object_id)
        || jumpstart_castable_from_graveyard(state, object_id)
        || has_harmonize_keyword(state, object_id)
        || has_flashback_keyword(state, object_id)
        || has_aftermath_keyword(state, object_id)
        || super::keywords::effective_disturb_cost(state, object_id).is_some()
        // CR 702.187b: Mayhem makes the graveyard a castable zone only while the
        // card was discarded this turn.
        || (was_discarded_this_turn(state, object_id)
            && super::keywords::effective_mayhem_cost(state, object_id).is_some())
}

fn mayhem_castable_from_graveyard(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    was_discarded_this_turn(state, object_id)
        && super::keywords::effective_mayhem_cost(state, object_id).is_some()
        && state
            .objects
            .get(&object_id)
            .is_some_and(|o| o.zone == Zone::Graveyard && o.owner == player)
}

fn upsert_keyword_by_kind(keywords: &mut Vec<Keyword>, keyword: Keyword) {
    if let Some(existing) = keywords
        .iter_mut()
        .find(|existing| existing.kind() == keyword.kind())
    {
        *existing = keyword;
    } else {
        keywords.push(keyword);
    }
}

pub(crate) fn requires_per_instance_resolution(kind: KeywordKind) -> bool {
    matches!(
        kind,
        // CR 702.175b: each Offspring instance is paid and triggers separately.
        KeywordKind::Offspring
            // CR 702.56b: each Replicate instance is paid and triggers separately.
            | KeywordKind::Replicate
    )
}

fn requires_per_instance_keyword(keyword: &Keyword) -> bool {
    if requires_per_instance_resolution(keyword.kind()) {
        return true;
    }

    matches!(
        keyword,
        // CR 702.153b: each Casualty instance is paid and triggers separately.
        Keyword::Casualty(_)
            // CR 702.157b: each Squad instance is paid and triggers separately.
            | Keyword::Squad(_)
    )
}

fn merge_spell_keyword(keywords: &mut Vec<Keyword>, keyword: Keyword, preserve_instances: bool) {
    if preserve_instances && requires_per_instance_keyword(&keyword) {
        keywords.push(keyword);
    } else {
        upsert_keyword_by_kind(keywords, keyword);
    }
}

/// CR 601.2a: Single matcher-side authority for "what zone did this spell get
/// cast from" at SpellCast-event time. Encapsulates the placeholder vs
/// ability-context storage split so the trigger matcher (and any future
/// caller) never has to know which of the two `cast_from_zone` sites is
/// populated for a given spell.
///
/// Lookup order:
/// 1. Stack entry's `ResolvedAbility.context.cast_from_zone` — populated for
///    instants/sorceries with on-resolve abilities at `casting_costs.rs`
///    just before the `GameEvent::SpellCast` is emitted.
/// 2. Object's `cast_from_zone` field — populated for permanent spells with
///    no spell-level ability (the placeholder branch).
///
/// Returns `None` only if the lookup races a stack-pop or the object is
/// missing; SpellCast events always carry a real origin per CR 601.2a, so
/// callers should fail-closed on `None` rather than fire spuriously.
pub(crate) fn spell_cast_origin(state: &GameState, object_id: ObjectId) -> Option<Zone> {
    // CR 601.2a: ability-context first — the typical instant/sorcery path
    // where `casting_costs.rs` writes `ability.context.cast_from_zone` before
    // emitting the SpellCast event.
    if let Some(zone) = state
        .stack
        .iter()
        .rfind(|e| e.id == object_id)
        .and_then(|e| e.ability())
        .and_then(|a| a.context.cast_from_zone)
    {
        return Some(zone);
    }
    // Fallback: placeholder/permanent path where `cast_from_zone` is stamped
    // on the object directly.
    state.objects.get(&object_id).and_then(|o| o.cast_from_zone)
}

/// CR 601.2a + CR 603.4: Look up the pre-announcement zone for a spell that
/// is currently mid-cast. `obj.zone` stays at the origin until `finalize_cast`
/// performs the Hand→Stack move itself, but should the ordering ever change
/// this fallback preserves correctness for filters like "spells you cast from
/// exile have convoke" that must evaluate against the pre-announcement zone.
fn pending_cast_origin_zone_for(state: &GameState, object_id: ObjectId) -> Option<Zone> {
    if let Some(pc) = state.waiting_for.pending_cast_ref() {
        if pc.object_id == object_id {
            return Some(pc.origin_zone);
        }
    }
    if let Some(pc) = state.pending_cast.as_ref() {
        if pc.object_id == object_id {
            return Some(pc.origin_zone);
        }
    }
    None
}

fn granted_spell_keywords(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(spell_obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    // CR 601.2a: Prefer cast_from_zone (stamped during finalize_cast and persists
    // through SpellCast event) over pending_cast_origin_zone_for (transient and
    // cleared after finalize_cast). This ensures origin zone is available when
    // triggers are processed for filters like "InZone { zone: Hand }".
    let origin_zone = spell_obj
        .cast_from_zone
        .or_else(|| pending_cast_origin_zone_for(state, object_id))
        .unwrap_or(spell_obj.zone);

    let mut keywords = Vec::new();
    // CR 702.26b + CR 604.1: Functioning gate owned by
    // `battlefield_active_statics`; inline `def.condition` check removed.
    for (source_obj, def) in super::functioning_abilities::game_active_statics(state) {
        let StaticMode::CastWithKeyword { keyword } = &def.mode else {
            continue;
        };

        let matches = def.affected.as_ref().is_none_or(|filter| {
            super::filter::spell_object_matches_filter_from_state(
                state,
                spell_obj,
                origin_zone,
                caster,
                filter,
                source_obj.id,
                &state.all_creature_types,
            )
        });
        if !matches {
            continue;
        }

        merge_spell_keyword(&mut keywords, keyword.clone(), false);
    }

    // CR 611.2c: Player-scoped flash-timing grants applied by activated/triggered
    // abilities (e.g. Teferi +1) live in the TCE table, not on a battlefield static.
    transient_granted_spell_keywords(state, caster, spell_obj, origin_zone, &mut keywords, false);

    // CR 601.2f: One-shot "the next spell …" keyword/flash grants (Insist, Quicken, Wand).
    apply_pending_next_spell_keyword_grants(state, caster, object_id, &mut keywords, false);

    keywords
}

fn granted_spell_keyword_instances(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(spell_obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let origin_zone = spell_obj
        .cast_from_zone
        .or_else(|| pending_cast_origin_zone_for(state, object_id))
        .unwrap_or(spell_obj.zone);

    let mut keywords = Vec::new();
    for (source_obj, def) in super::functioning_abilities::game_active_statics(state) {
        let StaticMode::CastWithKeyword { keyword } = &def.mode else {
            continue;
        };

        let matches = def.affected.as_ref().is_none_or(|filter| {
            super::filter::spell_object_matches_filter_from_state(
                state,
                spell_obj,
                origin_zone,
                caster,
                filter,
                source_obj.id,
                &state.all_creature_types,
            )
        });
        if matches {
            merge_spell_keyword(&mut keywords, keyword.clone(), true);
        }
    }

    transient_granted_spell_keywords(state, caster, spell_obj, origin_zone, &mut keywords, true);
    apply_pending_next_spell_keyword_grants(state, caster, object_id, &mut keywords, true);

    keywords
}

/// CR 611.2c + CR 601.3b: Player-scoped spell-casting keyword grants (e.g. Teferi,
/// Time Raveler's +1 "you may cast sorcery spells as though they had flash") are
/// registered by `effect.rs` as `SpecificPlayer { id }`-bound transient continuous
/// effects rather than battlefield statics, so the grant survives the source
/// permanent leaving play and expires on its own duration (CR 611.2a). This scan is
/// the player-scoped counterpart to the `game_active_statics` loop in
/// `granted_spell_keywords`; it mirrors the condition gating of the sibling player
/// query `transient_grants_static_mode_to_player` (static_abilities.rs).
fn transient_granted_spell_keywords(
    state: &GameState,
    caster: PlayerId,
    spell_obj: &crate::game::game_object::GameObject,
    origin_zone: Zone,
    keywords: &mut Vec<Keyword>,
    preserve_instances: bool,
) {
    for tce in &state.transient_continuous_effects {
        let TargetFilter::SpecificPlayer { id } = tce.affected else {
            continue;
        };
        if id != caster {
            continue;
        }
        // CR 603.4 + CR 608.2h: mirror `transient_grants_static_mode_to_player`'s
        // dual-condition gating exactly.
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        for modification in &tce.modifications {
            let ContinuousModification::GrantStaticAbility { definition } = modification else {
                continue;
            };
            let StaticMode::CastWithKeyword { keyword } = &definition.mode else {
                continue;
            };
            // CR 611.2c: the grant is bound to the grantee (outer SpecificPlayer
            // gate); its lifetime is the stated duration, independent of the
            // source's presence OR control. Match only the spell's type axis — do
            // not re-derive "you" from the (possibly stolen/relocated) source
            // object. `spell_object_matches_filter_from_state` resolves
            // `ControllerRef::You` against the *current* source controller, which
            // becomes an opponent if Teferi is stolen before the grantee's next
            // turn; stripping the controller axis preserves the SORCERY type axis
            // (and any others) while removing that stale-source dependency. The
            // spell being evaluated is by construction the grantee's own cast
            // (`caster` == the bound player), so controller scoping is already
            // guaranteed by the call context plus the outer gate.
            let affected = definition.affected.as_ref().map(|filter| {
                let mut filter = filter.clone();
                if let TargetFilter::Typed(tf) = &mut filter {
                    tf.controller = None;
                }
                filter
            });
            let matches = affected.as_ref().is_none_or(|filter| {
                super::filter::spell_object_matches_filter_from_state(
                    state,
                    spell_obj,
                    origin_zone,
                    caster,
                    filter,
                    tce.source_id,
                    &state.all_creature_types,
                )
            });
            if matches {
                merge_spell_keyword(keywords, keyword.clone(), preserve_instances);
            }
        }
    }
}

/// CR 118.9 + CR 604.1: Collect an alternative MANA cost granted to `object_id`
/// by a `CastWithAlternativeCost` static on the battlefield whose `affected`
/// filter matches this spell.
///
/// CR 118.9a: only one alternative cost is ultimately applied to a spell, and
/// the spell's controller chooses which. The casting pipeline currently surfaces
/// a single alternative-vs-printed choice (`AdditionalCost::Choice`), so when
/// multiple grants match (e.g. Rooftop Storm and Fist of Suns both active) this
/// returns the first in deterministic battlefield-scan order rather than
/// prompting the controller to choose among them. Offering a choice across
/// multiple simultaneous grants needs a multi-alternative choice surface and is
/// a known limitation tracked for follow-up, not implemented here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GrantedSpellAlternativeCost {
    pub(super) cost: AbilityCost,
    pub(super) timing_permission: Option<CastTimingPermission>,
}

pub(super) fn granted_spell_alternative_cost(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Option<GrantedSpellAlternativeCost> {
    let spell_obj = state.objects.get(&object_id)?;
    let origin_zone = pending_cast_origin_zone_for(state, object_id).unwrap_or(spell_obj.zone);

    // CR 604.1: Functioning gate owned by `game_active_statics`.
    for (source_obj, def) in super::functioning_abilities::game_active_statics(state) {
        let StaticMode::CastWithAlternativeCost {
            cost,
            timing_permission,
        } = &def.mode
        else {
            continue;
        };

        let matches = def.affected.as_ref().is_none_or(|filter| {
            super::filter::spell_object_matches_filter_from_state(
                state,
                spell_obj,
                origin_zone,
                caster,
                filter,
                source_obj.id,
                &state.all_creature_types,
            )
        });
        if matches {
            return Some(GrantedSpellAlternativeCost {
                cost: cost.clone(),
                timing_permission: *timing_permission,
            });
        }
    }

    None
}

pub(crate) fn effective_spell_keywords(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let mut keywords = obj.keywords.clone();
    // CR 702.60b / CR 113.2c: printed duplicate keyword instances are preserved
    // in `obj.keywords`; granted spell keywords are currently merged by kind here.
    // A future granted-multi-instance keyword must collect those instances before
    // this upsert path if its rules require separate triggers.
    for keyword in granted_spell_keywords(state, caster, object_id) {
        upsert_keyword_by_kind(&mut keywords, keyword);
    }

    // CR 702.34a: The flashback keyword is granted while the object isn't on
    // the battlefield. Use the pre-announcement zone so flashback still
    // applies for spells being cast from graveyard even after `finalize_cast`
    // moves them to the stack.
    let effective_origin_zone = pending_cast_origin_zone_for(state, object_id).unwrap_or(obj.zone);
    if effective_origin_zone != Zone::Battlefield
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Flashback,
        )
    {
        upsert_keyword_by_kind(
            &mut keywords,
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
        );
    }

    keywords
}

pub(crate) fn effective_spell_keyword_instances(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let mut keywords = obj.keywords.clone();
    for keyword in granted_spell_keyword_instances(state, caster, object_id) {
        merge_spell_keyword(&mut keywords, keyword, true);
    }

    let effective_origin_zone = pending_cast_origin_zone_for(state, object_id).unwrap_or(obj.zone);
    if effective_origin_zone != Zone::Battlefield
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Flashback,
        )
    {
        upsert_keyword_by_kind(
            &mut keywords,
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
        );
    }

    keywords
}

pub(super) fn build_spell_meta(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Option<SpellMeta> {
    state.objects.get(&object_id).map(|obj| SpellMeta {
        types: object_type_names(obj),
        subtypes: obj.card_types.subtypes.clone(),
        keyword_kinds: effective_spell_keyword_kinds(state, caster, object_id),
        cast_from_zone: Some(pending_cast_origin_zone_for(state, object_id).unwrap_or(obj.zone)),
        mana_value: Some(obj.mana_cost.mana_value()),
        color_count: Some(obj.color.len() as u32),
        // CR 107.3 + CR 202.3e: structural "has {X}" property of the printed cost,
        // detected from shards (mana value alone can't reveal it — X contributes 0
        // off the stack).
        has_x_in_cost: obj.mana_cost.has_x(),
        // CR 708.4 + CR 702.37c / CR 702.168b: `is_face_down` means "this spell is
        // being CAST FACE DOWN" (morph/disguise/cloak — paying {3} to cast as a 2/2
        // face-down creature spell), NOT "the object is currently face down". Those
        // differ: foretell (CR 702.143a), hideaway, and other exile/library
        // concealment set `obj.face_down = true` while the card waits in exile, yet
        // such a card is CAST FACE UP (CR 702.143c: cast "even if it was cast for a
        // cost other than a foretell cost"). Mana payment runs against the origin
        // (exile) zone BEFORE the deferred origin->stack move clears `face_down`, so
        // sourcing this from raw `obj.face_down` would let a face-up foretold/hideaway
        // cast wrongly satisfy the `OnlyForFaceDownSpell` spend restriction (Tin
        // Street Gossip). No engine path casts a spell face down today:
        // `GameAction::PlayFaceDown` -> `game::morph::play_face_down` moves
        // hand->battlefield via the zone pipeline and charges no mana, never building
        // a `PaymentContext::Spell`. So the correct value at every current production
        // payment site is `false`. When a real CR 702.37c / 708.4 face-down CAST path
        // is built, set this from that cast's announced face-down intent; the gate
        // (`ManaRestriction::allows_spell`) already reads this field.
        is_face_down: false,
    })
}

fn object_type_names(obj: &crate::game::game_object::GameObject) -> Vec<String> {
    let mut names = obj
        .card_types
        .supertypes
        .iter()
        .map(|st| st.to_string())
        .chain(obj.card_types.core_types.iter().map(|ct| ct.to_string()))
        .collect::<Vec<_>>();
    if obj.color.is_empty() {
        names.push("Colorless".to_string());
    }
    names
}

pub(crate) fn effective_spell_keyword_kinds(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<KeywordKind> {
    let mut kinds = Vec::new();
    for keyword in effective_spell_keywords(state, caster, object_id) {
        let kind = keyword.kind();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    kinds
}

/// Check if an object has any permission allowing it to be cast from exile.
/// Uses explicit match arms (not `matches!`) so the compiler catches new variants.
fn has_exile_cast_permission(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
    turn_number: u32,
) -> bool {
    play_from_exile_permission_source(state, obj, player, turn_number).is_some()
        || obj.casting_permissions.iter().any(|p| match p {
            crate::types::ability::CastingPermission::AdventureCreature
            | crate::types::ability::CastingPermission::ExileWithEnergyCost => obj.owner == player,
            crate::types::ability::CastingPermission::ExileWithAltCost { .. }
            | crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. } => {
                exile_alt_cost_permission_supports_cast(state, obj, player, p, None)
            }
            crate::types::ability::CastingPermission::PlayFromExile { .. } => false,
            crate::types::ability::CastingPermission::WarpExile {
                castable_after_turn,
            } => obj.owner == player && turn_number > *castable_after_turn,
            crate::types::ability::CastingPermission::Plotted { turn_plotted } => {
                obj.owner == player && turn_number > *turn_plotted
            }
            crate::types::ability::CastingPermission::Foretold { turn_foretold, .. } => {
                obj.owner == player && turn_number > *turn_foretold
            }
        })
        // CR 601.2a + CR 113.6b: A `StaticMode::ExileCastPermission` static on a
        // battlefield permanent controlled by `player` may authorize this exile
        // card without any object-attached `CastingPermission`. Detected via the
        // per-turn pool + per-source filter; the helper performs the same checks
        // (per-turn frequency, pool membership, affected filter) used by
        // `exile_objects_castable_by_permission`.
        || exile_cast_permission_source(state, player, obj.id).is_some()
}

/// CR 305.1 + CR 601.2a: Lands in exile may be played by permissions that say
/// "play", but they never enter the spell-cast path.
///
/// EXILE-ONLY BY RULE: this predicate gates the battlefield-static
/// `StaticMode::ExileCastPermission` path (Maralen, The Matrix of Time). Those
/// statics function on cards exiled *with* their source (CR 113.6b), so a card
/// that has since left exile (e.g. milled into a graveyard by another effect)
/// must NOT be admitted — see the zone re-check at the static callers. Do not
/// widen this to other zones; the object-tagged `PlayFromExile` path uses
/// [`play_from_exile_object_in_cast_path`] instead.
fn exile_object_can_enter_cast_path(obj: &GameObject) -> bool {
    obj.zone == Zone::Exile
        && !obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
}

/// CR 701.17d + CR 305.1 + CR 601.2a: A card carrying an object-tagged
/// [`CastingPermission::PlayFromExile`] may enter the spell-cast path from
/// exile (impulse draw) OR from the graveyard (a mill effect that grants
/// permission to play "that card" — CR 701.17d — attaches the permission to the
/// milled card in the graveyard). Lands are excluded from the cast path in both
/// zones (CR 305.1: lands are *played*, not cast); a milled land flows through
/// [`graveyard_lands_playable_by_permission`] / [`exile_lands_playable_by_permission`]
/// instead.
///
/// This is the single DRY admission predicate for the three object-tagged
/// `PlayFromExile` consult sites (legal-actions surface, `prepare_spell_cast`).
/// It does NOT touch the battlefield-static path, which stays exile-only via
/// [`exile_object_can_enter_cast_path`].
fn play_from_exile_object_in_cast_path(obj: &GameObject) -> bool {
    matches!(obj.zone, Zone::Exile | Zone::Graveyard)
        && !obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
}

fn exile_object_castable_by_permission(
    state: &GameState,
    obj: &GameObject,
    player: PlayerId,
) -> bool {
    play_from_exile_object_in_cast_path(obj)
        && has_exile_cast_permission(state, obj, player, state.turn_number)
}

pub(super) fn cast_permission_constraint_allows_cast(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    constraint: &Option<crate::types::ability::CastPermissionConstraint>,
    resulting_mv: Option<u32>,
) -> bool {
    use crate::types::ability::{CastPermissionConstraint, QuantityExpr};

    match constraint {
        Some(CastPermissionConstraint::ManaValue {
            comparator,
            value: QuantityExpr::Fixed { value },
        }) if resulting_mv.is_none() => {
            comparator.evaluate(obj.mana_cost.mana_value() as i32, *value)
        }
        Some(CastPermissionConstraint::ManaValue { comparator, value }) => {
            let Some(resulting_mv) = resulting_mv else {
                return true;
            };
            let required = resolve_quantity(state, value, obj.controller, obj.id);
            comparator.evaluate(resulting_mv as i32, required)
        }
        None => true,
    }
}

fn exile_alt_cost_permission_grants_to_player(
    player: PlayerId,
    granted_to: Option<PlayerId>,
) -> bool {
    match granted_to {
        Some(allowed) => allowed == player,
        None => true,
    }
}

/// CR 601.2a + CR 118.9: Whether an `ExileWithAltCost` permission carries the
/// casting card's own printed mana cost (Jace −3 class) rather than a fixed
/// alternate cost or a free-cast zero.
fn exile_alt_cost_permission_uses_casting_cards_mana_cost(
    permission_cost: &ManaCost,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    match permission_cost {
        ManaCost::SelfManaCost => true,
        cost if cost.is_without_paying_mana() => false,
        cost => {
            if *cost == obj.mana_cost {
                return true;
            }
            obj.back_face
                .as_ref()
                .is_some_and(|bf| *cost == bf.mana_cost)
        }
    }
}

/// CR 709.3 + CR 712.11b + CR 601.2a: `CastFromZone` and similar grants stamp
/// `ExileWithAltCost { cost: obj.mana_cost }` when the card is permitted to be
/// cast for its normal mana cost. Split cards and spell//spell MDFCs choose a
/// face at cast time, so after face choice the payable cost is the active
/// face's mana cost — not the front-face snapshot stored on the permission
/// (#3987). Free-cast and fixed alternate costs must keep the stored permission
/// cost (CR 118.9a).
fn resolve_exile_with_alt_cost_permission_mana_cost(
    permission_cost: &ManaCost,
    obj: &crate::game::game_object::GameObject,
) -> ManaCost {
    if permission_cost.is_without_paying_mana() {
        return permission_cost.clone();
    }
    match permission_cost {
        ManaCost::SelfManaCost => obj.mana_cost.clone(),
        _ if obj.modal_back_face
            && exile_alt_cost_permission_uses_casting_cards_mana_cost(permission_cost, obj) =>
        {
            obj.mana_cost.clone()
        }
        other => other.clone(),
    }
}

fn simulate_chosen_split_spell_back_face(obj: &mut crate::game::game_object::GameObject) {
    swap_to_alternative_spell_face(obj);
    // Mirror `ChooseModalFace { back_face: true }` so affordability preview and
    // alt-cost resolution use the chosen face without re-prompting or swapping
    // back to the front half (#3987).
    obj.modal_back_face = true;
    if let Some(ref mut bf) = obj.back_face {
        bf.layout_kind = None;
    }
}

pub(super) fn exile_alt_cost_permission_supports_cast(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
    permission: &crate::types::ability::CastingPermission,
    resulting_mv: Option<u32>,
) -> bool {
    match permission {
        crate::types::ability::CastingPermission::ExileWithAltCost {
            granted_to,
            constraint,
            ..
        }
        | crate::types::ability::CastingPermission::ExileWithAltAbilityCost {
            granted_to,
            constraint,
            ..
        } => {
            exile_alt_cost_permission_grants_to_player(player, *granted_to)
                && cast_permission_constraint_allows_cast(state, obj, constraint, resulting_mv)
        }
        _ => false,
    }
}

pub(super) fn selected_exile_alt_cost_permission_accepts_resulting_mv(
    state: &GameState,
    object_id: ObjectId,
    player: PlayerId,
    resulting_mv: u32,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return true;
    };

    let Some(permission) = obj.casting_permissions.iter().find(|permission| {
        exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
    }) else {
        return true;
    };

    exile_alt_cost_permission_supports_cast(state, obj, player, permission, Some(resulting_mv))
}

pub(super) fn selected_exile_alt_cost_permission_casts_transformed(
    state: &GameState,
    object_id: ObjectId,
    player: PlayerId,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };

    obj.casting_permissions
        .iter()
        .find(|permission| {
            exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
        })
        .is_some_and(|permission| {
            matches!(
                permission,
                crate::types::ability::CastingPermission::ExileWithAltCost {
                    cast_transformed: true,
                    ..
                }
            )
        })
}

// CR 614.1c + CR 122.1: read the enters-with rider from the *consumed* cast-this-way
// permission only (the one supporting THIS cast), not any permission carrying a counter,
// so a non-consumed sibling permission's rider cannot leak onto this cast (CR 608.2c:
// apply the instructions belonging to this cast).
pub(super) fn selected_exile_alt_cost_permission_enters_with_counter(
    state: &GameState,
    object_id: ObjectId,
    player: PlayerId,
) -> Option<crate::types::counter::CounterType> {
    let obj = state.objects.get(&object_id)?;
    obj.casting_permissions
        .iter()
        .find(|permission| {
            exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
        })
        .and_then(|permission| match permission {
            crate::types::ability::CastingPermission::ExileWithAltCost {
                enters_with_counter,
                ..
            } => enters_with_counter.clone(),
            _ => None,
        })
}

pub(super) fn exile_alt_cost_permissions_accept_resulting_mv(
    state: &GameState,
    object_id: ObjectId,
    player: PlayerId,
    resulting_mv: u32,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return true;
    };

    let mut found_authorizing_permission = false;
    for permission in &obj.casting_permissions {
        match permission {
            crate::types::ability::CastingPermission::ExileWithAltCost { granted_to, .. }
            | crate::types::ability::CastingPermission::ExileWithAltAbilityCost {
                granted_to,
                ..
            } if exile_alt_cost_permission_grants_to_player(player, *granted_to) => {
                found_authorizing_permission = true;
                if exile_alt_cost_permission_supports_cast(
                    state,
                    obj,
                    player,
                    permission,
                    Some(resulting_mv),
                ) {
                    return true;
                }
            }
            _ => {}
        }
    }

    !found_authorizing_permission
}

fn source_has_collection_counter_play_permission(
    state: &GameState,
    source: ObjectId,
    player: PlayerId,
) -> bool {
    state.objects.get(&source).is_some_and(|source_obj| {
        source_obj.zone == Zone::Battlefield
            && source_obj.controller == player
            && active_static_definitions(state, source_obj)
                .any(|def| matches!(&def.mode, StaticMode::LinkedCollectionCounterPlayPermission))
    })
}

fn live_collection_counter_play_permission_source(
    state: &GameState,
    player: PlayerId,
) -> Option<ObjectId> {
    state.battlefield.iter().copied().find(|source| {
        !state.exile_play_permissions_used.contains(source)
            && source_has_collection_counter_play_permission(state, *source, player)
    })
}

fn has_collection_counter(obj: &crate::game::game_object::GameObject) -> bool {
    obj.counters
        .get(&crate::types::counter::CounterType::Generic(
            "collection".to_string(),
        ))
        .copied()
        .unwrap_or(0)
        > 0
}

pub(crate) fn play_from_exile_permission_source(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
    _turn_number: u32,
) -> Option<(ObjectId, CastFrequency)> {
    obj.casting_permissions.iter().find_map(|p| match p {
        crate::types::ability::CastingPermission::PlayFromExile {
            granted_to,
            frequency,
            source_id,
            exiled_by_ability_controller,
            card_filter,
            single_use_group,
            single_use,
            ..
        } if *granted_to == player => {
            let source = source_id.unwrap_or(obj.id);
            // CR 601.2a: A typed grant ("you may cast an instant or sorcery
            // spell from among those exiled cards") authorizes only exiled cards
            // matching `card_filter`. The filter is a printed object quality, so
            // evaluate it with a neutral (source/controller-free) context.
            if let Some(filter) = card_filter {
                let ctx = crate::game::filter::FilterContext::neutral();
                if !crate::game::filter::matches_target_filter(state, obj.id, filter, &ctx) {
                    return None;
                }
            }
            // CR 601.2a + CR 611.2a: A single-use grant authorizes at most one
            // cast across its whole duration window. The tracked set, not the
            // source permanent, is the grant identity because one source may
            // create overlapping "those exiled cards" effects.
            if *single_use {
                let group = single_use_group.as_ref()?;
                if state.exile_play_single_use_consumed.contains(group) {
                    return None;
                }
            }
            if *frequency == CastFrequency::OncePerTurn {
                if *exiled_by_ability_controller == Some(player) {
                    return has_collection_counter(obj)
                        .then(|| live_collection_counter_play_permission_source(state, player))
                        .flatten()
                        .map(|live_source| (live_source, *frequency));
                }
                if state.exile_play_permissions_used.contains(&source) {
                    return None;
                }
            }
            Some((source, *frequency))
        }
        _ => None,
    })
}

/// CR 601.2f: The printed mana-cost increase a spell incurs when it is cast via
/// an active [`CastingPermission::PlayFromExile`] grant that carries
/// `cast_cost_raise` ("Each spell cast this way costs {N} more to cast." —
/// Lightstall Inquisitor). Returns the increase from the first grant that
/// authorizes `player`. Mirrors the grantee gate used by
/// [`player_can_spend_as_any_color_for_spell`] for `mana_spend_permission`: the
/// spell object retains its exile-play permissions while it is on the stack, so
/// the raise is readable throughout cost determination (CR 601.2b–f). The cost
/// raise is a property of the grant, not a board-wide static, so it applies only
/// to spells cast via this permission.
fn exile_play_cast_cost_raise(
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> Option<ManaCost> {
    obj.casting_permissions.iter().find_map(|p| match p {
        CastingPermission::PlayFromExile {
            granted_to,
            cast_cost_raise: Some(raise),
            ..
        } if *granted_to == player => Some(raise.clone()),
        _ => None,
    })
}

/// CR 614.1c: Whether a land played via an active `PlayFromExile` grant must
/// enter the battlefield tapped ("Each land played this way enters tapped." —
/// Lightstall Inquisitor). Mirrors the grantee gate of
/// [`exile_play_cast_cost_raise`]; consumed by `handle_play_land` to seed the
/// tap state on the land's entry event.
pub(crate) fn exile_play_land_enters_tapped(
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    obj.casting_permissions.iter().any(|p| {
        matches!(
            p,
            CastingPermission::PlayFromExile {
                granted_to,
                land_enter_tapped: crate::types::zones::EtbTapState::Tapped,
                ..
            } if *granted_to == player
        )
    })
}

/// CR 601.2a + CR 603.7 + CR 611.2a: Returns the tracked-set identity of a `single_use`
/// [`CastingPermission::PlayFromExile`] on `obj` that authorizes `player` and
/// has not yet been consumed, if any. Used at cast finalization to record that
/// the grant's one allowed cast has been spent. Mirrors the grantee/filter
/// gating of [`play_from_exile_permission_source`] so a card that fails the
/// type filter never spends the slot.
pub(crate) fn single_use_play_from_exile_group(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> Option<TrackedSetId> {
    obj.casting_permissions.iter().find_map(|p| match p {
        crate::types::ability::CastingPermission::PlayFromExile {
            granted_to,
            card_filter,
            single_use_group,
            single_use: true,
            ..
        } if *granted_to == player => {
            let group = single_use_group.as_ref()?;
            if state.exile_play_single_use_consumed.contains(group) {
                return None;
            }
            if let Some(filter) = card_filter {
                let ctx = crate::game::filter::FilterContext::neutral();
                if !crate::game::filter::matches_target_filter(state, obj.id, filter, &ctx) {
                    return None;
                }
            }
            Some(*group)
        }
        _ => None,
    })
}

/// CR 601.2a + CR 611.2a: Spend a single-use `PlayFromExile` grant. Records the
/// `group` in `exile_play_single_use_consumed` and strips the now-void
/// `PlayFromExile { single_use_group == group, single_use: true }` permission
/// from every object still in exile, so the remaining cards in that tracked set
/// are no longer castable (Chandra, Hope's Beacon +1 grants one cast total
/// across its until-end-of-next-turn window).
pub(crate) fn consume_single_use_play_from_exile(state: &mut GameState, group: TrackedSetId) {
    state.exile_play_single_use_consumed.insert(group);
    for obj_id in state.exile.clone() {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.casting_permissions.retain(|p| {
                !matches!(
                    p,
                    crate::types::ability::CastingPermission::PlayFromExile {
                        single_use_group,
                        single_use: true,
                        ..
                    } if *single_use_group == Some(group)
                )
            });
        }
    }
}

#[cfg(test)]
fn player_can_spend_as_any_color_for_spell(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
) -> bool {
    player_can_spend_as_any_color_for_optional_spell(state, player, Some(source_id))
}

pub(super) fn player_can_spend_as_any_color_for_optional_spell(
    state: &GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
) -> bool {
    // CR 609.4b: When a spell object is in context, consult both the board-wide
    // (`spell_filter: None`) and spell-class-filtered (`Some`) statics; the
    // filtered form (Vizier of the Menagerie: "creature spells") is matched
    // against the spell object. With no spell in context (effect/activation
    // payments), only the unfiltered board-wide static applies.
    let static_grant = match source_id {
        Some(spell_id) => super::static_abilities::player_can_spend_as_any_color_for_spell_object(
            state, player, spell_id,
        ),
        None => super::static_abilities::player_can_spend_as_any_color(state, player),
    };
    static_grant
        || source_id
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| {
                obj.casting_permissions.iter().any(|permission| {
                    matches!(
                        permission,
                        crate::types::ability::CastingPermission::PlayFromExile {
                            granted_to,
                            mana_spend_permission:
                                Some(crate::types::ability::ManaSpendPermission::AnyTypeOrColor),
                            ..
                        } if *granted_to == player
                    )
                })
            })
        // CR 609.4b: A battlefield `StaticMode::ExileCastPermission` static may
        // grant "mana of any type can be spent to cast those spells" (Azula,
        // Cunning Usurper) for the cards in its exile pool. Unlike the per-card
        // `PlayFromExile` grant above, the concession lives on the static, so it
        // is re-derived from the source's pool + filter at spend time.
        || source_id
            .is_some_and(|id| exile_static_permission_grants_any_color(state, player, id))
}

pub(super) fn player_can_spend_as_any_color_for_payment(
    state: &GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    ctx: Option<&PaymentContext<'_>>,
) -> bool {
    if matches!(
        ctx,
        Some(PaymentContext::Effect | PaymentContext::Activation { .. })
    ) {
        super::static_abilities::player_can_spend_as_any_color(state, player)
    } else {
        player_can_spend_as_any_color_for_optional_spell(state, player, source_id)
    }
}

/// CR 601.2a + CR 611.2a: Check if an object has an alt-cost cast-from-exile
/// permission that authorizes this player and satisfies offer-time constraints.
fn has_alt_cost_permission_for(
    obj: &crate::game::game_object::GameObject,
    state: &GameState,
    player: PlayerId,
) -> bool {
    obj.casting_permissions.iter().any(|permission| {
        exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
    })
}

/// CR 601.2a: Object-level timed alt-cost grants that allow casting from the
/// graveyard without exiling first (Emry, Lurker in the Loch).
fn has_graveyard_timed_alt_cost_permission(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    obj.zone == Zone::Graveyard
        && obj.casting_permissions.iter().any(|permission| {
            exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
        })
}

/// CR 601.2a: Object-level alt-cost grants that allow casting a chosen card
/// from hand without moving it first (Electrodominance).
fn has_hand_alt_cost_permission(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    obj.zone == Zone::Hand
        && obj.casting_permissions.iter().any(|permission| {
            exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
        })
}

/// CR 608.2g: An object carries a *cast-during-resolution* alt-cost permission —
/// the runtime `ExileWithAltCost` stamped by `initiate_cast_during_resolution`,
/// identified by `resolution_cleanup.is_some()`. Unlike Cascade/Discover/Suspend
/// (whose hits are already in exile) and graveyard grants (Emry/Lurrus), a
/// free-cast window (Invoke Calamity, CR 601.2a "from your graveyard and/or
/// hand") may drive this cast on a card that is still in the controller's HAND.
/// The zone-specific gates (`obj.zone == Exile`, `has_graveyard_alt_cost`) do not
/// cover the hand origin, so the cost-zeroing alt-cost lookup must additionally
/// recognize this permission regardless of which zone the card is cast from —
/// otherwise a hand-origin free cast falls through to its printed mana cost.
fn has_during_resolution_alt_cost_permission(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    obj.casting_permissions.iter().any(|permission| {
        matches!(
            permission,
            crate::types::ability::CastingPermission::ExileWithAltCost {
                resolution_cleanup: Some(_),
                ..
            }
        ) && exile_alt_cost_permission_supports_cast(state, obj, player, permission, None)
    })
}

#[derive(Clone, Copy)]
struct GraveyardPermissionSource<'a> {
    source_id: ObjectId,
    filter: &'a TargetFilter,
    frequency: CastFrequency,
    graveyard_destination_replacement: Option<Zone>,
    /// CR 118.9 + CR 601.2f: Optional non-mana cost rider on the graveyard-cast
    /// static (Festival of Embers: additional pay-life). Borrowed from the static
    /// definition (kept `Copy` so the source struct stays `Copy`).
    extra_cost: &'a Option<crate::types::statics::CastExtraCost>,
}

/// CR 601.2a + CR 113.6b + CR 118.9: An active battlefield permanent carrying
/// `StaticMode::ExileCastPermission`. Captured during the "which permanents
/// grant a cast-from-exile permission to `player`?" scan so the caller can
/// (a) walk the per-turn rolling exile pool keyed on `source_id`, and (b)
/// stamp the per-source frequency slot at cast finalization.
#[derive(Clone, Copy)]
struct ExilePermissionSource<'a> {
    source_id: ObjectId,
    filter: &'a TargetFilter,
    frequency: CastFrequency,
    /// CR 118.9a: How the spell's mana cost is paid when cast via this
    /// permission. `WithoutPayingManaCost` is the Maralen shape (the printed
    /// mana cost is zeroed by `casting_costs`). `PayNormalCost` casts at the
    /// spell's normal cost — no shipping card uses this shape today, but the
    /// static keeps the axis available.
    cost: ExileCastCost,
    /// CR 305.1: `Play` admits lands (played) and non-land cards (cast); `Cast`
    /// admits only non-land spells. Captured so the cast path can skip lands for
    /// `Cast` sources and the land-play path can admit lands for `Play` sources.
    play_mode: CardPlayMode,
    /// CR 113.6b + CR 406.6: Which exile-link pool the source draws from —
    /// `ThisTurn` (per-turn rolling list) or `Persistent` (lifetime
    /// `exile_links`).
    pool: ExileCardPool,
    /// CR 117.1c: When the permission functions — `AnyTime` or `YourTurnOnly`.
    timing: ExileCastTiming,
    /// CR 609.4b: Optional any-type-mana spend concession riding alongside the
    /// permission (Azula, Cunning Usurper). `Some(AnyTypeOrColor)` lets the
    /// controller spend mana of any type to cast a spell offered by this source.
    mana_spend_permission: Option<crate::types::ability::ManaSpendPermission>,
    /// CR 601.3b + CR 702.8a: When `true`, spells cast via this permission may
    /// be cast as though they had flash (Azula, Cunning Usurper).
    grants_flash: bool,
    /// CR 118.9 + CR 601.2f: Optional non-mana cost rider on the exile-cast
    /// static (Valgavoth alternative pay-life; Dawnhand additional
    /// remove-counters). Borrowed from the static definition so the source struct
    /// stays `Copy`.
    extra_cost: &'a Option<crate::types::statics::CastExtraCost>,
}

/// CR 113.6b + CR 406.6: The set of exiled object ids this source's permission
/// may currently draw from, per its pool scope. `ThisTurn` reads the per-turn
/// rolling list; `Persistent` reads the lifetime `exile_links` set (the same
/// source-keyed set that backs `TargetFilter::ExiledBySource`).
fn exile_permission_pool(state: &GameState, source: &ExilePermissionSource<'_>) -> Vec<ObjectId> {
    match source.pool {
        ExileCardPool::ThisTurn => state
            .cards_exiled_with_source_this_turn
            .get(&source.source_id)
            .cloned()
            .unwrap_or_default(),
        // CR 406.6: lifetime per-source linked-exile pool.
        ExileCardPool::Persistent => {
            crate::game::players::linked_exile_cards_for_source(state, source.source_id)
                .iter()
                .map(|entry| entry.exiled_id)
                .collect()
        }
    }
}

/// CR 117.1c: Whether a source's timing gate is currently satisfied.
/// `YourTurnOnly` requires the active player to be the source controller;
/// `AnyTime` is always satisfied.
fn exile_permission_timing_active(
    state: &GameState,
    source: &ExilePermissionSource<'_>,
    player: PlayerId,
) -> bool {
    match source.timing {
        ExileCastTiming::AnyTime => true,
        ExileCastTiming::YourTurnOnly => state.active_player == player,
    }
}

/// CR 601.2a + CR 113.6b: Enumerate every battlefield permanent controlled by
/// `player` whose `StaticMode::ExileCastPermission` static is currently
/// functioning. The returned filter is owned by the static definition (via
/// `active_static_definitions`) and lives at least as long as the inferred
/// borrow.
///
/// Mirrors `graveyard_permission_sources` for the graveyard family — the
/// per-source pool then carves out the eligible cards.
fn exile_permission_sources(state: &GameState, player: PlayerId) -> Vec<ExilePermissionSource<'_>> {
    state
        .battlefield
        .iter()
        .copied()
        .filter_map(|source_id| {
            let obj = state.objects.get(&source_id)?;
            if obj.controller != player {
                return None;
            }
            active_static_definitions(state, obj).find_map(|definition| match definition.mode {
                // CR 305.1: `Cast` (Maralen) admits non-land spells; `Play` (The
                // Matrix of Time) admits lands and non-land cards. Both shapes
                // are surfaced here; the cast path skips lands and the land-play
                // path admits them, keyed on `play_mode`.
                StaticMode::ExileCastPermission {
                    frequency,
                    play_mode,
                    cost,
                    pool,
                    timing,
                    mana_spend_permission,
                    grants_flash,
                    ref extra_cost,
                } => definition
                    .affected
                    .as_ref()
                    .map(|filter| ExilePermissionSource {
                        source_id,
                        filter,
                        frequency,
                        cost,
                        play_mode,
                        pool,
                        timing,
                        mana_spend_permission,
                        grants_flash,
                        extra_cost,
                    }),
                _ => None,
            })
        })
        .collect()
}

/// CR 601.2a + CR 113.6b + CR 118.9: Cards in exile castable via a
/// `StaticMode::ExileCastPermission` static from a battlefield permanent
/// (Maralen, Fae Ascendant). Returns `(exiled_object_id, source_permanent_id,
/// frequency)` so the caller can stamp the per-turn slot at finalize-cast time.
///
/// The candidate pool is `state.cards_exiled_with_source_this_turn[source_id]`
/// — only cards exiled "with" the source during the current turn qualify. The
/// static's `affected: TargetFilter` then constrains the eligible cards by
/// type, mana value, etc. Per-source frequency is enforced before filter
/// evaluation so a consumed `OncePerTurn` slot prunes the source out cheaply.
fn exile_objects_castable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId, CastFrequency)> {
    // Hot-path fast exit: this runs once per legal-actions computation (and so
    // once per AI-search node). When no card is tracked in either exile pool, no
    // `ExileCastPermission` static can offer a card — short-circuit before
    // `exile_permission_sources` scans the whole battlefield. The `ThisTurn`
    // (Maralen) shape reads `cards_exiled_with_source_this_turn`; the
    // `Persistent` (The Matrix of Time) shape reads `exile_links`. With both
    // empty there is nothing to offer, matching the ~100% of board states with
    // no exile-cast permanent in play.
    if state.cards_exiled_with_source_this_turn.is_empty() && state.exile_links.is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let sources = exile_permission_sources(state, player);
    for source in &sources {
        if !exile_cast_frequency_available(state, source.source_id, source.frequency) {
            continue;
        }
        // CR 117.1c: A `YourTurnOnly` permission offers nothing outside the
        // controller's turn.
        if !exile_permission_timing_active(state, source, player) {
            continue;
        }
        let pool = exile_permission_pool(state, source);
        let ctx =
            super::filter::FilterContext::from_source_with_controller(source.source_id, player);
        for &exiled_id in &pool {
            // CR 400.7: An exiled card may have left exile since being tagged
            // (e.g. milled into a graveyard by another effect). Re-check zone
            // before offering it for cast.
            let Some(obj) = state.objects.get(&exiled_id) else {
                continue;
            };
            if !exile_object_can_enter_cast_path(obj) {
                continue;
            }
            if super::filter::matches_target_filter(state, exiled_id, source.filter, &ctx) {
                results.push((exiled_id, source.source_id, source.frequency));
            }
        }
    }
    results
}

/// CR 601.2a: Returns true if the `source_id`'s per-turn exile-cast slot is
/// still available under `frequency`. `Unlimited` is always available;
/// `OncePerTurn` consults `state.exile_cast_permissions_used`.
fn exile_cast_frequency_available(
    state: &GameState,
    source_id: ObjectId,
    frequency: CastFrequency,
) -> bool {
    match frequency {
        CastFrequency::Unlimited => true,
        CastFrequency::OncePerTurn => !state.exile_cast_permissions_used.contains(&source_id),
        // CR 110.4 is graveyard-permission-only — Maralen-style exile-cast
        // permissions have no per-permanent-type axis. Treat as a single
        // OncePerTurn slot if the variant ever appears.
        CastFrequency::OncePerTurnPerPermanentType => {
            !state.exile_cast_permissions_used.contains(&source_id)
        }
    }
}

/// CR 601.2a + CR 113.6b: Find the (source, frequency, cost) triple
/// authorizing `player` to cast `exiled_id` via a
/// `StaticMode::ExileCastPermission`, or `None` when no functioning static
/// authorizes the cast. Used by `prepare_spell_cast` / `casting_costs` to tag
/// the `CastingVariant::ExilePermission` context and zero out the mana cost
/// when the static is the `WithoutPayingManaCost` shape.
pub(crate) fn exile_cast_permission_source(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
) -> Option<(ObjectId, CastFrequency, ExileCastCost)> {
    let obj = state.objects.get(&exiled_id)?;
    if !exile_object_can_enter_cast_path(obj) {
        return None;
    }
    // Same empty-pool fast exit as `exile_objects_castable_by_permission`: with
    // both exile pools empty no static can authorize the cast, so skip the
    // battlefield scan in `exile_permission_sources`.
    if state.cards_exiled_with_source_this_turn.is_empty() && state.exile_links.is_empty() {
        return None;
    }
    let sources = exile_permission_sources(state, player);
    sources.into_iter().find_map(|source| {
        if !exile_cast_frequency_available(state, source.source_id, source.frequency) {
            return None;
        }
        // CR 117.1c: A `YourTurnOnly` permission does not authorize a cast
        // outside the controller's turn.
        if !exile_permission_timing_active(state, &source, player) {
            return None;
        }
        let pool = exile_permission_pool(state, &source);
        if !pool.contains(&exiled_id) {
            return None;
        }
        let ctx =
            super::filter::FilterContext::from_source_with_controller(source.source_id, player);
        if !super::filter::matches_target_filter(state, exiled_id, source.filter, &ctx) {
            return None;
        }
        Some((source.source_id, source.frequency, source.cost))
    })
}

/// CR 601.2a + CR 113.6b: Find the full `ExileCastPermission` source authorizing
/// `player` to cast `exiled_id`, including its payment/timing concessions
/// (`mana_spend_permission`, `grants_flash`). Shares the gating logic with
/// `exile_cast_permission_source` (frequency slot, your-turn timing, pool
/// membership, affected filter) but surfaces the concession fields so the
/// any-type-mana and flash wiring can consult them. Returns `None` when no
/// functioning static authorizes the cast.
///
/// CR 601.2a: When `elected_source` is `Some`, only the static carried by that
/// `ObjectId` is eligible — the per-source pool keyed by `source_id` in
/// `exile_permission_sources` makes the elected `CastingVariant::ExilePermission`
/// source uniquely addressable. This is mandatory for cost lookups (extra-cost
/// rider): with two active permissions for the same exiled spell (one
/// normal-cost, one Valgavoth pay-life alternative), the first-match scan would
/// otherwise apply the wrong source's cost treatment regardless of which
/// permission the player elected. A `None` elected source restores the
/// any-authorizing-source scan used by the concession queries (any-type-mana,
/// flash) where no single permission is committed to.
fn exile_cast_permission_source_full(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
    elected_source: Option<ObjectId>,
) -> Option<ExilePermissionSource<'_>> {
    let obj = state.objects.get(&exiled_id)?;
    if !exile_object_can_enter_cast_path(obj) {
        return None;
    }
    if state.cards_exiled_with_source_this_turn.is_empty() && state.exile_links.is_empty() {
        return None;
    }
    let sources = exile_permission_sources(state, player);
    sources.into_iter().find(|source| {
        // CR 601.2a: Bind to the elected permission when one was committed. A
        // mismatched (or no-longer-functioning) elected source fails closed.
        if elected_source.is_some_and(|elected| elected != source.source_id) {
            return false;
        }
        if !exile_cast_frequency_available(state, source.source_id, source.frequency) {
            return false;
        }
        if !exile_permission_timing_active(state, source, player) {
            return false;
        }
        let pool = exile_permission_pool(state, source);
        if !pool.contains(&exiled_id) {
            return false;
        }
        let ctx =
            super::filter::FilterContext::from_source_with_controller(source.source_id, player);
        super::filter::matches_target_filter(state, exiled_id, source.filter, &ctx)
    })
}

/// CR 609.4b: True when an `ExileCastPermission` static granting "mana of any
/// type can be spent to cast those spells" (Azula, Cunning Usurper) authorizes
/// `player` to cast `exiled_id`. Consulted by
/// `player_can_spend_as_any_color_for_spell` so the any-type-mana concession is
/// scoped to spells offered by that static, mirroring the per-card
/// `CastingPermission::PlayFromExile.mana_spend_permission` path.
pub(crate) fn exile_static_permission_grants_any_color(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
) -> bool {
    exile_cast_permission_source_full(state, player, exiled_id, None).is_some_and(|source| {
        matches!(
            source.mana_spend_permission,
            Some(crate::types::ability::ManaSpendPermission::AnyTypeOrColor)
        )
    })
}

/// CR 601.3b + CR 702.8a: True when an `ExileCastPermission` static granting
/// "you may cast them as though they had flash" (Azula, Cunning Usurper)
/// authorizes `player` to cast `exiled_id`. Consulted by the cast-timing check
/// in `prepare_spell_cast` so the spell may be cast at instant speed.
pub(crate) fn exile_static_permission_grants_flash(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
) -> bool {
    exile_cast_permission_source_full(state, player, exiled_id, None)
        .is_some_and(|source| source.grants_flash)
}

/// CR 118.9 + CR 601.2f: When `exiled_id` is castable via the
/// `ExileCastPermission` static carried by `elected_source`, return that
/// permission's `extra_cost` rider (Valgavoth alternative pay-life; Dawnhand
/// additional remove-counters). Consulted by the cast pipeline to (a) zero the
/// mana cost for `Alternative` shapes and (b) route the `AbilityCost` through
/// `pay_additional_cost`.
///
/// CR 601.2a: `elected_source` MUST be the `CastingVariant::ExilePermission`
/// source committed to for this cast. Two active permissions for the same exiled
/// spell (e.g. a normal-cost source plus Valgavoth's pay-life alternative) carry
/// different cost treatments; binding to the elected source guarantees the spell
/// is charged according to the permission the player actually cast through, not
/// whichever functioning source the battlefield scan reaches first.
pub(crate) fn exile_static_permission_extra_cost(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
    elected_source: ObjectId,
) -> Option<crate::types::statics::CastExtraCost> {
    exile_cast_permission_source_full(state, player, exiled_id, Some(elected_source))
        .and_then(|source| source.extra_cost.clone())
}

/// CR 601.2a: The `ExileCastPermission` source `exiled_id`'s cast commits to.
/// Prefers the source already recorded on a `CastingVariant::ExilePermission`
/// (`variant`); otherwise re-derives it from the same first-match scan that
/// stamps the offered candidate (`build_cast_offers` / candidate generation), so
/// legality checks running before variant election (`can_cast_prepared_now`,
/// `effective_spell_cost`) bind to the permission the cast will commit to.
///
/// Returns `None` when no `ExileCastPermission` static authorizes the cast — an
/// impulse `PlayFromExile` or other on-object exile permission carries no static
/// extra-cost rider, so the per-source binding is skipped and no rider applies.
pub(crate) fn elected_exile_permission_source(
    state: &GameState,
    player: PlayerId,
    exiled_id: ObjectId,
    variant: Option<CastingVariant>,
) -> Option<ObjectId> {
    variant
        .and_then(CastingVariant::exile_permission_source)
        .or_else(|| {
            exile_cast_permission_source(state, player, exiled_id).map(|(source, _, _)| source)
        })
}

fn graveyard_permission_sources(
    state: &GameState,
    player: PlayerId,
    play_mode_filter: Option<CardPlayMode>,
) -> Vec<GraveyardPermissionSource<'_>> {
    let mut source_ids: Vec<ObjectId> = state.battlefield.iter().copied().collect();
    if let Some(player_data) = state.players.iter().find(|p| p.id == player) {
        source_ids.extend(player_data.graveyard.iter().copied());
    }

    source_ids
        .into_iter()
        .filter_map(|source_id| {
            let obj = state.objects.get(&source_id)?;
            let source_belongs_to_player = match obj.zone {
                Zone::Battlefield => obj.controller == player,
                _ => obj.owner == player,
            };
            if !source_belongs_to_player {
                return None;
            }
            active_static_definitions(state, obj)
                .filter(|definition| graveyard_permission_functions_in_zone(definition, obj.zone))
                .find_map(|definition| match definition.mode {
                    StaticMode::GraveyardCastPermission {
                        frequency,
                        play_mode,
                        graveyard_destination_replacement,
                        ref extra_cost,
                    } if graveyard_permission_play_mode_matches(play_mode, play_mode_filter) => {
                        definition
                            .affected
                            .as_ref()
                            .map(|filter| GraveyardPermissionSource {
                                source_id,
                                filter,
                                frequency,
                                graveyard_destination_replacement,
                                extra_cost,
                            })
                    }
                    _ => None,
                })
        })
        .collect()
}

fn graveyard_permission_functions_in_zone(definition: &StaticDefinition, zone: Zone) -> bool {
    if zone == Zone::Battlefield {
        definition.active_zones.is_empty() || definition.active_zones.contains(&Zone::Battlefield)
    } else {
        definition.active_zones.contains(&zone)
    }
}

fn graveyard_permission_play_mode_matches(
    play_mode: CardPlayMode,
    play_mode_filter: Option<CardPlayMode>,
) -> bool {
    match play_mode_filter {
        None => true,
        Some(CardPlayMode::Play) => play_mode == CardPlayMode::Play,
        Some(CardPlayMode::Cast) => true,
    }
}

/// CR 110.4 + CR 601.2a: For a `OncePerTurnPerPermanentType` source (Muldrotha),
/// returns all available permanent-type slots that the graveyard object qualifies for.
///
/// Each element is a `CoreType` whose `(source_id, slot_type)` entry is not yet
/// present in `graveyard_cast_permissions_used_per_type`. Returns an empty vec if
/// every permanent type the object carries has already been consumed by this source
/// this turn, or if the object is not a permanent (CR 110.4).
///
/// Order matches `CoreType::PERMANENT_TYPES` (CR 110.4 enumeration).
pub(crate) fn available_permanent_type_slots(
    state: &GameState,
    source_id: ObjectId,
    object_id: ObjectId,
) -> Vec<crate::types::card_type::CoreType> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    crate::types::card_type::CoreType::PERMANENT_TYPES
        .iter()
        .copied()
        .filter(|core_type| {
            obj.card_types.core_types.contains(core_type)
                && !state
                    .graveyard_cast_permissions_used_per_type
                    .contains(&(source_id, *core_type))
        })
        .collect()
}

/// CR 110.4 + CR 601.2a: For a `OncePerTurnPerPermanentType` source (Muldrotha),
/// pick an available permanent-type slot that the graveyard object qualifies for.
///
/// Returns `Some(slot_type)` if the object has at least one permanent type whose
/// `(source_id, slot_type)` entry is not yet present in
/// `graveyard_cast_permissions_used_per_type`. Returns `None` if every permanent
/// type the object carries has already been consumed by this source this turn,
/// or if the object is not a permanent (per CR 110.4 instants/sorceries are not
/// permanent types).
///
/// Selection order matches `CoreType::PERMANENT_TYPES` (CR 110.4 enumeration).
/// CR 305.1: lands are picked here too — Muldrotha's "play a land or cast a
/// permanent spell of each permanent type from your graveyard" treats land as
/// one of the permanent type slots.
pub(crate) fn pick_per_permanent_type_slot(
    state: &GameState,
    source_id: ObjectId,
    object_id: ObjectId,
) -> Option<crate::types::card_type::CoreType> {
    available_permanent_type_slots(state, source_id, object_id)
        .into_iter()
        .next()
}

/// CR 601.2a: Returns true if a graveyard-cast source's frequency slot is
/// available for the given object. Centralizes the
/// `OncePerTurn` (per-source) vs `OncePerTurnPerPermanentType` (per-source +
/// per-CR-110.4-permanent-type) vs `Unlimited` (always-available) check so the
/// per-frequency logic lives in one place.
fn frequency_slot_available(
    state: &GameState,
    source_id: ObjectId,
    object_id: ObjectId,
    frequency: CastFrequency,
) -> bool {
    match frequency {
        CastFrequency::Unlimited => true,
        CastFrequency::OncePerTurn => !state.graveyard_cast_permissions_used.contains(&source_id),
        // CR 110.4: At least one permanent-type slot must remain unused.
        CastFrequency::OncePerTurnPerPermanentType => {
            pick_per_permanent_type_slot(state, source_id, object_id).is_some()
        }
    }
}

/// CR 601.2a: Find the first valid permission source for a specific graveyard object.
/// Returns the permission source so the caller can track per-turn usage and
/// preserve any destination replacement rider.
fn graveyard_permission_source(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<GraveyardPermissionSource<'_>> {
    if state.objects.get(&object_id).is_some_and(|obj| {
        obj.card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
    }) {
        return None;
    }
    graveyard_permission_sources(state, player, Some(CardPlayMode::Cast))
        .into_iter()
        .find(|source| {
            // CR 604.2 + CR 110.4: Skip if this source's slot has already been used.
            if !frequency_slot_available(state, source.source_id, object_id, source.frequency) {
                return false;
            }
            super::filter::matches_target_filter(
                state,
                object_id,
                source.filter,
                &super::filter::FilterContext::from_source_with_controller(
                    source.source_id,
                    player,
                ),
            )
        })
}

/// CR 601.2f: When `object_id` is castable from the graveyard via a
/// `GraveyardCastPermission` static that carries an `extra_cost` rider (Festival
/// of Embers' additional pay-life), return the rider. Consulted by the cast
/// pipeline to route the additional `AbilityCost` through `pay_additional_cost`.
pub(crate) fn graveyard_static_permission_extra_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<crate::types::statics::CastExtraCost> {
    graveyard_permission_source(state, player, object_id)
        .and_then(|source| source.extra_cost.clone())
}

fn filter_has_keyword_kind_constraint(filter: &TargetFilter, kind: KeywordKind) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::HasKeywordKind { value } if *value == kind)),
        TargetFilter::And { filters } => filters
            .iter()
            .any(|inner| filter_has_keyword_kind_constraint(inner, kind)),
        _ => false,
    }
}

fn has_graveyard_cast_permission_without_keyword_constraint(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    kind: KeywordKind,
) -> bool {
    graveyard_permission_sources(state, player, Some(CardPlayMode::Cast))
        .into_iter()
        .any(|source| {
            !filter_has_keyword_kind_constraint(source.filter, kind)
                && frequency_slot_available(state, source.source_id, object_id, source.frequency)
                && super::filter::matches_target_filter(
                    state,
                    object_id,
                    source.filter,
                    &super::filter::FilterContext::from_source_with_controller(
                        source.source_id,
                        player,
                    ),
                )
        })
}

/// CR 401.5 + CR 118.9 + CR 601.2a: Find the (single) top card of `player`'s
/// library if a battlefield static grants `TopOfLibraryCastPermission` whose
/// `affected` filter matches it. Returns
/// `(top_card_id, source_id, frequency, alt_cost)` for the *selected*
/// authorizing permission.
///
/// CR 601.2a: When more than one permission can authorize the same top-of-
/// library cast, an `Unlimited` authorizer (Realmwalker, Future Sight, Bolas's
/// Citadel) is preferred over a bounded `OncePerTurn` one (Assemble the
/// Players, Johann). The unlimited permission alone suffices, so the player is
/// not forced to spend a once-per-turn slot — selecting it preserves the
/// bounded slot for a later cast this turn. The `frequency` of the selected
/// source is what drives per-turn-slot consumption at `finalize_cast`; the
/// selected source/frequency is threaded through the casting context rather
/// than independently rescanned, so availability and consumption agree on the
/// single authorizing permission.
///
/// Filter eligibility is re-evaluated each call because the top of library
/// changes between priority windows; callers (`spell_objects_available_to_cast`,
/// `prepare_spell_cast`) invoke this fresh each lookup. `play_mode_filter`
/// gates which permissions count: `Some(CardPlayMode::Cast)` for the spell-
/// availability path, `Some(CardPlayMode::Play)` for land plays. `None` lets
/// any mode through.
pub(crate) fn top_of_library_permission_source(
    state: &GameState,
    player: PlayerId,
    play_mode_filter: Option<CardPlayMode>,
) -> Option<(
    ObjectId,
    ObjectId,
    CastFrequency,
    Option<crate::types::ability::AbilityCost>,
)> {
    let player_data = state.players.iter().find(|p| p.id == player)?;
    let &top_id = player_data.library.front()?;
    // CR 601.2a: Collect every permission that can authorize this cast, then
    // prefer an `Unlimited` authorizer so a bounded `OncePerTurn` slot is only
    // spent when nothing else authorizes the cast.
    let mut selected: Option<(
        ObjectId,
        CastFrequency,
        Option<crate::types::ability::AbilityCost>,
    )> = None;
    for &src_id in &state.battlefield {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }
        let Some((frequency, alt_cost)) = active_static_definitions(state, obj)
            .find_map(|s| match &s.mode {
                StaticMode::TopOfLibraryCastPermission {
                    play_mode,
                    frequency,
                    alt_cost,
                } => {
                    // Gate by play_mode: Cast permissions cover only spells;
                    // Play permissions cover both lands and non-land spells
                    // (CR 305.1). When the caller specifies a mode, only
                    // permissions matching that mode (or wider) qualify.
                    let mode_matches = match play_mode_filter {
                        None => true,
                        Some(CardPlayMode::Play) => *play_mode == CardPlayMode::Play,
                        Some(CardPlayMode::Cast) => true,
                    };
                    if !mode_matches {
                        return None;
                    }
                    // CR 601.2a: A `OncePerTurn` permission winks out for the rest
                    // of the turn once a spell has been cast through this source
                    // (Assemble the Players, Johann). `Unlimited` permissions
                    // (Realmwalker, Future Sight, Bolas's Citadel) never consult
                    // the used-set.
                    if matches!(frequency, CastFrequency::OncePerTurn)
                        && state.top_of_library_cast_permissions_used.contains(&src_id)
                    {
                        return None;
                    }
                    s.affected
                        .as_ref()
                        .map(|f| (f, *frequency, alt_cost.clone()))
                }
                _ => None,
            })
            .and_then(|(filter, frequency, alt_cost)| {
                super::filter::matches_target_filter(
                    state,
                    top_id,
                    filter,
                    &super::filter::FilterContext::from_source_with_controller(src_id, player),
                )
                .then_some((frequency, alt_cost))
            })
        else {
            continue;
        };
        // CR 601.2a: An `Unlimited` authorizer always wins — it preserves any
        // bounded slot. Otherwise keep the first match found.
        let prefer = frequency.is_unlimited()
            || selected
                .as_ref()
                .is_none_or(|(_, sel_freq, _)| !sel_freq.is_unlimited());
        if prefer {
            selected = Some((src_id, frequency, alt_cost));
        }
        if frequency.is_unlimited() {
            break;
        }
    }
    selected.map(|(src_id, frequency, alt_cost)| (top_id, src_id, frequency, alt_cost))
}

/// CR 702.170a + CR 702.170f: Return the `(top_library_card, grant_source)` pair
/// when the player may take the plot special action on the top card of their
/// library. Fblthp, Lost on the Range is the type specimen ("The top card of
/// your library has plot." + "You may plot nonland cards from the top of your
/// library.").
///
/// Plot-from-library is two distinct CR roles, modeled as two statics, and BOTH
/// must hold for the top card:
/// - GRANT (`StaticMode::TopOfLibraryHasPlot`, CR 702.170a) — the top card *has*
///   the plot ability. Eligible iff the top card matches the UNION of all active
///   grants' `affected` filters (Fblthp L3 = `Any`).
/// - PERMISSION (`StaticMode::TopOfLibraryPlotPermission`, CR 702.170f) — an
///   effect lets the plot ability function from a zone other than hand and
///   permits taking the action there. Eligible iff the top card matches the
///   UNION of all active permissions' `affected` filters (Fblthp L4 = nonland).
///
/// Requiring both is rules-correct: a grant alone leaves a plot ability that
/// (CR 702.170a) only functions in hand, so a library card can't be plotted
/// without a CR 702.170f permission; a permission alone has no plot ability to
/// act on. UNION within each role means two INDEPENDENT plot-from-top sources
/// each authorize their own eligibility (no cross-source veto), while AND across
/// the two roles enforces "has plot" ∧ "may plot it here". For Fblthp the net
/// eligible set is `Any ∩ nonland = nonland`; the nonland restriction is purely
/// the permission's filter (Fblthp's printed L4) — CR 702.170f itself has no
/// land/nonland clause, so there is NO separate hard-gate (a future land-
/// permitting plot-from-top card would correctly allow lands).
///
/// Categorically distinct from [`top_of_library_permission_source`] (CR 601.2a —
/// a `Library → Stack` cast with no exile). This authorizes the CR 702.170 plot
/// special action: `Library → Exile` face up now, then a free `Exile → Stack`
/// cast on a later turn. The positional top-only restriction (CR 702.170f — "the
/// card is exiled from the zone it is in") lives HERE, not in the activation-zone
/// gate; the top of library is re-derived each call because it changes between
/// priority windows. The returned source is an authorizing grant permanent.
pub(crate) fn top_of_library_plot_source(
    state: &GameState,
    player: PlayerId,
) -> Option<(ObjectId, ObjectId)> {
    let player_data = state.players.iter().find(|p| p.id == player)?;
    let &top_id = player_data.library.front()?;

    // Scan the player's battlefield once, classifying active plot statics into
    // the two CR roles and UNION-matching each role's `affected` filter against
    // the current top card.
    let mut grant_source: Option<ObjectId> = None; // first grant whose filter matches
    let mut has_permission = false; // any permission whose filter matches
    for &src_id in &state.battlefield {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }
        for s in active_static_definitions(state, obj) {
            let role_is_grant = match s.mode {
                StaticMode::TopOfLibraryHasPlot => true,
                StaticMode::TopOfLibraryPlotPermission => false,
                _ => continue,
            };
            // A `None` filter means no restriction (matches any top card).
            let matches = s.affected.as_ref().is_none_or(|f| {
                super::filter::matches_target_filter(
                    state,
                    top_id,
                    f,
                    &super::filter::FilterContext::from_source_with_controller(src_id, player),
                )
            });
            if !matches {
                continue;
            }
            if role_is_grant {
                grant_source.get_or_insert(src_id);
            } else {
                has_permission = true;
            }
        }
    }

    // CR 702.170a grant ∧ CR 702.170f permission: the top card must both HAVE
    // plot and be permitted to be plotted from the library.
    match grant_source {
        Some(src_id) if has_permission => Some((top_id, src_id)),
        _ => None,
    }
}

/// CR 401.5 + CR 305.1: Return the top-of-library land + source pair when a
/// battlefield static grants `TopOfLibraryCastPermission { play_mode: Play }`
/// and the top card is a land that matches the static's `affected` filter.
///
/// Future Sight, Bolas's Citadel, and Magus of the Future all carry the wider
/// `play_mode: Play` permission and so reach this path; Mystic Forge /
/// Realmwalker (cast-only) do not. CR 305.1 — lands are "played," not "cast,"
/// so the engine emits `GameAction::PlayLand` for the library top via this
/// helper rather than routing through the cast pipeline.
pub fn top_of_library_land_playable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Option<(ObjectId, ObjectId)> {
    let (top_id, src_id, _freq, _alt) =
        top_of_library_permission_source(state, player, Some(CardPlayMode::Play))?;
    let obj = state.objects.get(&top_id)?;
    // CR 305.1: Only lands reach this path; non-land cards under the same
    // permission flow through `spell_objects_available_to_cast`.
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return None;
    }
    Some((top_id, src_id))
}

/// CR 118.9 + CR 401.5: When `object_id` is the current top of `player`'s library
/// and a `TopOfLibraryCastPermission` static grants an alt-cost rider (Bolas's
/// Citadel: pay life equal to mana value), return that cost for castability
/// pre-checks and the `check_additional_cost_or_pay` payment path.
pub(crate) fn top_of_library_alt_ability_cost_for_object(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<crate::types::ability::AbilityCost> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone != Zone::Library || obj.owner != player {
        return None;
    }
    top_of_library_permission_source(state, player, Some(CardPlayMode::Cast)).and_then(
        |(top_id, _src, _freq, alt)| {
            if top_id == object_id {
                alt
            } else {
                None
            }
        },
    )
}

/// CR 601.2a + CR 401.5: When `object_id` is the current top of `player`'s
/// library, return the `(source, frequency)` of the `TopOfLibraryCastPermission`
/// static that the cast pipeline *selects* to authorize the cast. This is the
/// single authority threaded through the casting context to drive per-turn-slot
/// consumption — it mirrors how `CastingVariant::ExilePermission` /
/// `GraveyardPermission` carry their authorizing source through `finalize_cast`.
///
/// Delegates to [`top_of_library_permission_source`], which prefers an
/// `Unlimited` authorizer when one exists (CR 601.2a: an unlimited permission
/// alone suffices, so a bounded `OncePerTurn` slot must not be spent when an
/// unlimited one also matches). `finalize_cast` stamps
/// `top_of_library_cast_permissions_used` ONLY when the returned `frequency` is
/// `OncePerTurn` — an `Unlimited` selection never consumes a slot. Returns
/// `None` when no top-of-library permission authorizes casting `object_id`.
pub(crate) fn top_of_library_selected_permission(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<(ObjectId, CastFrequency)> {
    top_of_library_permission_source(state, player, Some(CardPlayMode::Cast)).and_then(
        |(top_id, src_id, frequency, _alt)| {
            // CR 401.5: only the actual top card is authorized by the permission.
            (top_id == object_id).then_some((src_id, frequency))
        },
    )
}

/// CR 604.2 + CR 305.1 + CR 701.17d: Find lands in the player's graveyard that
/// can be played, via either a `GraveyardCastPermission` static with
/// `play_mode: Play` (Muldrotha class) OR an object-tagged
/// [`CastingPermission::PlayFromExile`] (a milled land whose "you may play that
/// card" mill grant attached the permission to it in the graveyard — CR 701.17d,
/// Ark of Hunger / Tablet of Discovery milling a land). Returns
/// `(land_id, source_id)` for once-per-turn tracking by the play-land path.
pub fn graveyard_lands_playable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId)> {
    let mut results = Vec::new();
    let player_data = match state.players.iter().find(|p| p.id == player) {
        Some(p) => p,
        None => return results,
    };

    // CR 701.17d: Object-tagged `PlayFromExile` on a milled land in the
    // graveyard. Mirrors the object-tagged branch of
    // `exile_lands_playable_by_permission`.
    for &gy_obj_id in &player_data.graveyard {
        let Some(obj) = state.objects.get(&gy_obj_id) else {
            continue;
        };
        if !obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
        {
            continue;
        }
        if let Some((source, _)) =
            play_from_exile_permission_source(state, obj, player, state.turn_number)
        {
            results.push((gy_obj_id, source));
        }
    }

    let sources = graveyard_permission_sources(state, player, Some(CardPlayMode::Play));
    for source in &sources {
        let ctx =
            super::filter::FilterContext::from_source_with_controller(source.source_id, player);
        for &gy_obj_id in &player_data.graveyard {
            if let Some(obj) = state.objects.get(&gy_obj_id) {
                // CR 305.1: Only lands can be "played" (non-land cards require "cast")
                if !obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
                {
                    continue;
                }
                // CR 604.2 + CR 110.4: Per-source frequency slot check; for
                // `OncePerTurnPerPermanentType` (Muldrotha) the land slot is
                // its own per-permanent-type entry.
                if !frequency_slot_available(state, source.source_id, gy_obj_id, source.frequency) {
                    continue;
                }
                if super::filter::matches_target_filter(state, gy_obj_id, source.filter, &ctx) {
                    results.push((gy_obj_id, source.source_id));
                }
            }
        }
    }
    results
}

/// CR 305.1 + CR 113.6b + CR 406.6: Find the `StaticMode::ExileCastPermission`
/// source (if any) authorizing `player` to play the exiled land `land_id`. Only
/// `play_mode: Play` sources admit lands (CR 305.1: lands are played, not cast);
/// the source's pool scope, timing gate, frequency slot, and `affected` filter
/// must all pass. Mirrors `exile_cast_permission_source` for the land-play side.
fn exile_land_playable_by_static_permission(
    state: &GameState,
    player: PlayerId,
    land_id: ObjectId,
) -> Option<ObjectId> {
    if state.cards_exiled_with_source_this_turn.is_empty() && state.exile_links.is_empty() {
        return None;
    }
    let sources = exile_permission_sources(state, player);
    sources.into_iter().find_map(|source| {
        // CR 305.1: only `Play` sources let the controller play exiled lands.
        if source.play_mode != CardPlayMode::Play {
            return None;
        }
        if !exile_cast_frequency_available(state, source.source_id, source.frequency) {
            return None;
        }
        // CR 117.1c: a `YourTurnOnly` permission is inactive outside the
        // controller's turn.
        if !exile_permission_timing_active(state, &source, player) {
            return None;
        }
        let pool = exile_permission_pool(state, &source);
        if !pool.contains(&land_id) {
            return None;
        }
        let ctx =
            super::filter::FilterContext::from_source_with_controller(source.source_id, player);
        if !super::filter::matches_target_filter(state, land_id, source.filter, &ctx) {
            return None;
        }
        Some(source.source_id)
    })
}

/// CR 305.1 + CR 601.2a + CR 113.6b: Find exiled lands `player` may play, via
/// either the object-tagged `CastingPermission::PlayFromExile` (impulse draw) or
/// a battlefield `StaticMode::ExileCastPermission { play_mode: Play }` static
/// (The Matrix of Time). Returns `(land_id, source_id)` for once-per-turn
/// tracking by the play-land path.
pub fn exile_lands_playable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId)> {
    state
        .exile
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
            {
                return None;
            }
            // Object-tagged impulse permission first; fall back to the
            // battlefield-static exile-play permission.
            if let Some((source, _)) =
                play_from_exile_permission_source(state, obj, player, state.turn_number)
            {
                return Some((obj_id, source));
            }
            let source = exile_land_playable_by_static_permission(state, player, obj_id)?;
            Some((obj_id, source))
        })
        .collect()
}

/// CR 601.2b + CR 118.9a: Find the first `CastFromHandFree` static permission
/// source on the controller's battlefield whose filter admits the given spell.
/// Returns `(source_id, frequency)` so callers can track per-turn usage.
///
/// For `OncePerTurn` sources, the already-used set is consulted; exhausted sources
/// do not qualify. `Unlimited` sources always qualify if their filter matches.
fn cast_free_origin_admits_object(
    state: &GameState,
    player: PlayerId,
    obj: &crate::game::game_object::GameObject,
    origin: CastFreeOrigin,
) -> bool {
    if obj.owner != player {
        return false;
    }
    match origin {
        CastFreeOrigin::Hand => obj.zone == Zone::Hand,
        CastFreeOrigin::DefaultCastPermission => match obj.zone {
            Zone::Hand => true,
            Zone::Command => {
                state.format_config.command_zone
                    && (obj.is_commander
                        || (obj.is_signature_spell() && oathbreaker_on_battlefield(state, player)))
            }
            _ => false,
        },
    }
}

/// CR 114.4: `CastFromHandFree` granting sources function on the battlefield
/// (Omniscience, Zaffai, Dracogenesis) and from the command zone when they are
/// emblems (Tamiyo, Field Researcher). `active_static_definitions` applies the
/// CR 113.6b opt-in gate for non-emblem command-zone objects.
fn iter_cast_free_permission_source_ids(state: &GameState) -> impl Iterator<Item = ObjectId> + '_ {
    state
        .battlefield
        .iter()
        .chain(state.command_zone.iter())
        .copied()
}

fn cast_free_permission_from_source(
    state: &GameState,
    player: PlayerId,
    obj: &crate::game::game_object::GameObject,
    source_id: ObjectId,
) -> Option<CastFrequency> {
    let src_obj = state.objects.get(&source_id)?;
    if src_obj.controller != player {
        return None;
    }
    active_static_definitions(state, src_obj).find_map(|s| {
        let StaticMode::CastFromHandFree { frequency, origin } = s.mode else {
            return None;
        };
        // CR 601.2b: Skip if this source's once-per-turn slot was already used.
        if frequency == CastFrequency::OncePerTurn
            && state.hand_cast_free_permissions_used.contains(&source_id)
        {
            return None;
        }
        if !cast_free_origin_admits_object(state, player, obj, origin) {
            return None;
        }
        let filter = s.affected.as_ref()?;
        if super::filter::matches_target_filter(
            state,
            obj.id,
            filter,
            &super::filter::FilterContext::from_source_with_controller(source_id, player),
        ) {
            Some(frequency)
        } else {
            None
        }
    })
}

pub(crate) fn hand_cast_free_permission_source(
    state: &GameState,
    player: PlayerId,
    obj: &crate::game::game_object::GameObject,
) -> Option<(ObjectId, CastFrequency)> {
    iter_cast_free_permission_source_ids(state).find_map(|src_id| {
        cast_free_permission_from_source(state, player, obj, src_id)
            .map(|frequency| (src_id, frequency))
    })
}

/// CR 601.2b + CR 118.9a: `Unlimited` `CastFromHandFree` that zeroes mana cost on
/// the normal `CastSpell` path (Omniscience from hand; Dracogenesis from hand or
/// command-zone commanders).
fn unlimited_hand_cast_free_applies(
    state: &GameState,
    player: PlayerId,
    obj: &crate::game::game_object::GameObject,
    casting_variant: CastingVariant,
) -> bool {
    !matches!(casting_variant, CastingVariant::HandPermission { .. })
        && hand_cast_free_permission_source(state, player, obj)
            .is_some_and(|(_, frequency)| frequency == CastFrequency::Unlimited)
}

/// CR 601.2f: Whether `spell_id` matches a pending next-spell modifier's optional filter.
fn spell_matches_pending_next_spell_filter(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    entry: &crate::types::game_state::PendingNextSpellModifier,
) -> bool {
    let filter_source_id = entry.source_id.unwrap_or(spell_id);
    entry.spell_filter.as_ref().is_none_or(|filter| {
        spell_matches_cost_filter(state, caster, spell_id, filter, filter_source_id)
    })
}

/// CR 601.2f: First pending next-spell modifier index matching `caster`, `spell_id`, and `predicate`.
fn pending_next_spell_modifier_index(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    predicate: impl Fn(&NextSpellModifier) -> bool,
) -> Option<usize> {
    state.pending_next_spell_modifiers.iter().position(|entry| {
        entry.player == caster
            && spell_matches_pending_next_spell_filter(state, caster, spell_id, entry)
            && predicate(&entry.modifier)
    })
}

/// CR 601.2f: Apply keyword/flash grants from matching pending next-spell modifiers.
fn apply_pending_next_spell_keyword_grants(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    keywords: &mut Vec<Keyword>,
    preserve_instances: bool,
) {
    for entry in &state.pending_next_spell_modifiers {
        if entry.player != caster {
            continue;
        }
        if !spell_matches_pending_next_spell_filter(state, caster, spell_id, entry) {
            continue;
        }
        match &entry.modifier {
            NextSpellModifier::HasKeyword { keyword } => {
                merge_spell_keyword(keywords, keyword.clone(), preserve_instances);
            }
            NextSpellModifier::CastAsThoughFlash => {
                upsert_keyword_by_kind(keywords, Keyword::Flash);
            }
            NextSpellModifier::CantBeCountered | NextSpellModifier::WithoutPayingManaCost => {}
        }
    }
}

/// CR 601.2a + CR 113.6g: Stamp stack-resident grants from pending next-spell modifiers.
pub(super) fn apply_pending_next_spell_stack_grants(
    state: &mut GameState,
    caster: PlayerId,
    spell_id: ObjectId,
) {
    let stamp_cant_be_countered = state.pending_next_spell_modifiers.iter().any(|entry| {
        entry.player == caster
            && spell_matches_pending_next_spell_filter(state, caster, spell_id, entry)
            && matches!(entry.modifier, NextSpellModifier::CantBeCountered)
    });
    if stamp_cant_be_countered {
        if let Some(obj) = state.objects.get_mut(&spell_id) {
            if !obj
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBeCountered)
            {
                obj.static_definitions
                    .push(StaticDefinition::new(StaticMode::CantBeCountered));
            }
        }
    }
}

/// CR 601.2f: Remove pending next-spell modifiers whose filter matched this cast.
pub(super) fn consume_pending_next_spell_modifiers(
    state: &mut GameState,
    caster: PlayerId,
    spell_id: ObjectId,
) {
    let remove: Vec<usize> = state
        .pending_next_spell_modifiers
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            (entry.player == caster
                && spell_matches_pending_next_spell_filter(state, caster, spell_id, entry))
            .then_some(idx)
        })
        .collect();
    for idx in remove.into_iter().rev() {
        state.pending_next_spell_modifiers.remove(idx);
    }
}

/// Returns the effective mana cost for casting a spell, after all modifiers
/// (alt costs, commander tax, battlefield reducers, affinity).
/// Returns `None` if the object cannot be cast.
pub fn effective_spell_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<crate::types::mana::ManaCost> {
    prepare_spell_cast(state, player, object_id)
        .ok()
        .map(|p| p.mana_cost)
}

/// Returns the engine-effective mana cost for `object_id` **as if** all
/// situational restrictions (timing, "can't cast" statics, color identity,
/// per-turn limits, mana affordability) were already satisfied. Always applies
/// commander tax and every cost-modification static (Affinity, ReduceCost,
/// RaiseCost, pending one-shot reductions, etc.) so the display layer can show
/// the actual cost the player would pay if and when they could cast.
///
/// Returns `None` only for structural rejections — object missing, not in a
/// player-castable zone, or a land (which is played, not cast). All other
/// restrictions are deliberately suppressed.
///
/// This is the engine-authoritative answer for "what does this spell cost?"
/// and is the only source of truth the UI may consult for cost display.
pub fn display_spell_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<crate::types::mana::ManaCost> {
    prepare_spell_cast_for_display(state, player, object_id)
        .ok()
        .map(|p| p.mana_cost)
}

fn prepare_spell_cast(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Result<PreparedSpellCast, EngineError> {
    prepare_spell_cast_with_variant_override_inner(
        state,
        player,
        object_id,
        None,
        CastingMode::Actual,
    )
}

fn prepare_spell_cast_for_display(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Result<PreparedSpellCast, EngineError> {
    prepare_spell_cast_with_variant_override_inner(
        state,
        player,
        object_id,
        None,
        CastingMode::Display,
    )
}

/// CR 702.190a: Variant-overriding entry point for cast paths that need a
/// specific `CastingVariant` applied before timing/cost resolution (e.g., Sneak
/// forces declare-blockers timing regardless of the cost the mana-path picked).
fn prepare_spell_cast_with_variant_override(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    variant_override: Option<CastingVariant>,
) -> Result<PreparedSpellCast, EngineError> {
    prepare_spell_cast_with_variant_override_inner(
        state,
        player,
        object_id,
        variant_override,
        CastingMode::Actual,
    )
}

#[derive(Debug)]
struct CastingVariantChoiceSet {
    options: Vec<CastingVariantChoiceOption>,
    had_multiple_candidates: bool,
}

fn casting_variant_choice_set(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> CastingVariantChoiceSet {
    let mut candidates = casting_variant_candidates(state, player, object_id);
    candidates.dedup();
    let had_multiple_candidates = candidates.len() > 1;
    let mut options = Vec::new();

    for variant in candidates {
        let Ok(prepared) =
            prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))
        else {
            continue;
        };
        if !can_cast_prepared_now(state, player, &prepared) {
            continue;
        }
        options.push(CastingVariantChoiceOption {
            variant: prepared.casting_variant,
            mana_cost: prepared.mana_cost,
        });
    }

    CastingVariantChoiceSet {
        options,
        had_multiple_candidates,
    }
}

fn casting_variant_candidates(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Vec<CastingVariant> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();

    if obj.zone == Zone::Graveyard {
        if super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Escape)
        {
            candidates.push(CastingVariant::Escape);
        }
        if has_retrace_keyword(state, object_id) {
            candidates.push(CastingVariant::Retrace);
        }
        // CR 702.180a: Harmonize may be printed or granted to a graveyard card
        // (Songcrafter Mage), so query the effective off-zone keyword.
        if has_harmonize_keyword(state, object_id) {
            candidates.push(CastingVariant::Harmonize);
        }
        // CR 702.187b: Mayhem is available only while the card was discarded this
        // turn. The cost may be printed or granted to graveyard cards by a static
        // (Green Goblin), so query the effective off-zone keyword cost.
        if mayhem_castable_from_graveyard(state, player, object_id) {
            candidates.push(CastingVariant::Mayhem);
        }
        if super::keywords::effective_flashback_cost(state, object_id).is_some() {
            candidates.push(CastingVariant::Flashback);
        }
        if has_aftermath_keyword(state, object_id) {
            candidates.push(CastingVariant::Aftermath);
        }
        if jumpstart_castable_from_graveyard(state, object_id) {
            candidates.push(CastingVariant::JumpStart);
        }
        if super::keywords::effective_disturb_cost(state, object_id).is_some() {
            candidates.push(CastingVariant::Disturb);
        }
        if let Some(source) = graveyard_permission_source(state, player, object_id) {
            let slot_type = if source.frequency == CastFrequency::OncePerTurnPerPermanentType {
                let slots = available_permanent_type_slots(state, source.source_id, object_id);
                if slots.len() == 1 {
                    Some(slots[0])
                } else {
                    None
                }
            } else {
                None
            };
            candidates.push(CastingVariant::GraveyardPermission {
                source: source.source_id,
                frequency: source.frequency,
                slot_type,
                graveyard_destination_replacement: source.graveyard_destination_replacement,
            });
        }
        if has_graveyard_timed_alt_cost_permission(state, obj, player) {
            candidates.push(CastingVariant::Normal);
        }
    }

    if obj.zone == Zone::Exile {
        let has_alt_cost = obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::ExileWithAltCost { .. }));
        // CR 702.62a: Suspend candidate selection. Runtime-granted Suspend
        // (CR 604.1, e.g. Jhoira of the Ghitu / The Tenth Doctor) lives in
        // the effective off-zone keyword set, not `obj.keywords`, so query
        // through the off-zone-aware helper to match Flashback/Retrace/
        // Aftermath/Escape recognition in this file.
        if has_alt_cost
            && super::keywords::object_has_effective_keyword_kind(
                state,
                object_id,
                KeywordKind::Suspend,
            )
        {
            candidates.push(CastingVariant::Suspend);
        }
        if obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::Plotted { .. }))
        {
            candidates.push(CastingVariant::Plot);
        }
        if obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, CastingPermission::Foretold { .. }))
        {
            candidates.push(CastingVariant::Foretell);
        }
        // CR 601.2a + CR 113.6b + CR 118.9a: Cast-from-exile via a
        // `StaticMode::ExileCastPermission` source (Maralen, Fae Ascendant).
        // Detection is by per-source pool lookup, not by an on-object permission
        // — the static issues no `CastingPermission` decoration; eligibility is
        // re-derived each cast preparation from the per-turn pool plus the
        // static's `affected` filter.
        if let Some((source, frequency, _without_paying)) =
            exile_cast_permission_source(state, player, object_id)
        {
            candidates.push(CastingVariant::ExilePermission { source, frequency });
        }
    }

    // CR 702.173a: Freerunning is a static spell ability — the alt-cost
    // permission lives on the spell card (printed or granted via
    // `CastWithKeyword`) and only applies while the spell is in a castable
    // zone. Today the only printed home for Freerunning is hand-castable
    // spells (CR 601.2a default zone), so only the Zone::Hand branch surfaces
    // it. The eligibility predicate ("a player was dealt combat damage this
    // turn by an Assassin creature or commander you control") is read from
    // the per-turn ledger maintained in `triggers::collect_pending_triggers`.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, Keyword::Freerunning(_)))
        && state
            .assassin_or_commander_dealt_combat_damage_this_turn
            .contains(&player)
    {
        candidates.push(CastingVariant::Freerunning);
    }

    // CR 702.76a: Prowl — a hand alternative cost legal when a source the caster
    // controlled dealt combat damage to a player this turn and, at that time, had
    // any of this spell's creature types. The per-turn creature-type ledger is
    // snapshot at damage time (`creature_types_dealt_combat_damage_this_turn`).
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, Keyword::Prowl(_)))
        && prowl_damage_ledger_satisfied(state, player, object_id)
    {
        candidates.push(CastingVariant::Prowl);
    }

    // CR 702.117a: Surge — a hand alternative cost legal when the caster OR
    // one of their teammates (CR 810.5 doesn't share hand/casting resources,
    // but Surge's own text explicitly extends to teammates) has cast another
    // spell this turn. The surge spell isn't recorded in
    // `spells_cast_this_turn_by_player` yet at offer time, so any prior entry
    // for the caster or a teammate satisfies "another spell".
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, Keyword::Surge(_)))
        && std::iter::once(player)
            .chain(super::players::teammates(state, player))
            .any(|p| {
                state
                    .spells_cast_this_turn_by_player
                    .get(&p)
                    .is_some_and(|spells| !spells.is_empty())
            })
    {
        candidates.push(CastingVariant::Surge);
    }

    // CR 702.74a + CR 118.9: Evoke is a static alternative cost usable from any
    // zone the card can be cast from; surface it as a hand candidate so the gate
    // offers it when the printed cost is unaffordable. effective_spell_keywords
    // covers printed (obj.keywords) AND granted (CastWithKeyword) evoke.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Evoke(_)))
    {
        candidates.push(CastingVariant::Evoke);
    }

    // CR 702.96a + CR 118.9: Overload is a static alternative cost. Surface it as
    // a hand candidate so the gate offers it even when the printed cast has no legal
    // target (the overload mode requires none — CR 702.96b). effective_spell_keywords
    // covers printed (obj.keywords) AND granted (CastWithKeyword) overload.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Overload(_)))
    {
        candidates.push(CastingVariant::Overload);
    }

    // CR 702.119a-c + CR 118.9: Emerge is a hand-zone alternative cost that
    // requires sacrificing a creature and reducing the emerge cost by that
    // creature's mana value.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Emerge(_)))
    {
        candidates.push(CastingVariant::Emerge);
    }

    // CR 702.109a: Dash is an opt-in alternative cost from hand; surface it as a
    // candidate so the gate offers it (and so it is reachable when the printed
    // cost is unaffordable). Read the *effective* spell keywords so a Dash cost
    // granted by a static (CR 604.1) is honored, not just printed Dash.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Dash(_)))
    {
        candidates.push(CastingVariant::Dash);
    }

    // CR 702.152a: Blitz is an opt-in alternative cost from hand; surface it as a
    // candidate so the gate offers it (and so it is reachable when the printed
    // cost is unaffordable). Read the *effective* spell keywords so a Blitz cost
    // granted by a static (CR 604.1) is honored, not just printed Blitz.
    // CR 702.152b: only one Blitz may be applied to a spell, so the dedup-by-kind
    // `effective_spell_keywords` is the correct (single-instance) collector here.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Blitz(_)))
    {
        candidates.push(CastingVariant::Blitz);
    }

    // CR 702.137a: Spectacle is an opt-in alternative cost from hand, available
    // only if an opponent lost life this turn (a static ability functioning on
    // the stack). Surface the candidate only while that condition holds. Read
    // the *effective* spell keywords so a Spectacle cost granted by a static
    // (CR 604.1) is honored, not just printed Spectacle.
    if obj.zone == Zone::Hand
        && effective_spell_keywords(state, player, object_id)
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Spectacle(_)))
        && an_opponent_lost_life_this_turn(state, player)
    {
        candidates.push(CastingVariant::Spectacle);
    }

    // CR 702.102a: Fuse is a static ability on split cards that applies while
    // the card is in a player's hand. It lets the caster cast both halves as a
    // fused split spell. Only offered when the back face is the right (Split)
    // half so single-faced cards never surface it.
    let has_fuse_candidate = obj.zone == Zone::Hand
        && obj
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Fuse))
        && obj
            .back_face
            .as_ref()
            .is_some_and(|bf| bf.layout_kind == Some(LayoutKind::Split));
    if has_fuse_candidate {
        candidates.push(CastingVariant::Normal);
        candidates.push(CastingVariant::Fuse);
    }

    candidates
}

fn prepare_spell_cast_with_variant_override_inner(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    variant_override: Option<CastingVariant>,
    mode: CastingMode,
) -> Result<PreparedSpellCast, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    // CR 715.3d + CR 701.17d: Cards carrying an object-tagged play/cast
    // permission. Exile sources cover AdventureCreature / ExileWithAltCost /
    // impulse `PlayFromExile`; the graveyard branch covers a milled card whose
    // `PlayFromExile` was granted by a "you may play that card" mill effect
    // (CR 701.17d — the permission lands on the card in the graveyard). Lands
    // are excluded in both zones (CR 305.1) via
    // `play_from_exile_object_in_cast_path`.
    let has_object_tagged_play_permission = play_from_exile_object_in_cast_path(obj)
        && has_exile_cast_permission(state, obj, player, state.turn_number);
    let has_madness = obj.zone == Zone::Exile
        && matches!(variant_override, Some(CastingVariant::Madness))
        && obj.owner == player
        && obj
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Madness(_)));
    // CR 702.34 / CR 702.81 / CR 702.138 / CR 702.180: Cards in graveyard with
    // graveyard-cast keywords.
    let has_escape = obj.zone == Zone::Graveyard
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Escape,
        );
    let has_graveyard_cast_keyword =
        obj.zone == Zone::Graveyard && has_effective_graveyard_cast_keyword(state, object_id, obj);
    let has_mayhem = mayhem_castable_from_graveyard(state, player, object_id);
    // CR 601.2a + CR 117.1c: Graveyard cast via static permission (Lurrus, etc.).
    let graveyard_permission_src = if obj.zone == Zone::Graveyard && state.active_player == player {
        graveyard_permission_source(state, player, object_id)
    } else {
        None
    };
    let has_graveyard_permission = graveyard_permission_src.is_some();
    let has_graveyard_alt_cost = has_graveyard_timed_alt_cost_permission(state, obj, player);
    let has_hand_alt_cost = has_hand_alt_cost_permission(state, obj, player);
    // CR 608.2g: A free-cast window (Invoke Calamity) or targeted
    // during-resolution free-cast (Memory Plunder) may drive a cast on a card
    // still in its real origin zone. The runtime
    // `ExileWithAltCost { resolution_cleanup: Some(_) }` is the zone-agnostic
    // discriminator for that path; it must both authorize the cast and zero the
    // mana cost even when the card is neither in exile nor under a standing
    // graveyard alt-cost.
    let has_during_resolution_alt_cost =
        has_during_resolution_alt_cost_permission(state, obj, player);

    // CR 401.5 + CR 118.9 + CR 601.2a: Top-of-library cast via static permission
    // (Realmwalker, Future Sight, Bolas's Citadel, etc.). The card must be the
    // current top of `player`'s library AND match the static's `affected`
    // filter. The optional `alt_cost` flows through to `prepare_spell_cast`'s
    // alt-cost branch below, mirroring `ExileWithAltAbilityCost` semantics.
    let top_of_library_permission_src = if obj.zone == Zone::Library && obj.owner == player {
        top_of_library_permission_source(state, player, Some(CardPlayMode::Cast))
            .filter(|(top_id, _, _, _)| *top_id == object_id)
    } else {
        None
    };
    let has_top_of_library_permission = top_of_library_permission_src.is_some();

    // CR 601.2a + CR 611.2a: CastFromZone effects grant ExileWithAltCost on
    // opponent's cards. When the grant carries a `granted_to: Some(p)`
    // binding, only player `p` may consume it — see
    // `spell_objects_available_to_cast` for the parallel filter used at the
    // legal-actions surface.
    let has_unowned_exile_permission = obj.zone == Zone::Exile
        && obj.owner != player
        && has_alt_cost_permission_for(obj, state, player);
    let castable_zone = has_unowned_exile_permission
        || has_object_tagged_play_permission
        || has_during_resolution_alt_cost
        || (obj.owner == player
            && (obj.zone == Zone::Hand
                || (state.format_config.command_zone
                    && obj.zone == Zone::Command
                    && obj.is_commander)
                || (state.format_config.command_zone
                    && obj.zone == Zone::Command
                    && obj.is_signature_spell()
                    && oathbreaker_on_battlefield(state, player))
                || has_madness
                || has_graveyard_cast_keyword
                || has_mayhem
                || has_graveyard_permission
                || has_graveyard_alt_cost
                || has_top_of_library_permission));
    if !castable_zone {
        return Err(EngineError::InvalidAction(
            "Card is not in a castable zone".to_string(),
        ));
    }

    // CR 601.3 + CR 101.2 + CR 109.5: "Can't" beats "can" — check CantCastFrom statics.
    // Grafdigger's Cage: "Players can't cast spells from graveyards or libraries."
    // Drannith Magistrate: "Your opponents can't cast spells from anywhere other
    // than their hands." This overrides graveyard/library/exile/command casting
    // permissions (Escape, Lurrus, flashback, foretell, commander, etc.).
    if mode == CastingMode::Actual && is_blocked_from_casting_from_zone(state, obj, player) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents casting from this zone".to_string(),
        ));
    }

    // CR 101.2: Continuous casting prohibition — "can't" overrides "can".
    // E.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
    if mode == CastingMode::Actual && is_blocked_by_cant_cast_during(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents casting during this phase/turn".to_string(),
        ));
    }

    // CR 101.2: Temporary blanket prohibition — "can't cast spells this turn."
    // E.g., Silence: "Your opponents can't cast spells this turn."
    if mode == CastingMode::Actual && is_blocked_by_cant_cast_spells(state, player, Some(obj)) {
        return Err(EngineError::ActionNotAllowed(
            "A temporary effect prevents you from casting spells this turn".to_string(),
        ));
    }

    // CR 101.2: Blanket casting prohibition — "you can't cast [type] spells."
    // E.g., Steel Golem: "You can't cast creature spells."
    if mode == CastingMode::Actual && is_blocked_by_cant_be_cast(state, player, obj) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents you from casting this spell".to_string(),
        ));
    }

    if mode == CastingMode::Actual && is_blocked_by_cast_only_from_zones(state, obj, player) {
        return Err(EngineError::ActionNotAllowed(
            "A temporary effect prevents casting from this zone".to_string(),
        ));
    }

    if obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::ActionNotAllowed(
            "Lands are played, not cast".to_string(),
        ));
    }

    // CR 101.2 + CR 604.1: Per-turn casting limit — "can't cast more than N spells each turn."
    // E.g., Rule of Law, High Noon, Deafening Silence.
    if mode == CastingMode::Actual && is_blocked_by_per_turn_cast_limit(state, player, obj) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability limits the number of spells you can cast this turn".to_string(),
        ));
    }

    // Only Spell-kind abilities define the spell's on-cast effect and targets.
    // Activated abilities are irrelevant when casting the permanent spell.
    let ability_def = combined_spell_ability_def(obj);

    let flash_cost = restrictions::flash_timing_cost(state, player, obj);
    // ExileWithAltCost / ExileWithAltAbilityCost: override mana cost when
    // casting via an object-level alt-cost permission. The non-mana branch
    // (ExileWithAltAbilityCost) zeroes the mana cost — its `AbilityCost` is
    // routed through `pay_additional_cost` in `check_additional_cost_or_pay`
    // (CR 118.9 + CR 119.4).
    let alt_cost_from_exile = if obj.zone == Zone::Exile
        || has_graveyard_alt_cost
        || has_hand_alt_cost
        || has_during_resolution_alt_cost
    {
        // CR 611.2a: When a permission carries `granted_to: Some(p)`, only
        // player `p` may consume its cost override. Skip alt-cost permissions
        // bound to a different player so a non-grantee casting from the same
        // exiled card (theoretical — gated by `has_exile_cast_permission`
        // first) cannot accidentally inherit Jeleva's "without paying its mana
        // cost" cost-zero on cards exiled with Jeleva.
        obj.casting_permissions
            .iter()
            .find_map(|p| match p {
                crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. }
                    if exile_alt_cost_permission_supports_cast(state, obj, player, p, None) =>
                {
                    Some(resolve_exile_with_alt_cost_permission_mana_cost(cost, obj))
                }
                crate::types::ability::CastingPermission::Foretold { cost, .. } => {
                    Some(cost.clone())
                }
                crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. }
                    if exile_alt_cost_permission_supports_cast(state, obj, player, p, None) =>
                {
                    Some(crate::types::mana::ManaCost::zero())
                }
                _ => None,
            })
            .or_else(|| {
                // CR 118.9: Valgavoth, Terror Eater — an `ExileCastPermission`
                // static carrying an ALTERNATIVE extra-cost (pay life equal to
                // mana value) zeroes the spell's mana cost; the `AbilityCost`
                // body is paid by `check_additional_cost_or_pay`'s exile branch.
                // ADDITIONAL extra-costs (Dawnhand) leave the mana cost intact.
                //
                // CR 601.2a: Bind to the source the cast will commit to as its
                // `CastingVariant::ExilePermission` — the explicit override when
                // present, else the same first-match scan that stamps the offered
                // variant. This keeps the zeroing decision keyed to the elected
                // permission so a second active permission for the same exiled
                // spell can never substitute its cost treatment.
                let elected_source =
                    elected_exile_permission_source(state, player, object_id, variant_override)?;
                exile_static_permission_extra_cost(state, player, object_id, elected_source)
                    .and_then(|extra| {
                        matches!(extra.mode, crate::types::statics::CastCostMode::Alternative)
                            .then(crate::types::mana::ManaCost::zero)
                    })
            })
    } else if obj.zone == Zone::Library
        && top_of_library_permission_src
            .as_ref()
            .is_some_and(|(_, _, _, alt)| alt.is_some())
    {
        // CR 401.5 + CR 118.9: Bolas's Citadel — alt-cost rider on the static
        // grant zeros the spell's mana cost; the `AbilityCost` body is paid
        // by `check_additional_cost_or_pay`'s top-of-library branch.
        Some(crate::types::mana::ManaCost::zero())
    } else {
        None
    };

    // CR 107.14: ExileWithEnergyCost — zero mana cost, energy paid as additional cost.
    let energy_cost_from_exile = if obj.zone == Zone::Exile {
        obj.casting_permissions.iter().any(|p| {
            matches!(
                p,
                crate::types::ability::CastingPermission::ExileWithEnergyCost
            )
        })
    } else {
        false
    };

    // Warp: when casting from hand with Keyword::Warp, use the warp mana cost.
    let warp_cost = if obj.zone == Zone::Hand {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Warp(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };

    // CR 702.109a: Dash — when casting from hand with Keyword::Dash, the dash
    // mana cost replaces the printed cost (opt-in via `variant_override`). Read
    // the *effective* spell keywords so a Dash cost granted by a static
    // (CR 604.1) is honored, not just printed Dash.
    let dash_cost = if obj.zone == Zone::Hand {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Dash(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };

    // CR 702.152a: Blitz — when casting from hand with Keyword::Blitz, the blitz
    // mana cost replaces the printed cost (opt-in via `variant_override`). Read
    // the *effective* spell keywords so a Blitz cost granted by a static
    // (CR 604.1) is honored; CR 702.152b makes Blitz single-instance, so the
    // dedup-by-kind collector is correct.
    let blitz_cost = if obj.zone == Zone::Hand {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Blitz(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };

    // CR 702.137a: Spectacle — when casting from hand with Keyword::Spectacle, the
    // spectacle mana cost replaces the printed cost (opt-in via `variant_override`,
    // gated on an opponent having lost life this turn at offer time). Read the
    // *effective* spell keywords so a Spectacle cost granted by a static
    // (CR 604.1) is honored, not just printed Spectacle.
    let spectacle_cost = if obj.zone == Zone::Hand {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Spectacle(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };

    // CR 702.138: Escape — use escape mana cost when casting from graveyard.
    let escape_cost = if has_escape {
        super::keywords::effective_escape_data(state, object_id).map(|(cost, _)| cost)
    } else {
        None
    };

    // CR 702.180a: Harmonize — use the harmonize mana cost when casting from
    // graveyard. Off-zone-aware and `SelfManaCost`-resolving so a granted
    // harmonize whose cost equals the card's mana cost (Songcrafter Mage) is paid
    // correctly. Tap cost reduction is handled in
    // casting_costs::pay_and_push_adventure.
    let harmonize_cost = if obj.zone == Zone::Graveyard {
        super::keywords::effective_harmonize_cost(state, object_id)
    } else {
        None
    };

    // CR 702.34a: Flashback — use flashback cost when casting from graveyard.
    let flashback_cost = if obj.zone == Zone::Graveyard {
        super::keywords::effective_flashback_cost(state, object_id)
    } else {
        None
    };

    // CR 702.146a: Disturb — use disturb cost when casting from graveyard.
    let disturb_cost = if obj.zone == Zone::Graveyard {
        super::keywords::effective_disturb_cost(state, object_id)
    } else {
        None
    };

    // CR 702.187b: Mayhem — use the mayhem mana cost when casting from graveyard,
    // but only while the card was discarded this turn. The cost may be granted to
    // graveyard cards by a static (Green Goblin), so use the off-zone-aware lookup.
    let mayhem_cost = if obj.zone == Zone::Graveyard && was_discarded_this_turn(state, object_id) {
        super::keywords::effective_mayhem_cost(state, object_id)
    } else {
        None
    };

    // CR 702.190a: Sneak alt-cost when casting from HAND. The
    // `effective_sneak_cost` lookup goes through `effective_keyword_for_object`
    // so off-zone keyword grants (e.g., statics that grant Sneak to cards in
    // your hand) are visible. Sneak is NOT auto-selected as the active
    // `casting_variant` — it is opted into explicitly by
    // `handle_cast_spell_as_sneak` via `variant_override`, which enforces
    // declare-blockers timing (CR 702.190a), returns the unblocked attacker
    // as cost payment, and — for permanent spells only (CR 702.190b) —
    // places the permanent tapped+attacking on resolution.
    let sneak_cost = if obj.zone == Zone::Hand {
        super::keywords::effective_sneak_cost(state, object_id)
    } else {
        None
    };
    let web_slinging_cost = if obj.zone == Zone::Hand {
        super::keywords::effective_web_slinging_cost(state, player, object_id)
    } else {
        None
    };

    // CR 702.34a + CR 118.8 + CR 601.2f: Split flashback into mana vs non-mana
    // components for the payment pipeline. Compound flashback costs
    // ("Flashback—{1}{U}, Pay 3 life") are stored as
    // `FlashbackCost::NonMana(AbilityCost::Composite([Mana, ...]))`; we extract
    // the mana sub-cost so the spell pays its mana through the normal mana-payment
    // flow while the residual non-mana sub-costs are routed through
    // `pay_additional_cost`. Mirrors `extract_x_mana_cost` (casting_costs.rs).
    let (flashback_mana_cost, flashback_non_mana_cost) =
        split_flashback_cost_components(flashback_cost.as_ref());

    // Precedence: Escape > Retrace > Harmonize > Mayhem > Flashback > Aftermath >
    // Disturb > Jump-start > GraveyardPermission > Warp > Normal.
    // No standard card has multiple graveyard-cast keywords; if one did, the card's own
    // keyword overrides an external source's grant (GraveyardPermission).
    //
    // CR 702.190a: Sneak is not auto-selected from the keyword-presence chain —
    // it is opted into explicitly via `variant_override` by the
    // `handle_cast_spell_as_sneak` entry point. This preserves Sneak's
    // permission-aware eligibility (the HasKeywordKind filter on the granting
    // rider) while keeping the default cast path for GY creatures under
    // GraveyardCastPermission unchanged.
    // CR 702.62a: Suspend free-cast detection — when casting an exile-zone card
    // that has `Keyword::Suspend` AND an `ExileWithAltCost` permission (granted
    // by the synthesized last-counter trigger via `Effect::CastFromZone`), the
    // cast is the suspend "play it without paying its mana cost" path. Mirrors
    // Warp/Flashback's keyword-presence detection and avoids coupling
    // `Effect::CastFromZone` to a cast-variant override field.
    // CR 702.62a: Suspend cast detection. Reads the effective off-zone keyword
    // set so Suspend granted at runtime by Jhoira of the Ghitu / The Tenth Doctor
    // (CR 604.1) is recognized alongside printed Suspend.
    let is_suspend_cast = obj.zone == Zone::Exile
        && alt_cost_from_exile.is_some()
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Suspend,
        );

    // CR 702.170d: Plot free-cast detection — when casting an exile-zone card
    // with a `CastingPermission::Plotted { turn_plotted }` (on a later turn
    // than it was plotted), the cast is the plot "without paying its mana
    // cost" path. Mirrors `is_suspend_cast` — permission-keyed, no separate
    // keyword-presence check (Plot is a hand-zone activated ability; once the
    // card is in exile with the Plotted permission, the keyword's job is done).
    let is_plot_cast = obj.zone == Zone::Exile
        && obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, crate::types::ability::CastingPermission::Plotted { .. }));
    let is_foretell_cast = obj.zone == Zone::Exile
        && obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, crate::types::ability::CastingPermission::Foretold { .. }));

    let casting_variant = variant_override.unwrap_or_else(|| {
        if is_suspend_cast {
            CastingVariant::Suspend
        } else if is_plot_cast {
            CastingVariant::Plot
        } else if is_foretell_cast {
            CastingVariant::Foretell
        } else if escape_cost.is_some() {
            CastingVariant::Escape
        } else if has_retrace_keyword(state, object_id) && obj.zone == Zone::Graveyard {
            CastingVariant::Retrace
        } else if harmonize_cost.is_some() {
            CastingVariant::Harmonize
        } else if has_mayhem {
            CastingVariant::Mayhem
        } else if flashback_cost.is_some() {
            CastingVariant::Flashback
        } else if obj.zone == Zone::Graveyard
            && super::keywords::object_has_effective_keyword_kind(
                state,
                object_id,
                KeywordKind::Aftermath,
            )
        {
            CastingVariant::Aftermath
        } else if jumpstart_castable_from_graveyard(state, object_id) {
            CastingVariant::JumpStart
        } else if disturb_cost.is_some() {
            CastingVariant::Disturb
        } else if let Some(source) = graveyard_permission_src {
            // CR 110.4: For OncePerTurnPerPermanentType permissions, auto-pick
            // the slot when only one is available. When multiple slots are
            // available (multi-type card), leave `None` — the engine will
            // prompt the player to choose via `ChoosePermanentTypeSlot`.
            let slot_type = if source.frequency == CastFrequency::OncePerTurnPerPermanentType {
                let slots = available_permanent_type_slots(state, source.source_id, object_id);
                if slots.len() == 1 {
                    Some(slots[0])
                } else {
                    None
                }
            } else {
                None
            };
            CastingVariant::GraveyardPermission {
                source: source.source_id,
                frequency: source.frequency,
                slot_type,
                graveyard_destination_replacement: source.graveyard_destination_replacement,
            }
        } else if warp_cost.is_some() {
            CastingVariant::Warp
        } else {
            CastingVariant::Normal
        }
    });
    // CR 702.96a + CR 604.1: read the overload cost from effective keywords so a
    // granted Overload (CastWithKeyword) substitutes its cost, mirroring the
    // Evoke/Emerge effective-keyword cost reads below.
    let overload_cost = if casting_variant == CastingVariant::Overload {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Overload(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };
    // CR 702.162a: When the caller explicitly opted into More Than Meets the Eye
    // (via `variant_override = Some(CastingVariant::MoreThanMeetsTheEye)`),
    // substitute the alternative mana cost taken from the hand object's
    // `Keyword::MoreThanMeetsTheEye(cost)` payload. Mirrors the Overload pattern.
    let mtmte_cost = if casting_variant == CastingVariant::MoreThanMeetsTheEye {
        obj.keywords
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::MoreThanMeetsTheEye(cost) => Some(cost.clone()),
                _ => None,
            })
            .or_else(|| {
                obj.back_face.as_ref().and_then(|front_face| {
                    front_face.keywords.iter().find_map(|k| match k {
                        crate::types::keywords::Keyword::MoreThanMeetsTheEye(cost) => {
                            Some(cost.clone())
                        }
                        _ => None,
                    })
                })
            })
    } else {
        None
    };
    // CR 702.74a + CR 601.2f-h: When the caller explicitly opted into Evoke
    // (via `variant_override = Some(CastingVariant::Evoke)`), substitute the
    // evoke mana sub-cost taken from the hand object's `Keyword::Evoke(cost)`
    // payload. Non-mana evoke (Solitude et al.) has no mana sub-cost — the
    // mana component substitutes to `ManaCost::zero()` and the residual
    // non-mana cost is paid via the additional-cost path (CR 601.2h).
    let (evoke_cost, evoke_non_mana_cost) = if casting_variant == CastingVariant::Evoke {
        // CR 702.74a + CR 601.2f-h + CR 604.1: read evoke cost from effective
        // keywords so granted evoke (CastWithKeyword) substitutes its cost, not
        // just printed evoke.
        let effective_kws = effective_spell_keywords(state, player, object_id);
        let split = effective_kws.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Evoke(cost) => Some(split_evoke_cost_components(cost)),
            _ => None,
        });
        match split {
            Some((mana, non_mana)) => (mana, non_mana),
            None => (None, None),
        }
    } else {
        (None, None)
    };
    // CR 702.119a: When the caller explicitly opted into Emerge (via
    // `variant_override = Some(CastingVariant::Emerge)`), substitute the emerge
    // mana cost from the spell's effective `Keyword::Emerge(cost)`. The required
    // sacrifice and mana-value reduction are paid later as a cost component
    // (CR 702.119c, CR 601.2h).
    let emerge_cost = if casting_variant == CastingVariant::Emerge {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Emerge(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };
    // CR 702.103a + CR 118.9: When the caller explicitly opted into Bestow (via
    // `variant_override = Some(CastingVariant::Bestow)`), substitute the bestow
    // mana sub-cost taken from the object's `Keyword::Bestow(cost)` payload.
    // Mirrors the Evoke cost-selection split: a compound bestow cost
    // ("Bestow—{R}, Collect evidence 6." on Detective's Phoenix) has its mana
    // sub-cost substituted here and the residual non-mana sub-cost (Collect
    // evidence) paid via the additional-cost path (CR 601.2h). Read from
    // effective keywords so a graveyard-cast bestow (where the keyword may be
    // granted) resolves the same as a printed-keyword hand bestow.
    // The type-changing mutation (CR 702.103b: gain Aura subtype, gain `enchant
    // creature`, lose Creature type) is applied separately by
    // `handle_bestow_cost_choice` because it requires a `&mut GameState` handle
    // and needs to outlive `prepare_spell_cast_with_variant_override` (which
    // holds an immutable borrow).
    let (bestow_cost, bestow_non_mana_cost) = if casting_variant == CastingVariant::Bestow {
        let split = effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Bestow(cost) => {
                    Some(split_bestow_cost_components(cost))
                }
                _ => None,
            });
        match split {
            Some((mana, non_mana)) => (mana, non_mana),
            None => (None, None),
        }
    } else {
        (None, None)
    };
    // CR 702.140a: When the caller explicitly opted into Mutate (via
    // `variant_override = Some(CastingVariant::Mutate)`), substitute the mutate
    // mana cost taken from the hand object's `Keyword::Mutate(cost)` payload.
    // Mirrors the Bestow cost-selection pattern. The target requirement (a
    // non-Human creature you own, CR 702.140a) is attached separately in
    // `continue_with_prepared` because it needs a `&mut GameState` handle.
    let mutate_cost = if casting_variant == CastingVariant::Mutate {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Mutate(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.113a + CR 118.9: When the caller explicitly opted into Awaken (via
    // `variant_override = Some(CastingVariant::Awaken)`), read the
    // `Keyword::Awaken { count, cost }` payload from the hand object. `cost`
    // substitutes the printed mana cost (mirrors Overload / Bestow); `count` is
    // the number of +1/+1 counters the resolution rider places (CR 702.113a).
    // This is the sole awaken-cost substitution site; the standard resolver pays
    // the substituted cost and no call site inspects the awaken cost.
    let awaken_payload = if casting_variant == CastingVariant::Awaken {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Awaken { count, cost } => Some((*count, cost.clone())),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.148a + CR 118.9: When the caller explicitly opted into Cleave (via
    // `variant_override = Some(CastingVariant::Cleave)`), substitute the cleave
    // mana cost taken from the hand object's `Keyword::Cleave(cost)` payload.
    // Mirrors the Evoke / Overload / Bestow cost-selection pattern. The
    // text-changing effect (CR 702.148b → CR 612: remove bracketed text) is
    // applied separately by `handle_cleave_cost_choice` because it requires a
    // `&mut GameState` handle and must outlive this immutable-borrow function.
    let cleave_cost = if casting_variant == CastingVariant::Cleave {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Cleave(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.176a: When the caller explicitly opted into Impending (via
    // `variant_override = Some(CastingVariant::Impending)`), substitute the
    // impending mana cost taken from `Keyword::Impending { cost, .. }`.
    // Mirrors Overload / Bestow / Cleave / Awaken cost substitution.
    let impending_cost = if casting_variant == CastingVariant::Impending {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Impending { cost, .. } => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.160a: When the caller explicitly opted into Prototype (via
    // `variant_override = Some(CastingVariant::Prototype)`), substitute the
    // prototype mana cost carried by the keyword payload.
    let prototype_cost = if casting_variant == CastingVariant::Prototype {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Prototype { cost, .. } => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    let awaken_cost = awaken_payload.as_ref().map(|(_, cost)| cost.clone());
    // CR 601.2f + CR 118.9a: One-shot "the next spell … without paying its mana cost".
    let next_spell_without_paying = !casting_variant.uses_alternative_cost()
        && pending_next_spell_modifier_index(state, player, object_id, |modifier| {
            matches!(modifier, NextSpellModifier::WithoutPayingManaCost)
        })
        .is_some();

    // CR 601.2b + CR 118.9a: CastFromHandFree — static permission grants free
    // casting from the origin scope carried by the static. Auto-application is
    // restricted to `Unlimited` sources (Omniscience, Tamiyo emblem,
    // Dracogenesis); `OncePerTurn` sources (Zaffai) must be opted into
    // explicitly via a dedicated action to preserve the player's "may cast"
    // choice and make per-turn slot consumption visible at the action layer.
    let hand_cast_free = unlimited_hand_cast_free_applies(state, player, obj, casting_variant);

    // CR 118.9: Energy replaces mana cost entirely when casting with ExileWithEnergyCost.
    // CR 702.34a: Non-mana flashback costs use NoCost for mana (cost is paid separately).
    // CR 702.190a: sneak_cost only applies when the caster actually elected
    // the Sneak path (variant_override == Some(Sneak{..})). Otherwise a GY
    // creature with Sneak available plus another permission (e.g. Lurrus)
    // would erroneously use the Sneak cost for a non-Sneak cast.
    let effective_sneak_cost_for_path = if matches!(casting_variant, CastingVariant::Sneak { .. }) {
        sneak_cost
    } else {
        None
    };
    let effective_web_slinging_cost_for_path =
        if matches!(casting_variant, CastingVariant::WebSlinging { .. }) {
            web_slinging_cost
        } else {
            None
        };
    // CR 601.2b: HandPermission variant (A2 opt-in path for Zaffai) also pays
    // no mana cost — the granting static replaces the mana cost with nothing.
    let is_hand_permission_variant =
        matches!(casting_variant, CastingVariant::HandPermission { .. });
    // CR 113.6d + CR 118.9a + CR 601.2b: Whether the cast pays no mana cost is
    // decided by the ELECTED `ExileCastPermission`'s own cost shape — a cost-
    // modifying ability functions on the stack (CR 113.6d), only one alternative
    // cost applies (CR 118.9a), and the previously made choice of which
    // permission to cast through restricts the cost (CR 601.2b). The variant
    // carries the elected `source` (not the cost shape), so the static stays the
    // authority: read THAT source's `ExileCastCost` via the elected-source-aware
    // lookup, never a first-match battlefield scan that a second active
    // permission could substitute its shape into. With two functioning
    // permissions for the same exiled spell (one `WithoutPayingManaCost`, one
    // pay-normal), a first-match scan could free-cast the wrong source. Fail
    // closed (not-free) when the elected source no longer functions:
    // `exile_cast_permission_source_full(..., Some(source))` returns `None` (its
    // `find()` guard rejects a mismatched/dead elected source).
    let is_exile_permission_free_cast =
        if let CastingVariant::ExilePermission { source, .. } = &casting_variant {
            exile_cast_permission_source_full(state, player, object_id, Some(*source))
                .is_some_and(|src| matches!(src.cost, ExileCastCost::WithoutPayingManaCost))
        } else {
            false
        };
    // CR 118.9a: ExileWithAltCost { zero } / Discover / Suspend payoff — treat as
    // `NoCost` so the mana-payment phase is skipped identically to hand-free paths.
    let exile_alt_cost_free = alt_cost_from_exile
        .as_ref()
        .is_some_and(ManaCost::is_without_paying_mana);
    // CR 702.94a: Miracle alternative cost — pulled from `Keyword::Miracle(cost)`
    // on the hand object. Only honored when the caller explicitly opted into the
    // Miracle variant via the reveal prompt.
    let miracle_cost = if casting_variant == CastingVariant::Miracle {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Miracle(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    let madness_cost = if casting_variant == CastingVariant::Madness {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Madness(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.173a: Freerunning alternative cost — pulled from
    // `Keyword::Freerunning(cost)` on the hand object (or from
    // `effective_spell_keywords` when the keyword was granted via a
    // `CastWithKeyword` static, mirroring how `effective_spell_keywords` is
    // consulted at candidate enumeration). Only honored when the caller
    // explicitly opted into the Freerunning variant via the
    // `CastingVariantChoice` prompt.
    let freerunning_cost = if casting_variant == CastingVariant::Freerunning {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Freerunning(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };
    // CR 702.76a: When the caller opted into Prowl, substitute the prowl mana cost
    // from the `Keyword::Prowl(cost)` payload (printed or granted). Mirrors the
    // Freerunning/Overload cost-selection pattern.
    let prowl_cost = if casting_variant == CastingVariant::Prowl {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Prowl(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };
    // CR 702.117a: When the caller opted into Surge, substitute the surge mana
    // cost from the `Keyword::Surge(cost)` payload (printed or granted). Mirrors
    // the Freerunning/Prowl cost-selection pattern.
    let surge_cost = if casting_variant == CastingVariant::Surge {
        effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Surge(cost) => Some(cost.clone()),
                _ => None,
            })
    } else {
        None
    };
    // CR 702.34a: When the flashback cost is purely non-mana (e.g. Battle Screech's
    // "tap three white creatures"), the spell pays no mana through the normal flow.
    // For compound flashback costs ("{1}{U}, Pay 3 life") we still want the mana
    // sub-cost paid normally — `flashback_mana_cost` is `Some` in that case and is
    // selected by the `else` branch below.
    let pure_non_mana_flashback = casting_variant == CastingVariant::Flashback
        && flashback_non_mana_cost.is_some()
        && flashback_mana_cost.is_none();
    // CR 702.74a + CR 601.2f-h: Mirror of `pure_non_mana_flashback` for
    // Evoke. The MH2 Incarnations (Solitude et al.) have pure non-mana evoke
    // costs ("Exile a white card from your hand"); zero the mana cost so the
    // mana-payment phase pays nothing and the residual is routed through the
    // additional-cost path below.
    let pure_non_mana_evoke = casting_variant == CastingVariant::Evoke
        && evoke_non_mana_cost.is_some()
        && evoke_cost.is_none();
    // CR 702.103a + CR 601.2h: Mirror of `pure_non_mana_evoke` for Bestow. A
    // bestow card whose entire bestow cost is non-mana would zero the mana cost
    // so the residual is routed through the additional-cost path. Detective's
    // Phoenix pairs {R} with Collect evidence, so `bestow_cost` is `Some` and
    // this stays `false`; the axis is kept symmetric with the other compound
    // alternative costs for forward compatibility.
    let pure_non_mana_bestow = casting_variant == CastingVariant::Bestow
        && bestow_non_mana_cost.is_some()
        && bestow_cost.is_none();
    // CR 702.170d: Plot casts are always free — the Plotted permission encodes
    // "without paying its mana cost". Zero the mana cost at preparation time,
    // mirroring the hand-free / flashback-non-mana paths above.
    let effective_warp_cost_for_path = if casting_variant == CastingVariant::Warp {
        warp_cost
    } else {
        None
    };
    // CR 702.109a: substitute the dash mana cost only on the dash path (opt-in).
    let effective_dash_cost_for_path = if casting_variant == CastingVariant::Dash {
        dash_cost
    } else {
        None
    };
    // CR 702.152a: substitute the blitz mana cost only on the blitz path (opt-in).
    let effective_blitz_cost_for_path = if casting_variant == CastingVariant::Blitz {
        blitz_cost
    } else {
        None
    };
    // CR 702.137a: substitute the spectacle mana cost only on the spectacle path.
    let effective_spectacle_cost_for_path = if casting_variant == CastingVariant::Spectacle {
        spectacle_cost
    } else {
        None
    };
    let effective_escape_cost_for_path = if casting_variant == CastingVariant::Escape {
        escape_cost
    } else {
        None
    };
    let effective_harmonize_cost_for_path = if casting_variant == CastingVariant::Harmonize {
        harmonize_cost
    } else {
        None
    };
    let effective_mayhem_cost_for_path = if casting_variant == CastingVariant::Mayhem {
        mayhem_cost
    } else {
        None
    };
    let effective_flashback_mana_cost_for_path = if casting_variant == CastingVariant::Flashback {
        flashback_mana_cost
    } else {
        None
    };
    let effective_disturb_cost_for_path = if casting_variant == CastingVariant::Disturb {
        disturb_cost
    } else {
        None
    };
    let mut mana_cost = if energy_cost_from_exile
        || hand_cast_free
        || next_spell_without_paying
        || is_hand_permission_variant
        || is_exile_permission_free_cast
        || exile_alt_cost_free
        || pure_non_mana_flashback
        || pure_non_mana_evoke
        || pure_non_mana_bestow
        || casting_variant == CastingVariant::Plot
    {
        crate::types::mana::ManaCost::NoCost
    } else {
        miracle_cost
            .or(madness_cost)
            .or(evoke_cost)
            .or(emerge_cost)
            .or(overload_cost)
            .or(mtmte_cost)
            .or(bestow_cost)
            .or(mutate_cost)
            .or(awaken_cost)
            .or(cleave_cost)
            .or(impending_cost)
            .or(prototype_cost)
            .or(effective_escape_cost_for_path)
            .or(effective_harmonize_cost_for_path)
            .or(effective_mayhem_cost_for_path)
            .or(effective_flashback_mana_cost_for_path)
            .or(effective_disturb_cost_for_path)
            .or(effective_sneak_cost_for_path)
            .or(effective_web_slinging_cost_for_path)
            .or(alt_cost_from_exile)
            .or(effective_warp_cost_for_path)
            .or(effective_dash_cost_for_path)
            .or(effective_blitz_cost_for_path)
            .or(effective_spectacle_cost_for_path)
            .or(freerunning_cost)
            .or(prowl_cost)
            .or(surge_cost)
            .unwrap_or_else(|| obj.mana_cost.clone())
    };
    // CR 601.3b + CR 702.8a: A spell has effective flash from its own keywords
    // OR from a battlefield `StaticMode::ExileCastPermission` static granting
    // "you may cast them as though they had flash" (Azula, Cunning Usurper) for
    // the cards in its exile pool.
    let has_granted_flash = effective_spell_keyword_kinds(state, player, object_id)
        .contains(&KeywordKind::Flash)
        || exile_static_permission_grants_flash(state, player, object_id);
    let cast_outside_sorcery_timing = !restrictions::is_sorcery_speed_window(state, player);
    // CR 304.1: Instants can be cast any time a player has priority.
    // CR 301.1 / CR 306.1: Artifacts and planeswalkers are cast at sorcery speed.
    let mut cast_timing_permission = None;
    if mode == CastingMode::Actual {
        if let Err(base_timing_error) = restrictions::check_spell_timing(
            state,
            player,
            obj,
            ability_def.as_ref(),
            has_granted_flash,
            casting_variant,
        ) {
            // CR 702.8a: Flash permits instant-speed casting.
            if let Some(flash_cost) = flash_cost {
                restrictions::check_spell_timing(
                    state,
                    player,
                    obj,
                    ability_def.as_ref(),
                    true,
                    casting_variant,
                )?;
                mana_cost = restrictions::add_mana_cost(&mana_cost, &flash_cost);
                if cast_outside_sorcery_timing {
                    cast_timing_permission = Some(CastTimingPermission::AsThoughHadFlash);
                }
            } else if casting_costs::payable_spell_alternative_cost_for_timing(
                state,
                player,
                object_id,
                CastTimingPermission::AsThoughHadFlash,
            )
            .is_some()
            {
                // CR 118.9 + CR 702.8a: Some alternative-cost grants also
                // permit the spell to be cast as though it had flash, but only
                // when the spell is cast using that alternative cost.
                restrictions::check_spell_timing(
                    state,
                    player,
                    obj,
                    ability_def.as_ref(),
                    true,
                    casting_variant,
                )?;
                if cast_outside_sorcery_timing {
                    cast_timing_permission = Some(CastTimingPermission::AsThoughHadFlash);
                }
            } else if casting_costs::can_pay_offering_additional_cost(state, player, object_id) {
                // CR 702.48a: "[Quality] offering" — if the controller has a legal
                // sacrifice target, the spell may be cast at instant speed.
                // `CastTimingPermission::Offering` signals that the upcoming sacrifice
                // prompt is required (not optional) because the player used Offering
                // to unlock instant-speed timing.
                restrictions::check_spell_timing(
                    state,
                    player,
                    obj,
                    ability_def.as_ref(),
                    true,
                    casting_variant,
                )?;
                if cast_outside_sorcery_timing {
                    cast_timing_permission = Some(CastTimingPermission::Offering);
                }
            } else {
                return Err(base_timing_error);
            }
        } else if cast_outside_sorcery_timing && has_granted_flash {
            cast_timing_permission = Some(CastTimingPermission::AsThoughHadFlash);
        }
        restrictions::check_casting_restrictions(
            state,
            player,
            object_id,
            &obj.casting_restrictions,
        )?;

        if state.format_config.command_zone
            && !super::commander::can_cast_in_color_identity(
                state,
                &obj.color,
                &obj.mana_cost,
                player,
            )
        {
            return Err(EngineError::ActionNotAllowed(
                "Card is outside commander's color identity".to_string(),
            ));
        }
    }

    // CR 408.3 + CR 903.8: Commanders cast from the command zone incur a tax.
    if obj.zone == Zone::Command {
        let tax = super::commander::commander_tax(state, object_id);
        if tax > 0 {
            match &mut mana_cost {
                crate::types::mana::ManaCost::Cost { generic, .. } => {
                    *generic += tax;
                }
                crate::types::mana::ManaCost::NoCost => {
                    mana_cost = crate::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: tax,
                    };
                }
                crate::types::mana::ManaCost::SelfManaCost
                | crate::types::mana::ManaCost::SelfManaValue => {
                    // Self-referential placeholders should have been resolved before
                    // reaching here; treat as no-op for commander tax purposes.
                }
            }
        }
    }

    // CR 702.102c: The total cost of a fused split spell includes the mana cost
    // of each half. The front face's cost is already in `mana_cost`; add the
    // right (Split) half's cost so the combined printed cost becomes the base
    // that cost reductions/increases (CR 601.2f) then apply to.
    if matches!(casting_variant, CastingVariant::Fuse) {
        if let Some(back) = obj
            .back_face
            .as_ref()
            .filter(|bf| bf.layout_kind == Some(LayoutKind::Split))
        {
            mana_cost = restrictions::add_mana_cost(&mana_cost, &back.mana_cost);
        }
    }

    // CR 601.2f: Capture the tax-inclusive base BEFORE any cost reductions /
    // increases or {X} concretization. Threaded onto `PendingCast.base_cost` so
    // the full concrete cost can be recomputed from scratch for any chosen X with
    // floors applied LAST (`concrete_cost_for_x`).
    let base_mana_cost = mana_cost.clone();

    // CR 601.2f: Apply every cost modifier (self-spell statics, battlefield statics,
    // affinity, one-shot reductions, cost floor) in CR-correct order.
    apply_all_cost_modifiers(
        state,
        player,
        object_id,
        &mut mana_cost,
        Some(casting_variant),
    );

    // CR 702.96b-c: When casting with Overload, transform the spell's ability
    // tree so every target-bearing effect is promoted to its all-matching
    // counterpart (Destroy→DestroyAll, Pump→PumpAll, DealDamage→DamageAll,
    // Tap→TapAll, Bounce→ChangeZoneAll). The transformed effects carry no
    // TargetRef slots, so target selection is naturally skipped (CR 702.96c).
    let mut ability_def = ability_def;
    if casting_variant == CastingVariant::Overload {
        if let Some(def) = ability_def.as_mut() {
            super::effects::overload::transform_ability_def(def);
        }
    }

    // CR 702.113a: When casting with Awaken, append the awaken rider to the tail
    // of the spell's ability tree so the printed effect resolves first, then "put
    // N +1/+1 counters on target land you control; that land becomes a 0/0
    // Elemental creature with haste; it's still a land." The land target only
    // exists on the awaken variant (CR 702.113b) — a normal cast leaves the
    // ability tree untouched and requests no land target.
    if casting_variant == CastingVariant::Awaken {
        if let (Some(def), Some((count, _))) = (ability_def.as_mut(), awaken_payload.as_ref()) {
            super::effects::awaken::append_awaken_rider(def, *count);
        }
    }

    // CR 702.102d: As a fused split spell resolves, the controller follows the
    // instructions of the left half (this object's spell ability) and then the
    // right half (the Split back face's spell ability). Build the right half's
    // combined ability and append it to the tail of the left half's sub-chain so
    // resolution walks left → right in order.
    //
    // CR 601.2c: Both halves' targets are chosen at cast time in a single pass —
    // `build_target_slots` / `collect_target_slots` recurse the sub_ability chain
    // and `assign_targets_in_chain` distributes the chosen targets back across the
    // whole chain (left slots first, then right). No separate right-half
    // targeting phase or pending-cast side storage is required; the merged
    // ability chain is the single authority for target slots.
    if casting_variant == CastingVariant::Fuse {
        if let Some(back) = obj
            .back_face
            .as_ref()
            .filter(|bf| bf.layout_kind == Some(LayoutKind::Split))
        {
            let mut right_abilities = back
                .abilities
                .iter()
                .filter(|a| a.kind == AbilityKind::Spell);
            if let Some(first_right) = right_abilities.next() {
                let mut right = first_right.clone();
                for extra in right_abilities {
                    append_to_ability_def_sub_chain(&mut right, extra.clone());
                }
                match ability_def.as_mut() {
                    Some(def) => append_to_ability_def_sub_chain(def, right),
                    // Left half had no spell-level effect (rare for split cards);
                    // the right half alone becomes the spell's ability.
                    None => ability_def = Some(right),
                }
            }
        }
    }

    let origin_zone = obj.zone;
    Ok(PreparedSpellCast {
        object_id,
        card_id: obj.card_id,
        ability_def,
        mana_cost,
        base_mana_cost,
        modal: obj.modal.clone(),
        casting_variant,
        cast_timing_permission,
        origin_zone,
        payment_mode: CastPaymentMode::Auto,
    })
}

/// CR 601.2f: Apply every NON-FLOOR cost modifier to `mana_cost` in CR-correct
/// order: self-spell statics → battlefield statics → affinity → undaunted →
/// one-shot pending reductions. Floors (Trinisphere class) are deliberately
/// excluded so callers can run them LAST against a concrete cost. Every pass
/// reads `&GameState` only and is idempotent against a fresh base cost.
fn apply_non_floor_cost_modifiers(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    mana_cost: &mut ManaCost,
    casting_variant: Option<CastingVariant>,
) {
    // CR 601.2f: A spell cast via a `PlayFromExile` grant may carry a printed
    // cost increase ("Each spell cast this way costs {N} more to cast." —
    // Lightstall Inquisitor). Apply it FIRST, as an increase, so a later
    // reduction cannot be applied to the pre-raise cost — CR 601.2f determines
    // the total as base + increases − reductions, and a reduction can never take
    // the mana component below {0}.
    if let Some(obj) = state.objects.get(&object_id) {
        if let Some(raise) = exile_play_cast_cost_raise(obj, player) {
            *mana_cost = super::restrictions::add_mana_cost(mana_cost, &raise);
        }
    }
    // CR 601.2f: collect self-spell statics ("This spell costs
    // {N} less ...") and battlefield statics together so all increases apply
    // before any reductions across both passes.
    let mut collected =
        collect_self_spell_cost_modifiers(state, player, object_id, None, false, casting_variant);
    collected.extend(collect_battlefield_cost_modifiers(
        state, player, object_id, None, false,
    ));
    apply_cost_modifications_in_order(mana_cost, &collected);
    // CR 702.41a: Affinity — reduce cost by {1} per matching permanent controlled.
    apply_affinity_reduction(state, player, object_id, mana_cost);
    // CR 702.125a: Undaunted — reduce cost by {1} per living opponent you have.
    apply_undaunted_reduction(state, player, object_id, mana_cost);
    // CR 601.2f: One-shot pending cost reductions ("the next spell costs {N} less").
    apply_pending_spell_cost_reductions(state, player, object_id, mana_cost);
}

/// CR 601.2f: Apply every cost modifier to `mana_cost` in CR-correct order:
/// self-spell statics → battlefield statics → affinity → undaunted → one-shot
/// pending reductions → cost floor (Trinisphere, applied last). Every pass reads
/// `&GameState` only and is idempotent against a fresh base cost, so this
/// helper can be re-run after an additional cost (Bargain) is declared.
pub(super) fn apply_all_cost_modifiers(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    mana_cost: &mut ManaCost,
    casting_variant: Option<CastingVariant>,
) {
    apply_non_floor_cost_modifiers(state, player, object_id, mana_cost, casting_variant);
    // CR 601.2b + CR 601.2f: Cost-floor statics (Trinisphere class) — LAST, after
    // every additive/subtractive modifier so the floor sees the final mana
    // component. While the cost still contains `{X}`, X has mana value 0
    // (CR 107.3g), so flooring now would over-count the spell once X is paid
    // (CR 601.2b locks in the chosen X *before* the "directly affect the total
    // cost" step of CR 601.2f). Defer the floor for `{X}` costs to
    // `apply_post_x_cost_modifiers`, run from the ChooseX handler once X is concrete.
    if !casting_costs::cost_has_x(mana_cost) {
        apply_cost_floor(state, player, object_id, mana_cost);
    }
}

/// CR 601.2f: Apply the target-dependent cost modifiers (NO floor) to
/// `mana_cost`, in CR-correct order:
/// Strive per-target surcharge (CR 601.2f cost increase) → self-spell statics
/// that read the chosen targets → battlefield statics that read the chosen
/// targets. Floors are deliberately excluded so callers can run them LAST. The
/// `unselected-targets` case (no `TargetRef` in the static's filter) is a safe
/// no-op for the selected-targets passes.
pub(super) fn apply_target_dependent_cost_modifiers(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    ability: &ResolvedAbility,
    mana_cost: &mut ManaCost,
) {
    // CR 601.2f: Strive per-target cost increase. Targets are chosen in
    // CR 601.2c; costs are determined in CR 601.2f. Add
    // strive_cost * (num_targets - 1) to the total casting cost.
    if let Some(strive_cost) = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.strive_cost.clone())
    {
        let target_count = super::ability_utils::flatten_targets_in_chain(ability).len();
        for _ in 1..target_count {
            *mana_cost = super::restrictions::add_mana_cost(mana_cost, &strive_cost);
        }
    }
    let mut collected =
        collect_self_spell_cost_modifiers(state, player, object_id, Some(ability), true, None);
    collected.extend(collect_battlefield_cost_modifiers(
        state,
        player,
        object_id,
        Some(ability),
        true,
    ));
    apply_cost_modifications_in_order(mana_cost, &collected);
}

/// CR 601.2f: Recompute the FULL concrete pending cost for a known X. Floors
/// run LAST so they lock in against the real total (CR 601.2f "locked in").
/// Order: base (tax-inclusive) → concretize_x (CR 107.1b) → non-target
/// reductions → target-dependent reductions + Strive → THEN both floor channels.
///
/// X is concrete here, so both floor channels apply (they do not self-gate on
/// X — only the prepare-path callers gate). Selected targets come from the
/// cloned pending `ability`; the unselected-targets case no-ops safely.
pub(super) fn concrete_cost_for_x(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    ability: &ResolvedAbility,
    base: &ManaCost,
    x: u32,
) -> ManaCost {
    let mut cost = base.clone();
    cost.concretize_x(x);
    apply_non_floor_cost_modifiers(state, player, object_id, &mut cost, None);
    apply_target_dependent_cost_modifiers(state, player, object_id, ability, &mut cost);
    apply_cost_floor(state, player, object_id, &mut cost);
    apply_cost_floor_with_selected_targets(state, player, object_id, ability, &mut cost);
    cost
}

/// CR 601.2f + CR 702.41a: Build per-X total cost previews for the Choose-X UI.
/// Each entry is `(x, concrete_cost)` after Affinity/reductions/floors. Empty
/// when `base_cost` is unavailable or the legal range exceeds 100 values.
pub(super) fn build_choose_x_cost_previews(
    state: &GameState,
    player: PlayerId,
    pending: &PendingCast,
    min: u32,
    max: u32,
) -> Vec<(u32, ManaCost)> {
    let Some(base) = pending.base_cost.as_ref() else {
        return Vec::new();
    };
    if min > max || max.saturating_sub(min) > 100 {
        return Vec::new();
    }
    (min..=max)
        .map(|x| {
            (
                x,
                concrete_cost_for_x(state, player, pending.object_id, &pending.ability, base, x),
            )
        })
        .collect()
}

/// CR 601.2f + CR 107.3g: Re-derive a pending `{X}` spell's full concrete cost
/// AFTER the chosen X is known. Rebuilds from the captured tax-inclusive base
/// via `concrete_cost_for_x`, re-applying all reductions, target-dependent
/// modifiers, and Strive, with both floor channels run LAST (CR 601.2f locked
/// in). This replaces the floor-only post-X pass so that reduction capacity
/// exceeding the fixed non-X generic is no longer clamped at generic=0 while X
/// was symbolic (mana value 0, CR 107.3g).
///
/// Legacy/in-flight saved games (or any path that never threaded `base_cost`)
/// fall back to flooring the already-concretized `cost` — byte-identical to the
/// pre-change behavior.
pub(super) fn apply_post_x_cost_modifiers(
    state: &mut GameState,
    caster: PlayerId,
    object_id: ObjectId,
) {
    let Some(pending) = state.pending_cast.as_ref() else {
        return;
    };
    let Some(x) = pending.ability.chosen_x else {
        return;
    };
    let ability = pending.ability.clone();
    let new_cost = match pending.base_cost.clone() {
        Some(base) => concrete_cost_for_x(state, caster, object_id, &ability, &base, x),
        None => {
            // Legacy / in-flight saved game without a captured base: behavior
            // identical to the pre-change floor-only post-X pass.
            let mut cost = pending.cost.clone();
            apply_cost_floor(state, caster, object_id, &mut cost);
            apply_cost_floor_with_selected_targets(state, caster, object_id, &ability, &mut cost);
            cost
        }
    };
    debug_assert!(!casting_costs::cost_has_x(&new_cost));
    if let Some(pending) = state.pending_cast.as_mut() {
        pending.cost = new_cost;
    }
}

/// CR 601.2f + CR 118.9d: Apply the full cost-modifier stack (commander tax,
/// cost reductions, cost increases) to an arbitrary base mana cost. The base may
/// be the spell's printed mana cost OR an alternative cost (warp/evoke/overload/
/// bestow) — cost modifiers apply identically to alternative costs (CR 118.9d).
///
/// CR 903.8: The commander-tax surcharge applies only when the object is in the
/// command zone; alternative-cost bases are always hand cards, so they never
/// incur the tax.
pub(super) fn apply_cost_modifiers_to_base(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    base: ManaCost,
) -> Option<ManaCost> {
    let obj = state.objects.get(&object_id)?;
    let mut mana_cost = base;
    // CR 903.8: Commanders cast from the command zone incur a tax.
    if obj.zone == Zone::Command {
        let tax = super::commander::commander_tax(state, object_id);
        if tax > 0 {
            match &mut mana_cost {
                ManaCost::Cost { generic, .. } => *generic += tax,
                ManaCost::NoCost => {
                    mana_cost = ManaCost::Cost {
                        shards: vec![],
                        generic: tax,
                    };
                }
                ManaCost::SelfManaCost | ManaCost::SelfManaValue => {}
            }
        }
    }
    apply_all_cost_modifiers(state, player, object_id, &mut mana_cost, None);
    Some(mana_cost)
}

/// CR 601.2f + CR 601.2g: Re-derive a pending cast's total mana cost after an
/// optional additional cost (e.g. Bargain) is declared. CR 601.2f (additional
/// costs declared) precedes CR 601.2g/601.2h (total cost calculated and locked),
/// so re-running the cost-modifier passes here — after the Bargain opt-in is
/// resolved and `additional_cost_paid` is set, before mana payment — places the
/// final cost calculation in the CR-correct window.
///
/// The base is the spell's printed mana cost plus commander tax (CR 903.8). The
/// whole Bargain class (Hamlet Glutton, Ice Out, Johann's Stopgap) is cast for
/// its normal mana cost — Bargain is an *additional* cost, never an alternative
/// one — so the printed cost is the correct base.
pub(super) fn recompute_pending_cast_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<ManaCost> {
    let obj = state.objects.get(&object_id)?;
    apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
}

/// CR 601.2f: Apply self-spell cost modifications — `ReduceCost` / `RaiseCost`
/// statics printed on the spell being cast, with `affected = SelfRef` and `active_zones`
/// covering the card's current castable zone. Handles cards like Tolarian Terror where the cost reduction is
/// inherent to the spell and must apply before the spell resolves.
///
/// Test-only isolation helper: production cost calculation now collects self-spell
/// and battlefield modifiers together (CR 601.2f aggregate ordering) via
/// `collect_self_spell_cost_modifiers` + `apply_cost_modifications_in_order` in
/// `apply_non_floor_cost_modifiers`; this wrapper exists so tests can exercise the
/// self-spell pass in isolation.
#[cfg(test)]
fn apply_self_spell_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    let collected = collect_self_spell_cost_modifiers(state, caster, spell_id, None, false, None);
    apply_cost_modifications_in_order(mana_cost, &collected);
}

#[cfg(test)]
pub(super) fn apply_self_spell_cost_modifiers_with_selected_targets(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    ability: &ResolvedAbility,
    mana_cost: &mut ManaCost,
) {
    let collected =
        collect_self_spell_cost_modifiers(state, caster, spell_id, Some(ability), true, None);
    apply_cost_modifications_in_order(mana_cost, &collected);
}

struct CostModification {
    is_raise: bool,
    amount: ManaCost,
    multiplier: u32,
}

fn self_spell_cost_condition_matches(
    state: &GameState,
    condition: &StaticCondition,
    caster: PlayerId,
    spell_id: ObjectId,
    casting_variant: Option<CastingVariant>,
) -> bool {
    match condition {
        StaticCondition::And { conditions } => conditions.iter().all(|cond| {
            self_spell_cost_condition_matches(state, cond, caster, spell_id, casting_variant)
        }),
        StaticCondition::Or { conditions } => conditions.iter().any(|cond| {
            self_spell_cost_condition_matches(state, cond, caster, spell_id, casting_variant)
        }),
        StaticCondition::Not { condition } => {
            !self_spell_cost_condition_matches(state, condition, caster, spell_id, casting_variant)
        }
        StaticCondition::CastingAsVariant { variant } => casting_variant == Some(*variant),
        _ => super::layers::evaluate_condition(state, condition, caster, spell_id),
    }
}

fn collect_self_spell_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    selected_ability: Option<&ResolvedAbility>,
    target_sensitive_only: bool,
    casting_variant: Option<CastingVariant>,
) -> Vec<CostModification> {
    let Some(spell_obj) = state.objects.get(&spell_id) else {
        return Vec::new();
    };

    let mut collected = Vec::new();

    // CR 113.6 + CR 604.1: A static ability only functions in zones listed by
    // `active_zones`; battlefield-default (empty) statics do not apply here.
    // We iterate the spell's own static definitions without running the layer
    // pipeline: layers pre-compute battlefield characteristics, not cast-time
    // cost deltas on cards in hand.
    for def in spell_obj.static_definitions.iter_all() {
        if def.active_zones.is_empty() {
            continue;
        }
        if !def.active_zones.contains(&spell_obj.zone) {
            continue;
        }
        // CR 601.2f: Only self-referential cost statics apply here. Any other
        // `affected` scoping would indicate a battlefield-style static that
        // should be handled by the battlefield scanner.
        if !matches!(def.affected, Some(TargetFilter::SelfRef)) {
            continue;
        }

        let (amount, spell_filter, dynamic_count, is_raise) = match &def.mode {
            StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount,
                spell_filter,
                dynamic_count,
            } => (amount, spell_filter, dynamic_count, false),
            StaticMode::ModifyCost {
                mode: CostModifyMode::Raise,
                amount,
                spell_filter,
                dynamic_count,
            } => (amount, spell_filter, dynamic_count, true),
            _ => continue,
        };

        let has_target_filter = spell_filter
            .as_ref()
            .is_some_and(cost_filter_has_target_ref);
        if target_sensitive_only && !has_target_filter {
            continue;
        }
        if selected_ability.is_none() && has_target_filter {
            continue;
        }

        if let Some(ref filter) = spell_filter {
            let matches = if let Some(ability) = selected_ability {
                spell_matches_cost_filter_with_selected_targets(
                    state, caster, spell_id, filter, spell_id, ability,
                )
            } else {
                spell_matches_cost_filter(state, caster, spell_id, filter, spell_id)
            };
            if !matches {
                continue;
            }
        }

        // CR 604.1: Evaluate any trailing condition ("if you control a Wizard").
        if let Some(ref cond) = def.condition {
            if !self_spell_cost_condition_matches(state, cond, caster, spell_id, casting_variant) {
                continue;
            }
        }

        // CR 601.2f: Resolve the dynamic multiplier (e.g., "for each instant or
        // sorcery card in your graveyard"). Static amount with no multiplier = 1.
        let multiplier = if let Some(ref qty_ref) = dynamic_count {
            let qty_expr = crate::types::ability::QuantityExpr::Ref {
                qty: qty_ref.clone(),
            };
            super::quantity::resolve_quantity(state, &qty_expr, caster, spell_id).max(0) as u32
        } else {
            1
        };

        collected.push(CostModification {
            is_raise,
            amount: amount.clone(),
            multiplier,
        });
    }

    collected
}

fn cost_filter_has_target_ref(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|prop| {
            matches!(
                prop,
                crate::types::ability::FilterProp::Targets { .. }
                    | crate::types::ability::FilterProp::TargetsOnly { .. }
            )
        }),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(cost_filter_has_target_ref)
        }
        TargetFilter::Not { filter } => cost_filter_has_target_ref(filter),
        _ => false,
    }
}

fn target_ref_matches_cost_filter(
    state: &GameState,
    static_source_id: ObjectId,
    source_controller: PlayerId,
    target: &TargetRef,
    filter: &TargetFilter,
) -> bool {
    match target {
        TargetRef::Object(object_id) => {
            // CR 601.2f: Target-referenced cost filters ("that target this creature")
            // resolve SelfRef against the static's source permanent, not the spell
            // being cast.
            let ctx = super::filter::FilterContext::from_source_with_controller(
                static_source_id,
                source_controller,
            );
            if super::filter::matches_stack_target_filter(state, *object_id, filter, &ctx) {
                return true;
            }
            super::filter::matches_target_filter(state, *object_id, filter, &ctx)
        }
        TargetRef::Player(player_id) => super::filter::player_matches_target_filter_in_state(
            state,
            filter,
            *player_id,
            Some(source_controller),
        ),
    }
}

fn selected_targets_match_filter(
    state: &GameState,
    static_source_id: ObjectId,
    source_controller: PlayerId,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    require_all: bool,
) -> bool {
    let targets = flatten_targets_in_chain(ability);
    if targets.is_empty() {
        return false;
    }

    if require_all {
        targets.iter().all(|target| {
            target_ref_matches_cost_filter(
                state,
                static_source_id,
                source_controller,
                target,
                filter,
            )
        })
    } else {
        targets.iter().any(|target| {
            target_ref_matches_cost_filter(
                state,
                static_source_id,
                source_controller,
                target,
                filter,
            )
        })
    }
}

fn spell_matches_cost_filter_with_selected_targets(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    ability: &ResolvedAbility,
) -> bool {
    let Some(source_controller) = state.objects.get(&source_id).map(|obj| obj.controller) else {
        return false;
    };

    match filter {
        TargetFilter::Typed(tf) => {
            let non_target_props: Vec<_> = tf
                .properties
                .iter()
                .filter(|prop| {
                    !matches!(
                        prop,
                        crate::types::ability::FilterProp::Targets { .. }
                            | crate::types::ability::FilterProp::TargetsOnly { .. }
                    )
                })
                .cloned()
                .collect();
            let base = TargetFilter::Typed(crate::types::ability::TypedFilter {
                type_filters: tf.type_filters.clone(),
                controller: tf.controller.clone(),
                properties: non_target_props,
            });
            if !spell_matches_cost_filter(state, caster, spell_id, &base, source_id) {
                return false;
            }

            tf.properties.iter().all(|prop| match prop {
                crate::types::ability::FilterProp::Targets { filter } => {
                    selected_targets_match_filter(
                        state,
                        source_id,
                        source_controller,
                        ability,
                        filter,
                        false,
                    )
                }
                crate::types::ability::FilterProp::TargetsOnly { filter } => {
                    selected_targets_match_filter(
                        state,
                        source_id,
                        source_controller,
                        ability,
                        filter,
                        true,
                    )
                }
                _ => true,
            })
        }
        TargetFilter::Or { filters } => filters.iter().any(|inner| {
            spell_matches_cost_filter_with_selected_targets(
                state, caster, spell_id, inner, source_id, ability,
            )
        }),
        TargetFilter::And { filters } => filters.iter().all(|inner| {
            spell_matches_cost_filter_with_selected_targets(
                state, caster, spell_id, inner, source_id, ability,
            )
        }),
        TargetFilter::Not { filter: inner } => !spell_matches_cost_filter_with_selected_targets(
            state, caster, spell_id, inner, source_id, ability,
        ),
        _ => spell_matches_cost_filter(state, caster, spell_id, filter, source_id),
    }
}

/// CR 601.2f: Apply cost modifications from battlefield permanents with ReduceCost/RaiseCost statics.
///
/// Iterates all battlefield permanents and checks each static definition for cost modification
/// modes. For each applicable modifier, adjusts the spell's mana cost:
/// - ReduceCost: reduces generic mana (cannot go below 0)
/// - RaiseCost: increases generic mana
///
/// Player scope is checked via the `affected` filter on the StaticDefinition (You = source's
/// controller casts, Opponent = source's opponent casts, no controller = all players).
/// Spell type is checked via the `spell_filter` field in the StaticMode variant.
///
/// Test-only isolation helper: production cost calculation now collects self-spell
/// and battlefield modifiers together (CR 601.2f aggregate ordering) via
/// `collect_battlefield_cost_modifiers` + `apply_cost_modifications_in_order` in
/// `apply_non_floor_cost_modifiers`; this wrapper exists so tests can exercise the
/// battlefield pass in isolation.
#[cfg(test)]
fn apply_battlefield_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    let collected = collect_battlefield_cost_modifiers(state, caster, spell_id, None, false);
    apply_cost_modifications_in_order(mana_cost, &collected);
}

#[cfg(test)]
pub(super) fn apply_battlefield_cost_modifiers_with_selected_targets(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    ability: &ResolvedAbility,
    mana_cost: &mut ManaCost,
) {
    let collected =
        collect_battlefield_cost_modifiers(state, caster, spell_id, Some(ability), true);
    apply_cost_modifications_in_order(mana_cost, &collected);
}

/// CR 601.2f + CR 109.5: Cost-mod static conditions mix two player scopes.
/// `DuringYourTurn` binds to the source permanent's controller (Paladin Class),
/// while `SpellsCastThisTurn` / first-spell quantity gates bind to the spell
/// caster (Heartwood Storyteller Avatar's opponent-first tax).
fn evaluate_cost_mod_static_condition(
    state: &GameState,
    condition: &crate::types::ability::StaticCondition,
    caster: PlayerId,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    use crate::types::ability::StaticCondition;

    match condition {
        StaticCondition::DuringYourTurn => {
            super::layers::evaluate_condition(state, condition, source_controller, source_id)
        }
        StaticCondition::And { conditions } => conditions.iter().all(|c| {
            evaluate_cost_mod_static_condition(state, c, caster, source_controller, source_id)
        }),
        StaticCondition::Or { conditions } => conditions.iter().any(|c| {
            evaluate_cost_mod_static_condition(state, c, caster, source_controller, source_id)
        }),
        StaticCondition::Not { condition } => !evaluate_cost_mod_static_condition(
            state,
            condition,
            caster,
            source_controller,
            source_id,
        ),
        _ => super::layers::evaluate_condition(state, condition, caster, source_id),
    }
}

fn collect_battlefield_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    selected_ability: Option<&ResolvedAbility>,
    target_sensitive_only: bool,
) -> Vec<CostModification> {
    use crate::types::ability::ControllerRef;

    // CR 702.26b + CR 114.4 + CR 113.6b: Functioning gate (phased-out /
    // command-zone with Eminence-style opt-in) owned by
    // `game_functioning_statics`. We deliberately use the non-condition-
    // filtered helper here — CR 604.1 condition evaluation uses per-clause player
    // scope via `evaluate_cost_mod_static_condition`.
    //
    // CR 113.6b: cost-reduction statics that opt into the command zone via
    // `active_zones.contains(Command)` (Eminence — The Ur-Dragon, Edgar Markov)
    // function from the command zone for non-emblem objects; the per-static
    // `active_zones` filter below still enforces the static's declared zones
    // when the source is on the battlefield.
    //
    // CR 601.2f: the {0} floor is a property of the aggregate total
    // (base + all increases - all reductions), applied once. Collect every
    // matching modifier first, then apply ALL increases before ANY reductions, so
    // a reduction's `saturating_sub` floor can never clamp generic to 0 ahead of a
    // later increase (which would overcharge the spell, order-dependently).
    let mut collected = Vec::new();
    for (src_obj, def) in super::functioning_abilities::game_functioning_statics(state) {
        let bf_id = src_obj.id;
        let source_controller = src_obj.controller;

        {
            let (amount, spell_filter, dynamic_count, is_raise) = match &def.mode {
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    amount,
                    spell_filter,
                    dynamic_count,
                } => (amount, spell_filter, dynamic_count, false),
                StaticMode::ModifyCost {
                    mode: CostModifyMode::Raise,
                    amount,
                    spell_filter,
                    dynamic_count,
                } => (amount, spell_filter, dynamic_count, true),
                _ => continue,
            };

            let has_target_filter = spell_filter
                .as_ref()
                .is_some_and(cost_filter_has_target_ref);
            if target_sensitive_only && !has_target_filter {
                continue;
            }
            if selected_ability.is_none() && has_target_filter {
                continue;
            }

            // CR 113.6: SelfRef statics are self-cost-reduction ("this spell costs
            // {N} less") — handled by apply_self_spell_cost_modifiers for the spell
            // being cast. They must never apply from a battlefield permanent to
            // other spells.
            if matches!(def.affected, Some(TargetFilter::SelfRef)) {
                continue;
            }

            // CR 113.6 + CR 113.6b: A static functions only in its declared
            // zones. Empty `active_zones` means battlefield default; non-empty
            // means restrict to the listed zones. Eminence statics list both
            // Battlefield and Command and pass for either source zone.
            if def.active_zones.is_empty() {
                if src_obj.zone != Zone::Battlefield {
                    continue;
                }
            } else if !def.active_zones.contains(&src_obj.zone) {
                continue;
            }

            // CR 601.2f: Check player scope — does this modifier apply to spells the caster casts?
            // Must run before condition check so QuantityComparison resolves against the caster.
            if let Some(TargetFilter::Typed(ref tf)) = def.affected {
                match tf.controller {
                    Some(ControllerRef::You) if caster != source_controller => continue,
                    Some(ControllerRef::Opponent) if caster == source_controller => continue,
                    _ => {} // No controller restriction or matches
                }
            }

            // CR 601.2f: Check static condition — "as long as" / "during your turn"
            // clauses gate cost modification. `DuringYourTurn` uses the source
            // controller; spell-history quantity gates use the caster.
            if let Some(ref cond) = def.condition {
                if !evaluate_cost_mod_static_condition(
                    state,
                    cond,
                    caster,
                    source_controller,
                    bf_id,
                ) {
                    continue;
                }
            }

            // CR 601.2f: Check spell type filter — does the spell match?
            if let Some(ref filter) = spell_filter {
                let matches = if let Some(ability) = selected_ability {
                    spell_matches_cost_filter_with_selected_targets(
                        state, caster, spell_id, filter, bf_id, ability,
                    )
                } else {
                    spell_matches_cost_filter(state, caster, spell_id, filter, bf_id)
                };
                if !matches {
                    continue;
                }
            }

            // CR 601.2f: Calculate the modification amount.
            let base_amount = amount.clone();
            let multiplier = if let Some(ref qty_ref) = dynamic_count {
                let qty_expr = crate::types::ability::QuantityExpr::Ref {
                    qty: qty_ref.clone(),
                };
                super::quantity::resolve_quantity(state, &qty_expr, source_controller, bf_id).max(0)
                    as u32
            } else {
                1
            };

            // CR 601.2f: defer application so increases land before reductions.
            collected.push(CostModification {
                is_raise,
                amount: base_amount,
                multiplier,
            });
        }
    }

    collected
}

/// CR 601.2f + CR 118.8: Collect additional non-mana costs imposed by battlefield
/// statics once targets are chosen. Terror of the Peaks class.
pub(super) fn collect_imposed_additional_cast_costs(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    ability: &ResolvedAbility,
) -> Vec<AbilityCost> {
    use crate::types::ability::ControllerRef;

    let mut costs = Vec::new();
    for (src_obj, def) in super::functioning_abilities::game_functioning_statics(state) {
        let bf_id = src_obj.id;
        let source_controller = src_obj.controller;

        let StaticMode::ImposeAdditionalCost {
            cost,
            spell_filter,
            action: AdditionalCostTaxAction::Cast,
        } = &def.mode
        else {
            continue;
        };

        let has_target_filter = spell_filter
            .as_ref()
            .is_some_and(cost_filter_has_target_ref);
        if !has_target_filter {
            continue;
        }

        if matches!(def.affected, Some(TargetFilter::SelfRef)) {
            continue;
        }

        if def.active_zones.is_empty() {
            if src_obj.zone != Zone::Battlefield {
                continue;
            }
        } else if !def.active_zones.contains(&src_obj.zone) {
            continue;
        }

        if let Some(TargetFilter::Typed(ref tf)) = def.affected {
            match tf.controller {
                Some(ControllerRef::You) if caster != source_controller => continue,
                Some(ControllerRef::Opponent) if caster == source_controller => continue,
                _ => {}
            }
        }

        if let Some(ref cond) = def.condition {
            if !super::layers::evaluate_condition(state, cond, caster, bf_id) {
                continue;
            }
        }

        if let Some(ref filter) = spell_filter {
            if !spell_matches_cost_filter_with_selected_targets(
                state, caster, spell_id, filter, bf_id, ability,
            ) {
                continue;
            }
        }

        costs.push(cost.clone());
    }

    costs
}

fn apply_cost_modifications_in_order(mana_cost: &mut ManaCost, collected: &[CostModification]) {
    // CR 601.2f: apply all cost increases first, then all reductions, so the
    // single {0} floor (the `saturating_sub` in `apply_cost_mod_to_mana`) acts on
    // base + increases. Reductions among themselves commute (each floors at 0), so
    // their relative order is irrelevant.
    for modification in collected.iter().filter(|m| m.is_raise) {
        apply_cost_mod_to_mana(
            mana_cost,
            &modification.amount,
            modification.multiplier,
            true,
        );
    }
    for modification in collected.iter().filter(|m| !m.is_raise) {
        apply_cost_mod_to_mana(
            mana_cost,
            &modification.amount,
            modification.multiplier,
            false,
        );
    }
}

/// CR 601.2f: Apply battlefield-based cost-floor statics (Trinisphere class).
///
/// Per CR 601.2f, the cost-floor is one of the "any effects that directly
/// affect the total cost" applied after all RaiseCost / ReduceCost / pending
/// reductions / Affinity have settled, just before the cost is "locked in."
/// Trinisphere ruling (2020-08-07): "Finally, apply Trinisphere's effect if
/// the mana component of the spell's cost is less than three mana."
///
/// The floor never reduces a cost. When the current `mana_cost.mana_value()`
/// is below the floor, generic mana is added to bring the total to the floor —
/// colored requirements are never modified, per the Trinisphere reminder text
/// "Additional mana ... may be paid with any color of mana or colorless mana."
pub(super) fn apply_cost_floor(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    apply_cost_floor_inner(state, caster, spell_id, None, false, mana_cost);
}

pub(super) fn apply_cost_floor_with_selected_targets(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    ability: &ResolvedAbility,
    mana_cost: &mut ManaCost,
) {
    apply_cost_floor_inner(state, caster, spell_id, Some(ability), true, mana_cost);
}

fn apply_cost_floor_inner(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    selected_ability: Option<&ResolvedAbility>,
    target_sensitive_only: bool,
    mana_cost: &mut ManaCost,
) {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_functioning_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_functioning_statics(state) {
        let bf_id = bf_obj.id;

        let StaticMode::ModifyCost {
            mode: CostModifyMode::Minimum,
            ref amount,
            ref spell_filter,
            ..
        } = def.mode
        else {
            continue;
        };

        let has_target_filter = spell_filter
            .as_ref()
            .is_some_and(cost_filter_has_target_ref);
        if target_sensitive_only && !has_target_filter {
            continue;
        }
        if selected_ability.is_none() && has_target_filter {
            continue;
        }

        // CR 113.6: Statics that declare non-battlefield active_zones must not
        // fire from the battlefield. Empty active_zones = battlefield default.
        if !def.active_zones.is_empty() && !def.active_zones.contains(&Zone::Battlefield) {
            continue;
        }

        // CR 601.2f: Optional caster-scope check via `def.affected`. Trinisphere
        // emits `affected: None` (the floor applies to every spell), but a
        // future filtered floor variant ("each spell your opponents cast that
        // would cost less than ... ") could carry a `ControllerRef`-scoped
        // affected filter; honor it here for forward-compatibility.
        if let Some(crate::types::ability::TargetFilter::Typed(ref tf)) = def.affected {
            use crate::types::ability::ControllerRef;
            let source_controller = bf_obj.controller;
            match tf.controller {
                Some(ControllerRef::You) if caster != source_controller => continue,
                Some(ControllerRef::Opponent) if caster == source_controller => continue,
                _ => {}
            }
        }

        // CR 601.2f: Evaluate the static's `condition` ("as long as this artifact
        // is untapped") against the source permanent. Trinisphere's effect
        // turns off entirely when the artifact is tapped.
        if let Some(ref cond) = def.condition {
            if !super::layers::evaluate_condition(state, cond, caster, bf_id) {
                continue;
            }
        }

        // CR 601.2f: Spell-type filter narrows which spells are floored.
        if let Some(ref filter) = spell_filter {
            let matches = if let Some(ability) = selected_ability {
                spell_matches_cost_filter_with_selected_targets(
                    state, caster, spell_id, filter, bf_id, ability,
                )
            } else {
                spell_matches_cost_filter(state, caster, spell_id, filter, bf_id)
            };
            if !matches {
                continue;
            }
        }

        let floor = amount.mana_value();
        if floor == 0 {
            continue;
        }
        let current = mana_cost.mana_value();
        if current >= floor {
            continue;
        }
        let delta = floor - current;

        // Top up generic mana to reach the floor. Alternative-cost and
        // permission paths can reduce the payable mana component to zero
        // (`NoCost`); the floor still sees that zero mana component and adds
        // generic mana to reach the minimum.
        match mana_cost {
            ManaCost::Cost { generic, .. } => {
                *generic = generic.saturating_add(delta);
            }
            ManaCost::NoCost => {
                *mana_cost = ManaCost::generic(delta);
            }
            ManaCost::SelfManaCost | ManaCost::SelfManaValue => {}
        }
    }
}

/// Check if a spell matches a cost modification filter.
/// Handles both Typed filters (single type) and Or filters (combined types like instant/sorcery).
fn spell_matches_cost_filter(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    let Some(spell_obj) = state.objects.get(&spell_id) else {
        return false;
    };
    if !state.objects.contains_key(&source_id) {
        return false;
    }

    match filter {
        TargetFilter::Typed(_) => super::filter::spell_object_matches_filter_from_state(
            state,
            spell_obj,
            spell_obj.zone,
            caster,
            filter,
            source_id,
            &state.all_creature_types,
        ),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| spell_matches_cost_filter(state, caster, spell_id, inner, source_id)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|inner| spell_matches_cost_filter(state, caster, spell_id, inner, source_id)),
        TargetFilter::Not { filter: inner } => {
            !spell_matches_cost_filter(state, caster, spell_id, inner, source_id)
        }
        // CR 601.2e: Cost modifications only apply when the filter explicitly matches.
        // Fail-closed: unrecognized filter shapes do not universally reduce costs.
        _ => false,
    }
}

fn shard_reduction_color(shard: ManaCostShard) -> Option<ManaColor> {
    match shard {
        ManaCostShard::White => Some(ManaColor::White),
        ManaCostShard::Blue => Some(ManaColor::Blue),
        ManaCostShard::Black => Some(ManaColor::Black),
        ManaCostShard::Red => Some(ManaColor::Red),
        ManaCostShard::Green => Some(ManaColor::Green),
        _ => None,
    }
}

pub(super) fn cost_shard_matches_reduction(
    cost_shard: ManaCostShard,
    reduction: ManaCostShard,
) -> bool {
    shard_reduction_color(reduction).is_some_and(|color| cost_shard.contributes_to(color))
        || cost_shard == reduction
}

fn apply_shard_reduction(shards: &mut Vec<ManaCostShard>, reduction: ManaCostShard) {
    if let Some(index) = shards
        .iter()
        .position(|shard| cost_shard_matches_reduction(*shard, reduction))
    {
        shards.remove(index);
    }
}

/// CR 601.2f: Apply a single cost modification (reduce or raise) to a mana cost.
/// ReduceCost removes matching mana symbols and generic mana (not below zero).
/// RaiseCost adds the specified symbols and generic mana.
fn apply_cost_mod_to_mana(
    mana_cost: &mut ManaCost,
    base_amount: &ManaCost,
    multiplier: u32,
    is_raise: bool,
) {
    let (mod_shards, mod_generic) = match base_amount {
        ManaCost::Cost { shards, generic } => (shards, *generic * multiplier),
        _ => return,
    };

    if multiplier == 0 || (mod_generic == 0 && mod_shards.is_empty()) {
        return;
    }

    if matches!(mana_cost, ManaCost::NoCost) && is_raise {
        *mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 0,
        };
    }

    let ManaCost::Cost { shards, generic } = mana_cost else {
        return;
    };

    if is_raise {
        for _ in 0..multiplier {
            shards.extend(mod_shards.iter().copied());
        }
        *generic += mod_generic;
    } else {
        for _ in 0..multiplier {
            for shard in mod_shards {
                apply_shard_reduction(shards, *shard);
            }
        }
        *generic = generic.saturating_sub(mod_generic);
    }
}

/// CR 702.41a: Apply Affinity cost reduction from the spell's own keywords.
///
/// For each `Keyword::Affinity(type_filter)` on the spell, counts matching
/// permanents on the battlefield controlled by the caster and reduces the
/// spell's generic mana cost by that count (floor at 0).
/// CR 702.41b: Multiple Affinity instances each apply separately.
fn apply_affinity_reduction(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    if !state.objects.contains_key(&spell_id) {
        return;
    }
    for kw in effective_spell_keywords(state, caster, spell_id) {
        if let Keyword::Affinity(ref type_filter) = kw {
            let filter = TargetFilter::Typed(type_filter.clone());
            let ctx = super::filter::FilterContext::from_source(state, spell_id);
            let count = state
                .battlefield
                .iter()
                .filter(|&&id| {
                    let Some(obj) = state.objects.get(&id) else {
                        return false;
                    };
                    obj.controller == caster
                        && super::filter::matches_target_filter(state, id, &filter, &ctx)
                })
                .count() as u32;
            apply_cost_mod_to_mana(mana_cost, &ManaCost::generic(1), count, false);
        }
    }
}

/// CR 702.125a: Apply Undaunted cost reduction from the spell's own keyword.
///
/// "This spell costs {1} less to cast for each opponent you have." CR 702.125b:
/// players who have left the game are not counted — `players::opponents` already
/// returns only living opponents, so its length is exactly the CR count. Reduces
/// the spell's generic mana cost by that count (floor at 0; colored pips are
/// never reduced — `apply_cost_mod_to_mana` handles both).
fn apply_undaunted_reduction(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    if !state.objects.contains_key(&spell_id) {
        return;
    }
    let instances = effective_spell_keywords(state, caster, spell_id)
        .iter()
        .filter(|kw| matches!(kw, Keyword::Undaunted))
        .count() as u32;
    if instances > 0 {
        let opponents = super::players::opponents(state, caster).len() as u32;
        apply_cost_mod_to_mana(
            mana_cost,
            &ManaCost::generic(1),
            opponents * instances,
            false,
        );
    }
}

/// CR 601.2f: Apply one-shot pending cost reductions (read-only during cost calculation).
/// The matching entry is consumed later in `consume_pending_spell_cost_reduction`.
fn apply_pending_spell_cost_reductions(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    for r in &state.pending_spell_cost_reductions {
        if r.player != caster {
            continue;
        }
        let matches = match &r.spell_filter {
            None => true,
            Some(filter) => spell_matches_cost_filter(state, caster, spell_id, filter, spell_id),
        };
        if matches {
            apply_cost_mod_to_mana(mana_cost, &ManaCost::generic(1), r.amount, false);
            break; // Only apply the first matching reduction
        }
    }
}

/// CR 601.2f: Consume (remove) a one-shot pending cost reduction after a spell is cast.
pub(super) fn consume_pending_spell_cost_reduction(state: &mut GameState, caster: PlayerId) {
    if let Some(idx) = state
        .pending_spell_cost_reductions
        .iter()
        .position(|r| r.player == caster && r.spell_filter.is_none())
    {
        state.pending_spell_cost_reductions.remove(idx);
    }
}

/// CR 715.3a / CR 720.3a: Swap object characteristics to the alternative
/// spell face for casting. Saves the normal face in `back_face` for later
/// restoration.
fn swap_to_alternative_spell_face(obj: &mut crate::game::game_object::GameObject) {
    let alternative = match obj.back_face.take() {
        Some(b) => b,
        None => return,
    };
    let normal_snapshot = super::printed_cards::snapshot_object_face(obj);
    super::printed_cards::apply_back_face_to_object(obj, alternative);
    obj.back_face = Some(normal_snapshot);
}

/// CR 715 / CR 720: Returns the Adventure-family spell layout if this object
/// has normal creature characteristics plus an inset instant/sorcery spell
/// face that may be chosen while casting from hand.
fn alternative_spell_layout(obj: &crate::game::game_object::GameObject) -> Option<LayoutKind> {
    let back = obj.back_face.as_ref()?;
    use crate::types::card_type::CoreType;
    let back_is_spell = back
        .card_types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Instant | CoreType::Sorcery));
    let front_is_spell = obj
        .card_types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Instant | CoreType::Sorcery));
    // CR 715.3: Adventure permanents (creature or enchantment) may cast their
    // inset instant/sorcery spell face from hand.
    if !back_is_spell || front_is_spell {
        return None;
    }

    if back
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype.eq_ignore_ascii_case("Omen"))
    {
        return Some(LayoutKind::Omen);
    }
    if back
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype.eq_ignore_ascii_case("Adventure"))
    {
        return Some(LayoutKind::Adventure);
    }

    match back.layout_kind {
        Some(LayoutKind::Omen) => Some(LayoutKind::Omen),
        Some(LayoutKind::Adventure) => Some(LayoutKind::Adventure),
        Some(_) => None,
        None => Some(LayoutKind::Adventure),
    }
}

/// CR 709.3 / CR 709.3a-b: Split cards whose two faces are both castable
/// require a cast-time face choice — the same player decision as spell//spell
/// MDFCs. This covers spell//spell splits (Life // Death) and Room split
/// enchantments (Spiked Corridor // Torture Pit), whose halves are both cast as
/// the Room enchantment (CR 709.3) — without the choice only the front half
/// (left door) is ever reachable. Fuse split cards (Breaking // Entering) keep
/// the existing `CastingVariant::Fuse` prompt instead.
fn split_spell_face_choice_available(obj: &crate::game::game_object::GameObject) -> bool {
    let Some(back) = obj.back_face.as_ref() else {
        return false;
    };
    if back.layout_kind != Some(LayoutKind::Split) {
        return false;
    }
    if obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Fuse))
    {
        return false;
    }
    is_castable_split_face(&obj.card_types) && is_castable_split_face(&back.card_types)
}

/// CR 709.3: A split-card face is independently castable when it is an
/// instant/sorcery spell or a Room enchantment half (each Room door is a
/// separately castable enchantment spell, CR 709.3 / CR 709.5c).
fn is_castable_split_face(types: &crate::types::card_type::CardType) -> bool {
    use crate::types::card_type::CoreType;
    types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Instant | CoreType::Sorcery))
        || (types.core_types.contains(&CoreType::Enchantment)
            && types.subtypes.iter().any(|s| s == "Room"))
}

/// CR 712.11b + CR 709.3: Cast-time face choice for spell//spell MDFCs and
/// spell//spell split cards.
fn cast_spell_face_choice_available(obj: &crate::game::game_object::GameObject) -> bool {
    modal_spell_face_choice_available(obj) || split_spell_face_choice_available(obj)
}

/// CR 712.11b: Returns true if `obj` is a Modal double-faced card whose two
/// faces present a real *cast*-time face choice — i.e. both faces are spells
/// (neither is a land). This is the spell//spell MDFC class (Esika, God of the
/// Tree // The Prismatic Bridge and the other Kaldheim gods, Valki // Tibalt,
/// Halvar // Sword, etc.) where `CastSpell` must let the player choose which
/// face to put on the stack.
///
/// Land faces are deliberately excluded: a land MDFC face is put onto the
/// battlefield through the play-land special action (`handle_play_land`), which
/// runs its own `ModalFaceChoice`. A spell//land MDFC casts its spell (front)
/// face normally and plays its land (back) face via PlayLand, so neither needs
/// a cast-time choice here.
///
/// The gate keys off `back_face.layout_kind == Modal`, which
/// `snapshot_object_face` clears to `None` after a swap — so re-entry into the
/// cast pipeline for the chosen face does not re-prompt.
fn modal_spell_face_choice_available(obj: &crate::game::game_object::GameObject) -> bool {
    use crate::types::card_type::CoreType;
    let Some(back) = obj.back_face.as_ref() else {
        return false;
    };
    if back.layout_kind != Some(LayoutKind::Modal) {
        return false;
    }
    let front_is_land = obj.card_types.core_types.contains(&CoreType::Land);
    let back_is_land = back.card_types.core_types.contains(&CoreType::Land);
    !front_is_land && !back_is_land
}

/// CR 712.11b + CR 903.8: A cast-time face choice (a spell//spell Modal DFC, or
/// an Adventure/Omen alternative spell face) is offered both when casting from
/// hand and when a player casts their commander from the command zone. A
/// DFC/MDFC commander must let its owner choose which face to put on the stack —
/// e.g. casting The Prismatic Bridge (the back face of Esika, God of the Tree)
/// directly from the command zone (#1548). The downstream cast pipeline
/// (`ChooseModalFace` re-entry, affordability via `can_cast_object_now`, and the
/// commander-tax surcharge) is already zone-agnostic; only this prompt gate was
/// restricted to the hand.
fn cast_face_choice_offered_from_zone(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    obj.zone == Zone::Hand
        || (state.format_config.command_zone && obj.zone == Zone::Command && obj.is_commander)
}

/// CR 709.3 + CR 712.11b: Spell//spell split cards and spell//spell MDFCs need a
/// cast-time face choice from any zone that permits casting the card, not only
/// hand or command (#3987 — Life // Death from graveyard via Jace, Telepath
/// Unbound).
fn cast_spell_face_choice_offered_from_zone(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    if !cast_spell_face_choice_available(obj) {
        return false;
    }
    cast_face_choice_offered_from_zone(state, obj)
        || matches!(obj.zone, Zone::Graveyard | Zone::Exile)
}

fn casting_variant_for_alternative_spell(layout: LayoutKind) -> CastingVariant {
    match layout {
        LayoutKind::Adventure => CastingVariant::Adventure,
        LayoutKind::Omen => CastingVariant::Omen,
        LayoutKind::Single
        | LayoutKind::Split
        | LayoutKind::Flip
        | LayoutKind::Transform
        | LayoutKind::Meld
        | LayoutKind::Modal
        | LayoutKind::Prepare => {
            unreachable!("alternative_spell_layout only returns Adventure or Omen")
        }
    }
}

/// CR 715.3a / CR 720.3: Handle alternative spell-face choice and proceed with casting.
pub fn handle_adventure_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    creature: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_adventure_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        creature,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_adventure_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    creature: bool,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature {
        // Creature face is just a normal creature spell — delegate to the standard
        // cast pipeline so vanilla creature faces (no spell ability), modal cards,
        // X costs, and other shared casting features all work uniformly. Mirrors
        // the Warp/Overload "cast normally" pattern.
        return continue_cast_from_prepared(state, player, object_id, payment_mode, events);
    }

    let layout = state
        .objects
        .get(&object_id)
        .and_then(alternative_spell_layout)
        .ok_or_else(|| {
            EngineError::InvalidAction("Object has no castable alternative spell face".to_string())
        })?;
    let casting_variant = casting_variant_for_alternative_spell(layout);

    // CR 715.3a / CR 720.3a: Swap to alternative spell face characteristics.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        swap_to_alternative_spell_face(obj);
    }

    let mut prepared = prepare_spell_cast(state, player, object_id)?;
    prepared.casting_variant = casting_variant;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// Handle Warp cost choice and proceed with casting.
/// Warp is a custom keyword: cast for warp cost from hand, exile at next end step,
/// then may cast from exile later. On `AlternativeCastDecision::Normal`, the player
/// chose to cast normally — temporarily remove the Warp keyword so
/// `prepare_spell_cast` picks `CastingVariant::Normal`, then restore it and
/// continue through the standard casting pipeline.
pub fn handle_warp_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_warp_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_warp_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    // Exhaustive match so adding a third decision variant (e.g., `Decline`)
    // is a compile error here rather than silently routing through one of
    // the two existing branches.
    let normal_path = match decision {
        AlternativeCastDecision::Normal => true,
        AlternativeCastDecision::Alternative => false,
    };
    if normal_path {
        // Temporarily remove Warp keyword so prepare_spell_cast picks Normal.
        // Restore immediately after preparation to preserve the keyword for
        // future casting (e.g., if the spell is countered and returns to hand).
        let warp_kw = if let Some(obj) = state.objects.get_mut(&object_id) {
            let idx = obj
                .keywords
                .iter()
                .position(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)));
            idx.map(|i| obj.keywords.remove(i))
        } else {
            None
        };

        let result = continue_cast_from_prepared(state, player, object_id, payment_mode, events);

        // Only restore if the object is still in Hand (cast didn't proceed to stack).
        // If cast succeeded, the keyword is on the printed card and will be present
        // when the card returns to hand after being countered.
        if let Some(kw) = warp_kw {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                if obj.zone == Zone::Hand {
                    obj.keywords.push(kw);
                }
            }
        }

        return result;
    }

    // Alternative (Warp): prepare_spell_cast naturally picks CastingVariant::Warp
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.96a: Handle Overload cost choice and proceed with casting. For
/// `AlternativeCastDecision::Alternative`, the cast is prepared with
/// `CastingVariant::Overload` — the overload mana cost substitutes for the
/// printed cost and the spell's ability tree is transformed (target → each,
/// CR 702.96b-c). For `Normal`, the cast proceeds normally.
pub fn handle_overload_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_overload_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_overload_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    match decision {
        AlternativeCastDecision::Alternative => {
            let mut prepared = prepare_spell_cast_with_variant_override(
                state,
                player,
                object_id,
                Some(CastingVariant::Overload),
            )?;
            prepared.payment_mode = payment_mode;
            continue_with_prepared(state, player, prepared, events)
        }
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

/// CR 702.162a + CR 712.8c + CR 712.11a-c + CR 712.14a: Handle More Than Meets the Eye cost choice and
/// proceed with casting. For `AlternativeCastDecision::Alternative`, the cast is
/// prepared with `CastingVariant::MoreThanMeetsTheEye` — the MTMTE mana cost
/// substitutes for the printed cost and the spell is cast CONVERTED, so the
/// stack spell uses back-face characteristics and the resolving permanent
/// enters the battlefield with its back face up. For `Normal`, the cast
/// proceeds normally (front face).
pub fn handle_mtmte_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_mtmte_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_mtmte_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    match decision {
        AlternativeCastDecision::Alternative => continue_cast_with_alternative_spell_face(
            state,
            player,
            object_id,
            CastingVariant::MoreThanMeetsTheEye,
            payment_mode,
            events,
        ),
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

/// CR 702.113a: Handle Awaken cost choice and proceed with casting. For
/// `AlternativeCastDecision::Alternative`, the cast is prepared with
/// `CastingVariant::Awaken` — the awaken mana cost substitutes for the printed
/// cost and `append_awaken_rider` appends the "put N +1/+1 counters on target
/// land you control; that land becomes a 0/0 Elemental creature with haste"
/// rider to the tail of the spell's ability tree. The land target then exists
/// (CR 702.113b). For `Normal`, the cast proceeds normally with no rider and no
/// land target — the discriminating "normal cast does not awaken" path.
pub fn handle_awaken_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_awaken_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_awaken_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    match decision {
        AlternativeCastDecision::Alternative => {
            let mut prepared = prepare_spell_cast_with_variant_override(
                state,
                player,
                object_id,
                Some(CastingVariant::Awaken),
            )?;
            prepared.payment_mode = payment_mode;
            continue_with_prepared(state, player, prepared, events)
        }
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

/// CR 702.176a: Player chose the normal cast path for an Impending card.
pub fn handle_impending_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_impending_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

/// CR 702.176a: Route an Impending alternative-cost decision into the casting
/// pipeline. `Alternative` substitutes the impending mana cost (via
/// `CastingVariant::Impending`); `Normal` proceeds as a standard creature cast.
/// The ETB time-counter placement and "not a creature" handling occur at stack
/// resolution in `stack.rs`, not here.
pub fn handle_impending_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match decision {
        AlternativeCastDecision::Alternative => {
            let mut prepared = prepare_spell_cast_with_variant_override(
                state,
                player,
                object_id,
                Some(CastingVariant::Impending),
            )?;
            prepared.payment_mode = payment_mode;
            continue_with_prepared(state, player, prepared, events)
        }
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

fn prototype_form_from_object(
    obj: &crate::game::game_object::GameObject,
) -> Option<PrototypeFormState> {
    obj.keywords.iter().find_map(|keyword| {
        let Keyword::Prototype {
            cost,
            power: Some(power),
            toughness: Some(toughness),
        } = keyword
        else {
            return None;
        };
        Some(PrototypeFormState {
            mana_cost: cost.clone(),
            power: *power,
            toughness: *toughness,
            colors: prototype_colors_from_cost(cost),
        })
    })
}

fn prototype_colors_from_cost(cost: &ManaCost) -> Vec<ManaColor> {
    let ManaCost::Cost { shards, .. } = cost else {
        return Vec::new();
    };
    ManaColor::ALL
        .into_iter()
        .filter(|color| shards.iter().any(|shard| shard.contributes_to(*color)))
        .collect()
}

/// CR 702.160a: Apply the prototype alternative characteristics to the object
/// once the player chooses to cast it prototyped. This mutates only live
/// characteristics plus the typed marker; printed base characteristics remain
/// unchanged so zone cleanup and normal future casts can restore them.
fn apply_prototype_form(obj: &mut crate::game::game_object::GameObject) -> bool {
    let Some(form) = prototype_form_from_object(obj) else {
        return false;
    };
    obj.mana_cost = form.mana_cost.clone();
    obj.power = Some(form.power);
    obj.toughness = Some(form.toughness);
    obj.color = form.colors.clone();
    obj.prototype_form = Some(form);
    true
}

/// CR 702.160a + CR 400.7: Restore printed characteristics when a prototyped
/// cast is backed out before the object reaches a live Prototype zone, or when
/// zone cleanup turns it into a new object.
pub(crate) fn clear_prototype_form(obj: &mut crate::game::game_object::GameObject) {
    obj.prototype_form = None;
    obj.mana_cost = obj.base_mana_cost.clone();
    obj.power = obj.base_power;
    obj.toughness = obj.base_toughness;
    obj.color = obj.base_color.clone();
}

/// CR 702.160a: Player chose the normal or prototyped cast path for a Prototype
/// card. `Alternative` applies the secondary mana cost and P/T before
/// preparation so the announced stack spell already has prototype
/// characteristics; `Normal` proceeds as the printed spell.
pub fn handle_prototype_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_prototype_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_prototype_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match decision {
        AlternativeCastDecision::Alternative => {
            if !state
                .objects
                .get_mut(&object_id)
                .is_some_and(apply_prototype_form)
            {
                return Err(EngineError::InvalidAction(
                    "Prototype characteristics are unavailable for this object".to_string(),
                ));
            }
            let mut prepared = match prepare_spell_cast_with_variant_override(
                state,
                player,
                object_id,
                Some(CastingVariant::Prototype),
            ) {
                Ok(prepared) => prepared,
                Err(err) => {
                    if let Some(obj) = state.objects.get_mut(&object_id) {
                        clear_prototype_form(obj);
                    }
                    return Err(err);
                }
            };
            prepared.payment_mode = payment_mode;
            continue_with_prepared(state, player, prepared, events)
        }
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

/// CR 702.103b: Apply the bestow type-changing effect to a stack-bound or
/// hand-bound bestow card. Removes the Creature core type, adds the Aura
/// subtype, and grants `Keyword::Enchant(creature filter)` so the existing
/// Aura targeting path in `continue_with_prepared` finds it. Mutates both the
/// live (`card_types`/`keywords`) and base (`base_card_types`/`base_keywords`)
/// fields so the bestow form survives any layer-evaluation reset (layers reset
/// live characteristics from base on each pass, and stack objects are not
/// touched by layers, but battlefield re-entry resets are anchored on base
/// values too).
///
/// `bestow_form` is set to `Some(BestowFormState)` to mark the object as in
/// bestow form; `revert_bestow_aura_form` is the inverse operation.
///
/// Idempotent: safe to re-run after printed-face rehydration or layer resets
/// re-anchor `card_types` from a refreshed `base_card_types`.
pub(crate) fn apply_bestow_aura_form(obj: &mut crate::game::game_object::GameObject) {
    use crate::types::card_type::CoreType;
    // CR 702.103b: Remove the Creature core type while bestowed.
    obj.card_types
        .core_types
        .retain(|t| !matches!(t, CoreType::Creature));
    obj.base_card_types
        .core_types
        .retain(|t| !matches!(t, CoreType::Creature));
    // CR 702.103b: Gain the Aura subtype while bestowed. Idempotent push.
    if !obj.card_types.subtypes.iter().any(|s| s == "Aura") {
        obj.card_types.subtypes.push("Aura".to_string());
    }
    if !obj.base_card_types.subtypes.iter().any(|s| s == "Aura") {
        obj.base_card_types.subtypes.push("Aura".to_string());
    }
    // CR 702.103b: Gain `enchant creature`. The existing Aura targeting code
    // in `continue_with_prepared` reads `obj.keywords` for `Keyword::Enchant`,
    // so this grant routes the bestow Aura through the same target-selection
    // pipeline as a hard-cast Aura.
    let enchant_creature = Keyword::Enchant(TargetFilter::Typed(
        crate::types::ability::TypedFilter::creature(),
    ));
    if !obj
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Enchant(_)))
    {
        obj.keywords.push(enchant_creature.clone());
    }
    if !obj
        .base_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Enchant(_)))
    {
        obj.base_keywords.push(enchant_creature);
    }
    obj.bestow_form = Some(crate::game::game_object::BestowFormState);
}

/// CR 702.103e + CR 702.103f: Inverse of `apply_bestow_aura_form`. Restores the
/// Creature core type, removes the synthesized Aura subtype, and removes the
/// granted `enchant creature` keyword. Called when:
///   * Resolution-time illegal target (CR 702.103e) — revert before the spell
///     finishes resolving so it ETBs as a normal creature.
///   * Bestow Aura on the battlefield becomes unattached (CR 702.103f) —
///     revert and skip the unattached-aura SBA so it stays as an enchantment
///     creature.
///
/// Idempotent: a no-op if the object is not in bestow form.
pub(crate) fn revert_bestow_aura_form(obj: &mut crate::game::game_object::GameObject) {
    if obj.bestow_form.is_none() {
        return;
    }
    use crate::types::card_type::CoreType;
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        obj.card_types.core_types.push(CoreType::Creature);
    }
    if !obj.base_card_types.core_types.contains(&CoreType::Creature) {
        obj.base_card_types.core_types.push(CoreType::Creature);
    }
    obj.card_types.subtypes.retain(|s| s != "Aura");
    obj.base_card_types.subtypes.retain(|s| s != "Aura");
    obj.keywords.retain(|k| !matches!(k, Keyword::Enchant(_)));
    obj.base_keywords
        .retain(|k| !matches!(k, Keyword::Enchant(_)));
    obj.bestow_form = None;
}

/// CR 702.140a + CR 108.3 (B1): The mutate spell's target — "a non-Human creature
/// with the same owner as this spell." For a cast spell the owner is the caster,
/// so this is a non-Human creature the caster owns. Built from existing typed
/// primitives (no new `FilterProp`/variant): `Creature`, `Non(Subtype("Human"))`,
/// and `Owned { controller: You }`. Single authority used by both the cast-offer
/// gate and the target-attachment branch in `continue_with_prepared`. Also reused
/// by the CR 608.2b resolution-time re-validation in `stack::resolve_top` so the
/// cast-time and resolution-time legality predicates cannot drift.
pub(crate) fn mutate_target_filter() -> TargetFilter {
    use crate::types::ability::{ControllerRef, FilterProp, TypeFilter, TypedFilter};
    TargetFilter::Typed(
        TypedFilter::creature()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                "Human".to_string(),
            ))))
            .properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }]),
    )
}

/// CR 702.140a: Mark a hand/stack object as a mutating creature spell. Unlike
/// Bestow, mutate is NOT a type-changing effect — the spell stays a creature
/// spell (CR 702.140a) — so this only sets the typed marker. The target
/// requirement is attached in `continue_with_prepared` (the `mutate_form` branch,
/// mirroring the Aura/Enchant target-slot path). Idempotent.
fn apply_mutate_form(obj: &mut crate::game::game_object::GameObject) {
    if obj.mutate_form.is_some() {
        return;
    }
    obj.mutate_form = Some(crate::game::game_object::MutateFormState);
}

/// CR 702.140b: Clear the mutate marker. Called when the mutating creature
/// spell's target is illegal at resolution (the spell reverts to a plain creature
/// spell and enters the battlefield normally), and on a cast-preparation error so
/// a failed mutate cast leaves the hand object in its printed form. Idempotent.
pub fn revert_mutate_form(state: &mut GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.mutate_form = None;
    }
}

/// CR 702.103e + CR 702.103f: Public entry-point for bestow form revert.
/// Used by stack resolution (illegal-target revert) and SBA (unattached
/// override). Marks layers dirty so any continuous effects re-evaluate
/// against the new (creature) characteristics on the next layers pass.
pub fn revert_bestow_form(state: &mut GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        if obj.bestow_form.is_some() {
            revert_bestow_aura_form(obj);
            crate::game::layers::mark_layers_full(state);
        }
    }
}

/// CR 702.103a: Handle Bestow cost choice and proceed with casting. On
/// `AlternativeCastDecision::Alternative`, applies the bestow type-changing
/// effect to the hand object (CR 702.103b) and prepares the cast with
/// `CastingVariant::Bestow` (which substitutes the bestow mana cost for the
/// printed mana cost). On `Normal`, the cast proceeds as the printed Creature
/// spell.
///
/// Mirrors `handle_evoke_cost_choice` for the cost-selection branch and
/// `handle_adventure_choice` for the object-mutation-before-prepare branch.
pub fn handle_bestow_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_bestow_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_bestow_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    // Exhaustive match so adding a third decision variant (e.g., `Decline`)
    // is a compile error here rather than silently routing through one of
    // the two existing branches.
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        // CR 702.103b: Apply the type-changing bestow effect to the hand object
        // BEFORE preparing the cast, so timing/cost checks (Aura is a permanent
        // spell, sorcery-speed) and the targeting branch in
        // `continue_with_prepared` see the Aura form. The mutation is reverted
        // by `revert_bestow_form` if the spell is countered or its target is
        // illegal at resolution (CR 702.103e), and persists through the
        // stack→battlefield transition until the Aura becomes unattached
        // (CR 702.103f).
        if let Some(obj) = state.objects.get_mut(&object_id) {
            apply_bestow_aura_form(obj);
        }
        let mut prepared = match prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Bestow),
        ) {
            Ok(p) => p,
            Err(e) => {
                // Roll back the bestow type-changing mutation so the hand
                // object is left in its printed creature form for any retry
                // (the player got an error — they didn't commit to bestow).
                revert_bestow_form(state, object_id);
                return Err(e);
            }
        };
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.140a: Public entry-point for the Mutate cost choice (auto payment mode).
pub fn handle_mutate_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_mutate_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

/// CR 702.140a-c: Handle the Mutate cost choice and proceed with casting. On
/// `AlternativeCastDecision::Alternative`, mark the hand object as a mutating
/// creature spell (`apply_mutate_form`) BEFORE preparing the cast, then prepare
/// with `CastingVariant::Mutate` (which substitutes the mutate mana cost). On
/// `Normal`, the cast proceeds as the printed creature spell. Mirrors
/// `handle_bestow_cost_choice_with_payment_mode`.
pub fn handle_mutate_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    // Exhaustive match (a future third decision variant is a compile error here).
    match decision {
        AlternativeCastDecision::Alternative => {
            // CR 702.140a: mark the spell as mutating BEFORE preparing the cast,
            // so the `continue_with_prepared` target-attachment branch (mirroring
            // the Aura/Enchant path) sees the mutate form and requests the
            // non-Human creature target. Reverted by `revert_mutate_form` on a
            // preparation error or on an illegal target at resolution
            // (CR 702.140b).
            if let Some(obj) = state.objects.get_mut(&object_id) {
                apply_mutate_form(obj);
            }
            let mut prepared = match prepare_spell_cast_with_variant_override(
                state,
                player,
                object_id,
                Some(CastingVariant::Mutate),
            ) {
                Ok(p) => p,
                Err(e) => {
                    revert_mutate_form(state, object_id);
                    return Err(e);
                }
            };
            prepared.payment_mode = payment_mode;
            continue_with_prepared(state, player, prepared, events)
        }
        AlternativeCastDecision::Normal => {
            continue_cast_from_prepared(state, player, object_id, payment_mode, events)
        }
    }
}

/// CR 702.148a-b + CR 612: Apply the cleave text-changing effect to a hand
/// object by swapping in the bracket-removed ability set parsed at build time
/// (`obj.cleave_variant`). All four ability classes are replaced on both the
/// live and base fields (mirroring `apply_bestow_aura_form`'s dual-field write)
/// so the swap survives any layer-evaluation reset that anchors on base values.
///
/// The pre-swap state is captured into `obj.cleave_form` (a typed marker
/// mirroring `bestow_form`) so the printed form can be restored two ways: on a
/// cast-preparation `Err` via `revert_cleave_text_change`, and — critically —
/// when the spell leaves the stack via `apply_zone_exit_cleanup` (CR 702.148a:
/// the abilities function only while the spell is on the stack). Returns `false`
/// (no swap, no marker) if the object carries no `cleave_variant` — the cleave
/// path is only offered when the variant is present, so `false` here means a
/// malformed call rather than a normal cast and the caller falls through to a
/// printed-cost cast.
fn apply_cleave_text_change(obj: &mut crate::game::game_object::GameObject) -> bool {
    let Some(variant) = obj.cleave_variant.clone() else {
        return false;
    };
    obj.cleave_form = Some(crate::game::game_object::CleaveFormState {
        abilities: std::sync::Arc::clone(&obj.abilities),
        triggers: obj.trigger_definitions.clone(),
        statics: obj.static_definitions.clone(),
        replacements: obj.replacement_definitions.clone(),
        base_abilities: std::sync::Arc::clone(&obj.base_abilities),
        base_triggers: std::sync::Arc::clone(&obj.base_trigger_definitions),
        base_statics: std::sync::Arc::clone(&obj.base_static_definitions),
        base_replacements: std::sync::Arc::clone(&obj.base_replacement_definitions),
    });
    // CR 612: the cleave-cost text replaces the spell's printed text. Swap all
    // four ability classes — only `abilities` differs for the published cleave
    // cards, but projecting the full set is defensive and future-proof.
    obj.abilities = std::sync::Arc::new(variant.abilities.clone());
    obj.trigger_definitions = variant.triggers.clone().into();
    obj.static_definitions = variant.static_abilities.clone().into();
    obj.replacement_definitions = variant.replacements.clone().into();
    obj.base_abilities = std::sync::Arc::new(variant.abilities);
    obj.base_trigger_definitions = std::sync::Arc::new(variant.triggers);
    obj.base_static_definitions = std::sync::Arc::new(variant.static_abilities);
    obj.base_replacement_definitions = std::sync::Arc::new(variant.replacements);
    true
}

/// CR 702.148a-b: Restore the printed ability set captured in `obj.cleave_form`
/// by `apply_cleave_text_change`, clearing the marker. Used on the
/// cast-preparation `Err` path (so a failed cleave cast leaves the hand object
/// in its printed form for any retry) and by `apply_zone_exit_cleanup` when the
/// cleave spell leaves the stack. Idempotent: a no-op if no cleave form is live.
pub(crate) fn revert_cleave_text_change(obj: &mut crate::game::game_object::GameObject) {
    let Some(snapshot) = obj.cleave_form.take() else {
        return;
    };
    obj.abilities = snapshot.abilities;
    obj.trigger_definitions = snapshot.triggers;
    obj.static_definitions = snapshot.statics;
    obj.replacement_definitions = snapshot.replacements;
    obj.base_abilities = snapshot.base_abilities;
    obj.base_trigger_definitions = snapshot.base_triggers;
    obj.base_static_definitions = snapshot.base_statics;
    obj.base_replacement_definitions = snapshot.base_replacements;
}

/// CR 702.148a-b + CR 612 + CR 118.9: Handle the Cleave cost choice and proceed
/// with casting. On `AlternativeCastDecision::Alternative`, apply the cleave
/// text-changing effect to the hand object BEFORE preparing the cast (so
/// `combined_spell_ability_def` reads the bracket-removed abilities), then
/// prepare with `CastingVariant::Cleave` (which substitutes the cleave mana cost
/// for the printed mana cost). On `Normal`, the cast proceeds as the printed
/// spell with no text change.
///
/// Mirrors `handle_bestow_cost_choice_with_payment_mode` for the
/// object-mutation-before-prepare seam — the Overload in-place transform seam
/// (which mutates the prepared spell ability after `combined_spell_ability_def`
/// has already read it) is not usable for cleave because the text change must be
/// visible to that read.
pub fn handle_cleave_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cleave_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cleave_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    // Exhaustive match so adding a third decision variant (e.g., `Decline`)
    // is a compile error here rather than silently routing through one of
    // the two existing branches.
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        // CR 702.148a-b + CR 612: Apply the cleave text-changing effect to the
        // hand object BEFORE preparing the cast. The pre-swap snapshot is stored
        // in `obj.cleave_form` so the printed form can be restored if
        // preparation fails — and, while the spell is on the stack, so the
        // zone-exit cleanup can revert the text change when the spell leaves the
        // stack (CR 702.148a).
        if let Some(obj) = state.objects.get_mut(&object_id) {
            apply_cleave_text_change(obj);
        }
        let mut prepared = match prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Cleave),
        ) {
            Ok(p) => p,
            Err(e) => {
                // Roll back the cleave text change so the hand object is left
                // in its printed form for any retry.
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    revert_cleave_text_change(obj);
                }
                return Err(e);
            }
        };
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.74a: Handle Evoke cost choice and proceed with casting. On
/// `AlternativeCastDecision::Alternative`, the cast is prepared with
/// `CastingVariant::Evoke` (which substitutes the evoke mana cost for the
/// printed mana cost). On `Normal`, the cast proceeds normally (no variant
/// override → `Normal`).
pub fn handle_evoke_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_evoke_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_evoke_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    // Exhaustive match so adding a third decision variant (e.g., `Decline`)
    // is a compile error here rather than silently routing through one of
    // the two existing branches.
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Evoke),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.119a-c: Handle Emerge cost choice and proceed with casting. On
/// `AlternativeCastDecision::Alternative`, the cast is prepared with
/// `CastingVariant::Emerge`, which substitutes the emerge mana cost and then
/// requires sacrificing a creature as the first cost component.
pub fn handle_emerge_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_emerge_cost_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        decision,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_emerge_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Emerge),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.109a: Resolve the player's Dash cost choice. Mirrors
/// `handle_evoke_cost_choice_with_payment_mode` — `Alternative` opts into
/// `CastingVariant::Dash` (which substitutes the dash mana cost and installs the
/// resolution riders), `Normal` casts for the printed cost.
pub fn handle_dash_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Dash),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.152a: Resolve the player's Blitz cost choice. Mirrors
/// `handle_evoke_cost_choice_with_payment_mode` — `Alternative` opts into
/// `CastingVariant::Blitz` (which substitutes the blitz mana cost and installs
/// the resolution riders), `Normal` casts for the printed cost.
pub fn handle_blitz_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Blitz),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.137a: Resolve the player's Spectacle cost choice. Mirrors
/// `handle_blitz_cost_choice_with_payment_mode` — `Alternative` opts into
/// `CastingVariant::Spectacle` (which substitutes the spectacle mana cost), and
/// `Normal` casts for the printed cost. Spectacle has no resolution riders; it
/// only changes how the cost is paid (CR 702.137a). The opponent-lost-life gate
/// is enforced at offer time, so reaching this handler means the option was legal.
pub fn handle_spectacle_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    let alt_path = match decision {
        AlternativeCastDecision::Alternative => true,
        AlternativeCastDecision::Normal => false,
    };
    if alt_path {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Spectacle),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 702.76a: Resolve the player's Prowl cost choice. Mirrors
/// `handle_spectacle_cost_choice_with_payment_mode` — `Alternative` opts into
/// `CastingVariant::Prowl` (which substitutes the prowl mana cost), and `Normal`
/// casts for the printed cost. Prowl is a pure cost substitution (CR 702.76a);
/// the prowl provenance tag is applied at resolution (stack.rs). The
/// dealt-combat-damage gate is enforced at offer time, so reaching this handler
/// means the option was legal.
pub fn handle_prowl_cost_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    decision: crate::types::actions::AlternativeCastDecision,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::actions::AlternativeCastDecision;
    if matches!(decision, AlternativeCastDecision::Alternative) {
        let mut prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Prowl),
        )?;
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// Shared continuation: call prepare_spell_cast and run the standard casting
/// pipeline (modal → targeting → payment). Extracted so handle_warp_cost_choice
/// and handle_cast_spell can share the same post-prepare logic.
fn continue_cast_from_prepared(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut prepared = prepare_spell_cast(state, player, object_id)?;
    if prepared.casting_variant == CastingVariant::Disturb {
        return continue_cast_with_alternative_spell_face(
            state,
            player,
            object_id,
            CastingVariant::Disturb,
            payment_mode,
            events,
        );
    }
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

fn continue_cast_with_alternative_spell_face(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    variant: CastingVariant,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 712.8c + CR 712.11a-c: cast-transformed/converted spells are put on
    // the stack back face up and evaluated using only back-face characteristics.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        swap_to_alternative_spell_face(obj);
    }
    let mut prepared =
        match prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant)) {
            Ok(prepared) => prepared,
            Err(err) => {
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    swap_to_alternative_spell_face(obj);
                }
                return Err(err);
            }
        };
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

fn continue_cast_with_variant(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    variant: CastingVariant,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let CastingVariant::GraveyardPermission {
        source,
        frequency: CastFrequency::OncePerTurnPerPermanentType,
        slot_type: None,
        ..
    } = variant
    {
        let slots = available_permanent_type_slots(state, source, object_id);
        if slots.len() > 1 {
            let card_id = state
                .objects
                .get(&object_id)
                .map(|obj| obj.card_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            return Ok(WaitingFor::ChoosePermanentTypeSlot {
                player,
                object_id,
                card_id,
                source,
                payment_mode,
                available_slots: slots,
            });
        }
    }

    if variant == CastingVariant::Bestow {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            apply_bestow_aura_form(obj);
        }
        let mut prepared =
            match prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))
            {
                Ok(prepared) => prepared,
                Err(err) => {
                    revert_bestow_form(state, object_id);
                    return Err(err);
                }
            };
        prepared.payment_mode = payment_mode;
        return continue_with_prepared(state, player, prepared, events);
    }

    if matches!(
        variant,
        CastingVariant::MoreThanMeetsTheEye | CastingVariant::Disturb
    ) {
        return continue_cast_with_alternative_spell_face(
            state,
            player,
            object_id,
            variant,
            payment_mode,
            events,
        );
    }

    let mut prepared =
        prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

pub fn handle_casting_variant_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    options: &[CastingVariantChoiceOption],
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_casting_variant_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        options,
        index,
        CastPaymentMode::Auto,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn handle_casting_variant_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    options: &[CastingVariantChoiceOption],
    index: usize,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    let option = options
        .get(index)
        .ok_or_else(|| EngineError::InvalidAction("Invalid cast variant choice".to_string()))?;
    let fresh_options = casting_variant_choice_set(state, player, object_id).options;
    if !fresh_options
        .iter()
        .any(|fresh| fresh.variant == option.variant)
    {
        return Err(EngineError::ActionNotAllowed(
            "Chosen cast variant is no longer legal".to_string(),
        ));
    }
    continue_cast_with_variant(
        state,
        player,
        object_id,
        option.variant,
        payment_mode,
        events,
    )
}

/// CR 702.190a + b: Cast a spell from HAND via the Sneak alternative cost.
///
/// Per CR 702.190a, "Sneak [cost]" reads: "Any time you could cast an instant
/// during your declare blockers step, you may cast this spell by paying
/// [cost] and returning an unblocked creature you control to its owner's
/// hand rather than paying this spell's mana cost." This applies to any card
/// type — creature, artifact, enchantment, planeswalker, sorcery, or instant.
///
/// Validates:
/// - `hand_object` is in `player`'s hand and matches `card_id`.
/// - `hand_object` has an effective Sneak cost (printed keyword or rider-
///   granted, via `effective_sneak_cost`).
/// - `creature_to_return` is an unblocked attacker controlled by `player`.
///
/// Builds a `CastingVariant::Sneak { returned_creature, placement }` override
/// where `placement` is `Some(SneakPlacement { .. })` only for permanent
/// spells (CR 702.190b) — instants and sorceries carry `None` and resolve
/// normally without an alongside-attacker placement.
///
/// Routes through the standard casting pipeline. `prepare_spell_cast_with_
/// variant_override` enforces declare-blockers timing (`restrictions.rs`) and
/// selects the Sneak mana cost. The returned creature is bounced to its
/// owner's hand at `finalize_cast_to_stack` (`casting_costs.rs`) as part of
/// paying the Sneak cost.
pub fn handle_cast_spell_as_sneak(
    state: &mut GameState,
    player: PlayerId,
    hand_object: ObjectId,
    card_id: CardId,
    creature_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_as_sneak_with_payment_mode(
        state,
        player,
        hand_object,
        card_id,
        creature_to_return,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cast_spell_as_sneak_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    hand_object: ObjectId,
    card_id: CardId,
    creature_to_return: ObjectId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Sanity: object exists, matches card_id, and is in the caster's hand.
    // CR 702.190a: Sneak is a hand-cast alt-cost; graveyard/exile casts are
    // not legal under this keyword.
    let obj = state.objects.get(&hand_object).ok_or_else(|| {
        EngineError::InvalidAction(format!("Object {hand_object:?} does not exist"))
    })?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {hand_object:?} does not match card_id {card_id:?}",
        )));
    }
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "Sneak-cast requires a hand card owned by the caster".to_string(),
        ));
    }

    // CR 702.190a: Must have an effective Sneak cost (intrinsic or granted).
    if super::keywords::effective_sneak_cost(state, hand_object).is_none() {
        return Err(EngineError::ActionNotAllowed(
            "Card has no Sneak permission".to_string(),
        ));
    }

    // CR 702.190b: Capture placement data from the returned creature's
    // `AttackerInfo` only for permanent spells — CR 702.190b applies only to
    // "a permanent spell whose sneak cost was paid" (CR 110.4b). Non-permanent
    // spells (instants/sorceries) resolve normally with no alongside-attacker
    // step. Delegates to the shared `stack::is_permanent_spell` helper so the
    // CR 110.4b definition lives in one place.
    let is_permanent_spell = super::stack::is_permanent_spell(state, hand_object);

    // CR 702.190a: The returned creature must be an unblocked attacker
    // controlled by `player`.
    let combat = state
        .combat
        .as_ref()
        .ok_or_else(|| EngineError::ActionNotAllowed("No active combat".to_string()))?;
    let attacker_info = combat
        .attackers
        .iter()
        .find(|a| a.object_id == creature_to_return)
        .cloned()
        .ok_or_else(|| {
            EngineError::ActionNotAllowed("Creature to return is not an attacker".to_string())
        })?;
    let is_blocked = combat
        .blocker_assignments
        .get(&creature_to_return)
        .is_some_and(|blockers| !blockers.is_empty());
    if is_blocked {
        return Err(EngineError::ActionNotAllowed(
            "Attacker is blocked".to_string(),
        ));
    }
    let returned_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or_else(|| EngineError::InvalidAction("Creature to return not found".to_string()))?;
    if returned_obj.controller != player {
        return Err(EngineError::ActionNotAllowed(
            "You don't control that creature".to_string(),
        ));
    }
    // CR 506.4 + CR 702.190a: Sneak may only return an unblocked attacker still
    // on the battlefield.
    if !super::combat::is_attacker_in_play(state, creature_to_return) {
        return Err(EngineError::ActionNotAllowed(
            "Attacker is no longer on the battlefield".to_string(),
        ));
    }

    let placement = if is_permanent_spell {
        Some(SneakPlacement {
            defender: attacker_info.defending_player,
            attack_target: attacker_info.attack_target,
        })
    } else {
        None
    };
    let variant = CastingVariant::Sneak {
        returned_creature: creature_to_return,
        placement,
    };

    let mut prepared =
        prepare_spell_cast_with_variant_override(state, player, hand_object, Some(variant))?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.188a: Cast a spell from HAND via the Web-slinging alternative cost.
///
/// Web-slinging returns a tapped creature the caster controls as part of the
/// casting cost and substitutes the keyword's mana cost for the spell's printed
/// mana cost. Unlike Sneak, it grants no special timing permission and does not
/// put permanents onto the battlefield attacking.
pub fn handle_cast_spell_as_web_slinging(
    state: &mut GameState,
    player: PlayerId,
    hand_object: ObjectId,
    card_id: CardId,
    creature_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_as_web_slinging_with_payment_mode(
        state,
        player,
        hand_object,
        card_id,
        creature_to_return,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cast_spell_as_web_slinging_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    hand_object: ObjectId,
    card_id: CardId,
    creature_to_return: ObjectId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state.objects.get(&hand_object).ok_or_else(|| {
        EngineError::InvalidAction(format!("Object {hand_object:?} does not exist"))
    })?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {hand_object:?} does not match card_id {card_id:?}",
        )));
    }
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "Web-slinging requires a hand card owned by the caster".to_string(),
        ));
    }

    if super::keywords::effective_web_slinging_cost(state, player, hand_object).is_none() {
        return Err(EngineError::ActionNotAllowed(
            "Card has no Web-slinging permission".to_string(),
        ));
    }

    let returned_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or_else(|| EngineError::InvalidAction("Creature to return not found".to_string()))?;
    if returned_obj.zone != Zone::Battlefield
        || returned_obj.controller != player
        || !returned_obj.tapped
        || !returned_obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
    {
        return Err(EngineError::ActionNotAllowed(
            "Web-slinging requires a tapped creature you control".to_string(),
        ));
    }

    let variant = CastingVariant::WebSlinging {
        returned_creature: creature_to_return,
    };
    let mut prepared =
        prepare_spell_cast_with_variant_override(state, player, hand_object, Some(variant))?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.188a + CR 601.2: Returns whether the player can cast this hand card
/// via Web-slinging with the specified tapped creature as the return cost.
///
/// This deliberately routes through the real casting entry point on a cloned
/// state so legal-action generation and action execution share timing, target,
/// restriction, and auto-mana-payment behavior.
pub fn can_cast_spell_as_web_slinging_now(
    state: &GameState,
    player: PlayerId,
    hand_object: ObjectId,
    creature_to_return: ObjectId,
) -> bool {
    let Some(card_id) = state.objects.get(&hand_object).map(|obj| obj.card_id) else {
        return false;
    };
    let mut simulated = state.clone();
    let mut events = Vec::new();
    handle_cast_spell_as_web_slinging_with_payment_mode(
        &mut simulated,
        player,
        hand_object,
        card_id,
        creature_to_return,
        CastPaymentMode::Auto,
        &mut events,
    )
    .is_ok()
}

/// CR 601.2b + CR 118.9a: Cast a spell from hand for free via a
/// `StaticMode::CastFromHandFree` permission source (Zaffai).
///
/// Validates:
/// - `object_id` is in the caster's hand and matches `card_id`.
/// - `source_id` controls an active `CastFromHandFree` static whose filter
///   matches `object_id`, and its once-per-turn slot (when applicable) has
///   not been consumed this turn.
///
/// Builds a `CastingVariant::HandPermission { source, frequency }` override and
/// routes through the standard casting pipeline. On finalize-to-stack,
/// `casting_costs.rs` records `source_id` in `hand_cast_free_permissions_used`
/// for `OncePerTurn` frequencies.
///
/// Omniscience's `Unlimited` silent path is NOT routed through here — it uses
/// `GameAction::CastSpell` with `CastingVariant::Normal` and a `NoCost`
/// short-circuit. This entry point is reserved for the opt-in choice surface.
pub fn handle_cast_spell_for_free(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_for_free_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        source_id,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cast_spell_for_free_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    source_id: ObjectId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    // CR 601.2b: Spell must be in the caster's hand.
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellForFree requires a hand card owned by the caster".to_string(),
        ));
    }
    // CR 601.2b + CR 400.7: The named granting source's permission must be
    // active and filter-matched. Source-specific validation avoids accepting a
    // stale legal action for one source only because an earlier battlefield
    // source also matches the spell.
    let frequency =
        cast_free_permission_from_source(state, player, obj, source_id).ok_or_else(|| {
            EngineError::ActionNotAllowed(
                "Named CastFromHandFree permission source does not admit this spell".to_string(),
            )
        })?;
    let variant = CastingVariant::HandPermission {
        source: source_id,
        frequency,
    };
    let mut prepared =
        prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.94a + CR 603.11: Cast a spell from hand via its Miracle alternative
/// mana cost after the player accepted the reveal prompt. Validates:
/// - `object_id` matches `card_id` and is in the caster's hand.
/// - The card still has `Keyword::Miracle(cost)` (layer effects between queue
///   and accept may have removed it — in that case the cast fails cleanly).
///
/// Builds a `CastingVariant::Miracle` override and routes through the shared
/// casting pipeline; `prepare_spell_cast_with_variant_override` substitutes
/// the miracle cost for the printed mana cost via the `Keyword::Miracle`
/// payload it discovers on the object.
pub fn handle_cast_spell_as_miracle(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_as_miracle_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cast_spell_as_miracle_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    // CR 702.94a: Miracle-revealed spells are cast from hand.
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellAsMiracle requires a hand card owned by the caster".to_string(),
        ));
    }
    // CR 702.94a: The keyword must still be present — it can have been removed
    // by layers / replacement effects between offer time and accept time.
    let has_miracle = obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Miracle(_)));
    if !has_miracle {
        return Err(EngineError::ActionNotAllowed(
            "Card no longer has miracle".to_string(),
        ));
    }
    let mut prepared = prepare_spell_cast_with_variant_override(
        state,
        player,
        object_id,
        Some(CastingVariant::Miracle),
    )?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.35a: Cast a discarded card from exile via its Madness alternative
/// mana cost after the madness triggered ability resolves.
pub fn handle_cast_spell_as_madness(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_as_madness_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        CastPaymentMode::Auto,
        events,
    )
}

pub fn handle_cast_spell_as_madness_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    if obj.zone != Zone::Exile || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellAsMadness requires an exiled card owned by the caster".to_string(),
        ));
    }
    let has_madness = obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Madness(_)));
    if !has_madness {
        return Err(EngineError::ActionNotAllowed(
            "Card no longer has madness".to_string(),
        ));
    }
    let mut prepared = prepare_spell_cast_with_variant_override(
        state,
        player,
        object_id,
        Some(CastingVariant::Madness),
    )?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

pub(super) struct ResolutionCastRequest {
    pub(super) constraint: Option<crate::types::ability::CastPermissionConstraint>,
    pub(super) cast_transformed: bool,
    pub(super) cleanup: crate::types::ability::ResolutionCastCleanup,
    pub(super) exile_instead_of_graveyard_on_resolve: bool,
}

/// CR 608.2g: Cast a Cascade/Discover hit *during resolution* of its source
/// spell, rather than granting a lingering permission that requires a separate
/// later `CastSpell`. The single authority that constructs the
/// cast-during-resolution `ExileWithAltCost` permission and drives the cast.
///
/// Pushes a cost-zeroing `ExileWithAltCost` permission carrying `constraint`
/// (the resulting-MV gate, evaluated at finalization once X is known),
/// `cast_transformed` (for Siege victory casts), and `cleanup` (the misses +
/// reject disposition, so a cast-time rejection can still bottom/hand the hit).
/// Then prepares and continues the cast on the `Auto` payment mode. The
/// returned `WaitingFor` falls through
/// `run_post_action_pipeline` normally, which fires the hit's own cast-triggers
/// (CR 702.85a, etc.) and returns priority to the active player — satisfying CR
/// 608.2g's "no player receives priority after it's cast" without any explicit
/// suppression (the opponent only gets priority later via normal passing).
///
/// Every during-resolution caster passes a `cleanup` — it is the marker that
/// arms the CR 608.2g timing bypass in `restrictions::check_spell_timing`, so a
/// sorcery cast while its trigger is still on the stack is not blocked by the
/// sorcery-speed / empty-stack / active-player gates. Cascade/Discover carry
/// the dig misses + an MV-reject disposition that bottoms/hands the hit. Suspend
/// (CR 702.62a) carries an empty-misses / `RemainExiled` cleanup whose sole
/// purpose is to arm that timing bypass — it has no dig and no MV gate, so it
/// never enters the cascade reject path.
pub(super) fn initiate_cast_during_resolution(
    state: &mut GameState,
    player: PlayerId,
    hit_card: ObjectId,
    request: ResolutionCastRequest,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ResolutionCastRequest {
        constraint,
        cast_transformed,
        cleanup,
        exile_instead_of_graveyard_on_resolve,
    } = request;
    if let Some(obj) = state.objects.get_mut(&hit_card) {
        // CR 601.2a + CR 601.2i: zero-cost permission consumed by
        // `prepare_spell_cast_with_variant_override`'s exile alt-cost scan.
        // `resolution_cleanup` is always `Some` here: it is the
        // cast-during-resolution discriminator that arms the CR 608.2g timing
        // bypass. Cascade/Discover carry their dig misses + MV-reject
        // disposition; Suspend (CR 702.62a) carries an empty-misses /
        // `RemainExiled` cleanup that has no dig and no MV gate, so it never
        // enters the cascade reject path.
        obj.casting_permissions
            .push(CastingPermission::ExileWithAltCost {
                cost: ManaCost::zero(),
                cast_transformed,
                constraint,
                granted_to: Some(player),
                resolution_cleanup: Some(cleanup),
                duration: None,
                exile_instead_of_graveyard_on_resolve,
                enters_with_counter: None,
            });
        if exile_instead_of_graveyard_on_resolve {
            crate::game::casting_costs::apply_exile_instead_of_graveyard_rider(state, hit_card);
        }
    }
    let mut prepared = prepare_spell_cast_with_variant_override(state, player, hit_card, None)?;
    prepared.payment_mode = CastPaymentMode::Auto;
    continue_with_prepared(state, player, prepared, events)
}

/// Cast a spell from hand (or command zone, exile, graveyard in Commander/alternate-cost formats).
pub fn handle_cast_spell(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_cast_spell_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        CastPaymentMode::Auto,
        events,
    )
}

fn normal_cast_choice_cost_and_affordability(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    obj: &GameObject,
) -> (ManaCost, bool) {
    // CR 601.2b + CR 118.9a: `Unlimited` `CastFromHandFree` (Omniscience)
    // replaces the printed mana cost with nothing on the normal path. Every
    // hand alternative-cost prompt must treat that path as affordable and
    // display `NoCost`; otherwise an affordable alternative cost can hide the
    // free normal cast.
    if unlimited_hand_cast_free_applies(state, player, obj, CastingVariant::Normal) {
        return (ManaCost::NoCost, true);
    }

    // CR 601.2f + CR 118.9d: normal-path affordability and displayed cost
    // reflect active cost modifiers before comparing against alternative costs.
    let normal_cost = apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
        .unwrap_or_else(|| obj.mana_cost.clone());
    let normal_affordable = can_pay_cost_after_auto_tap(state, player, object_id, &normal_cost);
    (normal_cost, normal_affordable)
}

pub fn handle_cast_spell_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 601.2a: Validate object identity and zone eligibility. The
    // candidate generator gates these upstream, but defense-in-depth catches
    // stale or illegal actions that bypass the generator (e.g., AI fallback
    // paths, multiplayer desync, or hand-crafted JS payloads).
    let obj = state.objects.get(&object_id).ok_or_else(|| {
        EngineError::InvalidAction(format!("Object {:?} does not exist", object_id,))
    })?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {:?} does not match card_id {:?}",
            object_id, card_id
        )));
    }
    // CR 601.2a: A spell can only be cast from a zone that permits it.
    // Hand and Command are always eligible. Exile, Graveyard, and Library
    // require an explicit permission (keyword or static). Stack is never
    // eligible (the spell is already on the stack). This mirrors the
    // zone check in `prepare_spell_cast` but catches illegal casts before
    // any keyword-choice prompts (Adventure, Warp, Evoke, Overload) that
    // would fire for hand-only objects.
    match obj.zone {
        Zone::Hand => {
            // CR 202.1b: A card with no mana cost (Inevitable Betrayal and other
            // suspend-only cards) has an unpayable cost.
            // CR 118.6: it can't be cast from hand by paying that cost; its legal
            // plays are via an effect/keyword (e.g. Suspend's exile activation),
            // which are separate actions/zones.
            // CR 118.6a: an effect that lets you cast it WITHOUT paying its mana
            // cost may still cast it — the `Unlimited` `CastFromHandFree`
            // permission (Omniscience) takes this normal path, so don't block it.
            // Defense-in-depth — the candidate generator already excludes the
            // no-permission case via `can_cast_object_now`.
            if matches!(obj.mana_cost, ManaCost::NoCost)
                && !unlimited_hand_cast_free_applies(state, player, obj, CastingVariant::Normal)
            {
                return Err(EngineError::InvalidAction(format!(
                    "Cannot cast {object_id:?} from hand — it has no mana cost (CR 118.6)",
                )));
            }
        }
        Zone::Command if state.format_config.command_zone && obj.uses_command_zone_rules() => {}
        Zone::Exile | Zone::Graveyard | Zone::Library => {
            // These zones are allowed only with permission — defer the
            // full permission check to `prepare_spell_cast` which already
            // validates each zone-specific permission exhaustively. No
            // early-reject here; just pass through.
        }
        zone => {
            return Err(EngineError::InvalidAction(format!(
                "Cannot cast {:?} from {:?} — not a castable zone",
                object_id, zone,
            )));
        }
    }

    // CR 707.10: `resolving_stack_entry` may intentionally persist after a
    // resolution for deferred self-copy choices, but a fresh normal cast starts
    // a new stack-object announcement outside that old resolution context.
    state.resolving_stack_entry = None;

    // CR 715.3 / CR 720.3: Adventure-family cards from hand (or a commander cast
    // from the command zone) require choosing the normal creature face or
    // alternative spell face.
    if let Some(obj) = state.objects.get(&object_id) {
        if cast_face_choice_offered_from_zone(state, obj) && alternative_spell_layout(obj).is_some()
        {
            return Ok(WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Adventure {
                    object_id,
                    card_id,
                    payment_mode,
                },
            });
        }
    }

    // CR 712.11b + CR 903.8: Spell//spell Modal DFCs from hand — or from the
    // command zone when the card is the player's commander — require choosing
    // which face to cast (Esika, God of the Tree // The Prismatic Bridge, etc.).
    // The `ChooseModalFace` handler swaps to the chosen face (if back) and
    // re-enters this function; the swap clears the back face's Modal
    // `layout_kind`, so the re-entry casts the chosen face without re-prompting.
    if let Some(obj) = state.objects.get(&object_id) {
        if cast_spell_face_choice_offered_from_zone(state, obj) {
            return Ok(WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode,
            });
        }
    }

    let variant_choices = casting_variant_choice_set(state, player, object_id);
    if variant_choices.options.len() > 1 {
        return Ok(WaitingFor::CastingVariantChoice {
            player,
            object_id,
            card_id,
            payment_mode,
            options: variant_choices.options,
        });
    }
    if variant_choices.had_multiple_candidates {
        if let Some(option) = variant_choices.options.first() {
            return continue_cast_with_variant(
                state,
                player,
                object_id,
                option.variant,
                payment_mode,
                events,
            );
        }
    }

    // Warp: when a hand card has Keyword::Warp and both costs are affordable,
    // present a choice. Auto-skip when only one cost is viable.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(warp_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Warp(cost) => Some(cost.clone()),
                _ => None,
            }) {
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let warp_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, warp_cost.clone())
                        .unwrap_or_else(|| warp_cost.clone());
                let warp_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &warp_cost_eff);
                if normal_affordable && warp_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Warp,
                        normal_cost,
                        alternative_cost: Some(warp_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                // If only normal is affordable, skip warp — prepare_spell_cast will
                // still detect Warp keyword but the player chose normal by necessity.
                // We handle this in handle_warp_cost_choice's override logic.
                if normal_affordable && !warp_affordable {
                    // Force normal cast by proceeding through handle_warp_cost_choice
                    return handle_warp_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Normal,
                        payment_mode,
                        events,
                    );
                }
                // If only warp or neither, let prepare_spell_cast handle it normally
                // (it will pick CastingVariant::Warp via precedence)
            }
        }
    }

    // CR 702.74a + CR 118.9: Evoke — when a hand card has Keyword::Evoke and
    // both costs are affordable, present a choice. Auto-skip when only one
    // cost is viable. Unlike Warp, Evoke is opt-in via variant_override (the
    // printed mana cost remains the default), so the only routing needed is
    // when the player picks the evoke cost.
    //
    // EvokeCost::Mana — original Lorwyn behavior (pure-mana alt cost).
    // EvokeCost::NonMana — MH2 Incarnations (Solitude et al.). The non-mana
    // portion is split out via `split_evoke_cost_components` so the mana
    // sub-cost (if any) flows through the normal mana-payment phase
    // (CR 601.2g) and the non-mana residual is paid via `pay_additional_cost`
    // (CR 601.2h). Affordability requires BOTH halves to be payable.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            // CR 702.74a + CR 604.1: effective keywords so granted evoke
            // routes/affords.
            if let Some(evoke_cost) = effective_spell_keywords(state, player, object_id)
                .into_iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Evoke(cost) => Some(cost),
                    _ => None,
                })
            {
                let (evoke_mana_part, evoke_non_mana_part) =
                    split_evoke_cost_components(&evoke_cost);
                // CR 601.2f + CR 118.9d: affordability and the displayed costs
                // must reflect active cost modifiers — applied to BOTH the printed
                // cost and the evoke mana sub-cost (CR 118.9d).
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let evoke_mana_eff = evoke_mana_part.as_ref().map(|m| {
                    apply_cost_modifiers_to_base(state, player, object_id, m.clone())
                        .unwrap_or_else(|| m.clone())
                });
                let evoke_mana_affordable = match &evoke_mana_eff {
                    Some(m) => can_pay_cost_after_auto_tap(state, player, object_id, m),
                    // CR 118.3: a zero mana cost is always payable.
                    None => true,
                };
                // CR 118.3 + CR 601.2h: non-mana sub-costs must be independently
                // payable for the evoke option to surface. `AbilityCost::is_payable`
                // walks the cost tree (Composite/Exile/Sacrifice/Discard/PayLife/...)
                // and validates each leaf against current game state.
                let evoke_non_mana_affordable = match &evoke_non_mana_part {
                    Some(ab_cost) => ab_cost.is_payable(state, player, object_id),
                    None => true,
                };
                let evoke_affordable = evoke_mana_affordable && evoke_non_mana_affordable;
                if normal_affordable && evoke_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Evoke,
                        normal_cost,
                        alternative_cost: evoke_mana_eff,
                        alternative_additional_cost: evoke_non_mana_part,
                    });
                }
                if !normal_affordable && evoke_affordable {
                    // Only evoke is payable — proceed via the evoke path.
                    return handle_evoke_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.119a-c: Emerge — when a hand card has Keyword::Emerge and both
    // costs are affordable, present a choice. Emerge affordability includes a
    // legal creature sacrifice and the reduced emerge cost after that
    // sacrificed creature's mana value is subtracted.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(emerge_cost) = effective_spell_keywords(state, player, object_id)
                .into_iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Emerge(cost) => Some(cost),
                    _ => None,
                })
            {
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let emerge_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, emerge_cost.clone())
                        .unwrap_or_else(|| emerge_cost.clone());
                let emerge_affordable =
                    casting_costs::can_pay_emerge_cost(state, player, object_id, &emerge_cost_eff);
                if normal_affordable && emerge_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Emerge,
                        normal_cost,
                        alternative_cost: Some(emerge_cost_eff),
                        alternative_additional_cost: Some(casting_costs::emerge_sacrifice_cost()),
                    });
                }
                if !normal_affordable && emerge_affordable {
                    return handle_emerge_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
            }
        }
    }

    // CR 702.109a + CR 118.9: Dash — opt-in pure-mana alternative cost. When a
    // hand card has Keyword::Dash and both the printed and dash costs are
    // affordable, present the choice; auto-route when only dash is payable.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(dash_cost) = effective_spell_keywords(state, player, object_id)
                .iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Dash(cost) => Some(cost.clone()),
                    _ => None,
                })
            {
                // CR 601.2f: affordability and displayed costs reflect active
                // cost modifiers, applied to both the printed and dash costs.
                let normal_cost =
                    apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
                        .unwrap_or_else(|| obj.mana_cost.clone());
                let dash_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, dash_cost.clone())
                        .unwrap_or(dash_cost);
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &normal_cost);
                let dash_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &dash_eff);
                if normal_affordable && dash_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Dash,
                        normal_cost,
                        alternative_cost: Some(dash_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && dash_affordable {
                    return handle_dash_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.152a + CR 118.9: Blitz — opt-in pure-mana alternative cost. When a
    // hand card has Keyword::Blitz and both the printed and blitz costs are
    // affordable, present the choice; auto-route when only blitz is payable.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            // CR 604.1: honor a Blitz cost granted by a static, not only printed
            // Blitz. CR 702.152b makes Blitz single-instance, so the dedup-by-kind
            // `effective_spell_keywords` collector is correct here.
            if let Some(blitz_cost) = effective_spell_keywords(state, player, object_id)
                .iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Blitz(cost) => Some(cost.clone()),
                    _ => None,
                })
            {
                // CR 601.2f: affordability and displayed costs reflect active
                // cost modifiers, applied to both the printed and blitz costs.
                let normal_cost =
                    apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
                        .unwrap_or_else(|| obj.mana_cost.clone());
                let blitz_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, blitz_cost.clone())
                        .unwrap_or(blitz_cost);
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &normal_cost);
                let blitz_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &blitz_eff);
                if normal_affordable && blitz_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Blitz,
                        normal_cost,
                        alternative_cost: Some(blitz_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && blitz_affordable {
                    return handle_blitz_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.137a + CR 118.9: Spectacle — opt-in pure-mana alternative cost,
    // available only if an opponent lost life this turn. When the gate holds and
    // both the printed and spectacle costs are affordable, present the choice;
    // auto-route when only the spectacle cost is payable. Mirrors the Blitz
    // opt-in flow (spectacle has no resolution riders).
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand && an_opponent_lost_life_this_turn(state, player) {
            if let Some(spectacle_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Spectacle(cost) => Some(cost.clone()),
                _ => None,
            }) {
                // CR 601.2f: affordability and displayed costs reflect active
                // cost modifiers, applied to both the printed and spectacle costs.
                let normal_cost =
                    apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
                        .unwrap_or_else(|| obj.mana_cost.clone());
                let spectacle_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, spectacle_cost.clone())
                        .unwrap_or(spectacle_cost);
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &normal_cost);
                let spectacle_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &spectacle_eff);
                if normal_affordable && spectacle_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Spectacle,
                        normal_cost,
                        alternative_cost: Some(spectacle_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && spectacle_affordable {
                    return handle_spectacle_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.76a + CR 118.9: Prowl — opt-in pure-mana alternative cost from
    // hand, available only if a creature the caster controlled dealt combat
    // damage to a player this turn while sharing one of the spell's creature
    // types. When the gate holds and both the printed and prowl costs are
    // affordable, present the choice; auto-route when only the prowl cost is
    // payable. Mirrors the Spectacle opt-in flow — prowl is a pure cost
    // substitution; its provenance is tagged at resolution (stack.rs) so "if its
    // prowl cost was paid" intervening-ifs can read it.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand && prowl_damage_ledger_satisfied(state, player, object_id) {
            if let Some(prowl_cost) = effective_spell_keywords(state, player, object_id)
                .into_iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Prowl(cost) => Some(cost),
                    _ => None,
                })
            {
                // CR 601.2f: affordability and displayed costs reflect active
                // cost modifiers, applied to both the printed and prowl costs.
                let normal_cost =
                    apply_cost_modifiers_to_base(state, player, object_id, obj.mana_cost.clone())
                        .unwrap_or_else(|| obj.mana_cost.clone());
                let prowl_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, prowl_cost.clone())
                        .unwrap_or(prowl_cost);
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &normal_cost);
                let prowl_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &prowl_eff);
                if normal_affordable && prowl_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Prowl,
                        normal_cost,
                        alternative_cost: Some(prowl_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && prowl_affordable {
                    return handle_prowl_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.96a: Overload — when a hand card has Keyword::Overload and both
    // costs are affordable, present a choice. Auto-skip when only one cost is
    // viable. Mirrors the Evoke opt-in flow: Overload is opt-in via
    // variant_override (the printed mana cost remains the default) so the only
    // routing needed is when the player picks the overload cost.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(overload_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Overload(cost) => Some(cost.clone()),
                _ => None,
            }) {
                // CR 601.2f + CR 118.9d: affordability and the displayed costs
                // must reflect active cost modifiers — applied to BOTH the printed
                // cost and the overload alternative cost (CR 118.9d).
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let overload_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, overload_cost.clone())
                        .unwrap_or_else(|| overload_cost.clone());
                let overload_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &overload_cost_eff);
                if normal_affordable && overload_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Overload,
                        normal_cost,
                        alternative_cost: Some(overload_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && overload_affordable {
                    // Only overload is payable — proceed via the overload path.
                    return handle_overload_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.162a: More Than Meets the Eye — when a hand card has
    // `Keyword::MoreThanMeetsTheEye(cost)` and both costs are affordable, present
    // a choice between the printed mana cost and the MTMTE alternative cost. Auto-
    // skip to the MTMTE path when only the alternative cost is payable. Mirrors the
    // Overload opt-in flow: MTMTE is opt-in via `variant_override` so a fall-through
    // proceeds as a normal (front-face) cast.
    //
    // CR 702.162a defines MTMTE as functioning in "any zone from which the spell
    // may be cast." This offer is intentionally narrowed to `Zone::Hand` for the
    // current class — every printed MTMTE card is cast from hand, matching every
    // other hand-zone alternative-cost keyword (Overload, Cleave, Evoke, ...).
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(mtmte_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::MoreThanMeetsTheEye(cost) => Some(cost.clone()),
                _ => None,
            }) {
                // CR 601.2f: affordability and the displayed costs must reflect
                // active cost modifiers — applied to BOTH the printed cost and the
                // MTMTE alternative cost.
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let mtmte_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, mtmte_cost.clone())
                        .unwrap_or_else(|| mtmte_cost.clone());
                let mtmte_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &mtmte_cost_eff);
                if normal_affordable && mtmte_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword:
                            crate::types::game_state::AlternativeCastKeyword::MoreThanMeetsTheEye,
                        normal_cost,
                        alternative_cost: Some(mtmte_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && mtmte_affordable {
                    // Only the MTMTE cost is payable — proceed via the MTMTE path.
                    return handle_mtmte_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.148a + CR 118.9: Cleave — when a hand card has `Keyword::Cleave(cost)`
    // and a parsed `cleave_variant` (the bracket-removed ability set), present a
    // choice between the printed mana cost and the cleave cost when both are
    // affordable. Auto-skip to the cleave path when only the cleave cost is
    // payable. Mirrors the Overload opt-in flow: cleave is opt-in via
    // `variant_override` so a fall-through proceeds as a normal (printed-text)
    // cast. The `cleave_variant.is_some()` gate guards against offering cleave on
    // an object whose alternate ability set was not parsed.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand && obj.cleave_variant.is_some() {
            if let Some(cleave_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Cleave(cost) => Some(cost.clone()),
                _ => None,
            }) {
                // CR 601.2f + CR 118.9d: affordability and the displayed costs
                // must reflect active cost modifiers — applied to BOTH the printed
                // cost and the cleave alternative cost (CR 118.9d).
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let cleave_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, cleave_cost.clone())
                        .unwrap_or_else(|| cleave_cost.clone());
                let cleave_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &cleave_cost_eff);
                if normal_affordable && cleave_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Cleave,
                        normal_cost,
                        alternative_cost: Some(cleave_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && cleave_affordable {
                    // Only cleave is payable — proceed via the cleave path.
                    return handle_cleave_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.103a: Bestow — when a card being cast has `Keyword::Bestow(cost)`
    // and both the printed creature cost AND the bestow cost are affordable AND
    // there is at least one legal creature to enchant, present the choice.
    // Auto-skip when only one path is viable (normal-only or bestow-only).
    // Mirrors the Evoke / Overload opt-in flow: bestow is opt-in via
    // `variant_override` so a fall-through proceeds as a normal creature cast.
    //
    // CR 702.103a: "Bestow represents a static ability that functions in any zone
    // from which you could play the card it's on." That means the bestow option
    // is offered from the HAND (the default castable zone) and from the GRAVEYARD
    // whenever a permission lets the card be cast from there (Detective's Phoenix:
    // "You may cast this card from your graveyard using its bestow ability.").
    // CR 702.103a + CR 118.9: a compound bestow cost ("{R}, Collect evidence 6")
    // splits into a mana sub-cost (substituted as the alternative mana cost) and
    // a residual non-mana sub-cost (Collect evidence) carried as the additional
    // cost, paid via `pay_additional_cost` — mirrors the Evoke non-mana split.
    if let Some(obj) = state.objects.get(&object_id) {
        let bestow_zone_ok = obj.zone == Zone::Hand
            || (obj.zone == Zone::Graveyard
                && graveyard_permission_source(state, player, object_id).is_some());
        if bestow_zone_ok {
            // CR 702.103a + CR 604.1: read bestow from effective keywords so a
            // bestow cost granted by a static is honored, not just printed bestow.
            if let Some(bestow_cost) = effective_spell_keywords(state, player, object_id)
                .iter()
                .find_map(|k| match k {
                    crate::types::keywords::Keyword::Bestow(cost) => Some(cost.clone()),
                    _ => None,
                })
            {
                // CR 702.103a + CR 303.4a: bestow turns the spell into an Aura
                // requiring a legal target. If no creature is legally enchantable,
                // bestow can't be chosen — fall through (to the creature cast from
                // hand, or to the graveyard-permission creature cast).
                let creature_filter =
                    TargetFilter::Typed(crate::types::ability::TypedFilter::creature());
                let has_legal_creature_target =
                    !targeting::find_legal_targets(state, &creature_filter, player, object_id)
                        .is_empty();
                // CR 601.2f-h + CR 118.9d: split the (possibly compound) bestow
                // cost into its mana sub-cost and Collect-evidence residual, then
                // apply active cost modifiers to the mana sub-cost.
                let (bestow_mana_part, bestow_non_mana_part) =
                    split_bestow_cost_components(&bestow_cost);
                let bestow_mana_eff = bestow_mana_part.as_ref().map(|m| {
                    apply_cost_modifiers_to_base(state, player, object_id, m.clone())
                        .unwrap_or_else(|| m.clone())
                });
                let bestow_mana_affordable = match &bestow_mana_eff {
                    Some(m) => can_pay_cost_after_auto_tap(state, player, object_id, m),
                    // CR 118.3: a zero mana cost is always payable.
                    None => true,
                };
                // CR 118.3 + CR 601.2h: the non-mana residual (Collect evidence)
                // must be independently payable for the bestow option to surface.
                let bestow_non_mana_affordable = match &bestow_non_mana_part {
                    Some(ab_cost) => ab_cost.is_payable(state, player, object_id),
                    None => true,
                };
                let bestow_affordable = bestow_mana_affordable && bestow_non_mana_affordable;
                // CR 601.2a: from the graveyard the "normal" creature cast is the
                // graveyard-permission cast (handled by the variant pipeline). From
                // the hand it's the printed creature cost. Compute the printed-cost
                // affordability only when casting from hand — a graveyard bestow
                // always routes through the bestow path (the permission grants the
                // cast; there is no separate hand-cost branch to compare against).
                let from_hand = obj.zone == Zone::Hand;
                let (normal_cost, normal_affordable) = if from_hand {
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj)
                } else {
                    (obj.mana_cost.clone(), false)
                };
                if from_hand && has_legal_creature_target && normal_affordable && bestow_affordable
                {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Bestow,
                        normal_cost,
                        alternative_cost: bestow_mana_eff,
                        alternative_additional_cost: bestow_non_mana_part,
                    });
                }
                if has_legal_creature_target && bestow_affordable {
                    // Bestow is the only viable path here: from hand the printed
                    // cost is unaffordable; from the graveyard the permission only
                    // grants the bestow cast. Proceed via the bestow path.
                    return handle_bestow_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                if !from_hand
                    && !has_graveyard_cast_permission_without_keyword_constraint(
                        state,
                        player,
                        object_id,
                        KeywordKind::Bestow,
                    )
                {
                    return Err(EngineError::InvalidAction(
                        "No legal bestow cast from graveyard".to_string(),
                    ));
                }
                // Otherwise (no legal target / unaffordable bestow): fall through
                // to the normal / graveyard-permission cast path. The graveyard
                // case is only legal when a separate permission grants a normal
                // cast, not merely a "using bestow" rider.
            }
        }
    }

    // CR 702.140a: Mutate — when a card being cast has `Keyword::Mutate(cost)`
    // and both the printed creature cost AND the mutate cost are affordable AND
    // there is at least one legal "non-Human creature you own" to merge with,
    // present the choice. Auto-skip when only one path is viable. Mirrors the
    // Bestow opt-in flow: mutate is opt-in via `variant_override`, so a
    // fall-through proceeds as a normal creature cast.
    //
    // Offered from the hand and from the command zone — CR 702.140a places no
    // zone restriction, and a mutate creature that is also a commander (e.g.
    // Otrimi, the Ever-Playful) is cast for its mutate cost straight from the
    // command zone (CR 903.9 cast permission applies; commander tax is added by
    // the normal cost pipeline).
    //
    // CR 702.140a + CR 108.3: "a non-Human creature with the same owner as this
    // spell" == a non-Human creature the caster owns (for a cast spell the owner
    // is the caster). B1: `TypeFilter::Non(Subtype("Human"))` +
    // `FilterProp::Owned { controller: You }` — no new filter prop / variant.
    if let Some(obj) = state.objects.get(&object_id) {
        if matches!(obj.zone, Zone::Hand | Zone::Command) {
            if let Some(mutate_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Mutate(cost) => Some(cost.clone()),
                _ => None,
            }) {
                let mutate_target_filter = mutate_target_filter();
                let has_legal_mutate_target =
                    !targeting::find_legal_targets(state, &mutate_target_filter, player, object_id)
                        .is_empty();
                // CR 601.2f + CR 118.9d: affordability and displayed costs reflect
                // active cost modifiers — applied to BOTH the printed cost and the
                // mutate alternative cost.
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let mutate_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, mutate_cost.clone())
                        .unwrap_or_else(|| mutate_cost.clone());
                let mutate_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &mutate_cost_eff);
                if has_legal_mutate_target && normal_affordable && mutate_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Mutate,
                        normal_cost,
                        alternative_cost: Some(mutate_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if has_legal_mutate_target && !normal_affordable && mutate_affordable {
                    // Only the mutate path is payable — proceed via mutate.
                    return handle_mutate_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only / no legal target / neither affordable):
                // fall through to the normal cast path.
            }
        }
    }

    // CR 702.113a: Awaken — when a hand card has `Keyword::Awaken { cost }` and
    // both the printed cost AND the awaken cost are affordable AND there is at
    // least one land you control to awaken, present the choice. Auto-skip when
    // only one path is viable. Mirrors the Overload / Bestow opt-in flow: awaken
    // is opt-in via `variant_override` so a fall-through proceeds as a normal
    // (non-awakening) cast.
    //
    // CR 601.2c + CR 702.113b: the awaken target (the land you control) only
    // exists if the awaken cost is paid. If you control no land, the awaken path
    // would have no legal target, so the only legal cast is the normal path —
    // fall through without offering the prompt (mirrors Bestow's
    // `has_legal_creature_target` gate).
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(awaken_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Awaken { cost, .. } => Some(cost.clone()),
                _ => None,
            }) {
                // CR 601.2c + CR 702.113b: a land you control must exist for the
                // awaken spell ability's target to be legal.
                let land_filter = TargetFilter::Typed(
                    crate::types::ability::TypedFilter::land()
                        .controller(crate::types::ability::ControllerRef::You),
                );
                let has_legal_land =
                    !targeting::find_legal_targets(state, &land_filter, player, object_id)
                        .is_empty();
                // CR 601.2f + CR 118.9d: affordability and the displayed costs
                // must reflect active cost modifiers — applied to BOTH the printed
                // cost and the awaken alternative cost (CR 118.9d).
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let awaken_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, awaken_cost.clone())
                        .unwrap_or_else(|| awaken_cost.clone());
                let awaken_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &awaken_cost_eff);
                if has_legal_land && normal_affordable && awaken_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Awaken,
                        normal_cost,
                        alternative_cost: Some(awaken_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if has_legal_land && !normal_affordable && awaken_affordable {
                    // Only awaken is payable — proceed via the awaken path.
                    return handle_awaken_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only / no legal land / neither affordable):
                // fall through to the normal cast path.
            }
        }
    }

    // CR 702.176a: Impending — when a hand card has `Keyword::Impending { cost, .. }`
    // and both costs are affordable, present a choice. Auto-skip when only one cost
    // is viable. Impending is opt-in via `variant_override` so a fall-through
    // proceeds as a normal creature cast with no time counters.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(impending_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Impending { cost, .. } => Some(cost.clone()),
                _ => None,
            }) {
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let impending_cost_eff =
                    apply_cost_modifiers_to_base(state, player, object_id, impending_cost.clone())
                        .unwrap_or_else(|| impending_cost.clone());
                let impending_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &impending_cost_eff);
                if normal_affordable && impending_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Impending,
                        normal_cost,
                        alternative_cost: Some(impending_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && impending_affordable {
                    // Only impending cost is payable — proceed via the impending path.
                    return handle_impending_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.160a: Prototype — when a hand card has complete prototype
    // secondary characteristics, present a choice between the printed mana cost
    // and the prototype cost when both are affordable. Prototype is opt-in via
    // `variant_override`, so falling through proceeds as the printed creature.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(prototype_form) = prototype_form_from_object(obj) {
                let (normal_cost, normal_affordable) =
                    normal_cast_choice_cost_and_affordability(state, player, object_id, obj);
                let prototype_cost_eff = apply_cost_modifiers_to_base(
                    state,
                    player,
                    object_id,
                    prototype_form.mana_cost.clone(),
                )
                .unwrap_or_else(|| prototype_form.mana_cost.clone());
                let prototype_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &prototype_cost_eff);
                if normal_affordable && prototype_affordable {
                    return Ok(WaitingFor::AlternativeCastChoice {
                        player,
                        object_id,
                        card_id,
                        payment_mode,
                        keyword: crate::types::game_state::AlternativeCastKeyword::Prototype,
                        normal_cost,
                        alternative_cost: Some(prototype_cost_eff),
                        alternative_additional_cost: None,
                    });
                }
                if !normal_affordable && prototype_affordable {
                    return handle_prototype_cost_choice_with_payment_mode(
                        state,
                        player,
                        object_id,
                        card_id,
                        crate::types::actions::AlternativeCastDecision::Alternative,
                        payment_mode,
                        events,
                    );
                }
            }
        }
    }

    // CR 110.4: For graveyard spells via OncePerTurnPerPermanentType, prompt
    // the player to choose which permanent type slot to consume when the card
    // has multiple available slots (multi-type permanents like Artifact Creature).
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Graveyard {
            if let Some(source) = graveyard_permission_source(state, player, object_id)
                .filter(|source| source.frequency == CastFrequency::OncePerTurnPerPermanentType)
            {
                let slots = available_permanent_type_slots(state, source.source_id, object_id);
                if slots.len() > 1 {
                    return Ok(WaitingFor::ChoosePermanentTypeSlot {
                        player,
                        object_id,
                        card_id,
                        source: source.source_id,
                        payment_mode,
                        available_slots: slots,
                    });
                }
            }
        }
    }

    continue_cast_from_prepared(state, player, object_id, payment_mode, events)
}

/// CR 110.4: Handle player's permanent type slot choice for a multi-type
/// graveyard cast via OncePerTurnPerPermanentType. Re-enters the casting
/// pipeline with the chosen slot injected into `CastingVariant`.
pub fn handle_permanent_type_slot_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    source: ObjectId,
    slot: crate::types::card_type::CoreType,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    handle_permanent_type_slot_choice_with_payment_mode(
        state,
        player,
        object_id,
        card_id,
        source,
        slot,
        CastPaymentMode::Auto,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn handle_permanent_type_slot_choice_with_payment_mode(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    source: ObjectId,
    slot: crate::types::card_type::CoreType,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let graveyard_destination_replacement = graveyard_permission_source(state, player, object_id)
        .filter(|permission| permission.source_id == source)
        .and_then(|permission| permission.graveyard_destination_replacement);
    let mut prepared = prepare_spell_cast_with_variant_override(
        state,
        player,
        object_id,
        Some(CastingVariant::GraveyardPermission {
            source,
            frequency: CastFrequency::OncePerTurnPerPermanentType,
            slot_type: Some(slot),
            graveyard_destination_replacement,
        }),
    )?;
    prepared.payment_mode = payment_mode;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 601.2a: Announce the spell by pushing a placeholder `StackEntry` onto
/// the stack. Called exactly once per spell cast, at the top of
/// `continue_with_prepared` / `continue_with_no_ability` /
/// `handle_adventure_choice` (i.e., after all pre-announcement choices like
/// Adventure/Warp/MDFC have resolved and `prepare_spell_cast` succeeded).
///
/// The stack entry is pushed with `ability: None` and `actual_mana_spent: 0`;
/// `finalize_cast` updates these in place once choices and costs are committed
/// and performs the `Zone::Stack` zone change for the object itself. Keeping
/// `obj.zone` equal to the origin zone (hand / graveyard / exile / command)
/// until finalize preserves CR-correct evaluation of off-zone continuous
/// effects (CR 604.3 — "each nonland card in your graveyard has escape", cast-
/// with-keyword statics that filter "spells you cast from exile", etc.). The
/// CR-visible invariant — "the spell is on the stack" — is expressed by the
/// presence of the StackEntry, not the object's zone field.
///
/// If the cast is aborted at any step (CR 601.2i), `handle_cancel_cast` pops
/// this entry; no zone reversion is needed because `obj.zone` never changed.
fn announce_spell_on_stack(
    state: &mut GameState,
    player: PlayerId,
    prepared: &PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) {
    stack::push_to_stack(
        state,
        StackEntry {
            id: prepared.object_id,
            source_id: prepared.object_id,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id: prepared.card_id,
                ability: None,
                casting_variant: prepared.casting_variant,
                actual_mana_spent: 0,
            },
        },
        events,
    );
}

/// Continue the casting pipeline from a PreparedSpellCast.
/// Handles modal selection, targeting, aura targeting, and mana payment.
/// Shared by handle_cast_spell and handle_warp_cost_choice.
fn continue_with_prepared(
    state: &mut GameState,
    player: PlayerId,
    prepared: PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Permanent spells with no spell ability skip modal/targeting/effect resolution
    // and proceed directly to cost payment — unless they are Auras (which target
    // via the Enchant keyword) or mutating creature spells (CR 702.140a: a vanilla
    // creature cast for its mutate cost still targets a non-Human creature you
    // own), both of which need the target-attachment path below.
    if prepared.ability_def.is_none() {
        let obj = state.objects.get(&prepared.object_id);
        let is_aura = obj
            .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
            .unwrap_or(false);
        // CR 702.140a: a mutating creature spell carries a target even with no
        // spell ability — route it through the mutate target-slot branch below.
        let is_mutate = obj.map(|obj| obj.mutate_form.is_some()).unwrap_or(false);
        if !is_aura && !is_mutate {
            return continue_with_no_ability(state, player, prepared, events);
        }
    }

    // CR 601.2a: The spell goes on the stack at announcement, before any
    // mode/target/cost steps. All subsequent branches construct a `PendingCast`
    // that references an object already on the stack.
    announce_spell_on_stack(state, player, &prepared, events);

    // Build the resolved ability from the ability_def, or a placeholder for auras
    // with no spell-level ability (aura targeting is via the Enchant keyword).
    let resolved = if let Some(ref ability_def) = prepared.ability_def {
        // CR 601.2c: The player announcing a spell with modes chooses the mode(s).
        if let Some(ref modal_choice) = prepared.modal {
            let placeholder = ResolvedAbility::new(
                *ability_def.effect.clone(),
                Vec::new(),
                prepared.object_id,
                player,
            );
            if modal_requires_additional_cost_declaration(modal_choice) {
                return casting_costs::begin_modal_additional_cost_declaration(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    placeholder,
                    prepared.mana_cost.clone(),
                    Some(prepared.base_mana_cost.clone()),
                    prepared.casting_variant,
                    prepared.cast_timing_permission,
                    modal_choice.clone(),
                    ability_def.distribute.clone(),
                    prepared.origin_zone,
                    prepared.payment_mode,
                    events,
                );
            }
            // Cap max_choices to actual mode count for count-capped modals.
            let mut capped = modal_choice_for_player(
                state,
                player,
                prepared.object_id,
                modal_choice,
                &crate::types::ability::SpellContext::default(),
            );
            // CR 700.2i: for a pawprint points-budget modal, `max_choices` is the
            // POINT BUDGET (Σ of chosen weights), NOT a mode count — do NOT clamp
            // it to `mode_count`. Mirrors the same discriminant branch in
            // `build_modal_choice` (parser) so the runtime prompt carries the full
            // budget (e.g. 5) rather than a count cap (3).
            if capped.mode_pawprints.is_empty() {
                capped.max_choices = capped.max_choices.min(capped.mode_count);
            }
            let target_constraints = target_constraints_from_modal(&capped);

            // Build a placeholder resolved ability -- will be replaced after mode selection
            let mut pending_modal = PendingCast::new(
                prepared.object_id,
                prepared.card_id,
                placeholder,
                prepared.mana_cost.clone(),
            );
            pending_modal.base_cost = Some(prepared.base_mana_cost.clone());
            pending_modal.casting_variant = prepared.casting_variant;
            pending_modal.cast_timing_permission = prepared.cast_timing_permission;
            pending_modal.distribute = ability_def.distribute.clone();
            pending_modal.target_constraints = target_constraints;
            pending_modal.origin_zone = prepared.origin_zone;
            pending_modal.payment_mode = prepared.payment_mode;
            // CR 700.2e: the mode-choice prompt is routed to the modal's
            // chooser (the controller for standard modals; the opponent for
            // "an opponent chooses —"). Target selection still belongs to the
            // controller (CR 115.1) — `pending_cast` keeps the caster.
            let mode_chooser = resolve_modal_chooser(state, &capped, player, prepared.object_id);
            let mode_abilities = state
                .objects
                .get(&prepared.object_id)
                .map(super::ability_utils::modal_spell_mode_abilities)
                .unwrap_or_default();
            let unavailable_modes = super::ability_utils::spell_modal_unavailable_modes(
                state,
                prepared.object_id,
                player,
                &capped,
                &mode_abilities,
            );
            return Ok(WaitingFor::ModeChoice {
                player: mode_chooser,
                modal: capped,
                pending_cast: Box::new(pending_modal),
                unavailable_modes,
            });
        }

        // CR 608.2 + CR 109.5: Use the canonical builder so the spell's full
        // typed ability surface — `player_scope` (CR 608.2: "Each opponent X"),
        // `kind`, `optional`, `optional_for`, `multi_target`, `unless_pay`,
        // `target_choice_timing`, `repeat_for`, `description`, `forward_result`,
        // `optional_targeting`, `target_selection_mode`, and the `else_ability`
        // branch — is preserved end-to-end into resolution. Hand-rolling a
        // partial copy here previously stripped `player_scope` from cast spells
        // (issue #310: Maddening Cacophony, Fractured Sanity), causing
        // `Each opponent mills N cards.` to mill the controller instead.
        build_resolved_from_def(ability_def, prepared.object_id, player)
    } else {
        // Aura placeholder — will carry targets from Enchant keyword targeting
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            Vec::new(),
            prepared.object_id,
            player,
        )
    };

    // 5. Handle targeting -- ensure layers evaluated before target legality
    super::layers::flush_layers(state);

    // Check if this is an Aura spell -- Auras target via Enchant keyword, not via effect targets
    // Re-read obj after evaluate_layers (which needs &mut state)
    let obj = state.objects.get(&prepared.object_id).unwrap();
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        if let Some(filter) = enchant_filter {
            let legal = targeting::find_legal_targets(state, &filter, player, prepared.object_id);
            if legal.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets for Aura".to_string(),
                ));
            }
            let target_slots = vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal,
                optional: false,
            }];
            if let Some(targets) = auto_select_targets(&target_slots, &[])? {
                let mut resolved = resolved;
                assign_targets_in_chain(state, &mut resolved, &targets)?;
                return check_additional_cost_or_pay(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    Some(prepared.base_mana_cost.clone()),
                    prepared.casting_variant,
                    prepared.cast_timing_permission,
                    prepared.origin_zone,
                    prepared.payment_mode,
                    events,
                );
            } else {
                let selection = begin_target_selection(&target_slots, &[])?;
                let mut pending_aura = PendingCast::new(
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    prepared.mana_cost.clone(),
                );
                pending_aura.base_cost = Some(prepared.base_mana_cost.clone());
                pending_aura.casting_variant = prepared.casting_variant;
                pending_aura.cast_timing_permission = prepared.cast_timing_permission;
                pending_aura.distribute = prepared
                    .ability_def
                    .as_ref()
                    .and_then(|a| a.distribute.clone());
                pending_aura.origin_zone = prepared.origin_zone;
                pending_aura.payment_mode = prepared.payment_mode;
                return Ok(WaitingFor::TargetSelection {
                    player,
                    pending_cast: Box::new(pending_aura),
                    target_slots,
                    mode_labels: Vec::new(),
                    selection,
                });
            }
        }
    }

    // CR 702.140a: Mutate — a mutating creature spell targets a non-Human creature
    // the caster owns (B1). The spell is NOT an Aura, so it doesn't go through the
    // Enchant branch above; instead it carries a single object target which the
    // resolution divert in `stack::resolve_top` reads. Mirrors the Aura
    // target-slot path: build the legal-target slot, auto-select or pause for
    // selection, and thread the target through `assign_targets_in_chain` (which,
    // for a vanilla creature with no target sink, simply stores it in
    // `ability.targets`).
    let obj = state.objects.get(&prepared.object_id).unwrap();
    if obj.mutate_form.is_some() {
        let filter = mutate_target_filter();
        let legal = targeting::find_legal_targets(state, &filter, player, prepared.object_id);
        if legal.is_empty() {
            // CR 702.140a: a mutating creature spell requires a legal target; if
            // none exists the mutate cost can't be paid. (The cast-offer gate
            // already screens this, so reaching here means the board changed.)
            return Err(EngineError::ActionNotAllowed(
                "No legal target for mutate".to_string(),
            ));
        }
        let target_slots = vec![crate::types::game_state::TargetSelectionSlot {
            legal_targets: legal,
            optional: false,
        }];
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                &prepared.mana_cost,
                Some(prepared.base_mana_cost.clone()),
                prepared.casting_variant,
                prepared.cast_timing_permission,
                prepared.origin_zone,
                prepared.payment_mode,
                events,
            );
        } else {
            let selection = begin_target_selection(&target_slots, &[])?;
            let mut pending_mutate = PendingCast::new(
                prepared.object_id,
                prepared.card_id,
                resolved,
                prepared.mana_cost.clone(),
            );
            pending_mutate.base_cost = Some(prepared.base_mana_cost.clone());
            pending_mutate.casting_variant = prepared.casting_variant;
            pending_mutate.cast_timing_permission = prepared.cast_timing_permission;
            pending_mutate.distribute = prepared
                .ability_def
                .as_ref()
                .and_then(|a| a.distribute.clone());
            pending_mutate.origin_zone = prepared.origin_zone;
            pending_mutate.payment_mode = prepared.payment_mode;
            return Ok(WaitingFor::TargetSelection {
                player,
                pending_cast: Box::new(pending_mutate),
                target_slots,
                mode_labels: Vec::new(),
                selection,
            });
        }
    }

    // CR 702.47a–e + CR 601.2b: Splice onto [subtype] is announced as the spell
    // is cast on the same pre-target declaration axis as Emerge/Casualty/etc.
    // It runs after the host ability is built and before later additional-cost
    // prompts because accepting merges text that may add targets to collect in
    // CR 601.2c and cost inputs to lock in CR 601.2f.
    let splice_eligible = splice::eligible_splice_cards(state, player, prepared.object_id);
    if !splice_eligible.is_empty() {
        return Ok(splice::begin_offer(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
            prepared.base_mana_cost.clone(),
            prepared.casting_variant,
            prepared.cast_timing_permission,
            prepared
                .ability_def
                .as_ref()
                .and_then(|a| a.distribute.clone()),
            prepared.origin_zone,
            prepared.payment_mode,
            player,
            splice_eligible,
        ));
    }

    // CR 702.119a-c + CR 601.2b/h: Emerge requires choosing which creature to
    // sacrifice as the player chooses to pay the emerge cost, then sacrificing
    // it as that cost is paid. Route this before any target selection so the
    // required sacrifice is declared on the CR 601.2b axis.
    if prepared.casting_variant == CastingVariant::Emerge {
        return casting_costs::begin_required_cost_before_targets(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost,
            Some(prepared.base_mana_cost.clone()),
            casting_costs::emerge_sacrifice_cost(),
            SpellCostSource::Emerge,
            prepared.casting_variant,
            prepared.cast_timing_permission,
            prepared
                .ability_def
                .as_ref()
                .and_then(|a| a.distribute.clone()),
            prepared.origin_zone,
            prepared.payment_mode,
            events,
        );
    }

    // CR 601.2b/c/f: When target cardinality depends on an announced X, defer
    // target selection until that X is chosen from the spell's required
    // additional cost or mana cost. CR 601.2d: a divided pool's target count is
    // also X-bounded (issue #2856), so the distribute flag participates.
    let prepared_distribute = prepared
        .ability_def
        .as_ref()
        .and_then(|a| a.distribute.clone());
    if ability_target_legality_needs_chosen_x(&resolved, prepared_distribute.as_ref()) {
        if let Some(required_cost) =
            casting_costs::required_additional_cost_can_declare_x(state, player, prepared.object_id)
        {
            return casting_costs::begin_required_cost_before_targets(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                prepared.mana_cost,
                Some(prepared.base_mana_cost.clone()),
                required_cost,
                SpellCostSource::Other,
                prepared.casting_variant,
                prepared.cast_timing_permission,
                prepared
                    .ability_def
                    .as_ref()
                    .and_then(|a| a.distribute.clone()),
                prepared.origin_zone,
                prepared.payment_mode,
                events,
            );
        }
        if casting_costs::cost_has_x(&prepared.mana_cost) {
            let mut pending_x = PendingCast::new(
                prepared.object_id,
                prepared.card_id,
                resolved,
                prepared.mana_cost.clone(),
            );
            pending_x.base_cost = Some(prepared.base_mana_cost.clone());
            pending_x.casting_variant = prepared.casting_variant;
            pending_x.cast_timing_permission = prepared.cast_timing_permission;
            pending_x.distribute = prepared
                .ability_def
                .as_ref()
                .and_then(|ability| ability.distribute.clone());
            pending_x.target_constraints = prepared
                .ability_def
                .as_ref()
                .map(|ability| ability.target_constraints.clone())
                .unwrap_or_default();
            pending_x.origin_zone = prepared.origin_zone;
            pending_x.payment_mode = prepared.payment_mode;
            pending_x.deferred_target_selection = true;
            state.pending_cast = Some(Box::new(pending_x));
            return casting_costs::enter_payment_step(state, player, None, events);
        }
    }

    // CR 601.2b + CR 702.33d: Kicker "instead" spells — prompt for kicker before
    // building unkicked target slots (Bloodchief's Thirst on Pyrogoyf, #3989).
    let has_kicker_cost = state
        .objects
        .get(&prepared.object_id)
        .and_then(|obj| obj.additional_cost.as_ref())
        .is_some_and(|additional| matches!(additional, AdditionalCost::Kicker { .. }));
    if has_kicker_cost && requires_additional_cost_declaration_before_targets(&resolved) {
        return casting_costs::begin_target_dependent_additional_cost_declaration(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost,
            Some(prepared.base_mana_cost.clone()),
            prepared.casting_variant,
            prepared.cast_timing_permission,
            prepared
                .ability_def
                .as_ref()
                .and_then(|a| a.distribute.clone()),
            prepared.origin_zone,
            prepared.payment_mode,
            events,
        );
    }

    let mut target_slots = build_target_slots(state, &resolved)?;
    // CR 601.2c + CR 601.2d: A fixed-amount divided spell (no X to announce, e.g.
    // "2 damage divided among up to three targets") must likewise offer at most
    // one slot per divisible unit — each chosen target needs ≥1 (issue #2856).
    super::ability_utils::cap_distribution_target_slots(
        state,
        &resolved,
        prepared_distribute.as_ref(),
        &mut target_slots,
    );
    if !target_slots.is_empty() {
        let target_constraints = prepared
            .ability_def
            .as_ref()
            .map(|ability| ability.target_constraints.clone())
            .unwrap_or_default();

        // CR 601.2b: Casualty (optional sacrifice) must be declared before targets are
        // chosen. Detect an effective Casualty cost and route through the deferred target
        // selection path so the sacrifice prompt appears first.
        if let Some(casualty_cost) =
            casting_costs::effective_casualty_additional_cost(state, player, prepared.object_id)
        {
            return casting_costs::begin_optional_cost_before_targets(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                prepared.mana_cost,
                Some(prepared.base_mana_cost.clone()),
                casualty_cost,
                SpellCostSource::Other,
                prepared.casting_variant,
                prepared.cast_timing_permission,
                prepared
                    .ability_def
                    .as_ref()
                    .and_then(|a| a.distribute.clone()),
                prepared.origin_zone,
                prepared.payment_mode,
                events,
            );
        }

        // CR 702.56a: Replicate is a repeatable optional additional cost, so it
        // must be declared before targets are chosen just like Casualty.
        if let Some(replicate_cost) =
            casting_costs::effective_replicate_additional_cost(state, player, prepared.object_id)
        {
            return casting_costs::begin_optional_cost_before_targets(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                prepared.mana_cost,
                Some(prepared.base_mana_cost.clone()),
                replicate_cost,
                SpellCostSource::Other,
                prepared.casting_variant,
                prepared.cast_timing_permission,
                prepared
                    .ability_def
                    .as_ref()
                    .and_then(|a| a.distribute.clone()),
                prepared.origin_zone,
                prepared.payment_mode,
                events,
            );
        }

        // CR 702.48a/b: Offering sacrifice must be declared before targets are chosen.
        // When cast_timing_permission == Offering, the player used Offering to unlock
        // instant-speed timing and is required to pay the sacrifice. Otherwise it is
        // optional (sorcery-speed cast with optional Offering).
        if let Some(offering_quality) =
            casting_costs::effective_offering_quality(state, player, prepared.object_id)
        {
            let offering_cost = casting_costs::effective_offering_additional_cost(
                state,
                player,
                prepared.object_id,
            )
            .expect("offering quality implies offering additional cost");
            let required = prepared.cast_timing_permission == Some(CastTimingPermission::Offering);
            if required {
                // CR 702.48b: Required when cast used instant-speed timing via Offering.
                return casting_costs::begin_required_cost_before_targets(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    prepared.mana_cost,
                    Some(prepared.base_mana_cost.clone()),
                    casting_costs::offering_sacrifice_cost(&offering_quality),
                    SpellCostSource::Offering,
                    prepared.casting_variant,
                    prepared.cast_timing_permission,
                    prepared
                        .ability_def
                        .as_ref()
                        .and_then(|a| a.distribute.clone()),
                    prepared.origin_zone,
                    prepared.payment_mode,
                    events,
                );
            } else {
                return casting_costs::begin_optional_cost_before_targets(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    prepared.mana_cost,
                    Some(prepared.base_mana_cost.clone()),
                    offering_cost,
                    SpellCostSource::Offering,
                    prepared.casting_variant,
                    prepared.cast_timing_permission,
                    prepared
                        .ability_def
                        .as_ref()
                        .and_then(|a| a.distribute.clone()),
                    prepared.origin_zone,
                    prepared.payment_mode,
                    events,
                );
            }
        }

        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &target_constraints)?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                &prepared.mana_cost,
                Some(prepared.base_mana_cost.clone()),
                prepared.casting_variant,
                prepared.cast_timing_permission,
                prepared.origin_zone,
                prepared.payment_mode,
                events,
            );
        }

        let selection = begin_target_selection_for_ability(
            state,
            &resolved,
            &target_slots,
            &target_constraints,
        )?;
        let mut pending_targets = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
        );
        pending_targets.base_cost = Some(prepared.base_mana_cost.clone());
        pending_targets.casting_variant = prepared.casting_variant;
        pending_targets.cast_timing_permission = prepared.cast_timing_permission;
        pending_targets.distribute = prepared
            .ability_def
            .as_ref()
            .and_then(|a| a.distribute.clone());
        pending_targets.target_constraints = target_constraints;
        pending_targets.origin_zone = prepared.origin_zone;
        pending_targets.payment_mode = prepared.payment_mode;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_targets),
            target_slots,
            mode_labels: Vec::new(),
            selection,
        });
    }

    // 6. Check additional cost, then pay mana cost
    check_additional_cost_or_pay(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        resolved,
        &prepared.mana_cost,
        Some(prepared.base_mana_cost.clone()),
        prepared.casting_variant,
        prepared.cast_timing_permission,
        prepared.origin_zone,
        prepared.payment_mode,
        events,
    )
}

/// CR 700.2a / CR 700.2e: Resolve a modal's `chooser` to the single `PlayerId`
/// that the `WaitingFor::ModeChoice` / `AbilityModeChoice` prompt names.
///
/// For `PlayerFilter::Controller` (every standard modal and the `you choose —`
/// alias) this is the controller — byte-identical to the historic behavior.
/// For `PlayerFilter::Opponent` (CR 700.2e — "an opponent chooses …") this is
/// the single opponent, resolved via the canonical
/// `effects::matches_player_scope` authority filtered over APNAP order. In the
/// 2-player engine this is unambiguous. Falls back to the controller if no
/// player matches (defensive — cannot happen in a live 2-player game).
fn resolve_modal_chooser(
    state: &GameState,
    modal: &crate::types::ability::ModalChoice,
    controller: PlayerId,
    source_id: ObjectId,
) -> PlayerId {
    if modal.chooser == crate::types::ability::PlayerFilter::Controller {
        return controller;
    }
    crate::game::players::apnap_order(state)
        .into_iter()
        .find(|&p| {
            crate::game::effects::matches_player_scope(
                state,
                p,
                &modal.chooser,
                controller,
                source_id,
            )
        })
        .unwrap_or(controller)
}

fn modal_requires_additional_cost_declaration(modal: &crate::types::ability::ModalChoice) -> bool {
    modal.constraints.iter().any(|constraint| {
        let crate::types::ability::ModalSelectionConstraint::ConditionalMaxChoices {
            condition,
            ..
        } = constraint
        else {
            return false;
        };
        matches!(
            condition,
            ModalSelectionCondition::AdditionalCostPaid { .. }
        )
    })
}

fn requires_additional_cost_declaration_before_targets(ability: &ResolvedAbility) -> bool {
    let Some(sub_ability) = ability.sub_ability.as_deref() else {
        return false;
    };
    matches!(
        sub_ability.condition,
        Some(AbilityCondition::AdditionalCostPaidInstead)
    ) && crate::game::triggers::extract_target_filter_from_effect(&sub_ability.effect).is_some()
}

/// Fast path for permanent spells with no spell-level ability.
/// Skips modal/targeting/effect — proceeds directly to cost payment.
fn continue_with_no_ability(
    state: &mut GameState,
    player: PlayerId,
    prepared: PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Auras always have a spell ability (Enchant keyword generates targeting),
    // so this path is only for non-Aura permanents.

    // CR 601.2a: Announce the spell onto the stack before any cost payment.
    announce_spell_on_stack(state, player, &prepared, events);

    // Build a placeholder resolved ability for cost-payment plumbing.
    // The PendingCast infrastructure requires a ResolvedAbility; it carries no
    // meaningful effect and will be discarded (pushed as `ability: None`) when
    // finalize_cast_to_stack detects no Spell-kind AbilityDefinition on the object.
    let placeholder = ResolvedAbility::new(
        Effect::Unimplemented {
            name: String::new(),
            description: None,
        },
        Vec::new(),
        prepared.object_id,
        player,
    );
    if prepared.casting_variant == CastingVariant::Emerge {
        return casting_costs::begin_required_cost_before_targets(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            placeholder,
            prepared.mana_cost,
            Some(prepared.base_mana_cost.clone()),
            casting_costs::emerge_sacrifice_cost(),
            SpellCostSource::Emerge,
            prepared.casting_variant,
            prepared.cast_timing_permission,
            None,
            prepared.origin_zone,
            prepared.payment_mode,
            events,
        );
    }
    check_additional_cost_or_pay(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        placeholder,
        &prepared.mana_cost,
        Some(prepared.base_mana_cost.clone()),
        prepared.casting_variant,
        prepared.cast_timing_permission,
        prepared.origin_zone,
        prepared.payment_mode,
        events,
    )
}

/// Returns true if the spell has at least one legal target (or requires no targets).
/// Used by phase-ai's legal_actions to avoid including uncastable spells in the action set.
pub fn spell_has_legal_targets(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    let mut simulated = state.clone();
    super::layers::flush_layers(&mut simulated);
    let Some(obj) = simulated.objects.get(&obj.id) else {
        return false;
    };

    // Aura spells target via the Enchant keyword rather than the effect's target field.
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        return enchant_filter.is_some_and(|filter| {
            !targeting::find_legal_targets(&simulated, &filter, player, obj.id).is_empty()
        });
    }

    // CR 700.2a-b: Modal spells are castable only when at least one mode has a
    // legal targeting assignment (or needs no targets).
    if let Some(ref modal) = obj.modal {
        let mode_abilities = super::ability_utils::modal_spell_mode_abilities(obj);
        let capped = modal_choice_for_player(
            &simulated,
            player,
            obj.id,
            modal,
            &crate::types::ability::SpellContext::default(),
        );
        let unavailable = super::ability_utils::spell_modal_unavailable_modes(
            &simulated,
            obj.id,
            player,
            &capped,
            &mode_abilities,
        );
        return unavailable.len() < capped.mode_count;
    }

    // Only Spell-kind abilities contribute targets when casting.
    // Activated/Database abilities are irrelevant to spell castability.
    let ability_def = match combined_spell_ability_def(obj) {
        Some(a) => a,
        None => return true, // Permanent with no spell abilities needs no targets
    };

    let resolved = build_resolved_from_def(&ability_def, obj.id, player);
    let base_ok = match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) if target_slots.is_empty() => true,
        Ok(target_slots) => has_legal_target_assignment_for_ability(
            &simulated,
            &resolved,
            &target_slots,
            &ability_def.target_constraints,
        ),
        Err(_) => false,
    };
    if base_ok {
        return true;
    }
    if kicker_instead_spell_has_legal_targets(&simulated, &ability_def, obj.id, player) {
        return true;
    }
    ability_target_legality_needs_chosen_x(&resolved, ability_def.distribute.as_ref())
        && (casting_costs::required_additional_cost_can_declare_x(&simulated, player, obj.id)
            .is_some()
            || casting_costs::cost_has_x(&obj.mana_cost))
}

/// CR 601.2b + CR 118.9a: Check whether `object_id` can legally be cast for
/// free via the given `source_id` right now. Mirrors `can_cast_object_now`'s
/// timing/targeting checks using a `CastingVariant::HandPermission { source,
/// frequency }` override so the mana cost is `NoCost` and the source's
/// once-per-turn slot (if any) is consulted.
pub fn can_cast_for_free_now(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    source_id: ObjectId,
    frequency: CastFrequency,
) -> bool {
    let variant = CastingVariant::HandPermission {
        source: source_id,
        frequency,
    };
    let Ok(prepared) =
        prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))
    else {
        return false;
    };
    let Some(obj) = state.objects.get(&prepared.object_id) else {
        return false;
    };
    // CR 118.9a: NoCost means mana affordability is automatic; the remaining
    // gate is legal-targets for targeted spells (permanent spells skip via
    // `spell_has_legal_targets` semantics).
    prepared.modal.is_some() || spell_has_legal_targets(state, obj, player)
}

/// CR 601.2b: Enumerate `(object_id, source_id, frequency)` candidates for
/// `CastSpellForFree` — for each hand-spell the caller could cast and each
/// active `CastFromHandFree { OncePerTurn }` permission source that admits it.
///
/// `Unlimited` sources (Omniscience) are intentionally excluded: they route
/// through the implicit `CastSpell` silent-free path to avoid duplicating the
/// same candidate action under two different action variants.
pub fn hand_cast_free_candidates(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId, CastFrequency)> {
    // CR 601.2b + CR 400.7: Collect active (source_id, frequency, filter)
    // triples for OncePerTurn permissions that haven't been consumed this turn.
    let sources: Vec<(ObjectId, TargetFilter, CastFrequency, CastFreeOrigin)> =
        iter_cast_free_permission_source_ids(state)
            .filter_map(|src_id| {
                let src_obj = state.objects.get(&src_id)?;
                if src_obj.controller != player {
                    return None;
                }
                active_static_definitions(state, src_obj).find_map(|s| match s.mode {
                    StaticMode::CastFromHandFree { frequency, origin } => {
                        if frequency == CastFrequency::OncePerTurn
                            && state.hand_cast_free_permissions_used.contains(&src_id)
                        {
                            None
                        } else if frequency == CastFrequency::OncePerTurn {
                            s.affected
                                .as_ref()
                                .map(|f| (src_id, f.clone(), frequency, origin))
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
            })
            .collect();

    if sources.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let Some(player_data) = state.players.iter().find(|p| p.id == player) else {
        return out;
    };
    for &hand_id in &player_data.hand {
        for (src_id, filter, frequency, origin) in &sources {
            let Some(obj) = state.objects.get(&hand_id) else {
                continue;
            };
            if !cast_free_origin_admits_object(state, player, obj, *origin) {
                continue;
            }
            let ctx = super::filter::FilterContext::from_source_with_controller(*src_id, player);
            if !super::filter::matches_target_filter(state, hand_id, filter, &ctx) {
                continue;
            }
            if can_cast_for_free_now(state, player, hand_id, *src_id, *frequency) {
                out.push((hand_id, *src_id, *frequency));
            }
        }
    }
    out
}

pub fn can_cast_object_now(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    // CR 702.61a: While a spell with split second is on the stack, players can't
    // cast spells (mana abilities are exempt per CR 702.61b, but spells are not).
    if super::keywords::stack_has_split_second(state) {
        return false;
    }
    let Ok(prepared) = prepare_spell_cast(state, player, object_id) else {
        // CR 715.3a / CR 720.3a: An Adventure instant/sorcery face may be
        // castable even when `prepare_spell_cast` fails on the creature face —
        // most commonly the sorcery-speed timing gate outside main phases.
        if let Some(obj) = state.objects.get(&object_id) {
            if alternative_spell_layout(obj).is_some()
                && cast_face_choice_offered_from_zone(state, obj)
            {
                let mut sim = state.clone();
                if let Some(sim_obj) = sim.objects.get_mut(&object_id) {
                    swap_to_alternative_spell_face(sim_obj);
                }
                if let Ok(prepared) = prepare_spell_cast(&sim, player, object_id) {
                    return can_cast_prepared_now(&sim, player, &prepared);
                }
            }
            // CR 709.3 + CR 712.11b: Spell//spell split cards and spell//spell
            // MDFCs may be castable via the other face even when prepare fails
            // on the current face — e.g. a graveyard permission cast of Life //
            // Death when only Death is affordable (#3987).
            if cast_spell_face_choice_available(obj)
                && cast_spell_face_choice_offered_from_zone(state, obj)
            {
                let mut sim = state.clone();
                if let Some(sim_obj) = sim.objects.get_mut(&object_id) {
                    simulate_chosen_split_spell_back_face(sim_obj);
                }
                return can_cast_object_now(&sim, player, object_id);
            }
        }
        let choices = casting_variant_choice_set(state, player, object_id);
        return !choices.options.is_empty();
    };
    can_cast_prepared_now(state, player, &prepared)
        || !casting_variant_choice_set(state, player, object_id)
            .options
            .is_empty()
}

/// CR 702.180a (issue #1550): Harmonize may tap up to one untapped creature
/// its controller controls to reduce only the generic portion of the cost by
/// that creature's power.
fn reduce_harmonize_cost_for_creature_power(cost: &ManaCost, power: u32) -> ManaCost {
    match cost {
        ManaCost::Cost { shards, generic } => ManaCost::Cost {
            shards: shards.clone(),
            generic: generic.saturating_sub(power),
        },
        ManaCost::NoCost | ManaCost::SelfManaCost | ManaCost::SelfManaValue => cost.clone(),
    }
}

/// CR 702.180a + CR 601.2h: Legal-action castability must mirror the real
/// Harmonize payment path. A candidate creature is tapped before mana payment,
/// so the affordability check runs against a simulated state with that
/// creature already tapped rather than assuming the same creature can also pay
/// the remaining mana cost.
fn can_feasibly_pay_harmonize_mana_cost(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    variant: CastingVariant,
    cost: &ManaCost,
) -> bool {
    if can_feasibly_pay_mana_cost(state, player, Some(source_id), cost) {
        return true;
    }
    let ManaCost::Cost { generic, .. } = cost else {
        return false;
    };
    if variant != CastingVariant::Harmonize || *generic == 0 {
        return false;
    }

    state
        .objects
        .values()
        .filter_map(|o| {
            if o.controller == player
                && o.zone == Zone::Battlefield
                && !o.tapped
                && o.card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature)
                && o.power.is_some_and(|power| power > 0)
            {
                Some((o.id, o.power.unwrap_or(0) as u32))
            } else {
                None
            }
        })
        .any(|(creature_id, power)| {
            let reduced_cost = reduce_harmonize_cost_for_creature_power(cost, power);
            let mut simulated = state.clone();
            let Some(creature) = simulated.objects.get_mut(&creature_id) else {
                return false;
            };
            creature.tapped = true;
            can_feasibly_pay_mana_cost(&simulated, player, Some(source_id), &reduced_cost)
        })
}

fn can_cast_prepared_now(
    state: &GameState,
    player: PlayerId,
    prepared: &PreparedSpellCast,
) -> bool {
    let Some(obj) = state.objects.get(&prepared.object_id) else {
        return false;
    };

    // CR 202.1b: A card with no mana cost (suspend-only cards like Inevitable
    // Betrayal) has an unpayable cost.
    // CR 118.6: it therefore can't be cast from hand by paying that cost. Its
    // only legal plays are via an effect/keyword — Suspend's exile activation,
    // the free-cast from exile, or an effect-granted `CastSpellForFree` — none
    // of which take this normal-hand-cast path.
    // CR 118.6a: the exception is an effect that lets you cast it WITHOUT paying
    // its mana cost. The only such effect routed through this normal-CastSpell
    // path is an `Unlimited` `CastFromHandFree` permission (Omniscience / Tamiyo
    // emblem), which `prepare_spell_cast` recognizes via the same predicate and
    // zeroes the cost. `OncePerTurn` sources (Zaffai) opt in via the dedicated
    // `CastSpellForFree` action instead. Block the normal hand cast otherwise.
    // Exile-zone copies (Prepare, Suspend, Discover, etc.) carry their own
    // `ExileWithAltCost` permission and must not hit this hand/command guard.
    if matches!(obj.zone, Zone::Hand | Zone::Command)
        && matches!(obj.mana_cost, ManaCost::NoCost)
        && !unlimited_hand_cast_free_applies(state, player, obj, prepared.casting_variant)
    {
        return false;
    }

    // CR 601.3d: A cast authorized only by a target-dependent flash option is
    // illegal unless a condition-satisfying target exists. Pre-target FEASIBILITY
    // analogue of the finalize-time target_dependent_flash_permission_satisfied
    // SATISFACTION gate. Also covers the Adventure recursion re-entry, since
    // every CastSpell path flows through can_cast_object_now.
    if prepared.cast_timing_permission == Some(CastTimingPermission::AsThoughHadFlash)
        && !restrictions::target_dependent_flash_permission_feasible(
            state,
            player,
            prepared.object_id,
        )
    {
        return false;
    }

    // CR 702.48a: When the Offering timing unlock was used, a legal sacrifice
    // target must still exist (state may have changed since prepare time).
    if prepared.cast_timing_permission == Some(CastTimingPermission::Offering)
        && !casting_costs::can_pay_offering_additional_cost(state, player, prepared.object_id)
    {
        return false;
    }

    // CR 702.138a: Escape requires the player to be able to pay its additional
    // (exile) cost — usually exiling other graveyard cards, plus any battlefield
    // exile clause (Lunar Hatchling's "Exile a land you control").
    if prepared.casting_variant == CastingVariant::Escape
        && !can_pay_escape_additional_cost(state, player, prepared.object_id)
    {
        return false;
    }

    // CR 702.81a: Retrace requires a discardable land card in hand.
    if prepared.casting_variant == CastingVariant::Retrace
        && !casting_costs::can_pay_retrace_additional_cost(state, player, prepared.object_id)
    {
        return false;
    }

    // CR 702.133a: Jump-start requires a discardable card (any card) in hand.
    if prepared.casting_variant == CastingVariant::JumpStart
        && !casting_costs::can_pay_jumpstart_additional_cost(state, player, prepared.object_id)
    {
        return false;
    }

    // CR 702.187b: Mayhem requires that you discarded this card this turn.
    if prepared.casting_variant == CastingVariant::Mayhem
        && !was_discarded_this_turn(state, prepared.object_id)
    {
        return false;
    }

    // CR 702.119a-c: Emerge affordability is the reduced emerge cost after
    // sacrificing a legal creature, not the unreduced `prepared.mana_cost`.
    if prepared.casting_variant == CastingVariant::Emerge {
        return (prepared.modal.is_some() || spell_has_legal_targets(state, obj, player))
            && casting_costs::can_pay_emerge_cost(
                state,
                player,
                prepared.object_id,
                &prepared.mana_cost,
            );
    }

    // CR 702.96b: a spell cast with overload "won't require any targets" and "may
    // affect objects that couldn't be chosen as legal targets". The generic gate
    // (spell_has_legal_targets) reads the UNMODIFIED printed obj ("... target
    // creature"); evaluate the TRANSFORMED prepared.ability_def instead, which
    // overload::transform_ability_def has already rewritten to target-less *All
    // effects (no TargetRef slots → trivially satisfiable).
    if prepared.casting_variant == CastingVariant::Overload {
        let overload_targets_ok = prepared.ability_def.as_ref().is_none_or(|def| {
            let resolved = build_resolved_from_def(def, prepared.object_id, player);
            match build_target_slots(state, &resolved) {
                Ok(slots) => {
                    slots.is_empty()
                        || has_legal_target_assignment_for_ability(
                            state,
                            &resolved,
                            &slots,
                            &def.target_constraints,
                        )
                }
                Err(_) => false,
            }
        });
        return overload_targets_ok
            && can_feasibly_pay_harmonize_mana_cost(
                state,
                player,
                prepared.object_id,
                prepared.casting_variant,
                &prepared.mana_cost,
            );
    }

    // CR 702.34a + CR 118.3 + CR 119.8: Flashback's non-mana cost (e.g. "pay N
    // life") is an additional cost. Pre-check affordability so a CantLoseLife
    // lock or insufficient life filters the flashback from legal actions.
    if prepared.casting_variant == CastingVariant::Flashback {
        if let Some(FlashbackCost::NonMana(ref cost)) =
            super::keywords::effective_flashback_cost(state, prepared.object_id)
        {
            if let Some(amount) = find_pay_life_cost(cost, state, player, prepared.object_id) {
                if !super::life_costs::can_pay_life_cast_or_activation_cost(state, player, amount) {
                    return false;
                }
            }
        }
    }

    // CR 401.5 + CR 118.9 + CR 119.8: Top-of-library alt-cost casts (Bolas's
    // Citadel) replace the mana cost with a PayLife cost equal to the spell's
    // mana value. Gate legal actions on life affordability so the UI never offers
    // a cast the payment pipeline would reject after the player taps mana.
    if let Some(alt_cost) =
        top_of_library_alt_ability_cost_for_object(state, player, prepared.object_id)
    {
        if let Some(amount) = find_pay_life_cost(&alt_cost, state, player, prepared.object_id) {
            if !super::life_costs::can_pay_life_cast_or_activation_cost(state, player, amount) {
                return false;
            }
        }
    }

    // CR 118.9 + CR 601.2f + CR 119.8: Graveyard/exile cast-permission statics
    // that carry a pay-life extra-cost rider (Valgavoth alternative; Festival of
    // Embers additional) must afford the life payment for the cast to be legal.
    // The remove-counters extra-cost (Dawnhand) carries no life payment, so
    // `find_pay_life_cost` returns `None` and this gate is a no-op for it.
    {
        // CR 601.2a: Bind the exile extra-cost rider to the source this cast
        // commits to — the recorded `ExilePermission` source if elected, else the
        // first-match scan that stamps the offered candidate (this legality check
        // runs on a `prepare_spell_cast` whose exile variant resolves to `Normal`
        // until the player elects it). An impulse `PlayFromExile` or other
        // on-object exile permission yields no static source and so no rider.
        let static_extra = match state.objects.get(&prepared.object_id).map(|o| o.zone) {
            Some(Zone::Exile) => elected_exile_permission_source(
                state,
                player,
                prepared.object_id,
                Some(prepared.casting_variant),
            )
            .and_then(|source| {
                exile_static_permission_extra_cost(state, player, prepared.object_id, source)
            }),
            Some(Zone::Graveyard) => {
                graveyard_static_permission_extra_cost(state, player, prepared.object_id)
            }
            _ => None,
        };
        if let Some(extra) = static_extra {
            if let Some(amount) = find_pay_life_cost(&extra.cost, state, player, prepared.object_id)
            {
                if !super::life_costs::can_pay_life_cast_or_activation_cost(state, player, amount) {
                    return false;
                }
            }
        }
    }

    // CR 601.2b + CR 118.3 + CR 119.8: Additional-cost affordability — any
    // `AbilityCost::PayLife` attached as an additional cost (Required or
    // Optional-but-required-to-cast) must be payable for the spell to be cast.
    // For Optional additional costs this is a false-negative in the locked case
    // only if the optional cost is the ONLY affordability gate, which is never
    // the case; the mana cost already has to be payable on its own.
    if let Some(AdditionalCost::Required(cost)) = state
        .objects
        .get(&prepared.object_id)
        .and_then(|o| o.additional_cost.as_ref())
    {
        if let Some(amount) = find_pay_life_cost(cost, state, player, prepared.object_id) {
            if !super::life_costs::can_pay_life_cast_or_activation_cost(state, player, amount) {
                return false;
            }
        }
    }

    // CR 702.172: Spree spells must afford at least one mode to be castable.
    // CR 117.1d + CR 601.2g: Use the feasibility predicate so non-tap mana
    // abilities (Sacrifice / Discard / PayLife) the controller could activate
    // manually during cost payment are counted as castable mana sources.
    if let Some(ref modal) = prepared.modal {
        if !modal.mode_costs.is_empty() {
            return modal.mode_costs.iter().any(|mode_cost| {
                let total = restrictions::add_mana_cost(&prepared.mana_cost, mode_cost);
                can_feasibly_pay_mana_cost(state, player, Some(prepared.object_id), &total)
            });
        }
    }

    // CR 117.1d + CR 601.2g: Feasibility, not just auto-tap, gates castability —
    // a player may activate sacrifice-/discard-/life-cost mana abilities during
    // payment (issue #562: KCI must expose Ichor Wellspring as castable).
    let creature_face_ok = (prepared.modal.is_some()
        || spell_has_legal_targets(state, obj, player))
        && can_feasibly_pay_harmonize_mana_cost(
            state,
            player,
            prepared.object_id,
            prepared.casting_variant,
            &prepared.mana_cost,
        );

    if creature_face_ok {
        return true;
    }

    if (prepared.modal.is_some() || spell_has_legal_targets(state, obj, player))
        && super::casting_costs::payable_spell_alternative_cost(state, player, prepared.object_id)
            .is_some()
    {
        return true;
    }

    // CR 715.3a / CR 720.3a: For Adventure-family cards, also evaluate the
    // alternative spell face. The creature face may be unaffordable while the
    // spell face is castable; in that case the card is still legally castable
    // and will prompt AdventureCastChoice.
    if alternative_spell_layout(obj).is_some() && cast_face_choice_offered_from_zone(state, obj) {
        let mut sim = state.clone();
        if let Some(sim_obj) = sim.objects.get_mut(&prepared.object_id) {
            swap_to_alternative_spell_face(sim_obj);
        }
        return can_cast_object_now(&sim, player, prepared.object_id);
    }

    // CR 712.11c: For a spell//spell Modal DFC, only the face that will be face
    // up on the stack is evaluated to determine if it can be cast — so the back
    // face must be tested independently. The front face may be unaffordable
    // (Esika, God of the Tree needs {1}{G}{G}) while the back face is castable
    // (The Prismatic Bridge needs {W}{U}{B}{R}{G}); the card is still legally
    // castable and will prompt ModalFaceChoice (CR 712.11b). Mirror the Adventure
    // recursion: swap to the back face and re-test. `simulate_chosen_split_spell_back_face`
    // clears the stashed face's `layout_kind`, so the recursive call does not
    // re-enter this branch (no infinite recursion).
    if cast_spell_face_choice_available(obj) {
        let mut sim = state.clone();
        if let Some(sim_obj) = sim.objects.get_mut(&prepared.object_id) {
            simulate_chosen_split_spell_back_face(sim_obj);
        }
        return can_cast_object_now(&sim, player, prepared.object_id);
    }

    false
}

/// Returns true if the player can pay this mana cost after auto-tapping
/// currently activatable mana sources in a cloned game state.
///
/// Used by legal action generation so the frontend and engine agree on whether
/// a spell is castable from the current board state.
fn can_pay_mana_cost_after_auto_tap_with_context(
    mut simulated: GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    excluded_sources: &HashSet<ObjectId>,
) -> bool {
    let mut tap_events: Vec<crate::types::events::GameEvent> = Vec::new();
    super::casting_costs::auto_tap_mana_sources_with_context_excluding(
        &mut simulated,
        player,
        cost,
        &mut tap_events,
        source_id,
        ctx,
        excluded_sources,
    );

    // CR 605.4a: A `TapsForMana` triggered mana ability (Leyline of Abundance /
    // Fertile Ground / Wild Growth / Utopia Sprawl class) resolves inline,
    // adding bonus mana to the pool, when a source is tapped for mana. The
    // auto-tap helper emits the `ManaAdded` events but does not resolve those
    // triggers; this affordability preview and the real cost-payment path share
    // `resolve_tap_mana_triggers_inline` as the single authority so they cannot
    // diverge (a divergence was the original bug — the preview said a spell was
    // castable while the real cast failed "Cannot pay mana cost").
    super::triggers::resolve_tap_mana_triggers_inline(&mut simulated, &mut tap_events, 0);

    let any_color = player_can_spend_as_any_color_for_payment(&simulated, player, source_id, ctx);
    // CR 107.4f + CR 118.1 + CR 118.3 + CR 119.8: Bundle the payer's
    // payment-time permissions (`any_color`, `max_life`, `life_colors`) so
    // K'rrik-style life-for-{B} grants are visible to the affordability check.
    let permissions =
        super::static_abilities::build_cost_permission_context(&simulated, player, any_color);
    simulated
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|player_data| {
            mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, ctx, permissions)
                || ctx.is_some_and(|ctx| {
                    matches!(ctx, PaymentContext::Spell(_))
                        && source_id.is_some_and(|source_id| {
                            can_pay_with_spell_tap_payments(
                                &simulated,
                                player,
                                source_id,
                                cost,
                                Some(ctx),
                                permissions,
                            )
                        })
                })
        })
}

/// CR 702.51a: Convoke functions on the spell being cast.
/// CR 702.126a: Improvise functions on the spell being cast.
/// Resolve the active tap-payment mode once from the spell's effective keyword set.
pub(super) fn spell_tap_payment_mode(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
) -> Option<ConvokeMode> {
    if !state.objects.contains_key(&source_id) {
        return None;
    }
    let effective_keywords = effective_spell_keywords(state, player, source_id);
    if effective_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Convoke))
    {
        Some(ConvokeMode::Convoke)
    } else if effective_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Waterbend))
    {
        Some(ConvokeMode::Waterbend)
    } else if effective_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Improvise))
    {
        Some(ConvokeMode::Improvise)
    } else if effective_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Delve))
    {
        // CR 702.66a: Delve exiles graveyard cards to pay generic mana.
        Some(ConvokeMode::Delve)
    } else {
        None
    }
}

fn can_pay_with_spell_tap_payments(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    permissions: crate::types::mana::CostPermissionContext,
) -> bool {
    let Some(mode) = spell_tap_payment_mode(state, player, source_id) else {
        return false;
    };
    let Some(player_data) = state.players.iter().find(|p| p.id == player) else {
        return false;
    };

    // CR 601.2h: This is an affordability preview only. The real payment still
    // flows through ManaPayment and the shared mana-payment algorithm.
    match mode {
        ConvokeMode::Improvise => {
            // CR 702.126a: Improvise lets players tap untapped artifacts to pay generic mana.
            let mut pool = player_data.mana_pool.clone();
            for (&object_id, obj) in &state.objects {
                if obj.is_improvise_eligible(player) {
                    pool.add(crate::types::mana::ManaUnit::convoke_payment(
                        crate::types::mana::ManaType::Colorless,
                        object_id,
                    ));
                }
            }
            mana_payment::can_pay_for_spell(&pool, cost, ctx, permissions)
        }
        ConvokeMode::Waterbend => {
            let mut pool = player_data.mana_pool.clone();
            for (&object_id, obj) in &state.objects {
                if obj.is_waterbend_eligible(player) {
                    pool.add(crate::types::mana::ManaUnit::new(
                        crate::types::mana::ManaType::Colorless,
                        object_id,
                        false,
                        Vec::new(),
                    ));
                }
            }
            mana_payment::can_pay_for_spell(&pool, cost, ctx, permissions)
        }
        ConvokeMode::Convoke => {
            // CR 702.51a: Convoke lets players tap untapped creatures to pay colored or generic mana.
            let options = state
                .objects
                .iter()
                .filter_map(|(_, obj)| {
                    if !obj.is_convoke_eligible(player) {
                        return None;
                    }
                    let mut choices = vec![crate::types::mana::ManaType::Colorless];
                    for color in &obj.color {
                        let mana_type = super::mana_sources::mana_color_to_type(color);
                        if !choices.contains(&mana_type) {
                            choices.push(mana_type);
                        }
                    }
                    Some(choices)
                })
                .collect::<Vec<_>>();
            can_pay_with_convoke_options(&player_data.mana_pool, cost, ctx, permissions, &options)
        }
        ConvokeMode::Delve => {
            // CR 702.66a: each card in the caster's graveyard can be exiled to pay
            // one generic mana. Model each as a generic-only colorless unit, exactly
            // like Improvise, so a spell castable only with delve is offered.
            let mut pool = player_data.mana_pool.clone();
            for (&object_id, obj) in &state.objects {
                if obj.zone == Zone::Graveyard && obj.owner == player {
                    pool.add(crate::types::mana::ManaUnit::convoke_payment(
                        crate::types::mana::ManaType::Colorless,
                        object_id,
                    ));
                }
            }
            mana_payment::can_pay_for_spell(&pool, cost, ctx, permissions)
        }
    }
}

// CR 702.51a: Evaluate valid creature-tap choices that can satisfy a convoke cost.
fn can_pay_with_convoke_options(
    base_pool: &crate::types::mana::ManaPool,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    permissions: crate::types::mana::CostPermissionContext,
    options: &[Vec<crate::types::mana::ManaType>],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let max_taps = cost.mana_value() as usize;
    if max_taps == 0 {
        return false;
    }

    let mut states = HashSet::from([[0u8; 6]]);
    for choices in options {
        let mut next = states.clone();
        for state in &states {
            if state.iter().map(|count| *count as usize).sum::<usize>() >= max_taps {
                continue;
            }
            for choice in choices {
                let mut candidate = *state;
                let index = mana_type_index(*choice);
                candidate[index] = candidate[index].saturating_add(1);
                next.insert(candidate);
            }
        }
        states = next;
    }

    states.into_iter().any(|counts| {
        let mut pool = base_pool.clone();
        for (index, count) in counts.into_iter().enumerate() {
            for _ in 0..count {
                pool.add(crate::types::mana::ManaUnit::convoke_payment(
                    mana_type_from_index(index),
                    ObjectId(0),
                ));
            }
        }
        mana_payment::can_pay_for_spell(&pool, cost, ctx, permissions)
    })
}

fn mana_type_index(mana_type: crate::types::mana::ManaType) -> usize {
    match mana_type {
        crate::types::mana::ManaType::White => 0,
        crate::types::mana::ManaType::Blue => 1,
        crate::types::mana::ManaType::Black => 2,
        crate::types::mana::ManaType::Red => 3,
        crate::types::mana::ManaType::Green => 4,
        crate::types::mana::ManaType::Colorless => 5,
    }
}

fn mana_type_from_index(index: usize) -> crate::types::mana::ManaType {
    match index {
        0 => crate::types::mana::ManaType::White,
        1 => crate::types::mana::ManaType::Blue,
        2 => crate::types::mana::ManaType::Black,
        3 => crate::types::mana::ManaType::Red,
        4 => crate::types::mana::ManaType::Green,
        _ => crate::types::mana::ManaType::Colorless,
    }
}

pub fn can_pay_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    let mut simulated = state.clone();
    super::layers::flush_layers(&mut simulated);
    let spell_meta = build_spell_meta(&simulated, player, source_id);

    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    can_pay_mana_cost_after_auto_tap_with_context(
        simulated,
        player,
        Some(source_id),
        cost,
        spell_ctx.as_ref(),
        &HashSet::new(),
    )
}

/// Castability-gate feasibility predicate. Returns true if `player` could pay
/// `cost` for casting `source_id` by **any** combination of auto-taps PLUS
/// manual activation of non-tap mana abilities (Sacrifice — KCI, Phyrexian
/// Altar, Ashnod's Altar; Discard — Lion's Eye Diamond; Pay Life; etc.) during
/// the cost-payment step.
///
/// This is at least as permissive as [`can_pay_cost_after_auto_tap`]: it short-
/// circuits to that path first and only attempts the manual extension when the
/// auto-tap simulator alone cannot cover the cost. Callers that must require
/// pure auto-payability (`pay_mana_cost`, the `Auto`-mode auto-finalize check
/// in `casting_costs::enter_payment_step`) must continue to call the auto-tap
/// predicate directly — only the castability/legal-actions surface widens to
/// "manual is reachable."
///
/// Colored-shard feasibility under non-tap sources is evaluated via
/// [`super::mana_sources::can_cover_shards_with_activatable_mana`], which
/// respects CR 106.6 spend restrictions and avoids double-counting the same
/// activation toward both shard and generic coverage (issues #583, #2011).
//
// CR 117.1d + CR 601.2g: Mana abilities (including sacrifice-cost,
// discard-cost, and pay-life mana abilities) may be activated during cost
// payment. Castability must account for them, or spells with feasibly payable
// costs are never offered (the original #562 bug).
pub(super) fn can_feasibly_pay_mana_cost(
    state: &GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    // CR 601.2f + CR 107.1b: Affordability must check a concrete X value, not
    // the symbolic `{X}` shard left in the cost (issue #2011: Kozilek's Command
    // `{X}{C}{C}` with only Eldrazi Temple was treated as uncastable). X only
    // adds generic mana, so X=0 is the cheapest concrete affordability probe.
    if let Some(sid) = source_id {
        if super::casting_costs::cost_has_x(cost) {
            let mut concrete = cost.clone();
            concrete.concretize_x(0);
            return can_feasibly_pay_mana_cost_without_x(state, player, Some(sid), &concrete);
        }
    }
    can_feasibly_pay_mana_cost_without_x(state, player, source_id, cost)
}

fn can_feasibly_pay_mana_cost_without_x(
    state: &GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    // CR 117.1d: Auto-tap path remains the fast path. Anything that can be
    // paid with only `{T}` activations was castable before this predicate
    // existed and must continue to be castable now.
    if let Some(sid) = source_id {
        if can_pay_cost_after_auto_tap(state, player, sid, cost) {
            return true;
        }
    }

    let crate::types::mana::ManaCost::Cost { .. } = cost else {
        // NoCost / SelfManaCost are unconditionally payable (they have no
        // mana shards to cover); the auto-tap path already returned true above
        // when `source_id` was `Some`, so this only fires for the rare
        // `source_id == None` callers.
        return true;
    };

    // Reduce the cost by the current floating mana pool. `reduce_cost_by_pool`
    // is the dry-run twin of the real payment path — it respects spell
    // restrictions and any-color permissions exactly as the real pay does.
    let Some(player_data) = state.players.iter().find(|p| p.id == player) else {
        return false;
    };

    let spell_meta = source_id.and_then(|sid| build_spell_meta(state, player, sid));
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    let any_color = source_id.is_some_and(|sid| {
        player_can_spend_as_any_color_for_payment(state, player, Some(sid), spell_ctx.as_ref())
    });
    let residual = mana_payment::reduce_cost_by_pool(
        &player_data.mana_pool,
        cost,
        spell_ctx.as_ref(),
        any_color,
        None,
    );

    let (residual_shards, residual_generic) = match &residual {
        crate::types::mana::ManaCost::NoCost
        | crate::types::mana::ManaCost::SelfManaCost
        | crate::types::mana::ManaCost::SelfManaValue => return true,
        crate::types::mana::ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    // CR 117.1d + CR 601.2g: Residual shard feasibility under non-tap mana
    // sources (issue #583: Vivi Ornitier {0} combination mana; extends #1234).
    let (shards_covered, shard_consumed) =
        super::mana_sources::can_cover_shards_with_activatable_mana(
            state,
            player,
            source_id,
            spell_ctx.as_ref(),
            residual_shards,
        );
    if !residual_shards.is_empty() && !shards_covered {
        return false;
    }
    if residual_generic == 0 {
        return true;
    }

    // CR 117.1d + CR 605.3a: Sum the per-permanent feasible mana capacity
    // across the controller's untapped non-excluded battlefield permanents.
    // Each contribution is the largest single mana-ability output the
    // controller could currently activate (covering Sacrifice / Discard /
    // PayLife costs that auto-tap cannot simulate).
    //
    // Subtract mana already allocated to shard coverage so one activation is
    // not counted twice (issue #583 review: power-2 Vivi must not cover {1}
    // generic after paying {U}{R}).
    //
    // The per-permanent sum over-counts in chain-sacrifice configurations
    // (e.g. 2× KCI + 1 fodder reports cap=4 when the actual reachable yield
    // is 2). The trade-off — over-count rather than under-count, since
    // under-count was the original #562 bug — is intentional. A bounded-
    // flow model that respects sacrifice/discard/life supply is tracked in
    // issue #1235.
    let excluded = source_id;
    let capacity: u32 = state
        .battlefield
        .iter()
        .filter(|id| Some(**id) != excluded)
        .map(|&id| {
            super::mana_sources::feasible_mana_capacity(state, id, player, spell_ctx.as_ref())
        })
        .sum::<u32>()
        .saturating_sub(shard_consumed);

    capacity >= residual_generic
}

/// Returns true if the player can pay this activated-ability mana cost after
/// auto-tapping currently activatable mana sources in a cloned game state.
pub fn can_pay_ability_mana_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    can_pay_ability_mana_cost_after_auto_tap_excluding(
        state,
        player,
        source_id,
        cost,
        &HashSet::new(),
    )
}

pub fn can_pay_ability_mana_cost_after_auto_tap_excluding(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    excluded_sources: &HashSet<ObjectId>,
) -> bool {
    let mut simulated = state.clone();
    super::layers::flush_layers(&mut simulated);

    let (source_types, source_subtypes) = activation_source_types(&simulated, source_id);
    // CR 106.6: All current callers of this preview path are tag-`None`
    // activations (mana abilities, ninjutsu, AI affordability). The real
    // tag-scoped gate (Quinjet power-up restriction) runs at payment time in
    // `pay_ability_mana_cost_*`, since `is_payable` defers mana affordability to
    // the payment step (CR 601.2g).
    let activation_ctx = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
        ability_tag: None,
    };

    can_pay_mana_cost_after_auto_tap_with_context(
        simulated,
        player,
        Some(source_id),
        cost,
        Some(&activation_ctx),
        excluded_sources,
    )
}

/// Returns true if the player can pay a resolution-time mana cost after
/// auto-tapping mana sources. This is distinct from spell-casting and
/// activated-ability payments: CR 106.6 restrictions that name those categories
/// must not become eligible for a generic "you may pay" effect during
/// resolution.
pub(super) fn can_pay_effect_mana_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    let mut simulated = state.clone();
    super::layers::flush_layers(&mut simulated);

    let mut tap_events: Vec<crate::types::events::GameEvent> = Vec::new();
    let effect_ctx = PaymentContext::Effect;
    super::casting_costs::auto_tap_mana_sources_with_context(
        &mut simulated,
        player,
        cost,
        &mut tap_events,
        Some(source_id),
        Some(&effect_ctx),
    );
    // CR 605.4a: Resolve coupled `TapsForMana` triggered mana abilities inline
    // so the bonus mana is in the simulated pool — same authority the real
    // payment path uses, keeping preview and execution in lockstep.
    super::triggers::resolve_tap_mana_triggers_inline(&mut simulated, &mut tap_events, 0);

    let any_color = player_can_spend_as_any_color_for_optional_spell(&simulated, player, None);
    // CR 107.4f + CR 118.1 + CR 118.3 + CR 119.8: Effect-time resolution
    // mana payments share the same payment-permission bundle as cast/activation.
    let permissions =
        super::static_abilities::build_cost_permission_context(&simulated, player, any_color);
    simulated
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|player_data| {
            mana_payment::can_pay_for_spell(
                &player_data.mana_pool,
                cost,
                Some(&effect_ctx),
                permissions,
            )
        })
}

// Target/mode selection handlers are in casting_targets module.
pub(crate) use super::casting_targets::{
    handle_choose_target, handle_select_modes, handle_select_targets,
};

/// Activate an ability from a permanent on the battlefield.
/// Check whether an ability cost includes a tap component (either directly or
/// within a composite). Used for pre-validation before presenting modal choices.
fn requires_untapped(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Composite { costs } => costs.iter().any(requires_untapped),
        _ => false,
    }
}

pub(super) fn ability_mana_payment_excluded_sources(
    cost: &AbilityCost,
    source_id: ObjectId,
) -> HashSet<ObjectId> {
    if requires_untapped(cost) {
        HashSet::from([source_id])
    } else {
        HashSet::new()
    }
}

/// Pay a mana cost by auto-tapping lands and deducting from the player's mana pool.
///
/// Used by spell casting (`pay_and_push`). Builds a `PaymentContext::Spell` from
/// the cast object's types so CR 106.6 spell-side restrictions (`allows_spell`)
/// gate which restricted mana is eligible. For ability activation, use
/// `pay_ability_mana_cost` instead so restrictions are evaluated against the
/// source permanent's types via `allows_activation`.
pub(super) fn pay_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_mana_cost_with_choices(state, player, source_id, cost, None, events)
}

/// CR 107.4f + CR 601.2f: Pay a spell's mana cost, honoring explicit per-shard
/// Phyrexian choices when provided. `None` preserves the legacy auto-decide
/// behavior (prefer mana, fall back to life).
pub(super) fn pay_mana_cost_with_choices(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    super::layers::flush_layers(state);

    let spell_meta = build_spell_meta(state, player, source_id);
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);

    let spent_units = auto_tap_and_pay_cost(
        state,
        player,
        source_id,
        cost,
        spell_ctx.as_ref(),
        phyrexian_choices,
        events,
    )?;

    let spent_convoke_sources = spent_units
        .iter()
        .filter(|unit| unit.is_convoke_payment())
        .map(|unit| unit.source_id)
        .collect::<HashSet<_>>();
    cleanup_unused_convoke_payments(state, player, source_id, &spent_convoke_sources);

    // CR 702.51a: Convoke taps are consumed by the payment algorithm but are
    // not mana spent to cast the spell.
    let mana_spent_units = spent_units
        .iter()
        .filter(|unit| !unit.is_convoke_payment())
        .cloned()
        .collect::<Vec<_>>();

    // CR 106.6: Apply mana spell grants to the spell being cast.
    apply_mana_spell_grants(state, source_id, &mana_spent_units);

    // CR 601.2h: Track whether mana was actually spent to cast this spell,
    // the per-color breakdown for Adamant-style intervening-if checks
    // (CR 207.2c), and source snapshots for "mana from <source>" queries.
    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.mana_spent_to_cast = false;
        obj.mana_spent_to_cast_amount = 0;
        obj.colors_spent_to_cast = crate::types::mana::ColoredManaCount::default();
        obj.mana_spent_source_snapshots.clear();
    }

    if !mana_spent_units.is_empty() {
        let source_snapshots: Vec<_> = mana_spent_units
            .iter()
            .filter_map(|unit| {
                state
                    .objects
                    .get(&unit.source_id)
                    .map(|source| source.snapshot_for_mana_spent())
                    .or_else(|| state.lki_cache.get(&unit.source_id).cloned())
                    .map(|lki| crate::types::game_state::ManaSpentSourceSnapshot {
                        source_id: unit.source_id,
                        lki,
                    })
            })
            .collect();
        if let Some(obj) = state.objects.get_mut(&source_id) {
            obj.mana_spent_to_cast = true;
            obj.mana_spent_to_cast_amount = mana_spent_units.len() as u32;
            for unit in &mana_spent_units {
                obj.colors_spent_to_cast.add_unit(unit);
            }
            obj.mana_spent_source_snapshots = source_snapshots;
        }
    }

    Ok(())
}

fn cleanup_unused_convoke_payments(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    spent_sources: &HashSet<ObjectId>,
) {
    let convoked_sources = state
        .pending_cast
        .as_ref()
        .filter(|pending| pending.object_id == source_id)
        .map(|pending| pending.convoked_creatures.clone())
        .or_else(|| {
            state
                .objects
                .get(&source_id)
                .map(|obj| obj.convoked_creatures.clone())
        })
        .unwrap_or_default();
    if convoked_sources.is_empty() {
        return;
    }

    let mut unused_sources = Vec::new();
    let spent_convoked_sources = convoked_sources
        .into_iter()
        .filter(|object_id| {
            let spent = spent_sources.contains(object_id);
            if !spent {
                unused_sources.push(*object_id);
            }
            spent
        })
        .collect::<Vec<_>>();

    if let Some(pending) = state
        .pending_cast
        .as_mut()
        .filter(|pending| pending.object_id == source_id)
    {
        pending.convoked_creatures = spent_convoked_sources.clone();
    }
    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.convoked_creatures = spent_convoked_sources;
    }

    for object_id in unused_sources {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.tapped = false;
        }
    }

    if let Some(player_data) = state.players.iter_mut().find(|p| p.id == player) {
        player_data
            .mana_pool
            .mana
            .retain(|unit| !unit.is_convoke_payment());
    }
}

/// CR 106.6: Pay the mana cost of an activated ability. Unlike `pay_mana_cost`
/// (which builds a spell context and consults `allows_spell`), this builds a
/// `PaymentContext::Activation` from the source permanent's core types and
/// subtypes so restrictions like Flamebraider's "activate abilities of
/// Elemental sources" and Heart of Ramos's "activate abilities only" are
/// enforced correctly at the spend gate.
///
/// Callers: `pay_ability_cost` for `AbilityCost::Mana` sub-costs. Spell-side
/// bookkeeping (mana-spent-to-cast, spell grants) is intentionally skipped —
/// those are cast-only concerns.
pub(super) fn pay_ability_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ability_tag: Option<crate::types::ability::AbilityTag>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_ability_mana_cost_excluding(
        state,
        player,
        source_id,
        cost,
        ability_tag,
        events,
        &HashSet::new(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_ability_mana_cost_excluding(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ability_tag: Option<crate::types::ability::AbilityTag>,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
    // CR 107.4b + CR 118.10: When this ability is paying its mana sub-cost while
    // funding an outer cost on the call stack, the outer cost's colored shard
    // demand is threaded so the sub-cost's generic pips are funded from
    // non-demanded mana. `None` for ordinary top-level ability activations.
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
) -> Result<(), EngineError> {
    pay_ability_mana_cost_with_choices_excluding(
        state,
        player,
        source_id,
        cost,
        ability_tag,
        None,
        events,
        excluded_sources,
        sub_cost_demand,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_ability_mana_cost_with_choices_excluding(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ability_tag: Option<crate::types::ability::AbilityTag>,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
) -> Result<(), EngineError> {
    super::layers::flush_layers(state);

    let (source_types, source_subtypes) = activation_source_types(state, source_id);
    let activation_ctx = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
        ability_tag,
    };

    let _spent_units = auto_tap_and_pay_cost_excluding(
        state,
        player,
        source_id,
        cost,
        Some(&activation_ctx),
        phyrexian_choices,
        events,
        excluded_sources,
        sub_cost_demand,
    )?;

    Ok(())
}

/// Pay a mana cost during effect resolution. Resolution-time "you may pay"
/// effects are neither spell casts nor activated-ability activations, so
/// restricted mana is checked through `PaymentContext::Effect`.
pub(super) fn pay_effect_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_non_cast_mana_cost(
        state,
        player,
        Some(source_id),
        cost,
        PaymentContext::Effect,
        events,
    )
}

/// CR 116.2m + CR 709.5e: Pay a special action's mana cost (e.g. a Room's unlock
/// cost) through a `PaymentContext::SpecialAction`, so CR 106.6 special-action
/// spend restrictions (Smoky Lounge's "spend this mana only to … unlock doors")
/// gate which restricted mana is eligible. Routes through the same single
/// authority as effect-time payments, differing only in the payment context.
pub(crate) fn pay_special_action_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
    action: crate::types::mana::SpecialAction,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_non_cast_mana_cost(
        state,
        player,
        source_id,
        cost,
        PaymentContext::SpecialAction(action),
        events,
    )
}

pub(crate) fn can_pay_special_action_mana_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
    action: crate::types::mana::SpecialAction,
) -> bool {
    let ctx = PaymentContext::SpecialAction(action);
    can_pay_mana_cost_after_auto_tap_with_context(
        state.clone(),
        player,
        source_id,
        cost,
        Some(&ctx),
        &HashSet::new(),
    )
}

/// CR 106.6: Single-authority core for non-cast, non-activation mana payments
/// (effect-resolution costs and special-action costs). Auto-taps sources,
/// validates affordability, and executes the spend with the given payment
/// context so restriction gating routes through the correct rules category.
fn pay_non_cast_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: Option<ObjectId>,
    cost: &crate::types::mana::ManaCost,
    ctx: PaymentContext<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    super::layers::flush_layers(state);

    let events_before = events.len();
    super::casting_costs::auto_tap_mana_sources_with_context(
        state,
        player,
        cost,
        events,
        source_id,
        Some(&ctx),
    );
    // CR 605.4a: Resolve coupled `TapsForMana` triggered mana abilities inline
    // so their bonus mana is in the pool before the affordability check.
    super::triggers::resolve_tap_mana_triggers_inline(state, events, events_before);

    let permissions = {
        let any_color = player_can_spend_as_any_color_for_optional_spell(state, player, None);
        super::static_abilities::build_cost_permission_context(state, player, any_color)
    };
    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        if !mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, Some(&ctx), permissions) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let (spent_units, life_payments) = mana_payment::pay_cost_with_demand_and_choices(
        &mut player_data.mana_pool,
        cost,
        None,
        Some(&ctx),
        permissions.any_color,
        None,
        permissions.life_colors,
        // CR 118.3a: non-cast mana costs (effects/special actions) are not pinnable.
        &[],
    )
    .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;
    if !spent_units.is_empty() && mana_payment::has_unspent_mana_continuous_effects(state) {
        state.layers_dirty.mark_full();
    }

    for payment in &life_payments {
        let amount = u32::try_from(payment.amount).unwrap_or(0);
        match super::life_costs::pay_life_as_cost(state, player, amount, events) {
            super::life_costs::PayLifeCostResult::Paid { .. } => {}
            super::life_costs::PayLifeCostResult::InsufficientLife
            | super::life_costs::PayLifeCostResult::Prohibited => {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay Phyrexian life cost".to_string(),
                ));
            }
        }
    }

    Ok(())
}

/// Shared mana-payment core: auto-taps sources, validates affordability,
/// executes the spend with the given payment context, and processes any
/// Phyrexian life payments. Returns the spent units so spell-specific callers
/// can apply grants / bookkeeping. Single authority for restriction gating.
fn auto_tap_and_pay_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<Vec<crate::types::mana::ManaUnit>, EngineError> {
    auto_tap_and_pay_cost_excluding(
        state,
        player,
        source_id,
        cost,
        ctx,
        phyrexian_choices,
        events,
        &HashSet::new(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn auto_tap_and_pay_cost_excluding(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
) -> Result<Vec<crate::types::mana::ManaUnit>, EngineError> {
    let events_before = events.len();
    super::casting_costs::auto_tap_mana_sources_with_context_excluding(
        state,
        player,
        cost,
        events,
        Some(source_id),
        ctx,
        excluded_sources,
    );
    // CR 605.4a: Resolve coupled `TapsForMana` triggered mana abilities inline
    // so their bonus mana is in the pool before the affordability check (and
    // before the spend). The post-action trigger scan skips what is resolved
    // here via the `FromTapTriggersResolved` marker — no double-fire.
    super::triggers::resolve_tap_mana_triggers_inline(state, events, events_before);

    // CR 107.4f + CR 118.1 + CR 118.3 + CR 119.8: Bundle payment-time permissions
    // (`any_color`, `max_life`, `life_colors`) once for the cast — K'rrik-style
    // life-for-{B} grants flow through the same dry-run + execution helpers.
    let permissions = {
        let any_color =
            player_can_spend_as_any_color_for_payment(state, player, Some(source_id), ctx);
        super::static_abilities::build_cost_permission_context(state, player, any_color)
    };
    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        if !mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, ctx, permissions) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    // CR 107.4b + CR 601.2f: The real spend is demand-aware. The hand demand
    // (other cards in hand needing colors) is the existing soft hybrid-resolution
    // signal; the incoming `sub_cost_demand` is the outer cost's reserved colored
    // shards when this payment is a nested mana sub-cost (CR 118.10). Combine the
    // two by element-wise max so a color reserved by EITHER is deprioritized when
    // paying a generic pip — preventing the spend from consuming a floated color
    // the outer cost still needs (Dimir/Gruul Signet bug). Computed BEFORE the
    // mutable pool borrow below to avoid a borrow-checker conflict (WATCH-ITEM #2).
    let hand_demand = mana_payment::compute_hand_color_demand(state, player, source_id);
    let combined_demand: mana_payment::ColorDemand = match sub_cost_demand {
        Some(outer) => {
            let mut d = hand_demand;
            for (slot, &reserved) in d.iter_mut().zip(outer.iter()) {
                *slot = (*slot).max(reserved);
            }
            d
        }
        None => hand_demand,
    };
    // CR 118.3a: read the caster's player-directed pin hints for THIS spell.
    // `finalize_mana_payment` moves them onto the transient `active_payment_pins`
    // (it `take()`s `pending_cast` before the spend, so they can't be read from
    // there). The funnel early-outs to legacy ordering when this slice is empty,
    // so non-manual casts and activated-ability / sub-cost payments are unaffected.
    let pins: Vec<crate::types::mana::ManaPipId> = state.active_payment_pins.clone();
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let (spent_units, life_payments) = mana_payment::pay_cost_with_demand_and_choices(
        &mut player_data.mana_pool,
        cost,
        Some(&combined_demand),
        ctx,
        permissions.any_color,
        phyrexian_choices,
        permissions.life_colors,
        &pins,
    )
    .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;
    if !spent_units.is_empty() && mana_payment::has_unspent_mana_continuous_effects(state) {
        state.layers_dirty.mark_full();
    }

    // CR 107.4f + CR 118.3b + CR 119.4 + CR 119.8: Each Phyrexian shard paid
    // with life routes through the single-authority life-cost helper so the
    // deduction IS a life-loss event (replacement pipeline + CantLoseLife
    // short-circuit apply consistently).
    for payment in &life_payments {
        let amount = u32::try_from(payment.amount).unwrap_or(0);
        match super::life_costs::pay_life_as_cast_or_activation_cost(state, player, amount, events)
        {
            super::life_costs::PayLifeCostResult::Paid { .. } => {}
            super::life_costs::PayLifeCostResult::InsufficientLife
            | super::life_costs::PayLifeCostResult::Prohibited => {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay Phyrexian life cost".to_string(),
                ));
            }
        }
    }

    Ok(spent_units)
}

/// CR 106.6: Build (core-types, subtypes) slices for a `PaymentContext::Activation`
/// from the source object. Mirrors `build_spell_meta`'s type extraction so
/// `allows_activation` and `allows_spell` consult identically-shaped strings.
pub(super) fn activation_source_types(
    state: &GameState,
    source_id: ObjectId,
) -> (Vec<String>, Vec<String>) {
    state
        .objects
        .get(&source_id)
        .map(|obj| {
            let types = object_type_names(obj);
            let subtypes = obj.card_types.subtypes.clone();
            (types, subtypes)
        })
        .unwrap_or_default()
}

/// CR 106.6: Read the keyword tag of the ability at `ability_index` on
/// `source_id`. Threaded into `PaymentContext::Activation` so tag-scoped mana
/// spend restrictions (Quinjet: "spend this mana only to activate power-up
/// abilities") can gate which mana is eligible for the activation being paid.
pub(super) fn activation_ability_tag(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<crate::types::ability::AbilityTag> {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.abilities.get(ability_index))
        .and_then(|def| def.ability_tag)
}

/// CR 106.6: When mana with spell grants is spent to cast a spell, apply those
/// grants to the spell object (e.g., "that spell can't be countered").
fn apply_mana_spell_grants(
    state: &mut GameState,
    spell_id: ObjectId,
    spent_units: &[crate::types::mana::ManaUnit],
) {
    let has_cant_be_countered = spent_units
        .iter()
        .any(|u| u.grants.contains(&ManaSpellGrant::CantBeCountered));

    if has_cant_be_countered {
        if let Some(obj) = state.objects.get_mut(&spell_id) {
            // Only add if not already present (idempotent).
            if !obj
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBeCountered)
            {
                obj.static_definitions
                    .push(StaticDefinition::new(StaticMode::CantBeCountered));
            }
        }
    }

    let Some(caster) = state.objects.get(&spell_id).map(|obj| obj.controller) else {
        return;
    };
    let spell_meta = build_spell_meta(state, caster, spell_id);
    let mut keyword_grants = Vec::new();
    for grant in spent_units.iter().flat_map(|unit| unit.grants.iter()) {
        let ManaSpellGrant::AddKeywordUntilEndOfTurn {
            keyword,
            restriction,
        } = grant
        else {
            continue;
        };
        if restriction.as_ref().is_some_and(|restriction| {
            !spell_meta
                .as_ref()
                .is_some_and(|meta| restriction.allows_spell(meta))
        }) {
            continue;
        }
        if !keyword_grants.contains(keyword) {
            keyword_grants.push(keyword.clone());
        }
    }

    for keyword in keyword_grants {
        state.add_transient_continuous_effect(
            spell_id,
            caster,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: spell_id },
            vec![ContinuousModification::AddKeyword { keyword }],
            None,
        );
    }

    // CR 106.6 + CR 603.3b: Reflexive "when you spend this mana to cast a
    // [filter] spell, [effect]" triggers (Lapis Orb of Dragonkind, Scaled
    // Nurturer, Gilanra). For each spent unit whose grant matches the spell,
    // queue the controller's ability for the same post-announcement placement
    // path used by other cost-payment triggers so same-controller ordering and
    // target/mode setup stay under the trigger dispatcher.
    for unit in spent_units {
        for grant in &unit.grants {
            let ManaSpellGrant::TriggerOnSpend {
                restriction,
                ability,
            } = grant
            else {
                continue;
            };
            // CR 106.6: Gate the reflexive trigger on the spend filter. Most
            // restrictions are evaluated purely from `SpellMeta` via
            // `allows_spell`; the commander-relational filter
            // (`SharesCreatureTypeWithCommander`) needs game state and is
            // evaluated here, the single authoritative spend-check site.
            let passes = match restriction.as_ref() {
                None => true,
                Some(crate::types::mana::ManaRestriction::SharesCreatureTypeWithCommander) => {
                    spell_meta.as_ref().is_some_and(|meta| {
                        // CR 205.3m + CR 903.3: the spell must be a creature AND
                        // share at least one creature type with the controller's
                        // commander(s).
                        let is_creature = meta
                            .types
                            .iter()
                            .any(|t| t.eq_ignore_ascii_case("Creature"));
                        if !is_creature {
                            return false;
                        }
                        let commander_types =
                            super::commander::commander_creature_types(state, caster);
                        meta.subtypes
                            .iter()
                            .any(|s| commander_types.iter().any(|c| c.eq_ignore_ascii_case(s)))
                    })
                }
                Some(restriction) => spell_meta
                    .as_ref()
                    .is_some_and(|meta| restriction.allows_spell(meta)),
            };
            if !passes {
                continue;
            }
            let timestamp = state.next_timestamp() as u32;
            let resolved =
                super::ability_utils::build_resolved_from_def(ability, unit.source_id, caster);
            super::triggers::defer_pending_trigger(
                state,
                super::triggers::PendingTrigger {
                    source_id: unit.source_id,
                    controller: caster,
                    condition: None,
                    ability: resolved,
                    timestamp,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: None,
                    modal: None,
                    mode_abilities: vec![],
                    description: ability.description.clone(),
                    may_trigger_origin: None,
                    subject_match_count: None,
                    die_result: None,
                },
            );
        }
    }
}

// Ability-activation cost payment authority extracted to `super::costs`
// (Phase 1 of the cost-payment unification plan). These `pub use` shims keep
// every existing `casting::*` / `super::casting::*` call site compiling
// unchanged while the implementation lives in `game/costs.rs`.
pub use super::costs::pay_ability_cost;
pub(crate) use super::costs::{
    pause_cost_payment_for_replacement_choice, pay_ability_cost_for_activation, PaymentOutcome,
};

fn pending_activation_after_cost_pause(
    source_id: ObjectId,
    resolved: ResolvedAbility,
    ability_index: usize,
    remaining_cost: Option<AbilityCost>,
) -> PendingCast {
    let mut pending = PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
    pending.activation_cost = remaining_cost;
    pending.activation_ability_index = Some(ability_index);
    pending
}

/// CR 118.12: Pay an "unless pays" or other non-spell/non-activation mana
/// cost. These payments happen outside spell casting and ability activation,
/// so CR 106.6 restricted mana must be checked through `PaymentContext::Effect`.
pub fn pay_unless_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_effect_mana_cost(state, player, ObjectId(0), cost, events)
}

/// Walk a cost tree and return the waterbend mana cost if present.
fn find_waterbend_cost(cost: &AbilityCost) -> Option<&ManaCost> {
    match cost {
        AbilityCost::Waterbend { cost } => Some(cost),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_waterbend_cost),
        _ => None,
    }
}

/// Walk a cost tree and return the first non-SelfRef sacrifice `(count, filter)`
/// found, if any. The `count` is honored so multi-permanent sacrifice costs
/// ("Sacrifice two creatures:") are modeled correctly.
pub(super) fn find_non_self_sacrifice_cost(cost: &AbilityCost) -> Option<(u32, &TargetFilter)> {
    match cost {
        AbilityCost::Sacrifice(cost) if !matches!(cost.target, TargetFilter::SelfRef) => cost
            .requirement
            .fixed_count()
            .map(|count| (count, &cost.target)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_sacrifice_cost),
        _ => None,
    }
}

/// Which battlefield-removing non-mana cost leg a composite carries. Each is a
/// distinct CR keyword action / zone change but all remove a permanent from the
/// battlefield (CR 701.21a Sacrifice / CR 701.13a Exile / plain bounce), so the
/// mana-leg detour in `handle_activate_ability` treats all three uniformly:
/// gate the CR 601.2g mana-first hoist when any is present.
pub(super) enum RemovalKind {
    Sacrifice,
    Exile,
    ReturnToHand,
}

/// CR 601.2g + CR 601.2h: first non-self battlefield-removing leg of `cost`, by
/// kind priority (Sacrifice > Exile > ReturnToHand). Composes the existing
/// per-kind cost walkers. Returns at most ONE leg; it gates the mana-first
/// detour in `handle_activate_ability` (presence check) — the kind it returns is
/// not used there because `push_activated_ability_to_stack` re-dispatches on each
/// per-kind walker after mana payment.
pub(super) fn find_non_self_battlefield_removal_cost(
    cost: &AbilityCost,
) -> Option<(u32, &TargetFilter, RemovalKind)> {
    if let Some((n, f)) = find_non_self_sacrifice_cost(cost) {
        return Some((n, f, RemovalKind::Sacrifice));
    }
    if let Some((n, f)) = find_battlefield_exile_cost(cost) {
        return Some((n, f, RemovalKind::Exile));
    }
    if let Some((n, Some(f))) = find_return_to_hand_cost(cost) {
        // Mirror the Sacrifice/Exile SelfRef exclusion: a self-bounce is the
        // source's own removal, not a board-shrinking non-mana leg in the
        // CR 601.2h ordering sense. Recognizing it would let the lone witness
        // remove the source and false-REJECT a self-bounce whose mana leg the
        // source itself feeds.
        if !matches!(f, TargetFilter::SelfRef) {
            return Some((n, f, RemovalKind::ReturnToHand));
        }
    }
    None
}

/// CR 701.13a: first non-self Exile leg whose *effective* source zone is the
/// battlefield, reusing the live zone classifier
/// `cost_payability::exile_cost_effective_zone` (a `zone: None` + non-permanent
/// filter resolves to Hand and MUST NOT route here — that would false-reject a
/// payable hand-exile composite). A `None` filter is out of scope. The
/// `SelfRef`-first arm is required: a SelfRef filter may be permanent-implying
/// and would otherwise pass the battlefield gate.
pub(super) fn find_battlefield_exile_cost(cost: &AbilityCost) -> Option<(u32, &TargetFilter)> {
    match cost {
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => None,
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if super::cost_payability::exile_cost_effective_zone(*zone, filter.as_ref())
            == Zone::Battlefield =>
        {
            filter.as_ref().map(|f| (*count, f))
        }
        AbilityCost::Composite { costs } => costs.iter().find_map(find_battlefield_exile_cost),
        _ => None,
    }
}

pub(crate) fn find_non_self_discard(
    cost: &AbilityCost,
) -> Option<(&QuantityExpr, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Discard {
            count,
            filter,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            ..
        } => Some((count, filter.as_ref())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_discard),
        _ => None,
    }
}

fn has_self_ref_discard_cost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Discard {
            self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
            ..
        } => true,
        AbilityCost::Composite { costs } => costs.iter().any(has_self_ref_discard_cost),
        _ => false,
    }
}

/// CR 117.1 + CR 400.7j + CR 608.2k: Self-discard activation costs move the
/// source out of hand before the ability resolves, so ability-scoped filters
/// like Transmute's same-mana-value search need a public-characteristics
/// snapshot attached to the resolving ability before cost payment.
pub(crate) fn stamp_self_ref_discard_cost_paid_object(
    state: &GameState,
    source_id: ObjectId,
    ability: &mut ResolvedAbility,
    cost: &AbilityCost,
) {
    if !has_self_ref_discard_cost(cost) {
        return;
    }
    if let Some(obj) = state.objects.get(&source_id) {
        ability.set_cost_paid_object_recursive(CostPaidObjectSnapshot {
            object_id: source_id,
            lki: obj.snapshot_for_mana_spent(),
        });
    }
}

/// CR 118.3 + CR 602.2b: Detect a non-self "exile a card from hand/graveyard"
/// activation cost requiring interactive card selection (Jhoira of the Ghitu).
/// Self-ref exile (Scavenge, Suspend) returns `None` — that shape is auto-paid
/// by `pay_ability_cost`'s self-ref exile arm and never back-referenced as a
/// cost-paid object. Recurses into `Composite`.
pub(super) fn find_non_self_exile(
    cost: &AbilityCost,
) -> Option<(u32, Zone, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => None,
        AbilityCost::Exile {
            count,
            zone: Some(z @ (Zone::Hand | Zone::Graveyard)),
            filter,
        } => Some((*count, *z, filter.as_ref())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_exile),
        _ => None,
    }
}

/// CR 117.1 + CR 601.2b: Detect an `ExileWithAggregate` activation cost (Baron
/// Helmut Zemo's Boast) requiring an interactive "exile any number reaching the
/// aggregate threshold" selection. Returns a borrowed view of its parameters.
/// Recurses into `Composite`.
#[allow(clippy::type_complexity)]
pub(super) fn find_exile_with_aggregate_cost(
    cost: &AbilityCost,
) -> Option<(
    &TargetFilter,
    crate::types::ability::AggregateFunction,
    crate::types::ability::ObjectProperty,
    crate::types::ability::Comparator,
    i32,
    Zone,
)> {
    match cost {
        AbilityCost::ExileWithAggregate {
            filter,
            function,
            property,
            comparator,
            value,
            zone,
        } => Some((filter, *function, *property, *comparator, *value, *zone)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_exile_with_aggregate_cost),
        _ => None,
    }
}

/// CR 702.167a/b: Detect a craft materials cost requiring interactive object
/// selection across the battlefield/graveyard union. Returns `(count,
/// materials)`. Recurses into `Composite` (the synthesized craft cost is a
/// `Composite[Mana, Exile{SelfRef}, ExileMaterials]`).
fn find_craft_materials_cost(cost: &AbilityCost) -> Option<(CostObjectCount, &TargetFilter)> {
    match cost {
        AbilityCost::ExileMaterials { materials, count } => Some((*count, materials)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_craft_materials_cost),
        _ => None,
    }
}

pub(super) fn find_tap_creatures_cost(
    cost: &AbilityCost,
) -> Option<(&TapCreaturesRequirement, &TargetFilter)> {
    match cost {
        AbilityCost::TapCreatures {
            requirement,
            filter,
        } => Some((requirement, filter)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_tap_creatures_cost),
        _ => None,
    }
}

fn find_targeted_remove_counter_cost(
    cost: &AbilityCost,
) -> Option<(
    u32,
    &crate::types::counter::CounterMatch,
    &TargetFilter,
    CounterCostSelection,
)> {
    match cost {
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: Some(target),
            selection,
        } => Some((*count, counter_type, target, *selection)),
        AbilityCost::Composite { costs } => {
            costs.iter().find_map(find_targeted_remove_counter_cost)
        }
        _ => None,
    }
}

/// Shared eligibility helper for hand-card cost payments — returns every card
/// in `player`'s hand matching `filter` (if any), excluding the cast source.
/// Used by both discard-as-cost (CR 601.2b) and exile-from-hand-as-cost
/// (Force of Will family). The destination zone (graveyard vs exile) is the
/// caller's concern; the eligibility set is identical.
fn find_eligible_hand_cost_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let effective_filter = super::cost_payability::exile_cost_effective_filter(filter);
    let filter_ref = effective_filter.as_ref();
    let ctx = super::filter::FilterContext::from_source(state, source);
    state
        .players
        .get(player.0 as usize)
        .map(|player_state| {
            player_state
                .hand
                .iter()
                .copied()
                .filter(|&id| {
                    id != source
                        && filter_ref.is_none_or(|f| {
                            super::filter::matches_target_filter(state, id, f, &ctx)
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn find_eligible_discard_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    find_eligible_hand_cost_targets(state, player, source, filter)
}

/// CR 601.2b + CR 601.2h: Eligible cards for an `AbilityCost::Exile` payment
/// whose `zone` is `Hand` (pitch spells) or `Graveyard` (escape, CR 702.138a).
/// The cast source itself is never eligible. The cost's `TargetFilter` is
/// applied uniformly in both branches — escape today carries no filter, but
/// any future graveyard-source exile cost with a filter relies on this.
pub(crate) fn find_eligible_exile_for_cost_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    zone: ExileCostSourceZone,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let effective_filter = super::cost_payability::exile_cost_effective_filter(filter);
    let filter_ref = effective_filter.as_ref();
    match zone {
        ExileCostSourceZone::Hand => {
            find_eligible_hand_cost_targets(state, player, source, filter_ref)
        }
        ExileCostSourceZone::Graveyard => {
            let ctx = super::filter::FilterContext::from_source(state, source);
            state
                .players
                .get(player.0 as usize)
                .map(|p| {
                    p.graveyard
                        .iter()
                        .copied()
                        .filter(|&id| {
                            id != source
                                && filter_ref.is_none_or(|f| {
                                    super::filter::matches_target_filter(state, id, f, &ctx)
                                })
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }
}

fn find_one_of_cost(cost: &AbilityCost) -> Option<&Vec<AbilityCost>> {
    match cost {
        AbilityCost::OneOf { costs } => Some(costs),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_one_of_cost),
        _ => None,
    }
}

pub(super) fn find_return_to_hand_cost(cost: &AbilityCost) -> Option<(u32, Option<&TargetFilter>)> {
    match cost {
        // CR 118.12: This helper currently only handles the default
        // battlefield-source shape (`from_zone: None`) and its explicit
        // spelling (`from_zone: Some(Battlefield)`). Cards with other
        // `from_zone` values use the unless-cost path in
        // `engine_payment_choices.rs`, not the activation-cost path here.
        AbilityCost::ReturnToHand {
            count,
            filter,
            from_zone: None | Some(Zone::Battlefield),
        } => Some((*count, filter.as_ref())),
        AbilityCost::ReturnToHand {
            from_zone: Some(_), ..
        } => None,
        AbilityCost::Composite { costs } => costs.iter().find_map(find_return_to_hand_cost),
        _ => None,
    }
}

pub(crate) fn find_eligible_return_to_hand_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && filter
                        .is_none_or(|f| super::filter::matches_target_filter(state, id, f, &ctx))
            })
        })
        .collect()
}

pub(crate) fn removable_counter_count(
    obj: &crate::game::game_object::GameObject,
    counter_type: &crate::types::counter::CounterMatch,
) -> u32 {
    match counter_type {
        crate::types::counter::CounterMatch::OfType(ty) => {
            obj.counters.get(ty).copied().unwrap_or(0)
        }
        // CR 118.3 + CR 122.1: A remove-counter cost removes one concrete
        // counter type from one object. Match the concrete-type choice used by
        // `resolve_counter_match_for_removal` by capping against the largest
        // removable stack, not the sum across unrelated counter types.
        crate::types::counter::CounterMatch::Any => {
            obj.counters.values().copied().max().unwrap_or(0)
        }
    }
}

pub(crate) fn removable_counter_count_for_cost_selection(
    obj: &crate::game::game_object::GameObject,
    counter_type: &crate::types::counter::CounterMatch,
    selection: CounterCostSelection,
) -> u32 {
    match (counter_type, selection) {
        (crate::types::counter::CounterMatch::Any, CounterCostSelection::AmongObjects) => {
            obj.counters.values().copied().sum()
        }
        _ => removable_counter_count(obj, counter_type),
    }
}

pub(crate) fn find_eligible_remove_counter_for_cost_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    target: &TargetFilter,
    counter_type: &crate::types::counter::CounterMatch,
    count: u32,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && super::filter::matches_target_filter(state, id, target, &ctx)
                    // CR 107.2 / CR 107.3a: variable remove-counter costs
                    // are eligible before the final count is announced.
                    && (is_variable_remove_counter_cost_count(count)
                        || removable_counter_count(obj, counter_type) >= count)
            })
        })
        .collect()
}

fn find_eligible_tap_creatures_for_cost(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    cost: &AbilityCost,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    let exclude_source = requires_untapped(cost);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if exclude_source && id == source {
                return false;
            }
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && !obj.tapped
                    && super::filter::matches_target_filter(state, id, filter, &ctx)
            })
        })
        .collect()
}

/// CR 702.34a + CR 118.8: Partition a flashback cost into its mana sub-cost (paid
/// through the normal mana-payment flow) and its residual non-mana sub-cost (paid
/// as an additional cost via `pay_additional_cost`).
///
/// Compound flashback costs ("Flashback—{1}{U}, Pay 3 life") are stored by the
/// parser as `FlashbackCost::NonMana(AbilityCost::Composite([Mana, PayLife, ...]))`.
/// This helper extracts the embedded `Mana` sub-cost so both halves of the cost
/// are paid through their proper pipelines. Mirrors `extract_x_mana_cost` in
/// casting_costs.rs.
///
/// Returns `(mana_sub_cost, non_mana_residual)`. Either may be `None`:
///   - Pure-mana flashback     → `(Some(mana), None)`
///   - Pure non-mana           → `(None, Some(cost))`
///   - Compound mana+non-mana  → `(Some(mana), Some(residual))`
pub(super) fn split_flashback_cost_components(
    flashback: Option<&FlashbackCost>,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    let Some(fb) = flashback else {
        return (None, None);
    };
    match fb {
        FlashbackCost::Mana(mana) => (Some(mana.clone()), None),
        FlashbackCost::NonMana(ab) => split_alt_cost_components(ab),
    }
}

/// CR 702.74a + CR 601.2f-h: Evoke twin of `split_flashback_cost_components`.
/// `EvokeCost::Mana` mirrors `FlashbackCost::Mana`; `EvokeCost::NonMana(...)`
/// delegates to the shared `split_alt_cost_components` walker.
pub(super) fn split_evoke_cost_components(
    evoke: &crate::types::keywords::EvokeCost,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    use crate::types::keywords::EvokeCost;
    match evoke {
        EvokeCost::Mana(mana) => (Some(mana.clone()), None),
        EvokeCost::NonMana(ab) => split_alt_cost_components(ab),
    }
}

/// CR 702.138a + CR 601.2f-h: Escape twin of `split_evoke_cost_components`.
/// `EscapeCost::Mana` is a bare mana sub-cost with no residual; `NonMana(...)`
/// (the printed compound — "[mana], Exile N other cards from your graveyard",
/// possibly with extra exile clauses on Lunar Hatchling) delegates to the
/// shared `split_alt_cost_components` walker, which extracts the mana sub-cost
/// for the normal mana flow (CR 601.2g) and returns the exile residual for
/// `pay_additional_cost` (CR 601.2h).
pub(super) fn split_escape_cost_components(
    escape: &crate::types::keywords::EscapeCost,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    use crate::types::keywords::EscapeCost;
    match escape {
        EscapeCost::Mana(mana) => (Some(mana.clone()), None),
        EscapeCost::NonMana(ab) => split_alt_cost_components(ab),
    }
}

/// CR 702.103a + CR 601.2f-h: Bestow twin of `split_evoke_cost_components`.
/// `BestowCost::Mana` mirrors `EvokeCost::Mana`; `BestowCost::NonMana(...)`
/// (e.g. Detective's Phoenix's "{R}, Collect evidence 6" stored as a Composite)
/// delegates to the shared `split_alt_cost_components` walker, which extracts the
/// `{R}` mana sub-cost for the normal mana flow (CR 601.2g) and returns the
/// Collect-evidence residual for `pay_additional_cost` (CR 601.2h).
pub(super) fn split_bestow_cost_components(
    bestow: &crate::types::keywords::BestowCost,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    use crate::types::keywords::BestowCost;
    match bestow {
        BestowCost::Mana(mana) => (Some(mana.clone()), None),
        BestowCost::NonMana(ab) => split_alt_cost_components(ab),
    }
}

/// CR 601.2f-h: Partition an arbitrary `AbilityCost` into its mana sub-cost
/// (paid through the normal mana-payment phase per CR 601.2g) and the
/// non-mana residual (paid via `pay_additional_cost` per CR 601.2h). Returns
/// `(Some(mana), None)` for a single mana cost, `(None, Some(cost))` for
/// pure non-mana, or `(Some(mana), Some(residual))` for compound costs like
/// "Flashback—{1}{U}, Pay 3 life". Lifted out of
/// `split_flashback_cost_components` so flashback/evoke share one walker.
pub(super) fn split_alt_cost_components(
    cost: &AbilityCost,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    match cost {
        AbilityCost::Mana { cost } => (Some(cost.clone()), None),
        AbilityCost::Composite { costs } => {
            // Find the (single) Mana sub-cost and partition the rest.
            let mana_idx = costs
                .iter()
                .position(|sub| matches!(sub, AbilityCost::Mana { .. }));
            match mana_idx {
                None => (
                    None,
                    Some(AbilityCost::Composite {
                        costs: costs.clone(),
                    }),
                ),
                Some(idx) => {
                    let mut remaining = costs.clone();
                    let AbilityCost::Mana { cost: extracted } = remaining.remove(idx) else {
                        unreachable!("position() guarantees Mana variant")
                    };
                    let residual = match remaining.len() {
                        0 => None,
                        1 => Some(remaining.into_iter().next().unwrap()),
                        _ => Some(AbilityCost::Composite { costs: remaining }),
                    };
                    (Some(extracted), residual)
                }
            }
        }
        other => (None, Some(other.clone())),
    }
}

/// Walk a cost tree and return the first `PayLife` amount found, resolved
/// against the given state/player/source context. Used to pre-validate
/// pay-life affordability before simulation, since `pay_ability_cost`
/// treats `AbilityCost::PayLife` as a no-op.
///
/// `QuantityExpr` resolves dynamically (e.g. War Room's
/// `QuantityRef::ColorsInCommandersColorIdentity`), so this helper must be
/// evaluated at activation time against the current game state.
fn find_pay_life_cost(
    cost: &AbilityCost,
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
) -> Option<u32> {
    match cost {
        AbilityCost::PayLife { amount } => {
            let resolved =
                super::quantity::resolve_quantity(state, amount, player, source_id).max(0) as u32;
            Some(resolved)
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .find_map(|c| find_pay_life_cost(c, state, player, source_id)),
        _ => None,
    }
}

/// CR 118.3: Find permanents controlled by `player` matching `filter` on the battlefield.
/// The source is eligible when it matches the printed filter; "another" is
/// represented by `FilterProp::Another` and enforced by `matches_target_filter`.
pub(super) fn find_eligible_sacrifice_targets(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.controller != player {
                return false;
            }
            if super::static_abilities::player_cant_sacrifice_as_cost(state, player, id) {
                return false;
            }
            super::filter::matches_target_filter(
                state,
                id,
                filter,
                &super::filter::FilterContext::from_source(state, source_id),
            )
        })
        .collect()
}

/// CR 118.3 + CR 601.2h: Activation-time affordability pre-gate. Delegates to
/// the single affordability authority [`super::costs::can_pay`], which composes
/// `AbilityCost::is_payable` (the CR 118.3 resource/choice-eligibility gate,
/// including the Waterbend auto-tap mana check) with a clone-and-dry-run of the
/// payment authority. This keeps legal-action generation in sync with
/// `handle_activate_ability`, so the AI never proposes an activation the submit
/// path would reject.
///
/// The bespoke non-self-Sacrifice / PayLife / TapCreatures pre-checks that used
/// to live here were deleted in Phase 5 — each duplicated logic already in
/// `is_payable` (proven by discriminating tests); a bare Waterbend cost is
/// answered by `is_payable`'s auto-tap check and skips the `can_pay` dry run
/// (gated on the bare `AbilityCost::Waterbend` shape).
fn can_pay_ability_cost_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    ability_tag: Option<crate::types::ability::AbilityTag>,
) -> bool {
    let excluded_sources = ability_mana_payment_excluded_sources(cost, source_id);
    super::costs::can_pay(
        state,
        player,
        source_id,
        cost,
        &super::costs::PaymentScope::Activation {
            excluded_sources: &excluded_sources,
            ability_tag,
        },
    )
}

/// CR 602.2a: Whether `player` may begin to activate an activated ability on
/// a permanent controlled by `source_controller`.
fn player_may_begin_activating(
    state: &GameState,
    player: PlayerId,
    source_controller: PlayerId,
    activator_filter: Option<&PlayerFilter>,
) -> bool {
    match activator_filter {
        None | Some(PlayerFilter::Controller) => player == source_controller,
        Some(PlayerFilter::All) => true,
        Some(PlayerFilter::Opponent) => {
            super::players::is_opponent(state, source_controller, player)
        }
        // Activator permission is only modeled for controller / all / opponent today.
        Some(_) => player == source_controller,
    }
}

pub fn can_activate_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    let Some(mut ability_def) = activation_ability_definition(state, source_id, ability_index)
    else {
        return false;
    };
    if !player_may_begin_activating(
        state,
        player,
        obj.controller,
        ability_def.activator_filter.as_ref(),
    ) {
        return false;
    }

    // CR 702.61a + CR 702.61b: While a spell with split second is on the stack,
    // players can't activate abilities that aren't mana abilities.
    if super::keywords::stack_has_split_second(state)
        && !super::mana_abilities::is_mana_ability(&ability_def)
    {
        return false;
    }

    // CR 602.1: Check activation zone — default to battlefield.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return false;
    }
    // CR 701.35a: Detained permanents' activated abilities can't be activated.
    if !obj.detained_by.is_empty() {
        return false;
    }
    // CR 702.170b + CR 116.2k + CR 602.1c: Plot is a SPECIAL ACTION, not an activated
    // ability, so the activated-ability prohibition gates must not block it. Mirrors
    // the guard in `handle_activate_ability` so the legality gate and the runtime
    // activation path agree (otherwise `candidates.rs` would never offer plot under a
    // Pithing-Needle / City-of-Solitude / Damping-Matrix-class static while
    // `handle_activate_ability` still permits it). Plot's timing is enforced by
    // AsSorcery in check_activation_restrictions below, outside this guard.
    if !is_plot_special_action(&ability_def) {
        // CR 602.5 + CR 603.2a: Consult active CantBeActivated statics — a player can't
        // begin to activate an ability that's prohibited from being activated. Note this
        // only affects activated abilities (CR 603.2a: triggered abilities are unaffected
        // and use SuppressTriggers instead).
        // CR 605.1a: The ability definition is passed through so the prohibition can apply
        // its mana-ability exemption (Pithing Needle class) via the single classifier authority.
        if is_blocked_by_cant_be_activated(state, player, source_id, &ability_def) {
            return false;
        }
        // CR 602.5 + CR 117.1b: Time-axis activation prohibition (City of Solitude class).
        if is_blocked_by_cant_activate_during(state, player, &ability_def) {
            return false;
        }
        if is_blocked_by_cant_activate_abilities(state, player, &ability_def) {
            return false;
        }
    }
    if restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )
    .is_err()
    {
        return false;
    }
    // CR 606.3: A loyalty ability may be activated only if no player has previously
    // activated a loyalty ability of *that permanent* this turn. The generic
    // `OnlyOnceEachTurn` activation restriction tracks per `(source_id, ability_index)`,
    // which is the wrong granularity — it would let each loyalty ability fire once.
    // Defer to `can_activate_loyalty`, the single authority for the per-permanent gate.
    if ability_def
        .cost
        .as_ref()
        .is_some_and(crate::types::ability::is_loyalty_ability_cost)
        && !super::planeswalker::can_activate_loyalty_ability(
            state,
            source_id,
            player,
            ability_index,
        )
    {
        return false;
    }
    // CR 302.6 + CR 602.5a: Universal summoning-sickness gate for {T}/{Q} activated
    // abilities on creatures. Applies to every activated ability regardless of Oracle
    // text, so it lives as a structural helper rather than an ActivationRestriction.
    if let Some(ref cost) = ability_def.cost {
        if restrictions::check_summoning_sickness_for_cost(state, obj, cost).is_err() {
            return false;
        }
    }
    // CR 601.2f: Apply self-referential cost reduction before affordability check.
    apply_cost_reduction(state, &mut ability_def, player, source_id);
    if ability_def.cost.as_ref().is_some_and(|cost| {
        !can_pay_ability_cost_now(state, player, source_id, cost, ability_def.ability_tag)
    }) {
        return false;
    }

    if let Some(ref modal) = ability_def.modal {
        if ability_def.cost.as_ref().is_some_and(requires_untapped) && obj.tapped {
            return false;
        }
        return modal.mode_count > 0;
    }

    // CR 608.2 + CR 109.5: Build via the canonical helper so target-slot
    // collection sees `multi_target`, `target_choice_timing`, `player_scope`,
    // and the rest of the ability surface that affects legality. Mirrors the
    // spell-cast path fix from issue #310.
    let resolved = build_resolved_from_def(&ability_def, source_id, player);

    let mut simulated = state.clone();
    super::layers::flush_layers(&mut simulated);

    if let Some(has_target) = simple_legal_target_assignment_exists_for_ability(
        &simulated,
        &resolved,
        &ability_def.target_constraints,
    ) {
        return has_target;
    }

    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            if target_slots.is_empty() {
                return true;
            }
            if ability_def.target_constraints.is_empty() && target_slots.len() == 1 {
                return target_slots[0].optional || !target_slots[0].legal_targets.is_empty();
            }
            has_legal_target_assignment_for_ability(
                &simulated,
                &resolved,
                &target_slots,
                &ability_def.target_constraints,
            )
        }
        Err(_) => {
            ability_target_legality_needs_chosen_x(&resolved, ability_def.distribute.as_ref())
                && ability_def.cost.as_ref().is_some_and(|cost| {
                    casting_costs::extract_x_mana_cost(cost).is_some()
                        || find_non_self_sacrifice_cost(cost)
                            .is_some_and(|(count, _)| count == u32::MAX)
                        || casting_costs::activation_cost_needs_x_choice(&resolved, cost)
                })
        }
    }
}

/// CR 608.2c: Evaluate an activated ability's intervening-if `condition` against
/// the CURRENT game state, as it would be evaluated at resolution. Returns `None`
/// when the ability has no condition (nothing is gated) or when the condition
/// depends on resolution-time context that does not exist before activation
/// (chosen targets, the cast/trigger event, mana spent, prior-effect amounts), so
/// callers must treat only `Some(false)` as "the payoff is gated off right now".
///
/// This is a decision aid for AI value heuristics — e.g. to avoid paying a cost
/// for a hideaway land's "play the exiled card if your creatures' total power is
/// 10 or greater" when the threshold is unmet. The engine deliberately does NOT
/// gate activation legality on this condition (CR 602.5 + the Shelldock Isle
/// ruling: the ability is legal to activate regardless; only the effect is gated
/// at resolution), so this must never be used as a legality gate.
pub fn ability_condition_currently_met(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<bool> {
    let obj = state.objects.get(&source_id)?;
    let def = obj.abilities.get(ability_index)?;
    let condition = def.condition.as_ref()?;
    if !ability_condition_is_board_state_evaluable(condition) {
        return None;
    }
    let resolved = build_resolved_from_def(def, source_id, obj.controller);
    Some(crate::game::effects::evaluate_condition(
        condition, state, &resolved,
    ))
}

/// True when `condition` resolves purely from persistent board/controller state,
/// so evaluating it before the ability is activated is meaningful (no chosen
/// targets, no cast/trigger event, no spell context). Conservative by design: any
/// shape not positively known to be board-state-only returns `false`, so callers
/// decline to judge it rather than read uninitialized resolution context. Covers
/// the hideaway / "Cost: do X if [board condition]" class (a `QuantityCheck` whose
/// operands are board/controller-relative); extend the allowlist as new
/// board-state condition shapes need pre-activation evaluation.
fn ability_condition_is_board_state_evaluable(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            quantity_expr_is_board_state_relative(lhs) && quantity_expr_is_board_state_relative(rhs)
        }
        _ => false,
    }
}

fn quantity_expr_is_board_state_relative(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Fixed { .. } => true,
        QuantityExpr::Ref { qty } => quantity_ref_is_board_state_relative(qty),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_expr_is_board_state_relative(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().all(quantity_expr_is_board_state_relative)
        }
        _ => false,
    }
}

fn quantity_ref_is_board_state_relative(qty: &QuantityRef) -> bool {
    // A player axis is concrete (resolvable now) unless it needs a chosen target
    // or an outer scoped-player iteration context.
    let player_is_concrete =
        |p: &PlayerScope| !matches!(p, PlayerScope::Target | PlayerScope::ScopedPlayer);
    match qty {
        QuantityRef::HandSize { player }
        | QuantityRef::LifeTotal { player }
        | QuantityRef::GraveyardSize { player }
        | QuantityRef::LifeLostThisTurn { player }
        | QuantityRef::PartySize { player }
        | QuantityRef::Speed { player } => player_is_concrete(player),
        QuantityRef::LifeAboveStarting | QuantityRef::StartingLifeTotal => true,
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. } => !filter_references_target_player(filter),
        QuantityRef::CountersOn { scope, .. }
        | QuantityRef::Power { scope }
        | QuantityRef::Toughness { scope }
        | QuantityRef::ObjectManaValue { scope }
        | QuantityRef::ObjectColorCount { scope }
        | QuantityRef::ObjectNameWordCount { scope }
        | QuantityRef::ObjectTypelineComponentCount { scope } => {
            matches!(scope, ObjectScope::Source)
        }
        // Conservative default: any ref not positively known to be
        // board/controller-relative (Variable/X, target-relative scopes,
        // cast/trigger-event context, etc.) makes the condition non-evaluable
        // before activation, so the helper returns `None`.
        _ => false,
    }
}

/// CR 602.2: To activate an ability is to put it onto the stack and pay its costs.
/// CR 602.2a: Only an object's controller can activate its activated ability unless
/// the object specifically says otherwise.
pub fn handle_activate_ability(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // CR 602.2a: Only players permitted by `activator_filter` may begin activation.
    let Some(mut ability_def) = activation_ability_definition(state, source_id, ability_index)
    else {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    };
    if !player_may_begin_activating(
        state,
        player,
        obj.controller,
        ability_def.activator_filter.as_ref(),
    ) {
        return Err(EngineError::NotYourPriority);
    }
    // CR 602.1: Check activation zone — default to battlefield.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return Err(EngineError::InvalidAction(format!(
            "Object is not in the correct zone (expected {:?})",
            required_zone
        )));
    }

    // CR 702.170b + CR 116.2k + CR 602.1c: Plot is a SPECIAL ACTION, not the
    // activation of an ability. CR 602.5/603.2a prohibitions ("can't activate
    // abilities") are typed to activated abilities only, so none of the three gates
    // below may block plot. Plot's own-turn / main-phase / empty-stack timing is
    // enforced separately by ActivationRestriction::AsSorcery via
    // check_activation_restrictions (below), which stays outside this guard — so
    // skipping these gates loses no timing protection.
    if !is_plot_special_action(&ability_def) {
        // CR 602.5 + CR 603.2a: Reject activation if any CantBeActivated static
        // prohibits the player from activating this permanent's activated abilities.
        // CR 605.1a: The exemption gate (Pithing Needle's "unless they're mana
        // abilities") is applied inside `is_blocked_by_cant_be_activated`.
        if is_blocked_by_cant_be_activated(state, player, source_id, &ability_def) {
            return Err(EngineError::ActionNotAllowed(
                "Activated abilities of this permanent can't be activated (CR 602.5)".to_string(),
            ));
        }
        // CR 602.5 + CR 117.1b: Reject activation if any CantActivateDuring static
        // prohibits activation during the current turn condition (City of Solitude class).
        if is_blocked_by_cant_activate_during(state, player, &ability_def) {
            return Err(EngineError::ActionNotAllowed(
                "Activated abilities can't be activated during this turn (CR 602.5 + CR 117.1b)"
                    .to_string(),
            ));
        }
        if is_blocked_by_cant_activate_abilities(state, player, &ability_def) {
            return Err(EngineError::ActionNotAllowed(
                "A temporary effect prevents activating this ability".to_string(),
            ));
        }
    }

    // CR 601.2f: Apply self-referential cost reduction before any cost payment.
    apply_cost_reduction(state, &mut ability_def, player, source_id);

    // CR 601.2b: If the activation cost requires a choice of object and no
    // legal object exists, the ability can't be activated.
    if let Some(ref cost) = ability_def.cost {
        if !cost.is_payable(state, player, source_id) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay activation cost".to_string(),
            ));
        }
    }

    restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    // CR 302.6 + CR 602.5a: Universal summoning-sickness gate for {T}/{Q} activated
    // abilities on creatures. Mirrors the check in `can_activate_ability_now` so both
    // the AI legality gate and the runtime activation path agree.
    if let Some(ref cost) = ability_def.cost {
        let obj = state.objects.get(&source_id).ok_or_else(|| {
            EngineError::InvalidAction("Object not found during summoning-sickness check".into())
        })?;
        restrictions::check_summoning_sickness_for_cost(state, obj, cost)?;
        if requires_untapped(cost) && obj.tapped {
            return Err(EngineError::ActionNotAllowed(
                "Cannot activate tap ability: permanent is tapped".to_string(),
            ));
        }
    }

    // CR 602.2b: Announce → choose modes → choose targets → pay costs.
    // Modal detection must happen BEFORE cost payment.
    if let Some(ref modal) = ability_def.modal {
        let modal = modal_choice_for_player(
            state,
            player,
            source_id,
            modal,
            &crate::types::ability::SpellContext::default(),
        );
        // Pre-validate tap cost for modals — fail fast before presenting the choice
        if ability_def.cost.as_ref().is_some_and(requires_untapped) {
            let obj = state.objects.get(&source_id).unwrap();
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
        }
        let mut unavailable_modes = compute_unavailable_modes(state, source_id, &modal);
        let x_dependent_modal_targets = ability_def.cost.as_ref().is_some_and(|cost| {
            ability_def.mode_abilities.iter().any(|mode| {
                let resolved = build_resolved_from_def(mode, source_id, player);
                (casting_costs::extract_x_mana_cost(cost).is_some()
                    || casting_costs::activation_cost_needs_x_choice(&resolved, cost))
                    && ability_target_legality_needs_chosen_x(&resolved, mode.distribute.as_ref())
            })
        });
        // CR 602.2b + CR 601.2b/c: When modal activated ability target legality
        // depends on an {X} activation cost, legality is not knowable until the
        // player chooses X after mode selection. Do not pre-disable those modes
        // using the unchosen-X target filter; the deferred target-selection path
        // validates the chosen X before targets are committed.
        if !x_dependent_modal_targets {
            super::ability_utils::filter_modes_by_target_legality(
                state,
                source_id,
                player,
                &ability_def.mode_abilities,
                &modal,
                &mut unavailable_modes,
            );
        }
        // CR 700.2a: The controller chooses modes while activating a modal
        // ability. If every mode is illegal due to unavailable selections or
        // unsatisfied targeting requirements, the ability cannot be activated.
        if unavailable_modes.len() >= modal.mode_count {
            return Err(EngineError::ActionNotAllowed(
                "No legal modes available for activated ability".to_string(),
            ));
        }
        // CR 700.2a / CR 700.2e: `AbilityModeChoice.player` is threaded
        // downstream as the activated ability's controller (cost payment,
        // stack `controller`, target selection — see `engine_modes.rs`), so
        // it stays the controller. An opponent-chooser ACTIVATED modal ability
        // would need to route only the mode prompt to the opponent while
        // control/cost/targets stay with the controller; a single-`PlayerId`
        // `AbilityModeChoice` cannot carry both, and no such card exists in
        // the corpus — opponent-chooser activated modals are deferred (the
        // parser still records `ModalChoice.chooser` for data fidelity).
        // Modal *spells* ARE routed at the `ModeChoice` constructor above,
        // where `pending_cast` retains the controller.
        return Ok(WaitingFor::AbilityModeChoice {
            player,
            modal,
            source_id,
            mode_abilities: ability_def.mode_abilities.clone(),
            is_activated: true,
            ability_index: Some(ability_index),
            ability_cost: ability_def.cost.clone(),
            unavailable_modes,
        });
    }

    // CR 608.2 + CR 109.5: Build via the canonical helper so the activated
    // ability's `player_scope`, `kind`, `optional`, `optional_for`,
    // `multi_target`, `target_choice_timing`, `unless_pay`, `description`,
    // `else_ability`, and other typed fields survive into resolution
    // (issue #310 — same root cause as the spell-cast path).
    let mut resolved = build_resolved_from_def(&ability_def, source_id, player);
    // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking
    // before any branch path that pushes this ability onto the stack.
    resolved.ability_index = Some(ability_index);

    // CR 118.3: Pre-check for non-self sacrifice costs — must detour to WaitingFor
    // before any cost payment, regardless of whether targets were auto-selected.
    if let Some(ref cost) = ability_def.cost {
        // CR 606.3: `can_activate_ability_now` gates legal-action generation,
        // but direct `GameAction::ActivateAbility` submissions must be rejected
        // here before the chosen-X detour can announce/pay a `[−X]` loyalty cost.
        if crate::types::ability::is_loyalty_ability_cost(cost)
            && !super::planeswalker::can_activate_loyalty_ability(
                state,
                source_id,
                player,
                ability_index,
            )
        {
            return Err(EngineError::ActionNotAllowed(
                "Cannot activate loyalty ability".to_string(),
            ));
        }

        if casting_costs::activation_cost_needs_x_choice(&resolved, cost) {
            // CR 602.2b + CR 601.2f: A non-mana activation cost that removes
            // X counters still needs the same X announcement step before any
            // mana or counter payment happens. Split fixed mana out so it
            // flows through ManaPayment, then pay the concretized residual cost.
            let (mana_cost, remaining) = split_alt_cost_components(cost);
            let mut pending_x = PendingCast::new(
                source_id,
                CardId(0),
                resolved,
                mana_cost.unwrap_or(ManaCost::NoCost),
            );
            pending_x.activation_cost = remaining;
            pending_x.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending_x));
            return casting_costs::enter_payment_step(state, player, None, events);
        }

        // CR 107.1b + CR 601.2f: When an activated ability's cost includes a mana
        // cost containing X — either directly (`Mana { cost }`) or as a sub-cost
        // of a Composite (e.g., `{X} + Discard a card`, `Tap + Pay {X}`) — divert
        // to ChooseXValue so X is chosen in step 601.2f BEFORE any cost is paid.
        // This MUST run before the non-self sacrifice/discard/exile detours below:
        // those return a `PayCost` `WaitingFor` and never resume into the X
        // announcement, so a `{X}`-plus-discard cost (Momir Basic emblem) would
        // otherwise pay the discard and treat X as 0. The remaining non-mana
        // sub-costs stay in `activation_cost` and are paid after ManaPayment via
        // the residual-cost handler (`finish_pending_cost_or_cast`), which already
        // surfaces a `PayCost::Discard` / `Sacrifice` / `Composite` for them.
        if let Some((mana_cost, remaining)) = casting_costs::extract_x_mana_cost(cost) {
            let mut pending_x = PendingCast::new(source_id, CardId(0), resolved, mana_cost);
            pending_x.activation_cost = remaining;
            pending_x.activation_ability_index = Some(ability_index);
            // CR 601.2f + CR 601.2h: POSITIVE signal — the residual non-mana tail
            // in `activation_cost` is still OUTSTANDING after mana payment, so
            // `push_activated_ability_to_stack` must re-surface a non-self discard
            // sub-cost. Only THIS path sets it; the discard-first detour below
            // already pays the discard and resumes with the flag unset.
            pending_x.activation_residual = ActivationResidual::XMana;
            state.pending_cast = Some(Box::new(pending_x));
            return casting_costs::enter_payment_step(state, player, None, events);
        }

        // CR 601.2g + CR 601.2h + CR 602.2b: A NON-X mana leg combined with a
        // non-self battlefield-removal cost (Sacrifice / battlefield Exile /
        // ReturnToHand) must pay the mana FIRST so the CR 601.2g mana-ability
        // window opens on the INTACT board — the removal (which can shrink
        // board-derived mana: Metalcraft/affinity/devotion) is paid LAST. Hoist
        // the mana leg through `enter_payment_step` and leave the removal tail as
        // the `ManaLeg` residual, which `push_activated_ability_to_stack`
        // re-surfaces after mana payment. This MUST run before the
        // Sacrifice/Exile/ReturnToHand pre-payment detours below, which pay the
        // removal FIRST (the pre-fix CR 601.2h ordering bug). The gate is
        // mana-leg-AND-removal: a bare `{N}` or `{N},{T}` has no removal leg
        // (`find_non_self_battlefield_removal_cost` → None) and a bare
        // `Sacrifice`/`Exile`/`Return` has no mana leg (`extract_mana_leg` → None);
        // both fall through to the unchanged paths. SelfRef removal is excluded by
        // the walkers. `{X}`-mana removals were already caught by the X detour
        // above, so any mana leg seen here is non-X (mutually exclusive residuals).
        if find_non_self_battlefield_removal_cost(cost).is_some() {
            if let Some((mana_cost, remaining)) = casting_costs::extract_mana_leg(cost) {
                let mut pending_leg = PendingCast::new(source_id, CardId(0), resolved, mana_cost);
                pending_leg.activation_cost = remaining;
                pending_leg.activation_ability_index = Some(ability_index);
                pending_leg.activation_residual = ActivationResidual::ManaLeg;
                state.pending_cast = Some(Box::new(pending_leg));
                return casting_costs::enter_payment_step(state, player, None, events);
            }
        }

        if let Some((count, sac_filter)) = find_non_self_sacrifice_cost(cost) {
            let eligible = find_eligible_sacrifice_targets(state, player, source_id, sac_filter);
            let (min_count, max_count) = sacrifice_cost_bounds(count, eligible.len());
            if eligible.len() < min_count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to sacrifice".into(),
                ));
            }
            let mut pending_sac =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_sac.activation_cost = Some(cost.clone());
            pending_sac.activation_ability_index = Some(ability_index);
            pending_sac.deferred_target_selection = true;
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Sacrifice,
                choices: eligible,
                count: max_count,
                min_count,
                resume: CostResume::Spell {
                    spell: Box::new(pending_sac),
                },
            });
        }

        if let Some((count, filter)) = find_non_self_discard(cost) {
            let count =
                super::quantity::resolve_quantity(state, count, player, source_id).max(0) as usize;
            let eligible = find_eligible_discard_targets(state, player, source_id, filter);
            if eligible.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard".into(),
                ));
            }
            let mut pending_discard =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_discard.activation_cost = Some(cost.clone());
            pending_discard.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Discard,
                choices: eligible,
                count,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending_discard),
                },
            });
        }

        // CR 117.1 + CR 601.2b + CR 602.2b: Pre-check for an `ExileWithAggregate`
        // cost (Baron Helmut Zemo's Boast — "Exile any number of black cards from
        // your graveyard with fifteen or more black mana symbols among their mana
        // costs"). The player chooses any subset of the eligible cards whose
        // aggregate satisfies the threshold; the handler validates the threshold
        // and (CR 608.2c) publishes the exiled cards as the tracked set the
        // `CastCopyOfCard` effect consumes. The effect target is `TrackedSet`
        // (resolution-time), not a declared target, so no target-selection
        // detour is needed.
        if let Some((filter, function, property, comparator, value, zone)) =
            find_exile_with_aggregate_cost(cost)
        {
            let eligible = super::cost_payability::eligible_exile_with_aggregate_objects(
                state, player, source_id, filter, zone,
            );
            // CR 118.3: payability was pre-checked above; re-derive the maximal
            // aggregate (exile-all) here so an unsatisfiable threshold fails fast.
            let total =
                super::quantity::aggregate_property_over(state, &eligible, function, property);
            if !comparator.evaluate(total, value) {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible cards to reach the exile threshold".into(),
                ));
            }
            let mut pending_agg =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_agg.activation_cost = Some(cost.clone());
            pending_agg.activation_ability_index = Some(ability_index);
            let max_count = eligible.len();
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileAggregate {
                    zone,
                    function,
                    property,
                    comparator,
                    value,
                    filter: filter.clone(),
                },
                choices: eligible,
                count: max_count,
                // CR 601.2b: "any number" reaching the threshold — the threshold
                // (not a fixed cardinality) is enforced by the handler. A nonzero
                // GE/Sum threshold can never be met by the empty set, so at least
                // one card is required; `min_count: 1` is the loose lower bound.
                min_count: 1,
                resume: CostResume::Spell {
                    spell: Box::new(pending_agg),
                },
            });
        }

        // CR 118.3 + CR 602.2b: Pre-check for non-self exile-from-hand/graveyard
        // costs. Untargeted abilities can detour to `WaitingFor::ExileForCost`
        // immediately; targeted abilities must choose their effect targets first
        // (CR 601.2c), then `casting_targets::pay_activation_costs_after_target_selection`
        // surfaces this same cost prompt before the ability reaches the stack.
        if let Some((count, zone, filter)) = find_non_self_exile(cost) {
            let has_effect_targets = {
                let slots = build_target_slots(state, &resolved)?;
                !slots.is_empty()
            };
            if !has_effect_targets {
                let narrow_zone = ExileCostSourceZone::try_from_zone(zone)
                    .expect("find_non_self_exile restricts zone to Hand or Graveyard");
                let eligible = find_eligible_exile_for_cost_targets(
                    state,
                    player,
                    source_id,
                    narrow_zone,
                    filter,
                );
                if eligible.len() < count as usize {
                    return Err(EngineError::ActionNotAllowed(
                        "Not enough eligible cards to exile".into(),
                    ));
                }
                let mut pending_exile =
                    PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
                pending_exile.activation_cost = Some(cost.clone());
                pending_exile.activation_ability_index = Some(ability_index);
                return Ok(WaitingFor::PayCost {
                    player,
                    kind: PayCostKind::ExileFromZone { zone: narrow_zone },
                    choices: eligible,
                    count: count as usize,
                    min_count: 0,
                    resume: CostResume::Spell {
                        spell: Box::new(pending_exile),
                    },
                });
            }
        }

        // CR 702.167a/b: Pre-check for a craft materials cost — detour to
        // `WaitingFor::PayCost { kind: ExileMaterials }` so the player selects
        // which permanents/graveyard cards to exile across the dual-zone union.
        // The full `Composite` cost (Mana + self-exile + materials) stays in
        // `activation_cost`; the mana and self-exile are paid by
        // `push_activated_ability_to_stack` after the selection completes
        // (CR 601.2h: remaining costs paid in any order). Mirrors the non-self
        // exile detour above.
        if let Some((count, materials)) = find_craft_materials_cost(cost) {
            let eligible = super::cost_payability::eligible_craft_materials(
                state, player, source_id, materials,
            );
            let min_count = count.min_count();
            let max_count = count.max_count(eligible.len());
            if eligible.len() < min_count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible materials to craft".into(),
                ));
            }
            let mut pending_craft =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_craft.activation_cost = Some(cost.clone());
            pending_craft.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileMaterials {
                    materials: materials.clone(),
                },
                choices: eligible,
                count: max_count,
                // CR 702.167a: "one or more" material costs set `min_count < count`;
                // exact material costs set both bounds to the same value.
                min_count,
                resume: CostResume::Spell {
                    spell: Box::new(pending_craft),
                },
            });
        }

        // CR 118.12a: Pre-check for OneOf costs — detour to WaitingFor before any cost payment.
        if let Some(costs) = find_one_of_cost(cost) {
            let mut pending_one_of =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_one_of.activation_cost = Some(cost.clone());
            pending_one_of.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::ActivationCostOneOfChoice {
                player,
                costs: costs.clone(),
                pending_cast: Box::new(pending_one_of),
            });
        }

        // CR 118.3: Pre-check for ReturnToHand costs — same WaitingFor detour pattern as
        // Sacrifice above. Ordering matters for Composite costs: Sacrifice wins if both are
        // present, but no real cards combine them.
        if let Some((count, filter)) = find_return_to_hand_cost(cost) {
            let eligible = find_eligible_return_to_hand_targets(state, player, source_id, filter);
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible permanents to return".into(),
                ));
            }
            let mut pending_return =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_return.activation_cost = Some(cost.clone());
            pending_return.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ReturnToHand,
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending_return),
                },
            });
        }

        // CR 118.3 + CR 122.1 + CR 602.2b: Pre-check targeted
        // remove-counter activation costs. The player chooses which matching
        // permanent supplies the counter before automatic cost components are
        // paid and the ability is put on the stack.
        if let Some((count, counter_type, target, selection)) =
            find_targeted_remove_counter_cost(cost)
        {
            let required_count = match selection {
                CounterCostSelection::SingleObject => count,
                CounterCostSelection::AmongObjects => 1,
            };
            let eligible = find_eligible_remove_counter_for_cost_targets(
                state,
                player,
                source_id,
                target,
                counter_type,
                required_count,
            );
            if eligible.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible permanents with counters".into(),
                ));
            }
            if selection == CounterCostSelection::AmongObjects {
                let removable_count = eligible
                    .iter()
                    .filter_map(|object_id| state.objects.get(object_id))
                    .map(|obj| {
                        removable_counter_count_for_cost_selection(obj, counter_type, selection)
                    })
                    .fold(0, u32::saturating_add);
                if removable_count < count {
                    return Err(EngineError::ActionNotAllowed(
                        "Not enough eligible counters to remove".into(),
                    ));
                }
            }
            let mut pending_counter =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_counter.activation_cost = Some(cost.clone());
            pending_counter.activation_ability_index = Some(ability_index);
            let max_count = match selection {
                CounterCostSelection::SingleObject => 1,
                CounterCostSelection::AmongObjects => eligible.len(),
            };
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::RemoveCounter {
                    counter_type: counter_type.clone(),
                    count,
                    selection,
                },
                choices: eligible,
                count: max_count,
                min_count: match selection {
                    CounterCostSelection::SingleObject => 0,
                    CounterCostSelection::AmongObjects => 1,
                },
                resume: CostResume::Spell {
                    spell: Box::new(pending_counter),
                },
            });
        }

        // CR 118.3: Pre-check for tap-creatures activation costs. Non-mana
        // activated abilities use the same WaitingFor flow as flashback tap
        // costs; completion resumes through `finish_pending_cost_or_cast`.
        if let Some((requirement, filter)) = find_tap_creatures_cost(cost) {
            // CR 602.1a: Activated-ability tap costs are fixed-count today
            // (Convoke-style). The aggregate "total power N" form is reserved for
            // Crew/Saddle/Teamwork, which are not dispatched through this path.
            let count = requirement.fixed_count().ok_or_else(|| {
                EngineError::ActionNotAllowed(
                    "Aggregate-power tap cost is not valid for this activation".into(),
                )
            })?;
            let eligible =
                find_eligible_tap_creatures_for_cost(state, player, source_id, cost, filter);
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible creatures to tap".into(),
                ));
            }
            let mut pending_tap =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_tap.activation_cost = Some(cost.clone());
            pending_tap.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::TapCreatures { aggregate: None },
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending_tap),
                },
            });
        }

        // Waterbend cost: detour to ManaPayment with Waterbend mode.
        if let Some(wb_cost) = find_waterbend_cost(cost) {
            let mut pending_wb = PendingCast::new(source_id, CardId(0), resolved, wb_cost.clone());
            pending_wb.activation_cost = Some(cost.clone());
            pending_wb.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending_wb));
            return casting_costs::enter_payment_step(
                state,
                player,
                Some(ConvokeMode::Waterbend),
                events,
            );
        }
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        let target_constraints = ability_def.target_constraints.clone();
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &target_constraints)?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;

            if let Some(ref cost) = ability_def.cost {
                if variable_speed_payment_range(cost, effective_speed(state, player)).is_some() {
                    return Ok(begin_variable_speed_payment(
                        state,
                        player,
                        source_id,
                        resolved,
                        cost.clone(),
                        ability_index,
                    ));
                }
                stamp_self_ref_discard_cost_paid_object(state, source_id, &mut resolved, cost);
                if let PaymentOutcome::Paused { remaining_cost } = pay_ability_cost_for_activation(
                    state,
                    player,
                    source_id,
                    cost,
                    activation_ability_tag(state, source_id, ability_index),
                    events,
                )? {
                    state.pending_cast = Some(Box::new(pending_activation_after_cost_pause(
                        source_id,
                        resolved.clone(),
                        ability_index,
                        remaining_cost,
                    )));
                    return Ok(state.waiting_for.clone());
                }
            }

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

            // CR 702.170b: plot's grant targets SelfRef (a context-ref), so
            // `build_target_slots` yields no slot and plot never takes this target
            // branch — it is intercepted in the no-target path below. Guard the
            // invariant: a future plot variant reaching here would silently revert
            // to the on-stack model, so relocate the intercept if this ever fires.
            debug_assert!(
                !is_plot_special_action(&ability_def),
                "plot special action reached the target branch; SelfRef should suppress its target slot"
            );

            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;

            stack::push_to_stack(
                state,
                StackEntry {
                    id: entry_id,
                    source_id,
                    controller: player,
                    kind: StackEntryKind::ActivatedAbility {
                        source_id,
                        ability: resolved,
                    },
                },
                events,
            );

            restrictions::record_ability_activation(state, source_id, ability_index);
            // CR 117.1b: Priority permits unbounded activation. `pending_activations`
            // is a per-priority-window AI-guard — see `GameState::pending_activations`.
            state.pending_activations.push((source_id, ability_index));
            events.push(GameEvent::AbilityActivated {
                player_id: player,
                source_id,
                // CR 606.2: Classify loyalty vs. normal from the source ability cost.
                kind: super::planeswalker::activated_ability_kind(state, source_id, ability_index),
            });
            // CR 702.142b: Emit additional event when a boast ability is activated.
            super::casting_targets::emit_keyword_ability_event_if_tagged(
                state,
                source_id,
                ability_index,
                player,
                events,
            );
            priority::clear_priority_passes(state);
            return Ok(WaitingFor::Priority { player });
        }

        let selection = begin_target_selection_for_ability(
            state,
            &resolved,
            &target_slots,
            &target_constraints,
        )?;
        let mut pending_target = PendingCast::new(
            source_id,
            CardId(0),
            resolved,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_target.activation_cost = ability_def.cost.clone();
        pending_target.activation_ability_index = Some(ability_index);
        pending_target.target_constraints = target_constraints;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_target),
            target_slots,
            mode_labels: Vec::new(),
            selection,
        });
    }

    if let Some(ref cost) = ability_def.cost {
        if variable_speed_payment_range(cost, effective_speed(state, player)).is_some() {
            return Ok(begin_variable_speed_payment(
                state,
                player,
                source_id,
                resolved,
                cost.clone(),
                ability_index,
            ));
        }
        stamp_self_ref_discard_cost_paid_object(state, source_id, &mut resolved, cost);
        if let PaymentOutcome::Paused { remaining_cost } = pay_ability_cost_for_activation(
            state,
            player,
            source_id,
            cost,
            activation_ability_tag(state, source_id, ability_index),
            events,
        )? {
            state.pending_cast = Some(Box::new(pending_activation_after_cost_pause(
                source_id,
                resolved.clone(),
                ability_index,
                remaining_cost,
            )));
            return Ok(state.waiting_for.clone());
        }
    }

    // CR 702.170b + CR 116.2k: Exiling a card using its plot ability is a
    // SPECIAL ACTION that doesn't use the stack. The self-exile cost paid above
    // already moved the card to exile (face up — CR 702.170 has no face-down
    // clause). Apply the `Plotted` grant IMMEDIATELY via the same single-
    // authority resolver the stack would otherwise have used on resolution, then
    // keep priority. No stack entry is created, and crucially no
    // `AbilityActivated` event is emitted: plot is a special action, not an
    // activated ability (CR 702.170b), so "whenever you activate an ability"
    // triggers must not fire and per-turn activation caps (`record_ability_
    // activation`) do not apply. `resolve` cannot fail for the SelfRef grant, but
    // the Result is mapped to `EngineError` rather than unwrapped.
    if is_plot_special_action(&ability_def) {
        super::effects::grant_permission::resolve(state, &resolved, events).map_err(|e| {
            EngineError::ActionNotAllowed(format!("plot special action failed: {e}"))
        })?;
        priority::clear_priority_passes(state);
        return Ok(WaitingFor::Priority { player });
    }

    // Push to stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id,
                ability: resolved,
            },
        },
        events,
    );

    restrictions::record_ability_activation(state, source_id, ability_index);
    // CR 117.1b: Priority permits unbounded activation. `pending_activations`
    // is a per-priority-window AI-guard — see `GameState::pending_activations`.
    state.pending_activations.push((source_id, ability_index));
    events.push(GameEvent::AbilityActivated {
        player_id: player,
        source_id,
        // CR 606.2: Classify loyalty vs. normal from the source ability cost.
        kind: super::planeswalker::activated_ability_kind(state, source_id, ability_index),
    });
    // CR 702.142b: Emit additional event when a boast ability is activated.
    super::casting_targets::emit_keyword_ability_event_if_tagged(
        state,
        source_id,
        ability_index,
        player,
        events,
    );

    priority::clear_priority_passes(state);

    Ok(WaitingFor::Priority { player })
}

/// CR 601.2i: If the player is unable or unwilling to complete a cast, the
/// process is reversed: the spell is removed from the stack and any costs
/// paid/choices made are rewound. The engine exposes this as
/// `GameAction::CancelCast` at each interactive WaitingFor step before mana is
/// actually debited.
///
/// For spell casts (distinguished by `activation_ability_index.is_none()`) the
/// StackEntry pushed at announcement (CR 601.2a) is removed here. The object's
/// `zone` field was left at the origin zone across the cast pipeline (see
/// `announce_spell_on_stack` / `finalize_cast` for the rationale), so no zone
/// reversion is needed — the object is already in its origin zone.
/// Activated-ability casts never placed an object on the stack during target
/// selection, so no stack rollback is needed for them.
pub fn handle_cancel_cast(
    state: &mut GameState,
    pending: &PendingCast,
    _events: &mut Vec<GameEvent>,
) {
    state.cancelled_casts.push(pending.object_id);

    let convoked_creatures = if pending.convoked_creatures.is_empty() {
        state
            .objects
            .get(&pending.object_id)
            .map(|obj| obj.convoked_creatures.clone())
            .unwrap_or_default()
    } else {
        pending.convoked_creatures.clone()
    };

    for object_id in &convoked_creatures {
        if let Some(obj) = state.objects.get_mut(object_id) {
            obj.tapped = false;
        }
    }
    let caster = pending.ability.controller;
    let delved_cards: Vec<ObjectId> = state
        .players
        .get(caster.0 as usize)
        .map(|player| {
            player
                .mana_pool
                .mana
                .iter()
                .filter(|unit| unit.is_convoke_payment())
                .map(|unit| unit.source_id)
                .filter(|&id| {
                    state
                        .objects
                        .get(&id)
                        .is_some_and(|obj| obj.zone == Zone::Exile)
                })
                .collect()
        })
        .unwrap_or_default();
    for object_id in &delved_cards {
        if state
            .objects
            .get(object_id)
            .is_some_and(|obj| obj.zone == Zone::Exile)
        {
            super::zones::move_to_zone(state, *object_id, Zone::Graveyard, _events);
        }
    }
    if !delved_cards.is_empty() {
        state.exile_links.retain(|link| {
            !(link.source_id == pending.object_id && delved_cards.contains(&link.exiled_id))
        });
        if let Some(exiled) = state
            .cards_exiled_with_source_this_turn
            .get_mut(&pending.object_id)
        {
            exiled.retain(|id| !delved_cards.contains(id));
            if exiled.is_empty() {
                state
                    .cards_exiled_with_source_this_turn
                    .remove(&pending.object_id);
            }
        }
    }
    for player in &mut state.players {
        player.mana_pool.mana.retain(|unit| {
            !(unit.is_convoke_payment() && convoked_creatures.contains(&unit.source_id))
                && !(unit.is_convoke_payment() && delved_cards.contains(&unit.source_id))
        });
    }
    if let Some(obj) = state.objects.get_mut(&pending.object_id) {
        obj.convoked_creatures.clear();
    }

    if pending.activation_ability_index.is_none() {
        // CR 601.2i: Remove the placeholder stack entry pushed at announcement.
        // No other player can interject between announce and cancel, so the
        // entry is still the topmost object for this cast.
        if let Some(pos) = state
            .stack
            .iter()
            .rposition(|entry| entry.id == pending.object_id)
        {
            state.stack.remove(pos);
            state.stack_paid_facts.remove(&pending.object_id);
        }
    }

    let restore_swapped_cast_face = pending
        .casting_variant
        .restores_front_face_after_stack_exit()
        || state
            .objects
            .get(&pending.object_id)
            .is_some_and(|obj| obj.modal_back_face);
    if restore_swapped_cast_face {
        // CR 601.2i + CR 712.11a / CR 709.3: backing out of a cast with an
        // alternative spell face before it completes restores the card's normal
        // front face in its origin zone.
        super::stack::restore_alternative_spell_normal_face(state, pending.object_id);
        if let Some(obj) = state.objects.get_mut(&pending.object_id) {
            obj.modal_back_face = false;
        }
    }

    if pending.casting_variant == CastingVariant::Prototype {
        // CR 601.2i + CR 702.160a: backing out of a prototyped cast before
        // costs are committed restores the printed characteristics in hand.
        if let Some(obj) = state.objects.get_mut(&pending.object_id) {
            clear_prototype_form(obj);
        }
    }

    if let Some(source_id) = pending.cancel_restore_prepared_source {
        // CR 601.2i + CR 722.3c: Prepare-copy cast cancellation must restore
        // the source's prepared marker and clear the synthetic copy object.
        if let Some(source) = state.objects.get_mut(&source_id) {
            if source.zone == Zone::Battlefield {
                source.prepared = Some(PreparedState);
            }
        }
        state.objects.remove(&pending.object_id);
    }
}

// Cost payment handlers are in casting_costs module.
pub(crate) use super::casting_costs::{
    handle_activation_cost_one_of_choice, handle_discard_for_cost, handle_return_to_hand_for_cost,
    handle_sacrifice_for_cost,
};

fn generic_mana_in_cost(cost: &AbilityCost) -> u32 {
    match cost {
        AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        } => *generic,
        AbilityCost::Composite { costs } => costs.iter().map(generic_mana_in_cost).sum(),
        _ => 0,
    }
}

fn total_mana_in_cost(cost: &AbilityCost) -> u32 {
    match cost {
        AbilityCost::Mana {
            cost: ManaCost::Cost { generic, shards },
        } => *generic + shards.len() as u32,
        AbilityCost::Composite { costs } => costs.iter().map(total_mana_in_cost).sum(),
        _ => 0,
    }
}

fn reduce_generic_in_cost_by(cost: &mut AbilityCost, remaining: &mut u32) {
    if *remaining == 0 {
        return;
    }

    match cost {
        AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        } => {
            let reduction = (*generic).min(*remaining);
            *generic -= reduction;
            *remaining -= reduction;
        }
        AbilityCost::Composite { costs } => {
            for sub in costs {
                reduce_generic_in_cost_by(sub, remaining);
            }
        }
        _ => {} // Non-mana costs unaffected
    }
}

/// CR 601.2f: Reduce generic mana in an ability cost without taking the total
/// mana in that cost below `minimum_mana`.
fn reduce_generic_in_cost_with_minimum_mana(
    cost: &mut AbilityCost,
    amount: u32,
    minimum_mana: u32,
) {
    let reducible = total_mana_in_cost(cost)
        .saturating_sub(minimum_mana)
        .min(generic_mana_in_cost(cost));
    let mut remaining = amount.min(reducible);
    reduce_generic_in_cost_by(cost, &mut remaining);
}

fn reduce_generic_in_cost(cost: &mut AbilityCost, amount: u32) {
    reduce_generic_in_cost_with_minimum_mana(cost, amount, 0);
}

/// CR 601.2f + CR 118.7: Increase the generic mana component of an ability cost
/// by `amount` (CR 601.2f: "plus all additional costs and cost increases").
/// The directional analogue of `reduce_generic_in_cost` — a cost increase only
/// ever grows the generic component (cost increases can't change colored
/// requirements). For a `Composite`, the increase is applied to the first mana
/// sub-cost so the net generic delta on the whole cost is exactly `amount`;
/// non-mana costs are unaffected. Skyseer's Chariot class (Raise direction).
fn increase_generic_in_cost(cost: &mut AbilityCost, amount: u32) {
    if amount == 0 {
        return;
    }
    match cost {
        AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        } => {
            *generic = generic.saturating_add(amount);
        }
        AbilityCost::Composite { costs } => {
            if let Some(sub) = costs
                .iter_mut()
                .find(|c| matches!(c, AbilityCost::Mana { .. }))
            {
                increase_generic_in_cost(sub, amount);
            }
        }
        _ => {} // Non-mana costs unaffected
    }
}

/// CR 601.2f: Apply self-referential cost reduction to an ability definition's cost.
/// Mutates `ability_def.cost` in place, reducing generic mana by `amount_per * count`.
fn apply_cost_reduction(
    state: &GameState,
    ability_def: &mut AbilityDefinition,
    player: PlayerId,
    source_id: ObjectId,
) {
    if let Some(ref reduction) = ability_def.cost_reduction {
        // CR 602.2b + CR 601.2f: A conditional flat reduction ("costs {N} less … if [cond]")
        // applies only when its gate holds at cost-determination time. `None` =
        // unconditional (the "for each" scaling form and all legacy reductions).
        let condition_met = reduction.condition.as_ref().is_none_or(|cond| {
            crate::game::restrictions::evaluate_condition(state, player, source_id, cond)
        });
        if condition_met {
            let count =
                super::quantity::resolve_quantity(state, &reduction.count, player, source_id);
            let reduce_by = (reduction.amount_per as i32 * count).max(0) as u32;
            if reduce_by > 0 {
                if let Some(ref mut cost) = ability_def.cost {
                    reduce_generic_in_cost(cost, reduce_by);
                }
            }
        }
    }

    // CR 702.170b + CR 116.2k: Plot is a SPECIAL ACTION, not the activation of an
    // ability. The activated-ability reducer's `keyword == "activated"` blanket arm
    // matches ANY ability regardless of tag and adjusts in BOTH directions, so it
    // would wrongly change a plot cost. Skip it for the synthesized plot shape; plot's
    // only cost adjustment is its dedicated special-action axis below
    // (ReduceActionCost { action: Plot }). A tag-keyed reducer can never match plot
    // anyway — the synthesized plot ability carries no `ability_tag`
    // (active_keyword == None) — so skipping the whole function is equivalent to
    // skipping just the "activated" arm, and clearer.
    if !is_plot_special_action(ability_def) {
        apply_static_activated_ability_cost_reduction(state, ability_def, source_id);
    }

    // CR 116.2k + CR 702.170: Plot is taken as a special action via a synthesized
    // hand activation whose effect grants the `Plotted` casting permission. Its mana
    // cost is adjusted ONLY by `ReduceActionCost { action: Plot }` statics (Doc
    // Aurlock) — the dedicated special-action axis. The generic activated-ability
    // reducer is skipped above for plot: its `keyword == "activated"` blanket arm
    // would otherwise match (and adjust) a plot cost even though plot is not the
    // activation of an ability (CR 702.170b).
    if is_plot_special_action(ability_def) {
        if let Some(cost) = ability_def.cost.as_mut() {
            reduce_special_action_in_ability_cost(state, player, SpecialAction::Plot, cost);
        }
    }
}

fn apply_static_activated_ability_cost_reduction(
    state: &GameState,
    ability_def: &mut AbilityDefinition,
    source_id: ObjectId,
) {
    // CR 601.2f: A `ReduceAbilityCost` static keyed on a keyword (e.g. "power-up")
    // also reduces a tagged activated ability whose tag matches that keyword
    // (Hulk reduces other creatures' power-up abilities). Read the activating
    // ability's tag keyword before the mutable borrow of its cost below.
    let active_keyword = ability_def
        .ability_tag
        .map(crate::types::ability::AbilityTag::keyword_str);

    let Some(cost) = ability_def.cost.as_mut() else {
        return;
    };

    for (static_source, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::ReduceAbilityCost {
            mode,
            keyword,
            amount,
            minimum_mana,
            dynamic_count,
        } = &def.mode
        else {
            continue;
        };
        if (keyword != "activated" && Some(keyword.as_str()) != active_keyword) || *amount == 0 {
            continue;
        }
        if def.affected.as_ref().is_some_and(|filter| {
            !super::filter::matches_target_filter(
                state,
                source_id,
                filter,
                &super::filter::FilterContext::from_source(state, static_source.id),
            )
        }) {
            continue;
        }
        // CR 601.2f + CR 208.1 + CR 113.7: When `dynamic_count` is present the
        // per-unit `amount` is multiplied by the resolved quantity (Agatha of
        // the Vile Cauldron: amount 1 × ~'s power). Resolve against the static's
        // own source so "~'s power" reads Agatha's post-layer power. Mirrors the
        // dynamic-count multiply in `keywords::apply_ability_cost_reduction`.
        let multiplier = dynamic_count.as_ref().map_or(1u32, |qty_ref| {
            let expr = crate::types::ability::QuantityExpr::Ref {
                qty: qty_ref.clone(),
            };
            super::quantity::resolve_quantity(
                state,
                &expr,
                static_source.controller,
                static_source.id,
            )
            .max(0) as u32
        });
        let effective = amount.saturating_mul(multiplier);
        // CR 118.7: Apply the adjustment in the static's direction. `Reduce`
        // subtracts generic mana (honoring the optional one-mana floor);
        // `Raise` adds generic mana (Skyseer's Chariot). `Minimum` is not
        // emitted for activated-ability statics and is treated as a no-op.
        match mode {
            CostModifyMode::Reduce => {
                reduce_generic_in_cost_with_minimum_mana(
                    cost,
                    effective,
                    minimum_mana.unwrap_or(0),
                );
            }
            CostModifyMode::Raise => increase_generic_in_cost(cost, effective),
            CostModifyMode::Minimum => {}
        }
    }
}

/// CR 116.2 + CR 118.7a: Reduce (or raise) the generic mana of a special
/// action's cost by the net adjustment of `player`'s active
/// `ReduceActionCost { action }` statics.
///
/// Single authority for plot (CR 116.2k / 702.170 — Doc Aurlock) and Room-door
/// unlock (CR 116.2m / 709.5e — Inquisitive Glimmer) special-action cost
/// reduction: both the plot activation path (`reduce_special_action_in_ability_cost`)
/// and `engine::handle_unlock_room_door` delegate here rather than inlining the
/// scan. CR 109.5: "your" plot / "you pay" unlock scopes to the static's
/// controller, so only statics controlled by `player` apply. CR 118.7a:
/// generic mana only — colored/colorless components are untouched.
pub(crate) fn apply_special_action_cost_reduction(
    state: &GameState,
    player: PlayerId,
    action: SpecialAction,
    mut cost: ManaCost,
) -> ManaCost {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if bf_obj.controller != player {
            continue;
        }
        let StaticMode::ReduceActionCost {
            action: static_action,
            mode,
            amount,
        } = &def.mode
        else {
            continue;
        };
        if *static_action != action || *amount == 0 {
            continue;
        }
        if let ManaCost::Cost {
            ref mut generic, ..
        } = cost
        {
            match mode {
                CostModifyMode::Reduce => *generic = generic.saturating_sub(*amount),
                CostModifyMode::Raise => *generic = generic.saturating_add(*amount),
                // CR 116.2: Minimum is not emitted for special-action costs.
                CostModifyMode::Minimum => {}
            }
        }
    }
    cost
}

/// CR 702.170a: True when `effect` is the synthesized plot special action's
/// effect — a `Plotted` casting-permission grant (see `synthesize_plot`). Shared
/// predicate so both the `AbilityDefinition` shape (`is_plot_special_action`) and
/// the `ResolvedAbility.effect` shape (the cost-pause resume-path invariant in
/// `casting_costs::push_activated_ability_to_stack`) classify plot identically.
pub(super) fn effect_is_plot_grant(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::GrantCastingPermission {
            permission: CastingPermission::Plotted { .. },
            ..
        }
    )
}

/// CR 702.170a: True when `ability_def` is the synthesized plot special action —
/// its effect grants the `Plotted` casting permission (see `synthesize_plot`).
/// Used to apply `ReduceActionCost { action: Plot }` reductions to the plot mana
/// cost without conflating plot with generic activated-ability reducers, and to
/// gate the CR 702.170b special-action intercept in `handle_activate_ability`.
fn is_plot_special_action(ability_def: &AbilityDefinition) -> bool {
    effect_is_plot_grant(&ability_def.effect)
}

/// CR 116.2 + CR 118.7a: Apply a special-action cost reduction to the mana
/// sub-cost(s) of an `AbilityCost`. The plot activation's cost is a `Composite`
/// wrapping the plot mana cost alongside the self-exile cost; this walks to the
/// `Mana` component and delegates the generic-mana adjustment to the
/// single-authority `apply_special_action_cost_reduction`.
fn reduce_special_action_in_ability_cost(
    state: &GameState,
    player: PlayerId,
    action: SpecialAction,
    cost: &mut AbilityCost,
) {
    match cost {
        AbilityCost::Mana { cost: mana } => {
            let reduced = apply_special_action_cost_reduction(
                state,
                player,
                action,
                std::mem::replace(mana, ManaCost::NoCost),
            );
            *mana = reduced;
        }
        AbilityCost::Composite { costs } => {
            for sub in costs.iter_mut() {
                if matches!(sub, AbilityCost::Mana { .. }) {
                    reduce_special_action_in_ability_cost(state, player, action, sub);
                }
            }
        }
        _ => {}
    }
}

/// CR 101.2: Check if a casting prohibition scope applies to the given caster.
/// Shared by CantBeCast, CantCastDuring, and PerTurnCastLimit.
fn casting_prohibition_scope_matches(
    who: &ProhibitionScope,
    caster: PlayerId,
    source_obj: &super::game_object::GameObject,
    state: &GameState,
) -> bool {
    let _ = source_obj;
    super::static_abilities::prohibition_scope_matches_player(who, caster, source_obj.id, state)
}

/// CR 601.3 + CR 101.2 + CR 109.5: Check if any active CantCastFrom static prevents
/// `caster` from casting the given object out of its current zone.
/// - Grafdigger's Cage ("Players can't cast spells from graveyards or libraries"):
///   `who = AllPlayers`, prohibited zones = {Graveyard, Library}.
/// - Drannith Magistrate ("Your opponents can't cast spells from anywhere other
///   than their hands"): `who = Opponents`, prohibited zones = every cast-capable
///   zone except the hand. The `who` axis means the static's own controller is
///   unaffected and only opponents are locked out of graveyard/exile/command casts.
fn is_blocked_from_casting_from_zone(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    caster: PlayerId,
) -> bool {
    // CR 601.2a: Casting from hand is never restricted by this class — the hand is
    // every printed allowed zone. Guard it before any filter evaluation.
    if obj.zone == Zone::Hand {
        return false;
    }

    let object_id = obj.id;
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantCastFrom { ref who } = def.mode else {
            continue;
        };
        // CR 109.5: The player axis — is the caster within the static's scope?
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }
        // CR 601.3: The affected filter encodes the prohibited zones via InAnyZone.
        if let Some(ref filter) = def.affected {
            if super::filter::matches_target_filter(
                state,
                object_id,
                filter,
                &super::filter::FilterContext::from_source(state, bf_obj.id),
            ) {
                return true;
            }
        }
    }
    false
}

/// CR 602.5 + CR 603.2a: Check if any active CantBeActivated static on the battlefield
/// prohibits the given player from activating the given permanent's activated abilities.
/// Each matching static contributes both an activator-axis check (`who` vs caster) AND
/// a permanent-axis check (`source_filter` vs the object whose ability is being activated).
///
/// Per CR 603.2a, this only affects ACTIVATED abilities; triggered abilities are suppressed
/// via the separate `SuppressTriggers` variant.
///
/// CR 605.1a: When the static carries `exemption: ManaAbilities` (Pithing Needle class),
/// abilities classified as mana abilities by the single authority
/// `mana_abilities::is_mana_ability` bypass the prohibition.
///
/// - Chalice of Life (`who=AllPlayers, source_filter=SelfRef`): prohibits Chalice's own
///   activations regardless of controller.
/// - Clarion Conqueror (`who=AllPlayers, source_filter=Artifact/Creature/Planeswalker`):
///   prohibits activation of any artifact/creature/planeswalker's activated abilities.
/// - Karn, the Great Creator (`who=AllPlayers, source_filter=Artifact with ControllerRef::Opponent`):
///   prohibits activation of opponent-controlled artifacts' activated abilities.
/// - Pithing Needle (`source_filter=HasChosenName, exemption=ManaAbilities`): prohibits
///   activation of named-card sources except their mana abilities.
pub(super) fn is_blocked_by_cant_be_activated(
    state: &GameState,
    caster: PlayerId,
    activating_source_id: ObjectId,
    activating_ability: &AbilityDefinition,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let bf_id = bf_obj.id;
        let StaticMode::CantBeActivated {
            ref who,
            ref source_filter,
            ref exemption,
        } = def.mode
        else {
            continue;
        };
        // CR 109.5: The "who" axis — is the caster within the scope?
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }
        // CR 602.5: The permanent-axis — does the object whose ability is being
        // activated match the static's filter? `ControllerRef` is resolved against
        // the static's source controller (`bf_id`), not the caster.
        let filter_ctx = super::filter::FilterContext::from_source(state, bf_id);
        if !super::filter::matches_target_filter(
            state,
            activating_source_id,
            source_filter,
            &filter_ctx,
        ) {
            continue;
        }
        // CR 605.1a: Apply the exemption gate. Routes through the single
        // `mana_abilities::is_mana_ability` classifier — no duplicated logic.
        match exemption {
            ActivationExemption::None => return true,
            ActivationExemption::ManaAbilities => {
                if !super::mana_abilities::is_mana_ability(activating_ability) {
                    return true;
                }
            }
        }
    }
    false
}

/// CR 117.1 + CR 604.1: Evaluate a `CastingProhibitionCondition` against the
/// current game state from the perspective of the static's source permanent
/// and the prospective caster/activator.
///
/// Single-authority condition evaluator shared by `is_blocked_by_cant_cast_during`
/// (CR 601.2) and `is_blocked_by_cant_activate_during` (CR 602.5). Inline
/// matching at the two call sites is forbidden — every new
/// `CastingProhibitionCondition` variant lands here exactly once.
///
/// `source_controller` is the controller of the static's source permanent (used
/// to bind possessive timing references such as "during your turn" — CR 109.5).
/// `caster` is the player whose action is being legality-checked (used for
/// timing predicates that scope to the active actor such as `NotSorcerySpeed`
/// or the distributive `NotDuringAffectedPlayersTurn`).
fn evaluate_casting_prohibition_condition(
    state: &GameState,
    when: &CastingProhibitionCondition,
    source_controller: PlayerId,
    caster: PlayerId,
) -> bool {
    match when {
        // CR 109.5: "during your turn" — bound to the static's source controller.
        CastingProhibitionCondition::DuringYourTurn => state.active_player == source_controller,
        // CR 506.1: "during combat" — any combat phase, game-wide.
        CastingProhibitionCondition::DuringCombat => state.phase.is_combat(),
        // CR 109.5 + CR 117.1a + CR 604.1: "only during your turn" — blocked
        // when it is NOT the source-controller's turn (Fires of Invention's
        // "your turn"). Differs from `NotDuringAffectedPlayersTurn`: this
        // binds to the static source's controller per CR 109.5.
        CastingProhibitionCondition::NotDuringYourTurn => state.active_player != source_controller,
        // CR 102.1 + CR 117.1a + CR 604.1: "only during their own turn" —
        // distributive per-affected-player binding (Dosan / City of Solitude).
        // Blocked when it is NOT the *caster's* turn. The pronoun "their own"
        // is not governed by CR 109.5 (which binds "you/your"); the
        // distributive reading follows from CR 102.1 + the template structure
        // of "[every player] can [action] only during their own [time]".
        CastingProhibitionCondition::NotDuringAffectedPlayersTurn => state.active_player != caster,
        // CR 117.1a + CR 117.1b: "only any time they could cast a sorcery"
        // — blocked when not at sorcery speed. `restrictions` owns the
        // sorcery-speed timing predicate (CR 307.1); never re-derive it.
        CastingProhibitionCondition::NotSorcerySpeed => {
            !super::restrictions::is_sorcery_speed_window(state, caster)
        }
    }
}

/// CR 101.2: Check if any CantCastDuring static on the battlefield prevents the
/// given player from casting spells during the current turn/phase.
/// E.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
/// E.g., Basandra, Battle Seraph: "Players can't cast spells during combat."
/// E.g., Dosan, the Falling Leaf (`who=AllPlayers, when=NotDuringAffectedPlayersTurn`):
///   each player can only cast on their own turn.
fn is_blocked_by_cant_cast_during(state: &GameState, caster: PlayerId) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantCastDuring { ref who, ref when } = def.mode else {
            continue;
        };
        // CR 101.2: Check if the caster is in the affected scope.
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }
        // CR 109.5 / CR 102.1: Bind the timing predicate via the single-authority
        // evaluator. The (source_controller, caster) pair is passed verbatim;
        // each `CastingProhibitionCondition` arm picks the binding it needs.
        if evaluate_casting_prohibition_condition(state, when, bf_obj.controller, caster) {
            return true;
        }
    }
    false
}

/// CR 602.5 + CR 117.1b: Check if any active `CantActivateDuring` static on
/// the battlefield prevents the given player from activating the given
/// activated ability during the current turn condition.
///
/// E.g., City of Solitude — both casting and activating are prohibited unless
/// it's the affected player's own turn.
///
/// CR 605.1a: When the static carries `exemption: ManaAbilities`, abilities
/// classified as mana abilities (CR 605.1a) by `mana_abilities::is_mana_ability`
/// bypass the prohibition. City of Solitude emits `ActivationExemption::None`
/// per its 2009-10-01 ruling ("This stops players from activating mana
/// abilities") — mana abilities are NOT exempt for that card.
pub(super) fn is_blocked_by_cant_activate_during(
    state: &GameState,
    activator: PlayerId,
    activating_ability: &AbilityDefinition,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantActivateDuring {
            ref who,
            ref when,
            ref exemption,
        } = def.mode
        else {
            continue;
        };
        if !casting_prohibition_scope_matches(who, activator, bf_obj, state) {
            continue;
        }
        if !evaluate_casting_prohibition_condition(state, when, bf_obj.controller, activator) {
            continue;
        }
        // CR 605.1a: Apply the exemption gate via the single classifier authority.
        match exemption {
            ActivationExemption::None => return true,
            ActivationExemption::ManaAbilities => {
                if !super::mana_abilities::is_mana_ability(activating_ability) {
                    return true;
                }
            }
        }
    }
    false
}

/// CR 101.2: Check if any CantBeCast static on the battlefield prevents
/// the given player from casting the given spell.
/// Handles scope-based checks (opponents, all players, controller, enchanted creature's
/// controller) and filter-based checks (type, mana value, chosen name, chosen card type).
fn is_blocked_by_cant_be_cast(
    state: &GameState,
    caster: PlayerId,
    spell_obj: &super::game_object::GameObject,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`
    // — including the per-static `condition` check; no inline duplication needed.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantBeCast { ref who } = def.mode else {
            continue;
        };

        // CR 101.2: Check if the caster is in the affected scope.
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }

        // CR 604.1: Check spell filter if present.
        if let Some(ref affected) = def.affected {
            if !cant_cast_filter_matches(state, spell_obj, affected, bf_obj) {
                continue;
            }
        }

        // CR 101.2 + CR 109.5 + CR 601.3a: per-affected-player applicability gate.
        // Angelic Arbiter restricts only opponents who attacked with a creature
        // this turn. Evaluated against the CASTER (CR 109.5), not the source's
        // controller. The source-relative `def.condition` functioning gate is
        // already applied upstream by `battlefield_active_statics`, so this is the
        // only additional gate needed — do NOT re-evaluate `def.condition` here.
        if let Some(ref cond) = def.per_player_condition {
            if !restrictions::evaluate_condition(state, caster, bf_obj.id, cond) {
                continue;
            }
        }

        return true;
    }
    false
}

/// CR 101.2: Check if a spell matches a CantBeCast affected filter.
/// Handles type filters, mana value comparisons, chosen name, and chosen card type.
/// Source-dependent filters (HasChosenName, IsChosenCardType) are resolved here
/// because they need the source permanent's chosen attributes.
fn cant_cast_filter_matches(
    state: &GameState,
    spell_obj: &super::game_object::GameObject,
    filter: &TargetFilter,
    source_obj: &super::game_object::GameObject,
) -> bool {
    use crate::types::ability::{ChosenAttribute, FilterProp};

    match filter {
        // CR 201.2: "spells with the chosen name" — match spell name against source's chosen name.
        TargetFilter::HasChosenName => {
            let chosen_name = source_obj.chosen_attributes.iter().find_map(|a| match a {
                ChosenAttribute::CardName(n) => Some(n.as_str()),
                _ => None,
            });
            chosen_name.is_some_and(|name| name.eq_ignore_ascii_case(&spell_obj.name))
        }
        // CR 205: Typed filter with IsChosenCardType requires source's chosen card type.
        TargetFilter::Typed(tf)
            if tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenCardType)) =>
        {
            let chosen_type = source_obj.chosen_attributes.iter().find_map(|a| match a {
                ChosenAttribute::CardType(ct) => Some(ct),
                _ => None,
            });
            let Some(chosen_type) = chosen_type else {
                return false;
            };
            spell_obj
                .card_types
                .core_types
                .iter()
                .any(|ct| ct == chosen_type)
        }
        // All other filters delegate to the spell record matcher.
        _ => {
            let record = SpellCastRecord {
                name: spell_obj.name.clone(),
                core_types: spell_obj.card_types.core_types.clone(),
                supertypes: spell_obj.card_types.supertypes.clone(),
                subtypes: spell_obj.card_types.subtypes.clone(),
                keywords: spell_obj.keywords.clone(),
                colors: spell_obj.color.clone(),
                mana_value: spell_obj.mana_cost.mana_value(),
                has_x_in_cost: super::casting_costs::cost_has_x(&spell_obj.mana_cost),
                from_zone: spell_obj.zone,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
                was_kicked: !spell_obj.kickers_paid.is_empty(),
            };
            super::filter::spell_record_matches_filter(
                &record,
                filter,
                source_obj.controller,
                &state.all_creature_types,
            )
        }
    }
}

/// CR 101.2 + CR 604.1: Check if any PerTurnCastLimit static on the battlefield prevents
/// the given player from casting the given spell this turn.
/// E.g., Rule of Law: "Each player can't cast more than one spell each turn."
/// E.g., Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
fn is_blocked_by_per_turn_cast_limit(
    state: &GameState,
    caster: PlayerId,
    spell_obj: &super::game_object::GameObject,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        {
            let StaticMode::PerTurnCastLimit {
                ref who,
                max,
                ref spell_filter,
            } = def.mode
            else {
                continue;
            };

            // CR 101.2: Check if the caster is in the affected scope.
            if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
                continue;
            }

            // If a spell filter is set, first check if the spell being cast matches.
            // E.g., Deafening Silence only limits noncreature spells — creature spells
            // are unaffected regardless of how many noncreature spells were cast.
            if let Some(filter) = spell_filter {
                let current_record = SpellCastRecord {
                    name: spell_obj.name.clone(),
                    core_types: spell_obj.card_types.core_types.clone(),
                    supertypes: spell_obj.card_types.supertypes.clone(),
                    subtypes: spell_obj.card_types.subtypes.clone(),
                    keywords: spell_obj.keywords.clone(),
                    colors: spell_obj.color.clone(),
                    mana_value: spell_obj.mana_cost.mana_value(),
                    has_x_in_cost: super::casting_costs::cost_has_x(&spell_obj.mana_cost),
                    from_zone: spell_obj.zone,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                    was_kicked: !spell_obj.kickers_paid.is_empty(),
                };
                if !super::filter::spell_record_matches_filter(
                    &current_record,
                    filter,
                    bf_obj.controller,
                    &state.all_creature_types,
                ) {
                    continue;
                }
            }

            // Count matching spells already cast this turn by this player.
            // The current spell has not yet been recorded (recording happens in
            // finalize_cast), so this correctly counts only prior spells.
            let cast_count = state
                .spells_cast_this_turn_by_player
                .get(&caster)
                .map(|records| match spell_filter {
                    None => records.len(),
                    Some(filter) => records
                        .iter()
                        .filter(|r| {
                            super::filter::spell_record_matches_filter(
                                r,
                                filter,
                                bf_obj.controller,
                                &state.all_creature_types,
                            )
                        })
                        .count(),
                })
                .unwrap_or(0);

            if cast_count >= max as usize {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
#[path = "casting_tests.rs"]
mod tests;
