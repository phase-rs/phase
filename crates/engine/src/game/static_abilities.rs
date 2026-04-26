use std::collections::HashMap;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::functioning_abilities::{active_static_definitions, battlefield_active_statics};
use crate::game::layers::evaluate_condition;
use crate::types::ability::{TargetFilter, TypedFilter};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::{ProhibitionScope, StaticMode};

/// Handler function type for static ability modes.
/// Receives the `StaticMode` variant the handler was registered under.
pub type StaticAbilityHandler =
    fn(state: &GameState, mode: &StaticMode, source_id: ObjectId) -> Vec<StaticEffect>;

/// Describes what a static ability does (returned by handlers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticEffect {
    /// Continuous effect -- evaluated through layers.rs, details in typed modifications.
    Continuous,
    /// Rule modification -- checked at specific game points.
    RuleModification { mode: String },
}

/// Context for checking if a static ability applies to a given scenario.
#[derive(Debug, Clone, Default)]
pub struct StaticCheckContext {
    pub source_id: Option<ObjectId>,
    pub target_id: Option<ObjectId>,
    pub player_id: Option<PlayerId>,
    pub card_name: Option<String>,
}

/// CR 604.1: Static ability registry — maps StaticMode keys to handlers.
pub fn build_static_registry() -> HashMap<StaticMode, StaticAbilityHandler> {
    let mut registry: HashMap<StaticMode, StaticAbilityHandler> = HashMap::new();

    // Core continuous mode (evaluated through layers)
    registry.insert(StaticMode::Continuous, handle_continuous);

    // Core rule-modification handlers with real logic
    registry.insert(StaticMode::CantAttack, handle_rule_mod);
    registry.insert(StaticMode::CantBlock, handle_rule_mod);
    registry.insert(StaticMode::CantAttackOrBlock, handle_rule_mod);
    registry.insert(StaticMode::CantBeTargeted, handle_rule_mod);
    // Note: CantBeCast is a data-carrying variant — runtime enforcement is in
    // casting.rs::is_blocked_by_cant_be_cast(). Coverage support is via is_data_carrying_static().
    //
    // CR 602.5 + CR 603.2a: CantBeActivated is a data-carrying variant (`who` + `source_filter`)
    // — runtime enforcement is in casting.rs::is_blocked_by_cant_be_activated() via
    // can_activate_ability_now(). Coverage support is via is_data_carrying_static().
    // Per CR 603.2a, activation-prohibition effects do NOT affect triggered abilities —
    // see SuppressTriggers for the triggered-ability side of the prohibition family.
    //
    // CR 701.23 + CR 609.3: CantSearchLibrary is a data-carrying variant — runtime
    // enforcement is in effects/search_library.rs::resolve(). Coverage support is via
    // is_data_carrying_static().
    //
    // CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers is a data-carrying variant —
    // runtime enforcement is in triggers.rs via event_is_suppressed_by_static_triggers().
    // Coverage support is via is_data_carrying_static(). Per CR 603.6d, static
    // "enters tapped" / "enters with counters" / "as X enters" effects are NOT
    // triggered and are unaffected by this variant.
    // CR 702.8a: CastWithFlash — card may be cast at instant speed.
    registry.insert(StaticMode::CastWithFlash, handle_rule_mod);
    // CR 601.2f: ReduceCost/RaiseCost are data-carrying variants — runtime checks are
    // in game/casting.rs::apply_battlefield_cost_modifiers(). Coverage support is via
    // is_data_carrying_static() in game/coverage.rs.
    // Note: ReduceAbilityCost runtime checks are in game/keywords.rs::apply_ability_cost_reduction().
    registry.insert(StaticMode::CantGainLife, handle_rule_mod);
    registry.insert(StaticMode::CantLoseLife, handle_rule_mod);
    registry.insert(StaticMode::MustAttack, handle_rule_mod);
    registry.insert(StaticMode::MustBlock, handle_rule_mod);
    // Note: CantDraw is a data-carrying variant — runtime enforcement is in
    // game/effects/draw.rs. Coverage support is via is_data_carrying_static().
    // Note: DoubleTriggers (CR 603.2d) is a data-carrying variant — runtime
    // enforcement is in triggers.rs::apply_trigger_doubling. Coverage support
    // is via is_data_carrying_static().
    registry.insert(StaticMode::IgnoreHexproof, handle_rule_mod);
    registry.insert(
        StaticMode::ExtraBlockers { count: Some(1) },
        handle_rule_mod,
    );
    registry.insert(StaticMode::ExtraBlockers { count: None }, handle_rule_mod);

    // Note: GraveyardCastPermission and CastFromHandFree are data-carrying variants —
    // runtime enforcement is in casting.rs. Coverage support is via is_data_carrying_static().

    // CR 509.1b: CantBeBlocked — creature cannot be blocked.
    registry.insert(StaticMode::CantBeBlocked, handle_cant_be_blocked);
    // CR 702.16: Protection prevents targeting, blocking, damage, and attachment.
    registry.insert(StaticMode::Protection, handle_protection);

    // Promoted static ability handlers -- Standard-relevant mechanics
    // CR 702.12: Indestructible — prevents destruction by lethal damage and destroy effects.
    registry.insert(StaticMode::Indestructible, handle_indestructible);
    // CR 113.6g: CantBeCountered — spell can't be countered by spells or abilities.
    registry.insert(StaticMode::CantBeCountered, handle_cant_be_countered);
    // CR 707.10: CantBeCopied — spell can't be copied by spells or abilities.
    // Runtime enforcement is in effects/copy_spell.rs via active_static_definitions.
    registry.insert(StaticMode::CantBeCopied, handle_cant_be_copied);
    registry.insert(StaticMode::CantBeDestroyed, handle_cant_be_destroyed);
    // CR 702.34: FlashBack — allows casting from graveyard, exiled after resolution.
    registry.insert(StaticMode::FlashBack, handle_flashback);
    // CR 702.18: Shroud — permanent cannot be the target of spells or abilities.
    registry.insert(StaticMode::Shroud, handle_shroud);
    // CR 702.11: Hexproof — affected player/permanent cannot be the target of
    // spells or abilities an opponent controls. Player-scope grant (e.g.,
    // Crystal Barricade's "You have hexproof.") surfaces as a `RuleModification`
    // marker analogous to Shroud.
    registry.insert(StaticMode::Hexproof, handle_hexproof);
    // CR 702.20: Vigilance — attacking doesn't cause this creature to tap.
    registry.insert(StaticMode::Vigilance, handle_static_vigilance);
    // CR 702.111: Menace — can't be blocked except by two or more creatures.
    registry.insert(StaticMode::Menace, handle_static_menace);
    // CR 702.17: Reach — can block creatures with flying.
    registry.insert(StaticMode::Reach, handle_static_reach);
    // CR 702.9: Flying — can't be blocked except by creatures with flying or reach.
    registry.insert(StaticMode::Flying, handle_static_flying);
    // CR 702.19: Trample — excess combat damage is assigned to the defending player.
    registry.insert(StaticMode::Trample, handle_static_trample);
    // CR 702.2: Deathtouch — any amount of damage dealt is lethal.
    registry.insert(StaticMode::Deathtouch, handle_static_deathtouch);
    // CR 702.15: Lifelink — damage dealt also causes controller to gain that much life.
    registry.insert(StaticMode::Lifelink, handle_static_lifelink);
    registry.insert(StaticMode::CantTap, handle_rule_mod);
    registry.insert(StaticMode::CantUntap, handle_rule_mod);
    // CR 509.1c: MustBeBlocked — this creature must be blocked if able.
    registry.insert(StaticMode::MustBeBlocked, handle_rule_mod);
    registry.insert(StaticMode::CantAttackAlone, handle_rule_mod);
    registry.insert(StaticMode::CantBlockAlone, handle_rule_mod);
    registry.insert(StaticMode::MayLookAtTopOfLibrary, handle_rule_mod);
    // CR 104.3b: CantLoseTheGame — player can't lose the game (Platinum Angel).
    // Runtime enforcement is in sba.rs::player_has_cant_lose().
    registry.insert(StaticMode::CantLoseTheGame, handle_rule_mod);
    // CR 104.2b: CantWinTheGame — a player can't win the game from effects
    // (Platinum Angel). Runtime enforcement is in effects/win_lose.rs::resolve_win
    // via player_has_cant_win(). Per CR 104.2a, the last-player-standing case
    // is not blocked by this static and is enforced by elimination::check_game_over.
    registry.insert(StaticMode::CantWinTheGame, handle_rule_mod);
    // CR 702.179e: Card-specific rule modification allowing speed to exceed 4.
    registry.insert(StaticMode::SpeedCanIncreaseBeyondFour, handle_rule_mod);
    // CR 609.4b: "You may spend mana as though it were mana of any color."
    // Runtime enforcement is in mana_payment.rs via player_can_spend_as_any_color().
    registry.insert(StaticMode::SpendManaAsAnyColor, handle_rule_mod);
    // CR 702.3b: CanAttackWithDefender — allows creatures with defender to attack.
    // Runtime enforcement is in combat.rs::validate_attack().
    registry.insert(StaticMode::CanAttackWithDefender, handle_rule_mod);
    // CR 510.1a: AssignNoCombatDamage — creature assigns no combat damage.
    // Runtime enforcement is in combat_damage.rs::combat_damage_amount().
    registry.insert(StaticMode::AssignNoCombatDamage, handle_rule_mod);
    // CR 502.3 + CR 113.6: UntapsDuringEachOtherPlayersUntapStep — second untap
    // pass during each other player's untap step (Seedborn Muse). Runtime
    // enforcement is in turns.rs::execute_untap, which scans for this variant
    // after the active player's normal untap pass.
    registry.insert(
        StaticMode::UntapsDuringEachOtherPlayersUntapStep,
        handle_rule_mod,
    );

    // CR 614.1d: Zone-based restriction handlers.
    // Enforcement happens in zones.rs (CantEnterBattlefieldFrom) and casting.rs (CantCastFrom),
    // not through the standard handler flow, but we register them as rule_mod so that
    // `check_static_ability` queries work.
    registry.insert(StaticMode::CantEnterBattlefieldFrom, handle_rule_mod);
    registry.insert(StaticMode::CantCastFrom, handle_rule_mod);
    // Note: CantCastDuring is a data-carrying variant — runtime enforcement will be in
    // casting.rs. Coverage support is via is_data_carrying_static().
    // Note: PerTurnCastLimit is a data-carrying variant — runtime enforcement is in
    // casting.rs::is_blocked_by_per_turn_cast_limit(). Coverage support is via is_data_carrying_static().

    // Promoted Tier 3 statics -- parser-produced, rule-modification handlers
    // CR 509.1b: BlockRestriction — restricts what a creature can block.
    registry.insert(StaticMode::BlockRestriction, handle_rule_mod);
    // CR 402.2: NoMaximumHandSize — player has no maximum hand size.
    registry.insert(StaticMode::NoMaximumHandSize, handle_rule_mod);
    // CR 305.2: MayPlayAdditionalLand — player may play additional lands.
    registry.insert(StaticMode::MayPlayAdditionalLand, handle_rule_mod);
    // CR 502.3: MayChooseNotToUntap — player may choose not to untap a permanent.
    registry.insert(StaticMode::MayChooseNotToUntap, handle_rule_mod);
    // Note: AdditionalLandDrop is a data-carrying variant — runtime checks are in
    // additional_land_drops(). Coverage support is via is_data_carrying_static().
    // CR 114.3: EmblemStatic — fallback for unparseable emblem static text.
    registry.insert(StaticMode::EmblemStatic, handle_rule_mod);
    // CR 701.38d: GrantsExtraVote — "While voting, you may vote an additional time."
    // Runtime enforcement is in game/effects/vote.rs::votes_per_session_for(), which
    // scans active_static_definitions at vote-session start. No continuous-effect
    // plumbing needed; registered here so coverage marks the card as supported.
    registry.insert(StaticMode::GrantsExtraVote, handle_rule_mod);

    // No generic `StaticMode::Other(...)` stubs are currently needed.
    //
    // Historical placeholder names (Devoid, Forecast, ETBReplacement,
    // DamageReduction, PreventDamage, DealtDamageInsteadExile,
    // AttackRestriction, MinBlockers, MaxBlockers, CantExistWithout,
    // LeavesPlay, ChangesZoneAll, ReduceCostEach, SetCost, AlternateCost)
    // were removed after audit confirmed zero parser emission and zero
    // runtime consumers. The real engine-level mechanics live in typed
    // variants or other subsystems:
    //   - Devoid / Forecast         → `Keyword` enum (CR 702.114 / 702.56)
    //   - ChangesZoneAll            → `TriggerMode::ChangesZoneAll`
    //   - PreventDamage             → `Effect::PreventDamage`
    //   - DamageReduction / cost-mod variants → typed `StaticMode` variants
    //     (`ReduceCost`, `RaiseCost`, `DefilerCostReduction`, etc.)
    //   - ETBReplacement / LeavesPlay → `ReplacementDefinition`
    //     (ChangeZone / Moved events)
    //
    // If a new card introduces a static pattern that genuinely needs a
    // runtime-recognized-but-no-op placeholder, add it here and document
    // the reason.

    // CR 305.2, CR 306.7, CR 701.3, CR 701.19, CR 701.21, CR 701.24, CR 701.27,
    // CR 702.5, CR 702.6, CR 120.1, CR 120.2: Prohibition-family statics are
    // registered as rule-modifications; runtime enforcement lives in the relevant
    // game modules (sacrifice, attach, transform, regenerate, casting, shuffle,
    // deal_damage) via `object_has_static_other` / `player_has_static_other`.
    let prohibitions = [
        "CantBeSacrificed",
        "CantBeEnchanted",
        "CantBeEquipped",
        "CantBeAttached",
        "CantTransform",
        "CantRegenerate",
        "CantPlayLand",
        "CantShuffle",
        "CantDealDamage",
        "CantBeDealtDamage",
        // CR 306.7: Planeswalker redirection was removed from the rules.
        // The static is still registered for coverage so cards with legacy
        // "can't be redirected" Oracle text (if any survive) don't explode,
        // but no runtime enforcement is wired because there's nothing to block.
        "CantPlaneswalkerRedirect",
    ];
    for mode in &prohibitions {
        registry.insert(StaticMode::Other((*mode).into()), handle_rule_mod);
    }

    registry
}

pub(crate) fn prohibition_scope_matches_player(
    scope: &ProhibitionScope,
    player: PlayerId,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let Some(source_obj) = state.objects.get(&source_id) else {
        return false;
    };
    match scope {
        ProhibitionScope::Opponents => player != source_obj.controller,
        ProhibitionScope::AllPlayers => true,
        ProhibitionScope::Controller => player == source_obj.controller,
        // CR 303.4e: For an Aura attached to an object ("enchanted creature's
        // controller"), the prohibition scopes to that object's current
        // controller. For an Aura attached directly to a player (CR 303.4 +
        // CR 702.5d, Curse cycle), the "enchanted player" IS the player and we
        // compare directly. Recall CR 303.4e: an Aura's controller is separate
        // from the enchanted player's controller — `source_obj.controller` would
        // give the wrong answer for the Curse case.
        ProhibitionScope::EnchantedCreatureController => match source_obj.attached_to {
            Some(crate::game::game_object::AttachTarget::Object(target_id)) => state
                .objects
                .get(&target_id)
                .is_some_and(|enchanted| enchanted.controller == player),
            Some(crate::game::game_object::AttachTarget::Player(pid)) => pid == player,
            None => false,
        },
    }
}

/// Handler for the Continuous mode -- layers.rs handles the actual evaluation.
/// CR 604.2: Continuous effects from static abilities apply via the layer system.
fn handle_continuous(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::Continuous]
}

/// Handler for rule-modification modes -- returns the mode as a RuleModification effect.
fn handle_rule_mod(
    _state: &GameState,
    mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: mode.to_string(),
    }]
}

/// Handler for CantBeBlocked -- creature cannot be blocked.
pub fn handle_cant_be_blocked(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeBlocked".to_string(),
    }]
}

/// Handler for Protection -- prevents damage, blocking, targeting, and enchanting
/// by sources with the specified quality.
/// CR 702.16: Protection is evaluated via keywords at runtime; the handler returns
/// a RuleModification marker for the registry/coverage system.
pub fn handle_protection(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Protection".to_string(),
    }]
}

/// Handler for Indestructible -- prevents destruction by lethal damage and destroy effects.
fn handle_indestructible(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Indestructible".to_string(),
    }]
}

/// Handler for CantBeCountered -- spell cannot be countered.
fn handle_cant_be_countered(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeCountered".to_string(),
    }]
}

/// Handler for CantBeCopied -- spell cannot be copied.
/// CR 707.10: Runtime enforcement in effects/copy_spell.rs via
/// active_static_definitions on the targeted spell's GameObject.
fn handle_cant_be_copied(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeCopied".to_string(),
    }]
}

/// Handler for CantBeDestroyed -- permanent cannot be destroyed.
fn handle_cant_be_destroyed(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeDestroyed".to_string(),
    }]
}

/// Handler for FlashBack -- allows casting from graveyard, exiled after resolution.
fn handle_flashback(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "FlashBack".to_string(),
    }]
}

/// Handler for Shroud -- permanent cannot be the target of spells or abilities.
fn handle_shroud(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Shroud".to_string(),
    }]
}

/// CR 702.11: Hexproof — surfaces a RuleModification marker so downstream
/// coverage/registry consumers see the grant. Runtime targeting for
/// permanent-scope hexproof flows through `Keyword::Hexproof` on the object
/// (granted via `ContinuousModification::AddKeyword` paths); the player-scope
/// marker mirrors `handle_shroud`.
fn handle_hexproof(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Hexproof".to_string(),
    }]
}

/// Handler for static-granted Vigilance (e.g., "All creatures you control have vigilance").
fn handle_static_vigilance(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Vigilance".to_string(),
    }]
}

/// Handler for static-granted Menace (requires 2+ blockers).
fn handle_static_menace(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Menace".to_string(),
    }]
}

/// Handler for static-granted Reach (can block flying).
fn handle_static_reach(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Reach".to_string(),
    }]
}

/// Handler for static-granted Flying.
fn handle_static_flying(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Flying".to_string(),
    }]
}

/// Handler for static-granted Trample.
fn handle_static_trample(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Trample".to_string(),
    }]
}

/// Handler for static-granted Deathtouch.
fn handle_static_deathtouch(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Deathtouch".to_string(),
    }]
}

/// Handler for static-granted Lifelink.
fn handle_static_lifelink(
    _state: &GameState,
    _mode: &StaticMode,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Lifelink".to_string(),
    }]
}

/// Check if any active static ability of the given mode applies to the context.
///
/// CR 604.1: Static abilities are always "on" — they don't use the stack.
/// Scans battlefield objects for static_definitions matching the mode,
/// then checks if the static's condition applies.
pub fn check_static_ability(
    state: &GameState,
    mode: StaticMode,
    context: &StaticCheckContext,
) -> bool {
    // CR 114.4: Abilities of emblems function in the command zone.
    // Check both battlefield objects and command zone emblems. The functioning
    // gate (phased-out + command-zone + condition) is owned by
    // `active_static_definitions`.
    let zones = state.battlefield.iter().chain(state.command_zone.iter());
    for &id in zones {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        for def in active_static_definitions(state, obj) {
            if def.mode != mode {
                continue;
            }

            // Check affected filter if present (typed TargetFilter)
            if let Some(ref affected) = def.affected {
                if !static_filter_matches(state, context, affected, id) {
                    continue;
                }
            }

            return true;
        }
    }

    false
}

/// CR 609.4b: Check if a player has the "spend mana as any color" static active.
/// Scans battlefield and command zone for `StaticMode::SpendManaAsAnyColor`
/// whose affected filter matches the given player.
pub fn player_can_spend_as_any_color(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::SpendManaAsAnyColor,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// CR 104.2b: Check if a player has active `CantWinTheGame` protection.
///
/// When `true`, effect-based win attempts (CR 104.2b, e.g., "target player wins
/// the game") targeting this player must be no-ops. Per CR 104.2a, the
/// last-player-standing path is not subject to this check and is enforced
/// directly in `elimination::check_game_over`.
pub fn player_has_cant_win(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::CantWinTheGame,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// CR 119.7: Check if a player has active `CantGainLife` protection.
///
/// When `true`, effects that would cause the player to gain life have no effect
/// (CR 119.7: "a replacement effect that would replace a life gain event
/// affecting that player won't do anything"). Callers must short-circuit BEFORE
/// invoking the replacement pipeline.
pub fn player_has_cant_gain_life(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::CantGainLife,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// CR 119.8: Check if a player has active `CantLoseLife` protection.
///
/// When `true`, effects that would cause the player to lose life (including
/// damage-to-life-loss conversion per CR 120.3) have no effect.
pub fn player_has_cant_lose_life(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::CantLoseLife,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// CR 702.16j: Check if a player has active "protection from everything".
///
/// Scans `state.transient_continuous_effects` for effects whose `affected`
/// filter pins this specific player and whose modifications include an
/// `AddKeyword { Protection(ProtectionTarget::Everything) }`. Respects the
/// optional `condition` on each transient.
///
/// This is the single authority for player-scoped protection-from-everything
/// enforcement — consulted by targeting (CR 702.16b + 702.16j), damage
/// (CR 702.16j + CR 615.1), and attack-target legality (CR 508.1b +
/// CR 702.16j). Callers never inspect the transient_continuous_effects
/// table directly.
///
/// Note: player-scoped protection uses the transient-effect table rather than
/// the battlefield-object `static_definitions` scan used by `CantGainLife`
/// etc. because a protected player can have zero permanents on the
/// battlefield (e.g., right after Teferi's Protection phases them all out).
pub fn player_has_protection_from_everything(state: &GameState, player_id: PlayerId) -> bool {
    use crate::types::ability::ContinuousModification;
    use crate::types::keywords::{Keyword, ProtectionTarget};
    for tce in &state.transient_continuous_effects {
        let TargetFilter::SpecificPlayer { id: affected_id } = tce.affected else {
            continue;
        };
        if affected_id != player_id {
            continue;
        }
        // CR 611.2b: ForAsLongAs durations re-evaluate their condition each cycle.
        if let crate::types::ability::Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        let grants_everything = tce.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Everything),
                }
            )
        });
        if grants_everything {
            return true;
        }
    }
    false
}

/// Allocation-free equivalent of `check_static_ability` for
/// `StaticMode::Other(String)` variants. Scans battlefield + command zone
/// for a static whose mode is `Other(s)` with `s == name`, whose `affected`
/// filter matches the given context, and whose `condition` (if any) is true.
///
/// Used for the prohibition-family statics (`CantBeSacrificed`, etc.) where
/// constructing `StaticMode::Other(name.to_string())` on every call would
/// allocate in potentially hot paths (damage resolution, sacrifice loops).
fn check_static_other_by_name(state: &GameState, name: &str, context: &StaticCheckContext) -> bool {
    // CR 114.4: Abilities of emblems function in the command zone.
    // Functioning gate owned by `active_static_definitions`.
    let zones = state.battlefield.iter().chain(state.command_zone.iter());
    for &id in zones {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };
        for def in active_static_definitions(state, obj) {
            match &def.mode {
                StaticMode::Other(s) if s == name => {}
                _ => continue,
            }
            if let Some(ref affected) = def.affected {
                if !static_filter_matches(state, context, affected, id) {
                    continue;
                }
            }
            return true;
        }
    }
    false
}

/// Check if a static ability named `name` applies to a specific object
/// (target-scoped query). Used for object-targeted prohibitions like
/// `CantBeSacrificed`, `CantBeEnchanted`, `CantTransform`, etc.
pub fn object_has_static_other(state: &GameState, object_id: ObjectId, name: &str) -> bool {
    check_static_other_by_name(
        state,
        name,
        &StaticCheckContext {
            target_id: Some(object_id),
            ..Default::default()
        },
    )
}

/// Check if a static ability named `name` applies to a specific player
/// (player-scoped query). Used for player-targeted prohibitions like
/// `CantPlayLand`, `CantShuffle`.
pub fn player_has_static_other(state: &GameState, player_id: PlayerId, name: &str) -> bool {
    check_static_other_by_name(
        state,
        name,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// Check if a static ability's affected filter matches the check context.
pub(crate) fn static_filter_matches(
    state: &GameState,
    context: &StaticCheckContext,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    if let Some(target_id) = context.target_id {
        return matches_target_filter(
            state,
            target_id,
            filter,
            &FilterContext::from_source(state, source_id),
        );
    }

    if let Some(player_id) = context.player_id {
        // For player-targeted checks, we still use the string-based player filter.
        // TargetFilter::Player variant just returns false for object matching,
        // so we need to check if this is a player-affecting filter.
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        match filter {
            TargetFilter::Any => return true,
            TargetFilter::Player => {
                // All players match
                return true;
            }
            TargetFilter::Typed(TypedFilter { controller, .. }) => {
                if let Some(ctrl) = controller {
                    return match ctrl {
                        crate::types::ability::ControllerRef::You => {
                            source_controller == Some(player_id)
                        }
                        crate::types::ability::ControllerRef::Opponent => {
                            source_controller.is_some() && source_controller != Some(player_id)
                        }
                        // CR 109.4: Static abilities have no ability-target context
                        // in which to resolve a target player. Fail closed — the
                        // parser never emits this variant for static filters.
                        crate::types::ability::ControllerRef::TargetPlayer => false,
                    };
                }
                return true;
            }
            _ => return true,
        }
    }

    // No specific target -- matches by default
    true
}

/// CR 305.2 + CR 505.6b: Count the number of additional land drops granted to
/// a player by static abilities on the battlefield.
/// Scans for both `MayPlayAdditionalLand` (+1) and `AdditionalLandDrop { count }`
/// (typed count determined at parse time).
pub fn additional_land_drops(state: &GameState, player: PlayerId) -> u8 {
    let context = StaticCheckContext {
        player_id: Some(player),
        ..Default::default()
    };

    let mut total: u8 = 0;

    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out
    // / command-zone / condition gate, so Azusa phased out correctly stops
    // granting land drops.
    for (obj, def) in battlefield_active_statics(state) {
        // CR 305.2: Determine the additional land count from the variant.
        let count = match def.mode {
            StaticMode::MayPlayAdditionalLand => 1,
            StaticMode::AdditionalLandDrop { count } => count,
            _ => continue,
        };

        // Check if this static applies to the given player
        if let Some(ref affected) = def.affected {
            if !static_filter_matches(state, &context, affected, obj.id) {
                continue;
            }
        }

        total = total.saturating_add(count);
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::StaticCondition;
    use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn test_registry_has_all_modes() {
        let registry = build_static_registry();
        // 1 Continuous + core rule-mod variants + 11 promoted prohibition
        // entries (CR 305.2, CR 306.7, CR 701.3, CR 701.19, CR 701.21,
        // CR 701.24, CR 701.27, CR 702.5, CR 702.6, CR 120.1, CR 120.2).
        // Phantom `StaticMode::Other(...)` stubs with no parser emission
        // were removed; if you're adding a new static mode, bump this lower
        // bound so the test reflects it.
        assert!(
            registry.len() >= 25,
            "Expected 25+ modes, got {}",
            registry.len()
        );
    }

    #[test]
    fn test_check_cant_attack() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pacifism Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Add CantAttack static targeting opponent's creatures
        let affected =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantAttack).affected(affected));

        let ctx = StaticCheckContext {
            target_id: Some(target),
            ..Default::default()
        };
        assert!(check_static_ability(&state, StaticMode::CantAttack, &ctx));
    }

    #[test]
    fn test_check_no_matching_static() {
        let state = setup();
        let ctx = StaticCheckContext {
            target_id: Some(ObjectId(99)),
            ..Default::default()
        };
        assert!(!check_static_ability(&state, StaticMode::CantAttack, &ctx));
    }

    #[test]
    fn test_cant_be_blocked_returns_rule_modification() {
        let state = setup();
        let effects = handle_cant_be_blocked(&state, &StaticMode::CantBeBlocked, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::RuleModification { mode } => {
                assert_eq!(mode, "CantBeBlocked");
            }
            _ => panic!("Expected RuleModification effect"),
        }
    }

    #[test]
    fn test_protection_returns_rule_modification() {
        let state = setup();
        let effects = handle_protection(&state, &StaticMode::Protection, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::RuleModification { mode } => {
                assert_eq!(mode, "Protection");
            }
            _ => panic!("Expected RuleModification effect"),
        }
    }

    #[test]
    fn test_continuous_mode_returns_effects() {
        let state = setup();
        let effects = handle_continuous(&state, &StaticMode::Continuous, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], StaticEffect::Continuous);
    }

    #[test]
    fn test_indestructible_returns_rule_modification() {
        let state = setup();
        let effects = handle_indestructible(&state, &StaticMode::Indestructible, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "Indestructible".to_string()
            }
        );
    }

    #[test]
    fn test_cant_be_countered_returns_rule_modification() {
        let state = setup();
        let effects = handle_cant_be_countered(&state, &StaticMode::CantBeCountered, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "CantBeCountered".to_string()
            }
        );
    }

    #[test]
    fn test_flashback_returns_rule_modification() {
        let state = setup();
        let effects = handle_flashback(&state, &StaticMode::FlashBack, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "FlashBack".to_string()
            }
        );
    }

    #[test]
    fn test_cant_be_destroyed_returns_rule_modification() {
        let state = setup();
        let effects = handle_cant_be_destroyed(&state, &StaticMode::CantBeDestroyed, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "CantBeDestroyed".to_string()
            }
        );
    }

    #[test]
    fn test_static_keyword_handlers_return_correct_modes() {
        let state = setup();

        type StaticHandlerTestCase<'a> = (
            fn(&GameState, &StaticMode, ObjectId) -> Vec<StaticEffect>,
            StaticMode,
            &'a str,
        );
        let test_cases: &[StaticHandlerTestCase<'_>] = &[
            (handle_static_vigilance, StaticMode::Vigilance, "Vigilance"),
            (handle_static_menace, StaticMode::Menace, "Menace"),
            (handle_static_reach, StaticMode::Reach, "Reach"),
            (handle_static_flying, StaticMode::Flying, "Flying"),
            (handle_static_trample, StaticMode::Trample, "Trample"),
            (
                handle_static_deathtouch,
                StaticMode::Deathtouch,
                "Deathtouch",
            ),
            (handle_static_lifelink, StaticMode::Lifelink, "Lifelink"),
            (handle_shroud, StaticMode::Shroud, "Shroud"),
        ];

        for (handler, mode, expected) in test_cases {
            let effects = handler(&state, mode, ObjectId(1));
            assert_eq!(
                effects[0],
                StaticEffect::RuleModification {
                    mode: expected.to_string()
                },
                "Handler for {} returned wrong mode",
                expected,
            );
        }
    }

    #[test]
    fn test_promoted_statics_no_longer_stubs() {
        let registry = build_static_registry();
        // Promoted statics should NOT return empty Vec (which stub does)
        let state = setup();

        // Typed variant (CantBeCountered uses a proper enum variant, not Other)
        let cant_be_countered_handler = registry
            .get(&StaticMode::CantBeCountered)
            .expect("CantBeCountered should be in registry");
        let effects = cant_be_countered_handler(&state, &StaticMode::CantBeCountered, ObjectId(1));
        assert!(
            !effects.is_empty(),
            "CantBeCountered should return non-empty effects"
        );

        let promoted_modes = [
            StaticMode::Indestructible,
            StaticMode::CantBeDestroyed,
            StaticMode::FlashBack,
            StaticMode::Vigilance,
            StaticMode::Menace,
            StaticMode::Reach,
            StaticMode::Flying,
            StaticMode::Trample,
            StaticMode::Deathtouch,
            StaticMode::Lifelink,
            StaticMode::Shroud,
            // Tier 3 promoted statics
            StaticMode::BlockRestriction,
            StaticMode::NoMaximumHandSize,
            StaticMode::MayPlayAdditionalLand,
            StaticMode::MayChooseNotToUntap,
            // Note: AdditionalLandDrop is data-carrying, not in registry
            StaticMode::EmblemStatic,
        ];
        for mode_key in &promoted_modes {
            let handler = registry
                .get(mode_key)
                .unwrap_or_else(|| panic!("{} should be in registry", mode_key));
            let effects = handler(&state, mode_key, ObjectId(1));
            assert!(
                !effects.is_empty(),
                "{} should return non-empty effects (no longer a stub)",
                mode_key
            );
        }
    }

    #[test]
    fn test_no_maximum_hand_size_check() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Reliquary Tower".to_string(),
            Zone::Battlefield,
        );

        // CR 402.2: Add NoMaximumHandSize static with "You" affected filter
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::NoMaximumHandSize).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        // Controller (Player 0) should have no max hand size
        let ctx_p0 = StaticCheckContext {
            player_id: Some(PlayerId(0)),
            ..Default::default()
        };
        assert!(check_static_ability(
            &state,
            StaticMode::NoMaximumHandSize,
            &ctx_p0
        ));

        // Opponent (Player 1) should still have max hand size
        let ctx_p1 = StaticCheckContext {
            player_id: Some(PlayerId(1)),
            ..Default::default()
        };
        assert!(!check_static_ability(
            &state,
            StaticMode::NoMaximumHandSize,
            &ctx_p1
        ));
    }

    #[test]
    fn test_no_maximum_hand_size_emblem_in_command_zone() {
        // CR 114.4: Abilities of emblems function in the command zone.
        let mut state = setup();
        let emblem_id = crate::game::zones::create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Emblem".to_string(),
            Zone::Command,
        );
        let obj = state.objects.get_mut(&emblem_id).unwrap();
        obj.is_emblem = true;
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::NoMaximumHandSize));

        // Controller (Player 0) should have no max hand size from emblem
        let ctx_p0 = StaticCheckContext {
            player_id: Some(PlayerId(0)),
            ..Default::default()
        };
        assert!(check_static_ability(
            &state,
            StaticMode::NoMaximumHandSize,
            &ctx_p0
        ));
    }

    #[test]
    fn test_additional_land_drops_none() {
        let state = setup();
        assert_eq!(additional_land_drops(&state, PlayerId(0)), 0);
    }

    #[test]
    fn test_additional_land_drops_exploration() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Exploration".to_string(),
            Zone::Battlefield,
        );

        // CR 305.2: "You may play an additional land on each of your turns"
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::MayPlayAdditionalLand)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    ))
                    .description("You may play an additional land on each of your turns.".into()),
            );

        assert_eq!(additional_land_drops(&state, PlayerId(0)), 1);
        // Opponent doesn't get the extra drop
        assert_eq!(additional_land_drops(&state, PlayerId(1)), 0);
    }

    #[test]
    fn test_additional_land_drops_two_additional() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Azusa".to_string(),
            Zone::Battlefield,
        );

        // CR 305.2: "You may play two additional lands on each of your turns"
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 2 })
                    .description("You may play two additional lands on each of your turns.".into()),
            );

        assert_eq!(additional_land_drops(&state, PlayerId(0)), 2);
    }

    #[test]
    fn test_additional_land_drops_stacks() {
        let mut state = setup();

        // Two Explorations on the battlefield
        for i in 0..2 {
            let source = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Exploration {}", i),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .static_definitions
                .push(
                    StaticDefinition::new(StaticMode::MayPlayAdditionalLand)
                        .affected(TargetFilter::Typed(
                            TypedFilter::default().controller(ControllerRef::You),
                        ))
                        .description(
                            "You may play an additional land on each of your turns.".into(),
                        ),
                );
        }

        // CR 305.2: Two Explorations = +2 additional land drops
        assert_eq!(additional_land_drops(&state, PlayerId(0)), 2);
    }

    #[test]
    fn test_additional_land_drops_all_players() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rites of Flourishing".to_string(),
            Zone::Battlefield,
        );

        // "Each player may play an additional land" — affects all players
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::MayPlayAdditionalLand)
                    .affected(TargetFilter::Player)
                    .description(
                        "Each player may play an additional land on each of their turns.".into(),
                    ),
            );

        assert_eq!(additional_land_drops(&state, PlayerId(0)), 1);
        assert_eq!(additional_land_drops(&state, PlayerId(1)), 1);
    }

    #[test]
    fn test_cant_untap_with_condition_met_blocks() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Alirios".to_string(),
            Zone::Battlefield,
        );

        // Add a Reflection creature so the IsPresent condition is met
        let reflection = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Reflection".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&reflection)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // CantUntap with condition "as long as you control a creature"
        let condition = StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(
                crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
            )),
        };
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantUntap)
                    .affected(TargetFilter::SelfRef)
                    .condition(condition),
            );

        let ctx = StaticCheckContext {
            target_id: Some(source),
            ..Default::default()
        };
        // Condition is met (we control a creature) — CantUntap should apply
        assert!(check_static_ability(&state, StaticMode::CantUntap, &ctx));
    }

    #[test]
    fn test_cant_untap_with_condition_not_met_allows() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Alirios".to_string(),
            Zone::Battlefield,
        );

        // CantUntap with condition "as long as you control a creature" — but no creature exists
        let condition = StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(
                crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
            )),
        };
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantUntap)
                    .affected(TargetFilter::SelfRef)
                    .condition(condition),
            );

        let ctx = StaticCheckContext {
            target_id: Some(source),
            ..Default::default()
        };
        // Condition not met (no creature controlled) — CantUntap should NOT apply
        assert!(!check_static_ability(&state, StaticMode::CantUntap, &ctx));
    }

    #[test]
    fn test_object_has_static_other_cant_be_sacrificed() {
        // End-to-end: a battlefield object carrying a self-ref
        // `StaticMode::Other("CantBeSacrificed")` static is observed by the
        // runtime guard `object_has_static_other(id, "CantBeSacrificed")`.
        // This proves the parser wiring emitted by oracle_static.rs is seen
        // by the sacrifice-path guard in `game::sacrifice`.
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hithlain Rope".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                    .affected(TargetFilter::SelfRef),
            );

        assert!(object_has_static_other(&state, source, "CantBeSacrificed"));
        // Sanity: unrelated prohibition name must NOT fire.
        assert!(!object_has_static_other(&state, source, "CantTransform"));
    }

    /// CR 702.16j: When a transient continuous effect grants a specific player
    /// `AddKeyword(Protection(Everything))`, the query returns true for that
    /// player and false for every other player — scoping is per-player.
    #[test]
    fn player_protection_query_per_player_scoping() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection Source".to_string(),
            Zone::Battlefield,
        );

        // Baseline: neither player has protection.
        assert!(!player_has_protection_from_everything(&state, PlayerId(0)));
        assert!(!player_has_protection_from_everything(&state, PlayerId(1)));

        // Register a transient effect granting protection to PlayerId(0).
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        assert!(
            player_has_protection_from_everything(&state, PlayerId(0)),
            "PlayerId(0) must be protected"
        );
        assert!(
            !player_has_protection_from_everything(&state, PlayerId(1)),
            "PlayerId(1) must not be protected — per-player scoping"
        );
    }

    /// CR 702.16j: Only `Protection(Everything)` triggers the query. Other
    /// protection qualities (color, card type) on a player do NOT satisfy
    /// `player_has_protection_from_everything` — they would have their own
    /// dedicated queries (deferred from this batch).
    #[test]
    fn player_protection_query_rejects_non_everything_qualities() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::{Keyword, ProtectionTarget};
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Not Teferi".to_string(),
            Zone::Battlefield,
        );

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)),
            }],
            None,
        );

        // Color protection is not "everything" — query returns false.
        assert!(!player_has_protection_from_everything(&state, PlayerId(0)));
    }

    /// CR 704.5: When the transient effect is expired/removed, the player is
    /// no longer protected.
    #[test]
    fn player_protection_query_false_after_effect_removed() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection Source".to_string(),
            Zone::Battlefield,
        );

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );
        assert!(player_has_protection_from_everything(&state, PlayerId(0)));

        // Remove the transient — mirrors the cleanup path in layers.rs.
        state.transient_continuous_effects.clear();
        assert!(!player_has_protection_from_everything(&state, PlayerId(0)));
    }
}
