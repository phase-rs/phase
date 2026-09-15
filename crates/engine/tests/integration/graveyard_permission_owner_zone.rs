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
use engine::types::keywords::{BestowCost, Keyword, KeywordKind};
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

/// THE row for the fourth call site,
/// `has_graveyard_cast_permission_without_keyword_constraint`.
///
/// That consumer short-circuits on its FIRST conjunct:
/// `!filter_has_keyword_kind_constraint(source.filter, kind)`. So a permission
/// carrying a `HasKeywordKind { Bestow }` rider can never witness the changed
/// matcher on the third conjunct -- the fixture must use an UNCONSTRAINED
/// permission (the plain Muldrotha shape, "you may cast creature cards from
/// your graveyard"), which is what this row installs.
///
/// The assertion is the consumer's own verdict, queried directly, so the row
/// cannot pass on an unrelated failure the way a bare `is_err()` can: it is a
/// POSITIVE assertion that the consumer answers `true` for a card whose LKI
/// controller is the thief. Pre-fix it answers `false` -- the stale LKI
/// controller fails the `controller: You` axis on the third conjunct, which is
/// exactly the matcher this PR changes.
#[test]
fn the_bestow_keyword_constraint_consumer_resolves_against_the_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_creature_to_hand(PlayerId(0), "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::generic(1));
    let bestowed = builder.id();

    // A legal Aura host, so nothing below can be explained by an absent target.
    let host = scenario
        .add_creature(PlayerId(0), "Grizzly Bears", 2, 2)
        .id();

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
        // CR 702.103b: bestow cards are Enchantment Creatures, so the bestow
        // form removes only Creature and leaves Enchantment for the Aura.
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
    for _ in 0..2 {
        add_mana(&mut runner, PlayerId(0), ManaType::Colorless);
    }

    assert_lki_diverges(runner.state(), bestowed, PlayerId(0), PlayerId(1));

    // CR 109.4 + CR 108.4a: the consumer's own verdict. This is the third
    // conjunct's matcher, reached only because the permission is unconstrained.
    // Pre-fix: false.
    assert!(
        engine::game::casting::has_graveyard_cast_permission_without_keyword_constraint_for_test(
            runner.state(),
            PlayerId(0),
            bestowed,
            KeywordKind::Bestow,
        ),
        "CR 109.4 + CR 108.4a: the bestow keyword-constraint consumer must resolve an \
         unconstrained graveyard permission against the card's OWNER, even though an \
         opponent controlled it when it died"
    );

    // GUARD: the same consumer must not answer `true` for a card owned by
    // another player -- the fix substitutes the owner axis, it does not drop it.
    let opponent_card = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(5157),
        PlayerId(1),
        "Opponent Creature".to_string(),
        Zone::Graveyard,
    );
    assert!(
        !engine::game::casting::has_graveyard_cast_permission_without_keyword_constraint_for_test(
            runner.state(),
            PlayerId(0),
            opponent_card,
            KeywordKind::Bestow,
        ),
        "CR 108.4a: the owner substitution must not widen the bestow consumer to a card \
         owned by another player"
    );

    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "staging: the prospective Aura host must still be on the battlefield"
    );
}

/// The bestow CAST from a graveyard is blocked by a defect unrelated to owner
/// scoping; this row pins that blocker so it cannot regress silently and so the
/// row above is not mistaken for cast-path coverage.
///
/// `handle_bestow_cost_choice_with_payment_mode` calls `apply_bestow_aura_form`
/// -- which per CR 702.103b strips the Creature core type -- BEFORE calling
/// `prepare_spell_cast_with_variant_override`. That re-evaluates the graveyard
/// permission, whose filter is `creature cards`, against a card that is no
/// longer a creature, so the cast is refused with "Card is not in a castable
/// zone".
///
/// CR 702.103b puts the form change "as a spell cast bestowed is put onto the
/// stack" -- i.e. at CR 601.2a, AFTER the permission has authorized the cast --
/// so re-deriving the permission from the post-change types is the defect.
///
/// Verified independent of this PR: the diff touches neither
/// `apply_bestow_aura_form` nor any type filter, and the same refusal
/// reproduces with an unconstrained permission whichever controller the card
/// died under. Tracked separately rather than fixed here.
#[test]
fn bestow_cast_from_graveyard_is_blocked_by_the_aura_form_type_seam() {
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

    // The SPECIFIC blocker, not a bare `is_err()`.
    let message = match &result {
        Err(err) => format!("{err:?}"),
        Ok(_) => String::new(),
    };
    assert!(
        message.contains("Card is not in a castable zone"),
        "the bestow/graveyard blocker must remain exactly this refusal -- if this row starts \
         failing, the aura-form type seam was fixed and the consumer above should be \
         promoted to a full cast-path regression, got {result:?}"
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
