//! Issue #7822: manifesting a planeswalker card seeded LOYALTY COUNTERS onto
//! the face-down permanent and left `obj.loyalty` set — the client renders the
//! loyalty badge on a face-down 2/2, leaking that the card is a planeswalker.
//! CR 708.2a: a face-down permanent is a 2/2 creature — it has no loyalty (or
//! defense) characteristic; CR 306.5b seeds loyalty only for a planeswalker
//! entering as one.
//!
//! REVERT DISCRIMINATOR: without the loyalty/defense blanking in
//! `apply_face_down_creature_characteristics`, `intrinsic_etb_counters` reads
//! the card's printed loyalty during the face-down entry and the
//! no-loyalty-counter assertion fails.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    Effect, FaceDownProfile, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card::PrintedLoyalty;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};

const MANIFEST_DREAD: &str = "Manifest dread.";

#[test]
fn a_manifested_planeswalker_card_has_no_loyalty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario.add_card_to_library_top(P0, "Buried Walker");
    scenario.add_card_to_library_top(P0, "Second Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&walker).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // Review (#7827): initialize EVERY loyalty/defense field including the
        // base twins, so a hidden base value restored by a layer reset cannot
        // slip through.
        obj.loyalty = Some(4);
        obj.printed_loyalty = Some(PrintedLoyalty::Fixed(4));
        obj.base_loyalty = Some(4);
        obj.base_printed_loyalty = Some(PrintedLoyalty::Fixed(4));
        obj.defense = Some(3);
        obj.base_defense = Some(3);
    }
    runner.cast(spell).resolve();
    let WaitingFor::ManifestDreadChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "manifest dread must pause for a card choice, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![walker],
        })
        .expect("manifest choice must be accepted");
    runner.advance_until_stack_empty();

    let obj = runner
        .state()
        .objects
        .get(&walker)
        .expect("manifested object exists");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down, "manifested object is face down");
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty),
        None,
        "no loyalty counters may be seeded on a face-down entry (CR 708.2a)"
    );
    assert_eq!(
        obj.loyalty, None,
        "the face-down permanent has no loyalty characteristic to display"
    );
    assert_eq!(obj.defense, None);
    assert_eq!(obj.counters.get(&CounterType::Defense), None);

    // Review (#7827): force a layer re-derive — a base twin left set would be
    // written back into the live fields here.
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::evaluate_layers(runner.state_mut());
    let obj = runner
        .state()
        .objects
        .get(&walker)
        .expect("manifested object exists");
    assert_eq!(obj.loyalty, None, "no base twin may resurrect loyalty");
    assert_eq!(obj.printed_loyalty, None);
    assert_eq!(obj.base_loyalty, None);
    assert_eq!(obj.base_printed_loyalty, None);
    assert_eq!(obj.defense, None);
    assert_eq!(obj.base_defense, None);

    // The real card survives underneath. Manifest cannot legally turn a
    // planeswalker card face up (CR 701.34), so the restore pair is verified
    // at its authority: `apply_back_face_to_object` — the single restore path
    // every legal turn-up routes through.
    let back = obj.back_face.clone().expect("back face snapshot exists");
    assert_eq!(
        back.loyalty,
        Some(4),
        "the snapshot keeps the printed value"
    );
    let mut restored = obj.clone();
    engine::game::printed_cards::apply_back_face_to_object(&mut restored, back);
    assert_eq!(restored.loyalty, Some(4), "turn-up restores loyalty");
    assert_eq!(restored.defense, Some(3), "turn-up restores defense");
}

/// Review (#7827): the battle sibling — a manifested BATTLE card must not
/// enter with defense counters nor keep a defense characteristic (CR 310.4b
/// seeds defense only for a battle entering as one; CR 708.2a).
#[test]
fn a_manifested_battle_card_has_no_defense() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let battle = scenario.add_card_to_library_top(P0, "Buried Siege");
    scenario.add_card_to_library_top(P0, "Second Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&battle).unwrap();
        obj.card_types.core_types.push(CoreType::Battle);
        obj.defense = Some(5);
        obj.base_defense = Some(5);
    }
    runner.cast(spell).resolve();
    let WaitingFor::ManifestDreadChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "manifest dread must pause for a card choice, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![battle],
        })
        .expect("manifest choice must be accepted");
    runner.advance_until_stack_empty();

    let obj = runner
        .state()
        .objects
        .get(&battle)
        .expect("manifested object exists");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down);
    assert_eq!(
        obj.counters.get(&CounterType::Defense),
        None,
        "no defense counters may be seeded on a face-down entry"
    );
    assert_eq!(obj.defense, None);
    assert_eq!(obj.base_defense, None);
}

/// Review regression (#7827): an EXPLICITLY instructed entry counter is a
/// separate instruction (CR 122.1) and must survive a face-down entry — only
/// the INTRINSIC loyalty/defense seeding (CR 306.5b / CR 310.4b) is
/// suppressed. A production `Effect::ChangeZone` carrying BOTH
/// `face_down_profile` and an explicit +1/+1 entry counter moves a
/// planeswalker card from hand to the battlefield.
#[test]
fn an_explicit_entry_counter_survives_a_face_down_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Dread Engine", 1, 1).id();
    let walker = scenario
        .add_creature_to_hand(P0, "Buried Walker", 0, 0)
        .id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&walker).unwrap();
        obj.card_types.core_types.clear();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(4);
        obj.printed_loyalty = Some(PrintedLoyalty::Fixed(4));
    }

    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            origin: Some(Zone::Hand),
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 })],
            conditional_enter_with_counters: vec![],
            face_down_profile: Some(FaceDownProfile::vanilla_2_2()),
            enters_modified_if: None,
        },
        vec![TargetRef::Object(walker)],
        source,
        engine::types::player::PlayerId(0),
    );
    let mut events = Vec::new();
    engine::game::effects::change_zone::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the face-down entry must resolve");

    let obj = runner.state().objects.get(&walker).expect("entrant exists");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down, "the entrant is face down");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "the explicitly instructed +1/+1 counter must survive the face-down entry (CR 122.1)"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty),
        None,
        "the intrinsic loyalty seeding stays suppressed (CR 708.2a)"
    );
    assert_eq!(obj.loyalty, None);
}
