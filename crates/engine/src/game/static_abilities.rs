use std::collections::HashMap;
use std::sync::LazyLock;

use crate::game::combat::AttackTarget;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::functioning_abilities::{
    battlefield_active_statics, game_active_statics, game_functioning_statics,
};
use crate::game::layers::{evaluate_condition, evaluate_condition_with_recipient};
use crate::types::ability::{ContinuousModification, Duration, TargetFilter, TypedFilter};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::{
    CombatAloneAction, CombatAloneRequirement, CostPaymentProhibition, CrewAction,
    CrewContributionKind, ProhibitionScope, StaticMode,
};

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
    /// CR 508.1d: When checking scoped `CantAttack` statics (`attack_defended`),
    /// the declared attack target for the creature in `target_id`.
    pub attack_target: Option<AttackTarget>,
}

/// Process-wide cached static-ability registry.
///
/// Mirrors [`crate::game::trigger_matchers::trigger_registry`]: the registry
/// is a pure constant (`StaticMode` → fn-pointer), so it is built once.
/// `unimplemented_mechanics` consults it per battlefield object per `apply()`;
/// rebuilding it per call was a display-derivation hot-path cost.
static STATIC_REGISTRY: LazyLock<HashMap<StaticMode, StaticAbilityHandler>> =
    LazyLock::new(build_static_registry);

/// Cached accessor for the static-ability registry. Built once on first use.
pub fn static_registry() -> &'static HashMap<StaticMode, StaticAbilityHandler> {
    &STATIC_REGISTRY
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
    // CR 603.2 + CR 609.3: CantCauseSacrificeOrExile is a data-carrying variant —
    // runtime enforcement is in effects/sacrifice.rs and effects/change_zone.rs via
    // triggered_cause_sacrifice_or_exile_muzzled(). Coverage support is via
    // is_data_carrying_static().
    //
    // CR 603.2g + CR 603.6a + CR 700.4: SuppressTriggers is a data-carrying variant —
    // runtime enforcement is in triggers.rs via event_is_suppressed_by_static_triggers().
    // Coverage support is via is_data_carrying_static(). Per CR 603.6d, static
    // "enters tapped" / "enters with counters" / "as X enters" effects are NOT
    // triggered and are unaffected by this variant.
    // CR 702.8a: CastWithFlash — card may be cast at instant speed.
    registry.insert(StaticMode::CastWithFlash, handle_rule_mod);
    // CR 601.2f: ModifyCost (Reduce/Raise modes) is a data-carrying variant — runtime checks are
    // in game/casting.rs::apply_battlefield_cost_modifiers(). Coverage support is via
    // is_data_carrying_static() in game/coverage.rs.
    // Note: ReduceAbilityCost runtime checks are in game/keywords.rs::apply_ability_cost_reduction().
    registry.insert(StaticMode::CantGainLife, handle_rule_mod);
    registry.insert(StaticMode::CantLoseLife, handle_rule_mod);
    registry.insert(StaticMode::MustAttack, handle_rule_mod);
    registry.insert(StaticMode::MustBlock, handle_rule_mod);
    // Note: CantDraw is a data-carrying variant — runtime enforcement is in
    // game/effects/draw.rs. Coverage support is via is_data_carrying_static().
    // Note: DrawFromBottom (CR 121.1/613.11) is a data-carrying variant — its
    // top-vs-bottom selection is enforced in
    // game/effects/draw.rs::select_cards_to_draw, which all four draw-delivery
    // paths consult. Coverage support is via is_data_carrying_static().
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
    // CR 701.19c: CantBeRegenerated — a marked permanent's regeneration shields
    // are not applied the next time it would be destroyed. Passive rule
    // modification; runtime enforcement is in replacement.rs::destroy_applier via
    // object_has_active_cant_be_regenerated(). Registered as a rule-mod so coverage
    // marks the standalone "can't be regenerated" effect as supported.
    registry.insert(StaticMode::CantBeRegenerated, handle_rule_mod);
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
    // CR 509.1c: MustBeBlockedByAll — every creature able to block this creature
    // must do so ("All creatures able to block ~ do so"; enforced in combat.rs).
    registry.insert(StaticMode::MustBeBlockedByAll, handle_rule_mod);
    // CR 701.15b: Goaded — this creature must attack and avoid the goading
    // player if able. Runtime enforcement lives in combat.rs.
    registry.insert(StaticMode::Goaded, handle_rule_mod);
    // CR 506.5 + CR 508.1c + CR 509.1b: CombatAlone — parameterized "alone"
    // restriction. Runtime enforcement lives in combat.rs.
    registry.insert(
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::NeedsCompanion,
        },
        handle_rule_mod,
    );
    registry.insert(
        StaticMode::CombatAlone {
            action: CombatAloneAction::Block,
            requirement: CombatAloneRequirement::NeedsCompanion,
        },
        handle_rule_mod,
    );
    registry.insert(
        StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::MustBeSole,
        },
        handle_rule_mod,
    );
    // CR 702.122d: CantCrew — creature can't be tapped to pay a crew cost.
    registry.insert(StaticMode::CantCrew, handle_rule_mod);
    registry.insert(StaticMode::MayLookAtTopOfLibrary, handle_rule_mod);
    // CR 104.3b: CantLoseTheGame — player can't lose the game (Platinum Angel).
    // Runtime enforcement is in sba.rs::player_has_cant_lose().
    registry.insert(StaticMode::CantLoseTheGame, handle_rule_mod);
    // CR 104.2b: CantWinTheGame — a player can't win the game from effects
    // (Platinum Angel). Runtime enforcement is in effects/win_lose.rs::resolve_win
    // via player_has_cant_win(). Per CR 104.2a, the last-player-standing case
    // is not blocked by this static and is enforced by elimination::check_game_over.
    registry.insert(StaticMode::CantWinTheGame, handle_rule_mod);
    // CR 704.5j: LegendRuleDoesntApply — affected permanents are excluded from
    // the legend-rule SBA. Runtime enforcement is in sba.rs::legend_rule_exempt().
    registry.insert(StaticMode::LegendRuleDoesntApply, handle_rule_mod);
    // CR 702.179e: Card-specific rule modification allowing speed to exceed 4.
    registry.insert(StaticMode::SpeedCanIncreaseBeyondFour, handle_rule_mod);
    // CR 609.4b: "You may spend mana as though it were mana of any color."
    // Runtime enforcement is in mana_payment.rs via player_can_spend_as_any_color().
    // The board-wide (`spell_filter: None`) shape is registry-keyed here; the
    // spell-filtered (`Some`) shape (Vizier of the Menagerie) carries an
    // unbounded `TargetFilter` value space, so it gets coverage support via
    // `coverage::is_data_carrying_static` instead (mirrors SkipStep / RevealHand).
    registry.insert(
        StaticMode::SpendManaAsAnyColor { spell_filter: None },
        handle_rule_mod,
    );
    // CR 107.4f: PayLifeAsColoredMana — "For each {C} in a cost, you may pay
    // 2 life rather than pay that mana" (K'rrik, Son of Yawgmoth). Data-carrying
    // (ManaColor); registered per concrete instance via
    // `register_data_carrying_static_handler` style — but the static-ability
    // scan in `player_life_payment_colors` reads `static_definitions` directly,
    // so this loop-style registration is the right shape for parser support.
    for color in crate::types::mana::ManaColor::ALL {
        registry.insert(StaticMode::PayLifeAsColoredMana { color }, handle_rule_mod);
    }
    // CR 702.3b: CanAttackWithDefender — allows creatures with defender to attack.
    // Runtime enforcement is in combat.rs::validate_attack().
    registry.insert(StaticMode::CanAttackWithDefender, handle_rule_mod);
    // CR 509.1b + CR 609.4 + CR 702.14c: IgnoreLandwalkForBlocking — global
    // rule-modification static observed inside is_landwalk_unblockable.
    // Registered per discriminant shape (the `None` instance plus the five
    // basic-subtype instances) to mirror the data-carrying static precedent
    // (e.g., ExtraBlockers { count: None } row).
    registry.insert(
        StaticMode::IgnoreLandwalkForBlocking { qualifier: None },
        handle_rule_mod,
    );
    for q in ["Plains", "Island", "Swamp", "Mountain", "Forest"] {
        registry.insert(
            StaticMode::IgnoreLandwalkForBlocking {
                qualifier: Some(q.to_string()),
            },
            handle_rule_mod,
        );
    }
    // CR 602.5a: CanActivateAbilitiesAsThoughHaste — bypasses the summoning-sickness
    // gate on a creature's {T}/{Q} activated abilities (Tyvar, Jubilant Brawler).
    // Runtime enforcement is in restrictions.rs::summoning_sick_for_tap_ability().
    registry.insert(
        StaticMode::CanActivateAbilitiesAsThoughHaste,
        handle_rule_mod,
    );
    // CR 509.1b + CR 609.4 + CR 702.28b: CanBlockShadow — per-source permission to
    // block shadow attackers despite not having shadow (Heartwood Dryad, Wall of
    // Diffusion). Runtime enforcement is in combat.rs via `can_block_shadow_attacker`,
    // consulted by both validate_blockers_for_player and can_block_pair.
    registry.insert(StaticMode::CanBlockShadow, handle_rule_mod);
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
    // not through the standard handler flow, but we register CantEnterBattlefieldFrom as
    // rule_mod so that `check_static_ability` queries work.
    registry.insert(StaticMode::CantEnterBattlefieldFrom, handle_rule_mod);
    // Note: CantCastFrom is a data-carrying variant (carries `who` + the prohibited-zone
    // list on `affected`) — parameterized, so no registry entry. Runtime enforcement is in
    // casting.rs::is_blocked_from_casting_from_zone(). Coverage support is via
    // is_data_carrying_static().
    // Note: CantCastDuring is a data-carrying variant — runtime enforcement will be in
    // casting.rs. Coverage support is via is_data_carrying_static().
    // Note: CantActivateDuring is a data-carrying variant — runtime enforcement is in
    // casting.rs::is_blocked_by_cant_activate_during(). Coverage support is via
    // is_data_carrying_static(). Like CantBeActivated, parameterized — no registry entry.
    // Note: PerTurnCastLimit is a data-carrying variant — runtime enforcement is in
    // casting.rs::is_blocked_by_per_turn_cast_limit(). Coverage support is via is_data_carrying_static().

    // Promoted Tier 3 statics -- parser-produced, rule-modification handlers
    // Note: BlockRestriction is data-carrying — runtime enforcement is in
    // combat.rs::can_block_pair via blocker-side static scan. Coverage support
    // is via is_data_carrying_static().
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
    // CR 701.55c: GrantsExtraVillainousChoice — "If an opponent would face a
    // villainous choice, they face that choice an additional time." (The
    // Valeyard). Runtime enforcement is in
    // game/effects/choose_one_of.rs::villainous_extra_instances_for(), which
    // scans active_static_definitions when assembling the facing-player list. No
    // continuous-effect plumbing needed; registered here so coverage marks the
    // card as supported.
    registry.insert(StaticMode::GrantsExtraVillainousChoice, handle_rule_mod);

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
    //     (`ModifyCost`, `DefilerCostReduction`, etc.)
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

/// CR 603.2: True when the effect currently resolving was put on the stack as a
/// triggered ability (including delayed triggers created during resolution).
fn is_resolving_triggered_ability(state: &GameState) -> bool {
    use crate::types::game_state::StackEntryKind;
    state
        .resolving_stack_entry
        .as_ref()
        .is_some_and(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
}

/// CR 603.2 + CR 609.3: Check whether a triggered ability controlled by
/// `ability.controller` is muzzled from causing `acting_player` to sacrifice or
/// exile `object_id` by an active `CantCauseSacrificeOrExile` static.
///
/// E.g., The Master, Multiplied: "Triggered abilities you control can't cause
/// you to sacrifice or exile creature tokens you control."
pub(crate) fn triggered_cause_sacrifice_or_exile_muzzled(
    state: &GameState,
    ability: &crate::types::ability::ResolvedAbility,
    object_id: crate::types::identifiers::ObjectId,
    acting_player: crate::types::player::PlayerId,
) -> bool {
    use crate::types::statics::StaticMode;

    if !is_resolving_triggered_ability(state) {
        return false;
    }
    // "cause you to" — only the ability's controller is protected as the actor.
    if acting_player != ability.controller {
        return false;
    }
    for (bf_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantCauseSacrificeOrExile { ref cause } = def.mode else {
            continue;
        };
        if !prohibition_scope_matches_player(cause, ability.controller, bf_obj.id, state) {
            continue;
        }
        let Some(affected) = def.affected.as_ref() else {
            continue;
        };
        let ctx = crate::game::filter::FilterContext::from_source(state, bf_obj.id);
        if crate::game::filter::matches_target_filter(state, object_id, affected, &ctx) {
            return true;
        }
    }
    false
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
    // Perf: this is the O(N) whole-battlefield sweep that combat/untap legality
    // loops hoist an existence gate in front of (see
    // `functioning_abilities::any_functioning_static_mode`).
    crate::game::perf_counters::record_static_full_scan();
    // CR 114.4: Abilities of emblems function in the command zone.
    // Check both battlefield objects and command zone emblems. The functioning
    // gate is applied before context-specific condition evaluation below.
    for (obj, def) in game_functioning_statics(state) {
        if def.mode != mode {
            continue;
        }

        // Check affected filter if present (typed TargetFilter)
        if let Some(ref affected) = def.affected {
            if !static_filter_matches(state, context, affected, obj.id) {
                continue;
            }
        }

        if !static_condition_matches_context(state, obj.id, obj.controller, def, context) {
            continue;
        }

        // CR 508.1d: Scoped attack prohibitions (Eriette, Propaganda-family flat
        // restrictions) only apply when the declared target matches `attack_defended`.
        // When no target is in context (eligibility queries), skip scoped statics so
        // the creature remains able to attack other players.
        if matches!(
            def.mode,
            StaticMode::CantAttack | StaticMode::CantAttackOrBlock
        ) {
            if let Some(defended) = def.attack_defended.as_ref() {
                if !super::restrictions::attack_target_matches_defended_scope(
                    state,
                    context.attack_target.as_ref(),
                    defended,
                    obj.controller,
                    obj.owner,
                ) {
                    continue;
                }
            }
        }

        // CR 101.2 + CR 109.5: per-affected-player applicability gate. Evaluated
        // against the affected object's controller (the player whose creature/spell
        // is restricted), distinct from the source-relative `condition` gate above.
        // Used by "each opponent who [did X] this turn can't [Y]" prohibitions
        // (Angelic Arbiter's attack clause).
        if let Some(ref cond) = def.per_player_condition {
            let affected_player = context
                .target_id
                .and_then(|id| state.objects.get(&id))
                .map(|o| o.controller)
                .or(context.player_id);
            match affected_player {
                Some(p) => {
                    if !crate::game::restrictions::evaluate_condition(state, p, obj.id, cond) {
                        continue;
                    }
                }
                // No affected player in context -> cannot evaluate a per-player
                // gate; fail closed (skip this static) so an under-specified query
                // never over-applies the prohibition.
                None => continue,
            }
        }

        return true;
    }

    false
}

/// CR 611.1 + CR 611.3: Scan `state.transient_continuous_effects` for an effect
/// bound to `player_id` (via `TargetFilter::SpecificPlayer { id }`) whose
/// modifications include `AddStaticMode { mode }` matching the given `mode`.
/// Honors `ForAsLongAs` duration conditions and explicit `condition` gates.
///
/// This is the canonical query for player-scoped spell-applied restrictions
/// (e.g., Everybody Lives! fans out to per-player `SpecificPlayer` TCEs in
/// `effect.rs`). Callers (player_has_cant_win, player_has_cant_gain_life,
/// player_has_cant_lose_life) use this alongside `check_static_ability` so
/// both permanent-sourced and spell-sourced protection are covered.
pub(crate) fn transient_grants_static_mode_to_player(
    state: &GameState,
    player_id: PlayerId,
    mode: &StaticMode,
) -> bool {
    for tce in &state.transient_continuous_effects {
        let TargetFilter::SpecificPlayer { id: affected_id } = tce.affected else {
            continue;
        };
        if affected_id != player_id {
            continue;
        }
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        let grants_mode = tce.modifications.iter().any(|m| {
            matches!(m, ContinuousModification::AddStaticMode { mode: m_mode } if m_mode == mode)
        });
        if grants_mode {
            return true;
        }
    }
    false
}

/// CR 611.1 + CR 611.3: Object-scoped counterpart to
/// [`transient_grants_static_mode_to_player`]. Scan
/// `state.transient_continuous_effects` for an effect that grants
/// `AddStaticMode { mode }` and whose typed/filter `affected` matches
/// `object_id` (e.g. a spell granting "creatures your opponents control don't
/// untap during their controllers' next untap steps"). Honors the same
/// `ForAsLongAs` duration and explicit `condition` gates as the player sibling.
///
/// `SpecificObject { id }` affecteds are intentionally NOT matched here: those
/// are an exact-id lookup that callers already cover directly. This query exists
/// to cover the filter-scoped class (`Typed` / `AnyOf` / `SelfRef` resolved
/// against the source, etc.) that an exact-id scan misses. Mirrors the
/// `matches_target_filter` source-context resolution used by
/// `triggered_cause_sacrifice_or_exile_muzzled`.
pub(crate) fn transient_grants_static_mode_to_object(
    state: &GameState,
    object_id: ObjectId,
    mode: &StaticMode,
) -> bool {
    for tce in &state.transient_continuous_effects {
        // Exact-id and player-scoped affecteds are handled by the dedicated
        // SpecificObject / SpecificPlayer paths; this query owns the rest.
        if matches!(
            tce.affected,
            TargetFilter::SpecificObject { .. } | TargetFilter::SpecificPlayer { .. }
        ) {
            continue;
        }
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        let grants_mode = tce.modifications.iter().any(|m| {
            matches!(m, ContinuousModification::AddStaticMode { mode: m_mode } if m_mode == mode)
        });
        if !grants_mode {
            continue;
        }
        let ctx = FilterContext::from_source(state, tce.source_id);
        if matches_target_filter(state, object_id, &tce.affected, &ctx) {
            return true;
        }
    }
    false
}

/// CR 609.4b: Check if a player has an unfiltered ("any spell/cost")
/// "spend mana as any color/type" static active. Scans battlefield and command
/// zone for `StaticMode::SpendManaAsAnyColor { spell_filter: None }` whose
/// affected filter matches the given player.
///
/// This is the board-wide path (Chromatic Orrery, Joiner Adept) — used for cost
/// payments that have no spell object in context (effects, activations) and as
/// the base case of the spell-scoped check. Spell-filtered statics (Vizier of
/// the Menagerie) are NOT consulted here; see
/// [`player_can_spend_as_any_color_for_spell_object`].
pub fn player_can_spend_as_any_color(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::SpendManaAsAnyColor { spell_filter: None },
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    )
}

/// CR 609.4b: Check if `player_id` may spend mana of any type/color to cast the
/// spell object `spell_id`. True when either an unfiltered board-wide static is
/// active (the [`player_can_spend_as_any_color`] base case) OR a spell-filtered
/// `StaticMode::SpendManaAsAnyColor { spell_filter: Some(filter) }` controlled
/// by `player_id` is active and `spell_id` matches that filter (Vizier of the
/// Menagerie: "you may spend mana of any type to cast creature spells").
///
/// The filtered concession is re-derived against the spell object at spend time
/// (CR 609.4b: it affects only how a cost is paid, never the cost itself), so it
/// applies only to spells the controller casts that match the spell class and
/// never to non-spell payments.
pub fn player_can_spend_as_any_color_for_spell_object(
    state: &GameState,
    player_id: PlayerId,
    spell_id: ObjectId,
) -> bool {
    if player_can_spend_as_any_color(state, player_id) {
        return true;
    }
    // CR 604.1 + CR 113.6b: scan battlefield permanents plus command-zone
    // emblems (`game_active_statics`), matching the zone coverage of the
    // unfiltered base case above (`player_can_spend_as_any_color` →
    // `game_functioning_statics`); `active_static_definitions` already applies
    // the phased-out / condition gate. The filtered static is "you may" —
    // scoped to the source's controller.
    for (obj, def) in game_active_statics(state) {
        let StaticMode::SpendManaAsAnyColor {
            spell_filter: Some(ref filter),
        } = def.mode
        else {
            continue;
        };
        if obj.controller != player_id {
            continue;
        }
        let ctx = FilterContext::from_source_with_controller(obj.id, player_id);
        if matches_target_filter(state, spell_id, filter, &ctx) {
            return true;
        }
    }
    false
}

/// CR 107.4f + CR 118.1: Colors for which `player` may pay 2 life rather than
/// 1 colored mana, aggregated across all active `PayLifeAsColoredMana` sources
/// (K'rrik-class statics). Single authority for the K'rrik payment grant.
///
/// Scans active battlefield/command-zone statics for `PayLifeAsColoredMana`
/// whose `affected` filter resolves to the given player (player-scope; mirrors
/// the `player_can_spend_as_any_color` scan), and unions each granted
/// `ManaColor` into the returned bitmask.
pub fn player_life_payment_colors(
    state: &GameState,
    player_id: PlayerId,
) -> crate::types::mana::LifePaymentColors {
    use crate::types::mana::LifePaymentColors;
    let context = StaticCheckContext {
        player_id: Some(player_id),
        ..Default::default()
    };
    let mut colors = LifePaymentColors::EMPTY;
    // CR 604.1 + CR 702.26b: `battlefield_active_statics` owns the
    // phased-out / command-zone / condition gate.
    for (obj, def) in battlefield_active_statics(state) {
        let StaticMode::PayLifeAsColoredMana { color } = def.mode else {
            continue;
        };
        if let Some(ref affected) = def.affected {
            if !static_filter_matches(state, &context, affected, obj.id) {
                continue;
            }
        }
        colors.insert(color);
    }
    colors
}

/// CR 118.1: Assemble the per-payment permission bundle for `player` —
/// the single authority for constructing a `CostPermissionContext` at every
/// cost-payment entry point (spell cast, activation, alt-cost effect).
///
/// `any_color_for_source` is the `any_color` decision for the specific cost
/// being paid (cast vs effect vs activation may compute this differently);
/// callers pass it in so this helper stays cost-site-agnostic.
pub fn build_cost_permission_context(
    state: &GameState,
    player_id: PlayerId,
    any_color_for_source: bool,
) -> crate::types::mana::CostPermissionContext {
    crate::types::mana::CostPermissionContext {
        any_color: any_color_for_source,
        max_life: super::life_costs::max_phyrexian_life_payments(state, player_id),
        life_colors: player_life_payment_colors(state, player_id),
    }
}

/// CR 104.2b: Check if a player has active `CantWinTheGame` protection.
///
/// When `true`, effect-based win attempts (CR 104.2b, e.g., "target player wins
/// the game") targeting this player must be no-ops. Per CR 104.2a, the
/// last-player-standing path is not subject to this check and is enforced
/// directly in `elimination::check_game_over`.
///
/// Checks both battlefield permanents and spell-applied transient effects
/// (e.g., a sorcery that grants all players CantWinTheGame this turn).
pub fn player_has_cant_win(state: &GameState, player_id: PlayerId) -> bool {
    check_static_ability(
        state,
        StaticMode::CantWinTheGame,
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    ) || transient_grants_static_mode_to_player(state, player_id, &StaticMode::CantWinTheGame)
}

/// Single-player check shared by `player_has_cant_gain_life` and
/// `player_has_cant_lose_life`: does `player_id` itself (battlefield permanent
/// or spell-applied transient effect) have an active static of `mode`?
fn life_lock_active_for(state: &GameState, player_id: PlayerId, mode: StaticMode) -> bool {
    check_static_ability(
        state,
        mode.clone(),
        &StaticCheckContext {
            player_id: Some(player_id),
            ..Default::default()
        },
    ) || transient_grants_static_mode_to_player(state, player_id, &mode)
}

/// CR 119.7 + CR 810.9g: Check if a player has active `CantGainLife`
/// protection.
///
/// When `true`, effects that would cause the player to gain life have no effect
/// (CR 119.7: "a replacement effect that would replace a life gain event
/// affecting that player won't do anything"). Callers must short-circuit BEFORE
/// invoking the replacement pipeline.
///
/// Checks both battlefield permanents and spell-applied transient effects. CR
/// 810.9g: "If an effect says that a player can't gain life, no player on
/// that player's team can gain life" — in team-based formats the lock also
/// propagates from either teammate.
pub fn player_has_cant_gain_life(state: &GameState, player_id: PlayerId) -> bool {
    life_lock_active_for(state, player_id, StaticMode::CantGainLife)
        || (super::topology::has_two_headed_giant_shared_resources(state)
            && super::players::teammates(state, player_id)
                .into_iter()
                .any(|teammate| life_lock_active_for(state, teammate, StaticMode::CantGainLife)))
}

/// CR 119.8 + CR 810.9h: Check if a player has active `CantLoseLife`
/// protection.
///
/// When `true`, effects that would cause the player to lose life (including
/// damage-to-life-loss conversion per CR 120.3) have no effect.
///
/// Checks both battlefield permanents and spell-applied transient effects. CR
/// 810.9h: "If an effect says that a player can't lose life, no player on
/// that player's team can lose life or pay any amount of life other than 0"
/// — in team-based formats the lock also propagates from either teammate.
pub fn player_has_cant_lose_life(state: &GameState, player_id: PlayerId) -> bool {
    life_lock_active_for(state, player_id, StaticMode::CantLoseLife)
        || (super::topology::has_two_headed_giant_shared_resources(state)
            && super::players::teammates(state, player_id)
                .into_iter()
                .any(|teammate| life_lock_active_for(state, teammate, StaticMode::CantLoseLife)))
}

/// CR 702.11b + CR 702.11e: Check if `player_id` may target creatures as though
/// they didn't have hexproof, including "hexproof from [quality]" variants
/// (CR 702.11e: an "as though it didn't have hexproof" effect also defeats
/// hexproof-from-quality). This is the player-scoped grant (Detection Tower
/// class — "you may target ... as though it
/// didn't have hexproof"), keyed on a battlefield `IgnoreHexproof` static with
/// NO object `affected` filter, plus the per-player transient grant.
///
/// Object-scoped `IgnoreHexproof` statics (Nowhere to Run, `affected = Some`)
/// are deliberately excluded here — they are not player grants and must not
/// widen the bypass to every target `player_id` chooses. Those are evaluated
/// per-target by [`target_ignores_hexproof`].
pub fn player_ignores_hexproof(state: &GameState, player_id: PlayerId) -> bool {
    let player_scoped_grant = game_functioning_statics(state).any(|(obj, def)| {
        matches!(def.mode, StaticMode::IgnoreHexproof)
            && def.affected.is_none()
            && static_condition_matches_context(
                state,
                obj.id,
                obj.controller,
                def,
                &StaticCheckContext {
                    player_id: Some(player_id),
                    ..Default::default()
                },
            )
    });
    player_scoped_grant
        || transient_grants_static_mode_to_player(state, player_id, &StaticMode::IgnoreHexproof)
}

/// CR 702.11b + CR 702.11e: Whether a FUNCTIONING `IgnoreHexproof` static whose
/// `condition` currently holds and which is scoped by an object `affected` filter
/// makes `target_id` targetable as though it had no hexproof (CR 702.11e extends
/// the bypass to hexproof-from-quality). Nowhere to Run — "Creatures your
/// opponents control can be the targets of spells and abilities as though they
/// didn't have hexproof." The card carries no "you control" qualifier on the
/// spells or abilities, so the bypass applies to ANY targeting player: it is
/// keyed solely on the would-be target matching the static's `affected` filter
/// (evaluated from the static's own source), independent of the targeting
/// source's controller — hexproof (CR 702.11b) only ever blocks opponents, so
/// removing it for the matched permanents opens them to every player.
///
/// CR 604.1 + CR 613.1: mirrors [`player_ignores_hexproof`] — uses
/// `game_functioning_statics` (so a source whose abilities are suppressed, or a
/// phased-out / non-functioning source, grants nothing) and gates each static
/// through `static_condition_matches_context` with `target_id: Some(target_id)`
/// so an "as long as ..." condition is honored, and a condition that references
/// the would-be target (the recipient) is evaluated against that target rather
/// than skipped. Object-scoped (`affected = Some`) only; the player-scoped
/// Detection Tower form (`affected = None`) is handled by
/// [`player_ignores_hexproof`].
pub fn target_ignores_hexproof(state: &GameState, target_id: ObjectId) -> bool {
    game_functioning_statics(state).any(|(source_obj, def)| {
        matches!(def.mode, StaticMode::IgnoreHexproof)
            && def.affected.as_ref().is_some_and(|filter| {
                matches_target_filter(
                    state,
                    target_id,
                    filter,
                    &FilterContext::from_source(state, source_obj.id),
                )
            })
            && static_condition_matches_context(
                state,
                source_obj.id,
                source_obj.controller,
                def,
                &StaticCheckContext {
                    target_id: Some(target_id),
                    ..Default::default()
                },
            )
    })
}

/// CR 118.3 + CR 119.4b + CR 601.2h + CR 602.2b: Check whether a static
/// ability prohibits `player_id` from paying life as a cost.
///
/// This is cost-scoped and deliberately separate from `CantLoseLife`, which
/// also prevents damage/life-loss events. Paying 0 life remains legal under
/// CR 119.4b and is handled by callers before consulting this predicate.
pub fn player_cant_pay_life_as_cost(state: &GameState, player_id: PlayerId) -> bool {
    battlefield_active_statics(state).any(|(source_obj, def)| {
        matches!(
            &def.mode,
            StaticMode::CantPayCost {
                who,
                cost: CostPaymentProhibition::PayLife,
            } if prohibition_scope_matches_player(who, player_id, source_obj.id, state)
        )
    })
}

/// CR 118.3 + CR 601.2h + CR 602.2b: Check whether a static ability prohibits
/// `player_id` from sacrificing `object_id` as a cost.
///
/// The object filter is evaluated per candidate permanent so broad costs like
/// "sacrifice a permanent" can still be paid with legal objects outside the
/// prohibited filter (for Yasharn, lands remain legal).
pub fn player_cant_sacrifice_as_cost(
    state: &GameState,
    player_id: PlayerId,
    object_id: ObjectId,
) -> bool {
    battlefield_active_statics(state).any(|(source_obj, def)| {
        let StaticMode::CantPayCost {
            who,
            cost: CostPaymentProhibition::Sacrifice { filter },
        } = &def.mode
        else {
            return false;
        };
        if !prohibition_scope_matches_player(who, player_id, source_obj.id, state) {
            return false;
        }
        matches_target_filter(
            state,
            object_id,
            filter,
            &FilterContext::from_source(state, source_obj.id),
        )
    })
}

/// CR 702.16j: Check if a player has active "protection from everything".
///
/// Scans `state.transient_continuous_effects` for effects whose `affected`
/// filter pins this specific player and whose modifications include an
/// `AddKeyword { Protection(ProtectionTarget::Everything) }`. Respects the
/// optional `condition` on each transient.
///
/// This is an internal sub-query of `player_protection_from` — the new single
/// authority for player-scoped protection enforcement. This function scans only
/// the `transient_continuous_effects` table for the `Everything` arm; the
/// battlefield-static `PlayerProtection` arm is handled by `player_protection_from`.
///
/// Note: protection-from-everything uses the transient-effect table rather than
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

/// CR 702.16: Single authority for player-scoped protection enforcement.
///
/// Returns `true` if `player_id` has protection from `source` (identified by
/// `source` ObjectId). Consulted by targeting (CR 702.16b) and damage
/// prevention (CR 702.16e + CR 615.1).
///
/// Short-circuits on `player_has_protection_from_everything` (the transient-
/// effect `Everything` authority, CR 702.16j), then scans battlefield/command-
/// zone `PlayerProtection` statics — e.g. Serra's Emissary's "You ... have
/// protection from the chosen card type." `source` is `None` for queries with
/// no concrete source object; only the `Everything` short-circuit can fire then.
pub fn player_protection_from(
    state: &GameState,
    player_id: PlayerId,
    source: Option<ObjectId>,
) -> bool {
    use crate::game::keywords::source_matches_card_type;
    use crate::types::ability::ControllerRef;
    use crate::types::keywords::ProtectionTarget;

    // CR 702.16j: protection from everything covers every source.
    if player_has_protection_from_everything(state, player_id) {
        return true;
    }
    let Some(source_id) = source else {
        return false;
    };
    let context = StaticCheckContext {
        player_id: Some(player_id),
        ..Default::default()
    };
    // CR 114.4: Abilities of emblems function in the command zone.
    for (src_obj, def) in game_functioning_statics(state) {
        let StaticMode::PlayerProtection(ref target) = def.mode else {
            continue;
        };
        if let Some(ref affected) = def.affected {
            if !static_filter_matches(state, &context, affected, src_obj.id) {
                continue;
            }
        }
        if !static_condition_matches_context(state, src_obj.id, src_obj.controller, def, &context) {
            continue;
        }
        let protects = match target {
            // CR 702.16j: handled by the short-circuit above.
            ProtectionTarget::Everything => false,
            // CR 702.16 + CR 205.2: protection from the card type
            // chosen as the granting permanent (e.g. Serra's Emissary) entered.
            ProtectionTarget::ChosenCardType => state.objects.get(&source_id).is_some_and(|src| {
                src_obj
                    .chosen_card_type()
                    .and_then(|ct| ct.protection_quality_str())
                    .is_some_and(|quality| source_matches_card_type(src, quality))
            }),
            // CR 702.16k: "Protection from [a player]" at the player level — the
            // protected player has protection from each object the specified
            // player(s) control. "Each of your opponents" (CR 702.16i) → the
            // `Opponent` scope: any source NOT controlled by the protected
            // player is an opponent's object in 1v1 and free-for-all. Mirrors the
            // object-level arm in `game/keywords.rs::source_matches_protection_target`.
            ProtectionTarget::FromPlayer(scope) => {
                state
                    .objects
                    .get(&source_id)
                    .is_some_and(|src| match scope {
                        ControllerRef::Opponent => src.controller != player_id,
                        ControllerRef::You => src.controller == player_id,
                        // Target/chosen player refs have no static context here —
                        // fail closed (the parser never emits them for protection).
                        _ => false,
                    })
            }
            // Truly inert at the player level — no card grants these qualities to
            // a player; object-level grants of these qualities flow through the
            // `AddKeyword(Protection)` continuous path, not `PlayerProtection`.
            ProtectionTarget::ChosenColor
            | ProtectionTarget::Color(_)
            | ProtectionTarget::Multicolored
            | ProtectionTarget::Quality(_)
            | ProtectionTarget::CardType(_)
            | ProtectionTarget::Filter(_) => false,
        };
        if protects {
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
/// Also scans `state.transient_continuous_effects` for player-scoped
/// transients whose modifications include `AddStaticMode { mode: Other(name) }`
/// — this is the spell/activated-ability-applied form of the same prohibition
/// (Pardic Miner's "Target player can't play lands this turn" registers a
/// `SpecificPlayer { id }`-bound TCE; the static form scanned above lives on
/// a battlefield permanent's `static_definitions`). Mirrors the dual scan
/// pattern in `player_has_protection_from_everything` (CR 702.16j) — both
/// scopes must be consulted because the source object of a sacrifice-cost
/// activated ability has left the battlefield by resolution and so cannot
/// be found by `game_functioning_statics`.
///
/// Used for the prohibition-family statics (`CantBeSacrificed`, etc.) where
/// constructing `StaticMode::Other(name.to_string())` on every call would
/// allocate in potentially hot paths (damage resolution, sacrifice loops).
fn check_static_other_by_name(state: &GameState, name: &str, context: &StaticCheckContext) -> bool {
    // CR 114.4: Abilities of emblems function in the command zone.
    // Functioning gate is applied before context-specific condition evaluation.
    for (source_obj, def) in game_functioning_statics(state) {
        match &def.mode {
            StaticMode::Other(s) if s == name => {}
            _ => continue,
        }
        if let Some(ref affected) = def.affected {
            if !static_filter_matches(state, context, affected, source_obj.id) {
                continue;
            }
        }
        if !static_condition_matches_context(
            state,
            source_obj.id,
            source_obj.controller,
            def,
            context,
        ) {
            continue;
        }
        return true;
    }
    transient_grants_other_static_to_context(state, name, context)
}

/// CR 611.1 + CR 611.2c: Scan `state.transient_continuous_effects` for an effect
/// whose `affected` filter pins the context's player or object and whose
/// modifications grant `StaticMode::Other(name)` via `AddStaticMode`.
///
/// This is the spell/ability-applied counterpart to the
/// `game_functioning_statics` scan in `check_static_other_by_name`. A
/// transient continuous effect created by an activated ability (Pardic Miner)
/// or instant survives the source object's zone change (CR 400.7), so it must
/// be queried from the state-level TCE table rather than the per-object
/// `static_definitions`. Mirrors the dual-scan pattern in
/// `player_has_protection_from_everything` (CR 702.16j).
fn transient_grants_other_static_to_context(
    state: &GameState,
    name: &str,
    context: &StaticCheckContext,
) -> bool {
    for tce in &state.transient_continuous_effects {
        // CR 611.2c: The set of objects/players a transient continuous effect
        // affects is determined at registration; here we just confirm the bound
        // filter pins the context. The typical shape is a `SpecificObject` /
        // `SpecificPlayer` registration (player-scoped registration via
        // `register_transient_effect` fans `TargetFilter::Player` broadcasts
        // out into one per-player `SpecificPlayer` TCE), but the broadcast
        // `Player` variant is also matched defensively here so any call site
        // that registers a raw all-players TCE without fan-out (e.g. future
        // "Players can't play lands this turn" instants) is still observable
        // to player-scoped runtime queries.
        let pins_context = match (&tce.affected, context.target_id, context.player_id) {
            (TargetFilter::SpecificObject { id }, Some(target), _) => *id == target,
            (TargetFilter::SpecificPlayer { id }, _, Some(player)) => *id == player,
            (TargetFilter::Player, _, Some(_)) => true,
            _ => continue,
        };
        if !pins_context {
            continue;
        }
        // CR 611.2b: ForAsLongAs durations re-evaluate their condition each cycle.
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        let grants_named_other = tce.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::Other(s),
                } if s == name
            )
        });
        if grants_named_other {
            return true;
        }
    }
    false
}

fn static_condition_matches_context(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    def: &crate::types::ability::StaticDefinition,
    context: &StaticCheckContext,
) -> bool {
    def.condition.as_ref().is_none_or(|condition| {
        if let Some(recipient_id) = context.target_id {
            evaluate_condition_with_recipient(state, condition, controller, source_id, recipient_id)
        } else {
            evaluate_condition(state, condition, controller, source_id)
        }
    })
}

/// CR 702.122d: Returns true when the creature has an active "can't crew Vehicles" static.
pub fn object_has_cant_crew(state: &GameState, object_id: ObjectId) -> bool {
    state.objects.get(&object_id).is_some_and(|obj| {
        super::functioning_abilities::active_static_definitions(state, obj)
            .any(|def| def.mode == StaticMode::CantCrew)
    })
}

/// CR 702.122a / 702.171a / 702.184c: The power a creature contributes toward a
/// crew / saddle / station cost, after applying any active `CrewContribution`
/// static whose action list contains `action`. "Using its toughness rather than
/// its power" substitutes the creature's toughness for its base power; "as
/// though its power were N greater" adds N. Multiple deltas accumulate. The
/// result is clamped to 0, matching the plain `power.unwrap_or(0).max(0)` it
/// replaces.
pub fn object_crew_power_contribution(
    state: &GameState,
    object_id: ObjectId,
    action: CrewAction,
) -> i32 {
    let Some(obj) = state.objects.get(&object_id) else {
        return 0;
    };
    let mut base = obj.power.unwrap_or(0);
    let mut delta = 0;
    for def in super::functioning_abilities::active_static_definitions(state, obj) {
        if let StaticMode::CrewContribution { kind, actions } = &def.mode {
            if !actions.contains(&action) {
                continue;
            }
            match kind {
                CrewContributionKind::ToughnessInsteadOfPower => {
                    base = obj.toughness.unwrap_or(0);
                }
                CrewContributionKind::PowerDelta { delta: d } => delta += *d,
            }
        }
    }
    (base + delta).max(0)
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
            // CR 303.4e + CR 702.5d: Player Auras scope player-targeted static
            // checks (e.g. Grievous Wound's "enchanted player can't gain life")
            // to the attached player only.
            TargetFilter::AttachedTo => {
                return state.objects.get(&source_id).is_some_and(|source| {
                    match source.attached_to {
                        Some(crate::game::game_object::AttachTarget::Player(pid)) => {
                            pid == player_id
                        }
                        Some(crate::game::game_object::AttachTarget::Object(target_id)) => state
                            .objects
                            .get(&target_id)
                            .is_some_and(|enchanted| enchanted.controller == player_id),
                        None => false,
                    }
                });
            }
            TargetFilter::Controller => return source_controller == Some(player_id),
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
                        crate::types::ability::ControllerRef::ScopedPlayer => false,
                        crate::types::ability::ControllerRef::TargetPlayer => false,
                        crate::types::ability::ControllerRef::ParentTargetController => false,
                        crate::types::ability::ControllerRef::ParentTargetOwner => false,
                        crate::types::ability::ControllerRef::DefendingPlayer => false,
                        // CR 613.1: chosen-player scope has no static context here.
                        crate::types::ability::ControllerRef::SourceChosenPlayer => false,
                        // CR 109.4: Chosen-player scope has no static context.
                        crate::types::ability::ControllerRef::ChosenPlayer { .. } => false,
                        // CR 603.2 + CR 109.4: Triggering-player scope has no
                        // static context. Fail closed.
                        crate::types::ability::ControllerRef::TriggeringPlayer => false,
                        // CR 303.4b: Enchanted-player scope has no static context. Fail closed.
                        crate::types::ability::ControllerRef::EnchantedPlayer => false,
                    };
                }
                return true;
            }
            // CR 119.7 + CR 109.1: an object-scoped restriction is never a
            // player restriction. A transient `CantGainLife` grant bound to a
            // specific object — e.g. Screaming Nemesis redirecting its damage to
            // a CREATURE, which pins the rider's `ParentTarget` to
            // `SpecificObject { id }` — must NOT satisfy a player-scoped query
            // ("can this player gain life?"). Fail CLOSED for object-pin filters
            // so the redirect-to-creature case locks no player, while the
            // redirect-to-player case (bound `SpecificPlayer`) is handled by the
            // transient player-scope scan. Without this arm the catch-all below
            // fails open and locks every player whenever any creature carries a
            // granted `CantGainLife`.
            TargetFilter::SpecificObject { .. } | TargetFilter::SelfRef => return false,
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

    // CR 305.2 + CR 611.2c: A turn-scoped grant (Escape to the Wilds: "you may
    // play an additional land this turn") is a transient continuous effect, not
    // a battlefield static, so it is invisible to `battlefield_active_statics`.
    // Sum it from the TCE table here.
    total = total.saturating_add(transient_additional_land_drops(state, player));

    total
}

/// CR 305.2 + CR 611.2c: Sum the additional land drops a player is granted by
/// transient continuous effects (e.g. Escape to the Wilds' "play an additional
/// land this turn"). The typed-summing twin of
/// `transient_grants_other_static_to_context`: it mirrors that helper's
/// player-pin and duration/condition gates but accumulates the land-drop count
/// from each `AddStaticMode` modification rather than testing a named bool.
fn transient_additional_land_drops(state: &GameState, player: PlayerId) -> u8 {
    let mut total: u8 = 0;
    for tce in &state.transient_continuous_effects {
        // CR 611.2c: player-scoped registration fans `TargetFilter::Player`
        // broadcasts into per-player `SpecificPlayer` TCEs; the bare `Player`
        // variant is matched defensively for any raw all-players registration.
        let pins_player = match &tce.affected {
            TargetFilter::SpecificPlayer { id } => *id == player,
            TargetFilter::Player => true,
            _ => continue,
        };
        if !pins_player {
            continue;
        }
        // CR 611.2b: ForAsLongAs durations re-evaluate their condition each cycle.
        if let Duration::ForAsLongAs { ref condition } = tce.duration {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(ref condition) = tce.condition {
            if !evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        for m in &tce.modifications {
            if let ContinuousModification::AddStaticMode { mode } = m {
                total = total.saturating_add(match mode {
                    StaticMode::MayPlayAdditionalLand => 1,
                    StaticMode::AdditionalLandDrop { count } => *count,
                    _ => 0,
                });
            }
        }
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

    /// Issue #2879 + CR 305.2 + CR 611.2c: a turn-scoped transient grant (Escape
    /// to the Wilds: "you may play an additional land this turn") must be summed
    /// into `additional_land_drops` for the affected player only.
    #[test]
    fn transient_additional_land_drops_counted() {
        use crate::types::ability::{ContinuousModification, Duration};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Escape to the Wilds".to_string(),
            Zone::Battlefield,
        );

        // Baseline: no extra land drops.
        assert_eq!(additional_land_drops(&state, PlayerId(0)), 0);

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MayPlayAdditionalLand,
            }],
            None,
        );

        assert_eq!(
            additional_land_drops(&state, PlayerId(0)),
            1,
            "PlayerId(0) must get the transient extra land drop"
        );
        assert_eq!(
            additional_land_drops(&state, PlayerId(1)),
            0,
            "PlayerId(1) must not get it — per-player scoping"
        );
    }

    /// Issue #2879 (count >= 2 branch): a transient `AdditionalLandDrop { count }`
    /// sums its full count into `additional_land_drops`.
    #[test]
    fn transient_additional_land_drops_counts_multiple() {
        use crate::types::ability::{ContinuousModification, Duration};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Multi-land Grant".to_string(),
            Zone::Battlefield,
        );

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::AdditionalLandDrop { count: 2 },
            }],
            None,
        );

        assert_eq!(
            additional_land_drops(&state, PlayerId(0)),
            2,
            "AdditionalLandDrop count 2 must sum to 2"
        );
        assert_eq!(additional_land_drops(&state, PlayerId(1)), 0);
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

    /// CR 611.1 + CR 611.2c: A raw player-broadcast transient that grants a
    /// named player-scoped static is visible to each player query. Most current
    /// registration paths fan broadcasts out to `SpecificPlayer`, but the query
    /// accepts `TargetFilter::Player` so all-player one-shot restrictions remain
    /// observable if registered directly.
    #[test]
    fn player_static_other_query_matches_broadcast_transient() {
        use crate::types::ability::{ContinuousModification, Duration};

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Broadcast Source".to_string(),
            Zone::Battlefield,
        );

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::Player,
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Other("CantPlayLand".to_string()),
            }],
            None,
        );

        assert!(player_has_static_other(&state, PlayerId(0), "CantPlayLand"));
        assert!(player_has_static_other(&state, PlayerId(1), "CantPlayLand"));
        assert!(!player_has_static_other(&state, PlayerId(0), "CantShuffle"));
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

    /// CR 702.16k + CR 702.16i: A `PlayerProtection(FromPlayer(Opponent))` static
    /// (Absolute Virtue's "You have protection from each of your opponents.")
    /// makes its controller protected from every opponent-controlled source and
    /// NOT from its own sources. Exercises the runtime `FromPlayer` arm — the
    /// building block, not the card name.
    #[test]
    fn player_protection_from_opponent_grants_against_opponent_sources() {
        let mut state = setup();

        // The granting permanent, controlled by PlayerId(0), carries the static.
        let grantor = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Absolute Virtue".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&grantor)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::PlayerProtection(
                    crate::types::keywords::ProtectionTarget::FromPlayer(ControllerRef::Opponent),
                ))
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let opponent_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent's Bolt Source".to_string(),
            Zone::Battlefield,
        );
        let own_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "My Own Source".to_string(),
            Zone::Battlefield,
        );

        assert!(
            player_protection_from(&state, PlayerId(0), Some(opponent_source)),
            "controller must have protection from an opponent-controlled source"
        );
        assert!(
            !player_protection_from(&state, PlayerId(0), Some(own_source)),
            "controller must NOT have protection from its own source"
        );
        assert!(
            !player_protection_from(&state, PlayerId(1), Some(own_source)),
            "the opponent gains no protection — affected is the controller only"
        );
    }

    #[test]
    fn triggered_sacrifice_or_exile_muzzle_blocks_creature_tokens() {
        use crate::types::ability::{Effect, FilterProp, ResolvedAbility, TypedFilter};
        use crate::types::game_state::{StackEntry, StackEntryKind};
        use crate::types::identifiers::ObjectId;
        use crate::types::player::PlayerId;
        use crate::types::statics::ProhibitionScope;

        let mut state = setup();
        let master = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Master, Multiplied".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&master)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantCauseSacrificeOrExile {
                    cause: ProhibitionScope::Controller,
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::creature()
                        .properties(vec![FilterProp::Token])
                        .controller(ControllerRef::You),
                )),
            );

        let token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Myriad Copy".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&token).unwrap();
            obj.is_token = true;
            obj.card_types.core_types.push(CoreType::Creature);
        }

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
                face_down_profile: None,
            },
            vec![crate::types::ability::TargetRef::Object(token)],
            ObjectId(99),
            PlayerId(0),
        );

        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(1000),
            controller: PlayerId(0),
            source_id: ObjectId(99),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(99),
                ability: Box::new(ability.clone()),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });

        assert!(triggered_cause_sacrifice_or_exile_muzzled(
            &state,
            &ability,
            token,
            PlayerId(0),
        ));

        state.resolving_stack_entry = None;
        assert!(!triggered_cause_sacrifice_or_exile_muzzled(
            &state,
            &ability,
            token,
            PlayerId(0),
        ));
    }
}
