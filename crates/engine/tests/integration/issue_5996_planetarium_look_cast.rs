//! Issue #5996: Planetarium of Wan Shi Tong's private look must bind its
//! immediate optional cast to the exact looked-at library card.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{
    CardPlayMode, CastFromZoneDriver, DigSource, Effect, QuantityExpr, ResolvedAbility,
    SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PLANETARIUM_TRIGGER: &str = "Whenever you scry or surveil, look at the top card of your library. You may cast that card without paying its mana cost. Do this only once each turn.";
const LOOK_CAST_WITH_INDEPENDENT_TAIL: &str = "Whenever you scry or surveil, look at the top card of your library. You may cast that card without paying its mana cost. You gain 1 life.";
const BESEECH_THE_MIRROR: &str = "Bargain (You may sacrifice an artifact, enchantment, or token as you cast this spell.)\nSearch your library for a card, exile it face down, then shuffle. If this spell was bargained, you may cast the exiled card without paying its mana cost if that spell's mana value is 4 or less. Put the exiled card into your hand if it wasn't cast this way.";
const KIORA_SOVEREIGN_OF_THE_DEEP: &str = "Vigilance, ward {3}\nWhenever you cast a Kraken, Leviathan, Octopus, or Serpent spell from your hand, look at the top X cards of your library, where X is that spell's mana value. You may cast a spell with mana value less than X from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";

fn reach_planetarium_cast_offer() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Planetarium of Wan Shi Tong", 1, 1, PLANETARIUM_TRIGGER)
        .as_artifact();

    let sibling = scenario.add_card_to_library_top(P0, "Unlooked Library Sibling");
    let looked = scenario
        .add_spell_to_library_top(P0, "Planetarium Looked Spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Planetarium Scry Enabler", false, "Scry 1.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&scry_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: scry_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the scry enabler must cast");
    runner.advance_until_stack_empty();

    let WaitingFor::ScryChoice { player, cards } = runner.state().waiting_for.clone() else {
        panic!(
            "the enabler must reach its real scry choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0);
    assert_eq!(
        cards,
        vec![looked],
        "Scry 1 must inspect the staged top card"
    );
    runner
        .act(GameAction::SelectCards { cards })
        .expect("keeping the looked card on top must complete scry");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "Planetarium's parsed trigger must reach the optional cast decision; got {}",
        runner.waiting_for_kind()
    );
    assert_eq!(runner.state().last_revealed_ids, vec![looked]);
    assert_eq!(
        runner
            .state()
            .active_optional_effect_frame()
            .map(|frame| frame.ability.as_ref())
            .expect("the optional cast ability must be live")
            .targets,
        vec![TargetRef::Object(looked)],
        "the offer must bind the exact privately looked-at top card"
    );

    (runner, looked, sibling)
}

/// CR 701.20e + CR 608.2c + CR 608.2g: the look is private while the optional
/// decision is pending, and accepting casts the exact looked-at card during
/// the trigger's resolution.
#[test]
fn planetarium_accept_offers_private_top_identity_and_casts_it() {
    let (mut runner, looked, sibling) = reach_planetarium_cast_offer();

    let controller_view = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        controller_view.objects[&looked].name, "Planetarium Looked Spell",
        "the looking player must see the exact card offered for casting"
    );
    let opponent_view = filter_state_for_viewer(runner.state(), P1);
    assert_eq!(
        opponent_view.objects[&looked].name, "Hidden Card",
        "the private look and pending offer must not reveal the card to an opponent"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Planetarium's optional cast must succeed");

    assert_eq!(
        runner.state().objects[&looked].zone,
        Zone::Stack,
        "the exact looked card must enter the casting path during resolution"
    );
    assert_eq!(runner.state().objects[&sibling].zone, Zone::Library);
}

/// CR 701.20e + CR 608.2c: declining the positive cast offer leaves the looked
/// card and its library sibling in place.
#[test]
fn planetarium_decline_leaves_looked_card_in_library() {
    let (mut runner, looked, sibling) = reach_planetarium_cast_offer();

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining Planetarium's optional cast must succeed");

    assert_eq!(runner.state().objects[&looked].zone, Zone::Library);
    assert_eq!(runner.state().objects[&sibling].zone, Zone::Library);
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![looked, sibling],
        "declining must preserve the exact top-of-library order"
    );
}

/// CR 608.2c + CR 608.2d + CR 305.1: a land can't be chosen for an optional
/// cast. The impossible instruction is declined without leaking a permission,
/// while its independent sequential sibling still resolves.
#[test]
fn planetarium_land_top_does_not_offer_an_impossible_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(
            P0,
            "Planetarium Land-Tail Regression Source",
            1,
            1,
            LOOK_CAST_WITH_INDEPENDENT_TAIL,
        )
        .as_artifact();

    let looked = scenario.add_card_to_library_top(P0, "Planetarium Looked Land");
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Planetarium Scry Enabler", false, "Scry 1.")
        .id();

    let mut runner = scenario.build();
    let starting_life = runner.state().players[0].life;
    {
        let land = runner.state_mut().objects.get_mut(&looked).unwrap();
        land.card_types.core_types = vec![CoreType::Land];
        land.base_card_types = land.card_types.clone();
    }
    let card_id = runner.state().objects[&scry_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: scry_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the scry enabler must cast");
    runner.advance_until_stack_empty();

    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the enabler must reach its real scry choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(cards, vec![looked]);
    runner
        .act(GameAction::SelectCards { cards })
        .expect("keeping the land on top must complete scry");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Planetarium's land-top trigger must finish without a cast action; got {}",
        runner.waiting_for_kind()
    );
    assert!(
        runner.state().active_optional_effect_frame().is_none(),
        "no declined or failing cast action should remain pending"
    );
    assert_eq!(runner.state().last_revealed_ids, vec![looked]);
    assert_eq!(runner.state().objects[&looked].zone, Zone::Library);
    assert!(
        runner.state().objects[&looked]
            .casting_permissions
            .is_empty(),
        "declining the infeasible cast must not leak a permission onto the land"
    );
    assert_eq!(
        runner.state().players[0].life,
        starting_life + 1,
        "the independent sequential tail must resolve after the infeasible cast declines"
    );
}

/// CR 702.166a + CR 608.2d + CR 305.1: Beseech's bargained cast instruction
/// cannot offer a land as a spell. Its printed not-cast fallback moves the land
/// to hand without leaking the lingering permission used for castable cards.
#[test]
fn beseech_bargained_land_skips_cast_offer_and_runs_hand_fallback() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    }
    let bargain_artifact = scenario
        .add_creature(P0, "Bargain Artifact", 1, 1)
        .as_artifact()
        .id();
    let found_land = scenario
        .add_spell_to_library_top(P0, "Beseech Found Land", false)
        .as_land()
        .id();
    let beseech = scenario
        .add_spell_to_hand(P0, "Beseech the Mirror", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
            generic: 1,
        })
        .from_oracle_text_with_keywords(&["bargain"], BESEECH_THE_MIRROR)
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(beseech)
        .accept_optional()
        .sacrifice_with(&[bargain_artifact])
        .search_first_legal()
        .resolve();

    outcome.assert_zone(&[bargain_artifact, beseech], Zone::Graveyard);
    outcome.assert_zone(&[found_land], Zone::Hand);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Beseech's impossible land cast must not leave an optional prompt pending"
    );
    assert!(
        outcome.state().active_optional_effect_frame().is_none(),
        "the infeasible cast must be auto-declined rather than offered"
    );
    assert!(
        outcome.state().objects[&found_land]
            .casting_permissions
            .is_empty(),
        "auto-declining the land cast must not leak an exile-cast permission"
    );
}

/// CR 702.166a + CR 202.3 + CR 601.2e + CR 608.2d: Beseech cannot offer its
/// bargained free cast when the found nonland card exceeds mana value 4. The
/// declined impossible option must run the printed not-cast hand fallback.
#[test]
fn beseech_bargained_mana_value_five_skips_cast_offer_and_runs_hand_fallback() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    }
    let bargain_artifact = scenario
        .add_creature(P0, "Bargain Artifact", 1, 1)
        .as_artifact()
        .id();
    let found_spell = scenario
        .add_spell_to_library_top(P0, "Beseech Found Mana Value Five Spell", false)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let beseech = scenario
        .add_spell_to_hand(P0, "Beseech the Mirror", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
            generic: 1,
        })
        .from_oracle_text_with_keywords(&["bargain"], BESEECH_THE_MIRROR)
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(beseech)
        .accept_optional()
        .sacrifice_with(&[bargain_artifact])
        .search_first_legal()
        .resolve();

    outcome.assert_zone(&[bargain_artifact, beseech], Zone::Graveyard);
    outcome.assert_zone(&[found_spell], Zone::Hand);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Beseech's constrained-out spell must not leave an optional prompt pending"
    );
    assert!(
        outcome.state().active_optional_effect_frame().is_none(),
        "the infeasible constrained cast must be auto-declined rather than offered"
    );
    assert!(
        outcome.state().objects[&found_spell]
            .casting_permissions
            .is_empty(),
        "auto-declining the constrained cast must not leak an exile-cast permission"
    );
}

/// CR 202.3 + CR 305.1 + CR 601.2e + CR 608.2d: one land among Kiora's looked
/// cards does not make its optional cast impossible when another looked card is
/// an eligible spell. The real optional boundary must remain live with both
/// looked cards still untouched and the eligible spell bound to the offer.
#[test]
fn kiora_mixed_land_and_spell_keeps_eligible_cast_offer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA_SOVEREIGN_OF_THE_DEEP);
    for _ in 0..2 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    }
    let eligible_spell = scenario
        .add_spell_to_library_top(P0, "Kiora Eligible Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let looked_land = scenario
        .add_spell_to_library_top(P0, "Kiora Looked Land", false)
        .as_land()
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "Triggering Kraken", 2, 2)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(kraken).commit();
    assert_eq!(commit.state().objects[&kraken].zone, Zone::Stack);
    runner.resolve_top();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "the mixed looked set must retain Kiora's optional cast prompt; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().last_revealed_ids,
        vec![looked_land, eligible_spell],
        "Kiora must inspect the staged land-first mixed set"
    );
    assert!(
        runner
            .state()
            .active_optional_effect_frame()
            .map(|frame| frame.ability.as_ref())
            .expect("Kiora's optional CastFromZone must be live")
            .targets
            .contains(&TargetRef::Object(eligible_spell)),
        "the eligible spell must remain bound to Kiora's offer despite the land-first looked set"
    );
    for candidate in [looked_land, eligible_spell] {
        assert_eq!(runner.state().objects[&candidate].zone, Zone::Library);
        assert!(
            runner.state().objects[&candidate]
                .casting_permissions
                .is_empty(),
            "Kiora must not grant a casting permission before the optional decision"
        );
    }
}

/// CR 701.25a + CR 401.5 + CR 608.2d: surveilling the only library card into
/// the graveyard leaves Planetarium's subsequent look with no "that card"
/// referent, so no optional cast action can be offered.
#[test]
fn planetarium_empty_post_surveil_library_does_not_offer_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Planetarium of Wan Shi Tong", 1, 1, PLANETARIUM_TRIGGER)
        .as_artifact();

    let surveilled = scenario.add_card_to_library_top(P0, "Only Library Card");
    let surveil_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Planetarium Surveil Enabler", false, "Surveil 1.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&surveil_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: surveil_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the surveil enabler must cast");
    runner.advance_until_stack_empty();

    let WaitingFor::SurveilChoice { player, cards } = runner.state().waiting_for.clone() else {
        panic!(
            "the enabler must reach its real surveil choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0);
    assert_eq!(cards, vec![surveilled]);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("surveilling the only card into the graveyard must succeed");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Planetarium's empty-library look must finish without a cast action; got {}",
        runner.waiting_for_kind()
    );
    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().players[0].library.is_empty());
    assert_eq!(runner.state().objects[&surveilled].zone, Zone::Graveyard);
    assert!(runner.state().last_revealed_ids.is_empty());
}

/// CR 608.2c + CR 608.2d: when an empty look produces no exact "that card"
/// referent, normal chain target inheritance cannot substitute an unrelated
/// object. The impossible optional instruction is skipped, while its independent
/// sequential sibling still resolves.
#[test]
fn missing_look_referent_does_not_play_inherited_unrelated_object() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Empty Look Source", 1, 1).id();
    let unrelated = scenario
        .add_spell_to_graveyard(P0, "Unrelated Inherited Spell", true)
        .id();
    let mut runner = scenario.build();

    let mut independent_tail = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    independent_tail.sub_link = SubAbilityLink::SequentialSibling;

    let mut optional_play = ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Play,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::LingeringPermission,
            mana_spend_permission: None,
        },
        vec![],
        source,
        P0,
    )
    .sub_ability(independent_tail);
    optional_play.optional = true;

    let ability = ResolvedAbility::new(
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
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
        vec![TargetRef::Object(unrelated)],
        source,
        P0,
    )
    .sub_ability(optional_play);

    // Reach guards: the Dig has no card to publish, while its unrelated object
    // target is available for the generic parent-to-child inheritance branch.
    assert!(runner.state().players[0].library.is_empty());
    assert_eq!(ability.targets, vec![TargetRef::Object(unrelated)]);
    let cast = ability.sub_ability.as_deref().expect("cast child");
    assert!(cast.targets.is_empty());
    assert!(matches!(
        cast.effect,
        Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            mode: CardPlayMode::Play,
            driver: CastFromZoneDriver::LingeringPermission,
            ..
        }
    ));

    let starting_life = runner.state().players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("empty-look chain resolves");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "an impossible exact-parent play must not prompt"
    );
    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().last_revealed_ids.is_empty());
    assert!(
        runner.state().last_parent_target_missing_reason.is_none(),
        "the exact missing-parent provenance must be consumed by this handoff"
    );
    assert_eq!(
        runner.state().players[0].life,
        starting_life + 1,
        "skipping the optional play must preserve its independent sequential tail"
    );
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Graveyard);
    assert!(
        runner.state().objects[&unrelated]
            .casting_permissions
            .is_empty(),
        "the unrelated inherited object must receive no play permission"
    );
}
