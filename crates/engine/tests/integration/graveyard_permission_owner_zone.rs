//! CR 109.4 + CR 108.4a + CR 109.5: "your graveyard" is owner-scoped.
//!
//! **The rule.** CR 109.4: "Only objects on the stack or on the battlefield have
//! a controller. Objects that are neither on the stack nor on the battlefield
//! aren't controlled by any player." None of its six exceptions covers a card in
//! a graveyard. CR 108.4a then says anything asking for such a card's controller
//! "use its owner instead", and CR 109.5 spells out the same for the word itself:
//! "you"/"your" refer to "its owner (if it has no controller)".
//!
//! So a `your graveyard` permission filter must resolve against the card's
//! OWNER. The engine already knows this — `zones.rs` cites exactly these rules
//! when it resets a departing permanent's `controller` to the owner fallback, and
//! `filter::is_owner_scoped_zone` is `Hand | Library | Graveyard`. The four
//! graveyard-permission consumers in `game::casting` were simply never routed
//! through `filter::matches_target_filter_for_zone`, the documented single
//! authority for that substitution.
//!
//! **What actually diverged.** Not the live `controller` field: `zones.rs`'s
//! `reset_for_battlefield_exit` forces `base_controller = Some(owner)` and the
//! destination-keyed reset then writes the owner back, so a graveyard card's live
//! controller is already correct. The divergence is the **LKI cache**.
//! `filter::effective_controller` reads `state.lki_cache[id].controller` for any
//! object off the battlefield/stack under `ControllerLookup::LiveOrLki`, and the
//! LKI snapshot is taken BEFORE that reset — so for a permanent that died under
//! an opponent's control it holds the THIEF.
//!
//! `matches_target_filter_in_owner_zone` passes `LiveOnly` instead, which is what
//! cures it.
//!
//! **Blast radius: the printed graveyard-permission class**, which is why this
//! suite deliberately uses a printed Ramunap-Excavator-shaped source rather than
//! any one card's parsed text. Muldrotha, Karador, Lurrus and Ramunap Excavator
//! all silently refused a card their controller owned, if an opponent happened to
//! control it when it died, for as long as the stale LKI entry survived —
//! `turns.rs` clears the LKI cache on step transition, so exposure ends at the
//! next step boundary. That cache lifetime is an engine implementation detail,
//! not a Comprehensive Rules requirement.
//!
//! The failure direction is **mis-exclusion only**: the owner's own card is
//! refused. No card is wrongly admitted by the pre-fix behaviour, so nothing
//! depended on it.

use engine::game::casting::{
    graveyard_lands_playable_by_permission, spell_objects_available_to_cast,
};
use engine::game::scenario::GameScenario;
use engine::types::ability::{CardPlayMode, ControllerRef, TargetFilter, TypeFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::CastPaymentMode;
use engine::types::keywords::{BestowCost, Keyword};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;
use engine::types::{CardId, ObjectId, StaticDefinition};

/// A printed "you may play/cast <filter> from your graveyard" permission, of the
/// shape Ramunap Excavator / Muldrotha / Karador print.
fn printed_graveyard_permission(
    play_mode: CardPlayMode,
    types: Vec<TypeFilter>,
) -> StaticDefinition {
    StaticDefinition::new(StaticMode::GraveyardCastPermission {
        frequency: CastFrequency::Unlimited,
        play_mode,
        graveyard_destination_replacement: None,
        extra_cost: None,
        enters_with_counter: None,
        required_cast_keyword: None,
    })
    .affected(TargetFilter::Typed(TypedFilter {
        type_filters: types,
        // THE AXIS UNDER TEST. "your graveyard" — which for a card with no
        // controller (CR 109.4) must resolve to its OWNER.
        controller: Some(ControllerRef::You),
        ..Default::default()
    }))
}

/// Stage `card_id` as a permanent OWNED by `owner`, gain control of it with
/// `thief`, then let it die into its owner's graveyard.
///
/// Goes through the production zone-change path (`zones::move_to_zone`) rather
/// than hand-setting fields, so the resulting state is one the engine actually
/// produces: live `controller` reset to the owner, LKI holding the thief.
fn steal_then_bury(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: ObjectId,
    thief: PlayerId,
) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        object_id,
        Zone::Battlefield,
        &mut events,
    );
    {
        // CR 613.1b layer 2: an opponent gains control of the permanent.
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&object_id)
            .expect("staged object");
        obj.base_controller = Some(thief);
        obj.controller = thief;
    }
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), object_id, Zone::Graveyard, &mut events);
}

/// Assert the fixture really is in the divergent state this suite exists to
/// cover, BEFORE asserting anything about permissions.
///
/// Without this the rows would pass vacuously the moment anything advanced a
/// step (`turns.rs` clears the LKI cache on step transition) or the control
/// change failed to take — and the assertion under test would be measuring
/// nothing.
fn assert_lki_diverges(
    state: &engine::types::game_state::GameState,
    object_id: ObjectId,
    owner: PlayerId,
    thief: PlayerId,
) {
    let obj = &state.objects[&object_id];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "staging: the card must be in a graveyard"
    );
    assert_eq!(obj.owner, owner, "staging: owner");
    assert_eq!(
        obj.controller, owner,
        "staging: CR 109.4 — zones.rs resets the LIVE controller to the owner on exit, so the \
         live field is NOT the divergence this suite covers"
    );
    assert_eq!(
        state.lki_cache.get(&object_id).map(|lki| lki.controller),
        Some(thief),
        "staging: the LKI snapshot must still hold the THIEF — that is the divergence under test"
    );
}

/// THE discriminating row for the land path. Pre-fix this returned `[]`.
#[test]
fn a_land_that_died_under_an_opponents_control_is_still_its_owners_to_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_to_hand(PlayerId(0), "Forest").id();
    let mut runner = scenario.build();

    steal_then_bury(&mut runner, land, PlayerId(1));

    let source = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(5150),
        PlayerId(0),
        "Ramunap Excavator".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("permission source")
        .static_definitions
        .push(printed_graveyard_permission(
            CardPlayMode::Play,
            vec![TypeFilter::Land],
        ));

    assert_lki_diverges(runner.state(), land, PlayerId(0), PlayerId(1));

    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));
    assert!(
        playable.iter().any(|(object_id, _)| *object_id == land),
        "CR 109.4 + CR 108.4a: a card in a graveyard has no controller, so \"your \
         graveyard\" resolves to its OWNER — the land its owner is querying for must be \
         offered even though an opponent controlled it when it died, got {playable:?}"
    );
}

/// The CAST sibling of the row above, through the public legal-cast surface.
///
/// The land and spell halves are served by DIFFERENT consumers
/// (`graveyard_lands_playable_by_permission` vs
/// `graveyard_object_castable_by_permission_sources`), so one row cannot cover
/// both — a fix applied to only one would leave them disagreeing.
#[test]
fn a_spell_that_died_under_an_opponents_control_is_still_its_owners_to_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand(PlayerId(0), "Gravedigger", 2, 2)
        .id();
    let mut runner = scenario.build();

    steal_then_bury(&mut runner, creature, PlayerId(1));

    let source = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(5151),
        PlayerId(0),
        "Muldrotha, the Gravetide".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("permission source")
        .static_definitions
        .push(printed_graveyard_permission(
            CardPlayMode::Cast,
            vec![TypeFilter::Creature],
        ));

    assert_lki_diverges(runner.state(), creature, PlayerId(0), PlayerId(1));

    let castable = spell_objects_available_to_cast(runner.state(), PlayerId(0));
    assert!(
        castable.contains(&creature),
        "CR 109.4 + CR 108.4a: the owner's own creature card must be offered from their \
         graveyard even though an opponent controlled it when it died, got {castable:?}"
    );
}

/// GUARD: the substitution must not widen the permission to cards the querying
/// player does not own.
///
/// The fix replaces a controller comparison with an owner comparison — it must
/// not degrade into "match anything in any graveyard". This row fails if the
/// owner axis is dropped rather than substituted.
#[test]
fn the_owner_substitution_does_not_widen_to_another_players_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own_land = scenario.add_land_to_hand(PlayerId(0), "Forest").id();
    let opponent_land = scenario.add_land_to_hand(PlayerId(1), "Island").id();
    let mut runner = scenario.build();

    // Both die into their OWN graveyards; only P0's was ever stolen.
    steal_then_bury(&mut runner, own_land, PlayerId(1));
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        opponent_land,
        Zone::Graveyard,
        &mut events,
    );

    let source = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(5152),
        PlayerId(0),
        "Ramunap Excavator".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("permission source")
        .static_definitions
        .push(printed_graveyard_permission(
            CardPlayMode::Play,
            vec![TypeFilter::Land],
        ));

    let playable = graveyard_lands_playable_by_permission(runner.state(), PlayerId(0));

    // REACH-GUARD: the permission is live, so the negative below cannot pass
    // because nothing was granted at all.
    assert!(
        playable.iter().any(|(object_id, _)| *object_id == own_land),
        "reach-guard: the owner's own land must be offered, got {playable:?}"
    );
    assert!(
        !playable
            .iter()
            .any(|(object_id, _)| *object_id == opponent_land),
        "CR 108.4a: the owner substitution must not widen the permission to a card owned \
         by another player, got {playable:?}"
    );
}

// ---------------------------------------------------------------------------
// PRODUCTION-FLOW ROWS
//
// The three rows above query the enumeration helpers directly. That is the
// right altitude for proving the OWNER-vs-LKI axis, but it leaves the
// downstream consumers untested: `GameAction::PlayLand` and
// `GameAction::CastSpell` re-derive the permission through
// `graveyard_lands_playable_by_permission` and `graveyard_permission_source`
// respectively, and the bestow route adds a third consumer
// (`has_graveyard_cast_permission_without_keyword_constraint`). A regression
// confined to an elected-authority call site would leave the enumeration rows
// green while the action itself was refused.
//
// These rows therefore submit the real actions and assert they COMPLETE.
// ---------------------------------------------------------------------------

/// Give `player` one unit of `ty` for deterministic payment.
fn add_mana(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, ty: ManaType) {
    let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
    runner.state_mut().players[player.0 as usize]
        .mana_pool
        .add(unit);
}

/// Attach a printed graveyard permission to a fresh battlefield source owned by
/// `controller`, and return the source's object id.
fn stage_permission_source(
    runner: &mut engine::game::scenario::GameRunner,
    controller: PlayerId,
    card_id: CardId,
    name: &str,
    play_mode: CardPlayMode,
    types: Vec<TypeFilter>,
) -> ObjectId {
    let source = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("permission source")
        .static_definitions
        .push(printed_graveyard_permission(play_mode, types));
    source
}

/// PRODUCTION FLOW -- the land path, through `GameAction::PlayLand`.
///
/// `handle_play_land` re-derives the permission via
/// `graveyard_lands_playable_by_permission`; a stale-LKI refusal there surfaces
/// as a rejected action, not merely an empty offer list.
#[test]
fn playing_a_stolen_then_buried_land_from_its_owners_graveyard_is_accepted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_to_hand(PlayerId(0), "Forest").id();
    let mut runner = scenario.build();

    steal_then_bury(&mut runner, land, PlayerId(1));
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5153),
        "Ramunap Excavator",
        CardPlayMode::Play,
        vec![TypeFilter::Land],
    );

    assert_lki_diverges(runner.state(), land, PlayerId(0), PlayerId(1));

    let card_id = runner.state().objects[&land].card_id;
    // CR 305.1 + CR 108.4a: the land's owner plays their own card from their own
    // graveyard. The at-death controller is not a party to this permission.
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect(
            "CR 109.4 + CR 108.4a: playing a land from its OWNER's graveyard must be accepted \
             even though an opponent controlled it when it died",
        );

    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "the played land must actually reach the battlefield"
    );
}

/// PRODUCTION FLOW -- the normal cast path, through `GameAction::CastSpell`.
///
/// The graveyard arm of the cast-legality gate calls
/// `graveyard_permission_source` -- the ELECTED authority, a different consumer
/// from the enumeration helper the sibling row covers.
#[test]
fn casting_a_stolen_then_buried_creature_from_its_owners_graveyard_is_accepted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_creature_to_hand(PlayerId(0), "Gravedigger", 2, 2);
    builder.with_mana_cost(ManaCost::generic(1));
    let creature = builder.id();
    let mut runner = scenario.build();

    steal_then_bury(&mut runner, creature, PlayerId(1));
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5154),
        "Muldrotha, the Gravetide",
        CardPlayMode::Cast,
        vec![TypeFilter::Creature],
    );
    add_mana(&mut runner, PlayerId(0), ManaType::Colorless);

    assert_lki_diverges(runner.state(), creature, PlayerId(0), PlayerId(1));

    let card_id = runner.state().objects[&creature].card_id;
    // CR 601.2a + CR 108.4a: the graveyard permission is elected for the card's
    // OWNER, so the cast is legal.
    runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect(
            "CR 109.4 + CR 108.4a: casting a creature from its OWNER's graveyard must be \
             accepted even though an opponent controlled it when it died",
        );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the cast creature must resolve onto the battlefield"
    );
}

/// PRODUCTION FLOW -- the fourth call site,
/// `has_graveyard_cast_permission_without_keyword_constraint`, through
/// `GameAction::CastSpell`.
///
/// A stolen-then-buried Bestow creature under an UNCONSTRAINED creature-cast
/// permission with NO legal Aura host on the battlefield: the bestow lane
/// admits the graveyard (`graveyard_permission_source` is `Some`), finds the
/// Bestow keyword, then skips the Aura-form route for want of a target (CR
/// 702.103a + CR 303.4a: bestow needs a creature to enchant) and queries the
/// keyword-constraint consumer before falling through to the normal
/// graveyard-permission cast. The cast must COMPLETE as a creature.
///
/// The permission must be unconstrained: the consumer short-circuits on its
/// FIRST conjunct (`!filter_has_keyword_kind_constraint`), so a permission
/// carrying a `HasKeywordKind { Bestow }` rider never reaches the changed
/// owner/LKI matcher on the third conjunct.
///
/// Pre-fix this row fails at the changed matcher with "No legal bestow cast
/// from graveyard": the stale LKI controller (the thief) fails the
/// `controller: You` axis. CR 109.4 + CR 108.4a + CR 109.5 resolve "your
/// graveyard" to the card's OWNER.
#[test]
fn casting_a_stolen_then_buried_bestow_creature_with_no_host_completes_the_normal_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_creature_to_hand(PlayerId(0), "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::generic(1));
    let bestowed = builder.id();
    // NOTE: deliberately NO creature on the battlefield -- with no legal Aura
    // host the production path skips the Aura-form route and exercises the
    // keyword-constraint consumer on the fall-through to the normal cast,
    // without entering the separately documented Aura-form type seam below.

    let mut runner = scenario.build();
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bestowed)
            .expect("bestow card");
        // CR 702.103a: bestow is a static ability that functions in any zone
        // from which the card could be played.
        obj.keywords
            .push(Keyword::Bestow(BestowCost::Mana(ManaCost::generic(2))));
        // CR 702.103b: bestow cards are Enchantment Creatures.
        for types in [&mut obj.card_types, &mut obj.base_card_types] {
            if !types.core_types.contains(&CoreType::Enchantment) {
                types.core_types.push(CoreType::Enchantment);
            }
        }
    }

    steal_then_bury(&mut runner, bestowed, PlayerId(1));
    // UNCONSTRAINED, so the consumer's first conjunct passes and execution
    // reaches the owner/LKI matcher.
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5155),
        "Muldrotha, the Gravetide",
        CardPlayMode::Cast,
        vec![TypeFilter::Creature],
    );
    // Sufficient NORMAL mana for the printed {1}.
    add_mana(&mut runner, PlayerId(0), ManaType::Colorless);

    assert_lki_diverges(runner.state(), bestowed, PlayerId(0), PlayerId(1));

    // REACH-GUARD: the permission authorizes this card pre-cast, so a refusal
    // below is the keyword-constraint consumer and not an absent permission.
    let castable = spell_objects_available_to_cast(runner.state(), PlayerId(0));
    assert!(
        castable.contains(&bestowed),
        "reach-guard: the unconstrained permission must offer the card, got {castable:?}"
    );

    let card_id = runner.state().objects[&bestowed].card_id;
    // CR 601.2a + CR 108.4a: with no legal host the bestow option cannot be
    // chosen, so this is the plain graveyard-permission creature cast for the
    // card's OWNER.
    runner
        .act(GameAction::CastSpell {
            object_id: bestowed,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect(
            "CR 109.4 + CR 108.4a: the no-host fall-through must complete the normal cast \
             from the OWNER's graveyard even though an opponent controlled it when it died",
        );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&bestowed].zone,
        Zone::Battlefield,
        "the cast creature must resolve onto the battlefield"
    );
}

/// PRODUCTION-FLOW GUARD: the owner substitution must not let a player cast a
/// card they do not own, through the real action.
///
/// The thief here is the QUERYING player: the LKI snapshot holds P0, so the
/// pre-fix controller-axis matcher ADMITTED this very cast -- the mirror image
/// of the mis-exclusion above. Post-fix the owner axis refuses it at the
/// admission gate. Pre-fix this row fails because the cast completes; post-fix
/// the refusal carries the gate's specific message.
#[test]
fn casting_another_players_stolen_then_buried_bestow_creature_is_still_refused() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_creature_to_hand(PlayerId(1), "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::generic(1));
    let theirs = builder.id();

    let mut runner = scenario.build();
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&theirs)
            .expect("bestow card");
        // CR 702.103a: bestow is a static ability that functions in any zone
        // from which the card could be played.
        obj.keywords
            .push(Keyword::Bestow(BestowCost::Mana(ManaCost::generic(2))));
        // CR 702.103b: bestow cards are Enchantment Creatures.
        for types in [&mut obj.card_types, &mut obj.base_card_types] {
            if !types.core_types.contains(&CoreType::Enchantment) {
                types.core_types.push(CoreType::Enchantment);
            }
        }
    }

    // P0 steals P1's creature, then it dies into P1's graveyard: the live
    // controller resets to the owner (CR 109.4) while the LKI snapshot still
    // holds P0 -- the querying player.
    steal_then_bury(&mut runner, theirs, PlayerId(0));
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5157),
        "Muldrotha, the Gravetide",
        CardPlayMode::Cast,
        vec![TypeFilter::Creature],
    );
    add_mana(&mut runner, PlayerId(0), ManaType::Colorless);

    // REACH-GUARD: the fixture really is in the divergent state -- LKI holds
    // the querying player, which is exactly what the pre-fix matcher admitted.
    assert_lki_diverges(runner.state(), theirs, PlayerId(1), PlayerId(0));

    let card_id = runner.state().objects[&theirs].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: theirs,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    // The SPECIFIC refusal, not a bare `is_err()`.
    let message = match &result {
        Err(err) => format!("{err:?}"),
        Ok(_) => String::new(),
    };
    assert!(
        message.contains("Card is not in a castable zone"),
        "CR 108.4a: P0 must not cast a card owned by P1 even though the LKI snapshot holds \
         P0 itself -- the refusal must be the admission gate's specific message, got {result:?}"
    );
}

/// CR 702.103d + CR 118.9b: a bestow card in the graveyard under a permission
/// that admits only CREATURE cards can't be cast bestowed through it, because
/// "only its characteristics as modified by the bestow ability are evaluated
/// to determine if it can be cast", and a bestowed spell is an Aura enchantment,
/// not a creature (CR 702.103b). Bestow is optional, though, so the printed
/// creature cast the permission does authorize must still go ahead.
///
/// This row used to pin the whole cast being refused with "Card is not in a
/// castable zone": the graveyard bestow offer force-routed every such cast to
/// bestow, so the bestowed form failed the creature filter and the legal printed
/// cast was never reached. The offer now judges bestow on the bestowed form and
/// falls back to the printed cast.
#[test]
fn bestow_cast_from_graveyard_under_a_creature_permission_falls_back_to_the_printed_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_creature_to_hand(PlayerId(0), "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::generic(1));
    let bestowed = builder.id();
    scenario.add_creature(PlayerId(0), "Grizzly Bears", 2, 2);

    let mut runner = scenario.build();
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bestowed)
            .expect("bestow card");
        obj.keywords
            .push(Keyword::Bestow(BestowCost::Mana(ManaCost::generic(2))));
        for types in [&mut obj.card_types, &mut obj.base_card_types] {
            if !types.core_types.contains(&CoreType::Enchantment) {
                types.core_types.push(CoreType::Enchantment);
            }
        }
    }

    // No control change here: the blocker is independent of the owner axis.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), bestowed, Zone::Graveyard, &mut events);
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5158),
        "Muldrotha, the Gravetide",
        CardPlayMode::Cast,
        vec![TypeFilter::Creature],
    );
    for _ in 0..4 {
        add_mana(&mut runner, PlayerId(0), ManaType::Colorless);
    }

    // REACH-GUARD: the permission DOES authorize this card before the cast, so
    // the refusal below is the bestow seam and not an absent permission.
    let castable = spell_objects_available_to_cast(runner.state(), PlayerId(0));
    assert!(
        castable.contains(&bestowed),
        "reach-guard: the unconstrained permission must offer the card, got {castable:?}"
    );

    let card_id = runner.state().objects[&bestowed].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: bestowed,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });

    assert!(
        result.is_ok(),
        "the printed creature cast is legal under a creature-card permission, so the \
         cast must go ahead, got {result:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the spell must be on the stack"
    );
    let spell = &runner.state().objects[&bestowed];
    assert!(
        spell.card_types.core_types.contains(&CoreType::Creature)
            && !spell.card_types.subtypes.iter().any(|s| s == "Aura"),
        "CR 702.103d: a creature-card permission can't admit the bestowed Aura, so this \
         must be the printed creature cast, types: {:?}",
        spell.card_types
    );
}

/// PRODUCTION-FLOW GUARD: the substitution must not let a player play a card
/// they do not own, through the real action.
///
/// The enumeration guard above proves the offer list excludes it; this proves
/// the action itself is REFUSED, which is the property that actually matters.
#[test]
fn playing_another_players_graveyard_land_is_still_refused() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opponent_land = scenario.add_land_to_hand(PlayerId(1), "Island").id();
    let own_land = scenario.add_land_to_hand(PlayerId(0), "Forest").id();
    let mut runner = scenario.build();

    steal_then_bury(&mut runner, own_land, PlayerId(1));
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        opponent_land,
        Zone::Graveyard,
        &mut events,
    );
    stage_permission_source(
        &mut runner,
        PlayerId(0),
        CardId(5156),
        "Ramunap Excavator",
        CardPlayMode::Play,
        vec![TypeFilter::Land],
    );

    let opponent_card_id = runner.state().objects[&opponent_land].card_id;
    let result = runner.act(GameAction::PlayLand {
        object_id: opponent_land,
        card_id: opponent_card_id,
    });
    assert!(
        result.is_err(),
        "CR 108.4a: the owner substitution must not let P0 play a land owned by P1, got {result:?}"
    );

    // REACH-GUARD: the permission is live on this same state, so the refusal
    // above cannot be passing merely because nothing was granted at all.
    let own_card_id = runner.state().objects[&own_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: own_land,
            card_id: own_card_id,
        })
        .expect("reach-guard: P0's OWN land must still be playable from their graveyard");
}
