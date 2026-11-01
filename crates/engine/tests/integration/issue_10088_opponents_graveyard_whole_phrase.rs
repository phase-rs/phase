//! #10088: "cards in [your ]opponents' graveyards" is read by the shared nom
//! quantity grammar (`parse_quantity_ref` → `parse_zone_card_count` →
//! `parse_scoped_zone_ref`), not by a substring fallback arm inside
//! `parse_cda_quantity`. Extended text — an extremum head, a supertype
//! qualifier, a trailing relative clause — must fail closed instead of being
//! silently truncated to a bare opponents'-graveyard count, and the arithmetic
//! forms must compose over the whole-phrase reading.
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, Effect};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Consuming Aberration's verbatim Oracle text (AtomicCards): a CDA P/T line
/// plus its cast trigger.
const CONSUMING_ABERRATION: &str = "Consuming Aberration's power and toughness are each equal to \
                                   the number of cards in your opponents' graveyards.\nWhenever \
                                   you cast a spell, each opponent reveals cards from the top of \
                                   their library until they reveal a land card, then puts those \
                                   cards into their graveyard.";
/// Detritivore's P/T line (Suspend and its time-counter trigger omitted).
const DETRITIVORE_CDA: &str = "Detritivore's power and toughness are each equal to the number of \
                              nonbasic land cards in your opponents' graveyards.";
/// Synthetic control: the whole, unextended phrase.
const CONTROL_ENTERS: &str = "This creature enters with a number of +1/+1 counters on it equal to \
                              the number of cards in your opponents' graveyards.";

fn has_unimplemented_ability(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Unimplemented { .. }))
}

/// CR 613.4a: true when the object carries a CDA-style dynamic P/T static
/// (layer 7a).
fn has_dynamic_pt_static(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .static_definitions
        .as_slice()
        .iter()
        .flat_map(|def| def.modifications.iter())
        .any(|m| {
            matches!(
                m,
                ContinuousModification::SetDynamicPower { .. }
                    | ContinuousModification::SetDynamicToughness { .. }
            )
        })
}

/// P1's graveyard holds two creature cards; P0's graveyard holds one land card
/// (the scenario builder sets no Basic supertype, so it is a nonbasic land
/// card — but it sits in P0's OWN graveyard, which `CountScope::Opponents`
/// excludes). CR 102.2: P1 is P0's only opponent, so opponents' graveyards = 2.
fn stage_graveyards(scenario: &mut GameScenario) {
    scenario.add_creature_to_graveyard(P1, "Opp Bear A", 2, 2);
    scenario.add_creature_to_graveyard(P1, "Opp Bear B", 2, 2);
    scenario.add_land_to_graveyard(P0, "Forest");
}

/// CR 614.1c + CR 122.6: an "enters with … counters" amount the parser cannot
/// read must fail closed — no counters placed and an honest gap marker — while
/// the readable whole phrase places its count. Each row casts the positive
/// control in the same game.
#[test]
fn issue_10088_extended_opponents_graveyard_tails_place_no_counter() {
    for quantity in [
        "the greatest number of cards an opponent has drawn this turn among cards in your \
         opponents' graveyards",
        "the number of nonbasic land cards in your opponents' graveyards",
        "the number of cards in your opponents' graveyards that were put there from the \
         battlefield this turn",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        stage_graveyards(&mut scenario);
        let control = scenario
            .add_creature_to_hand_from_oracle(P0, "Opp-Graveyard Control", 1, 1, CONTROL_ENTERS)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let oracle = format!(
            "This creature enters with a number of +1/+1 counters on it equal to {quantity}."
        );
        let witness = scenario
            .add_creature_to_hand_from_oracle(P0, "Extended Opp-Graveyard Tail", 1, 1, &oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();

        // Positive reach guard: the whole phrase places two counters.
        assert!(!has_unimplemented_ability(&runner, control), "{quantity}");
        let positive = runner.cast(control).resolve();
        positive.assert_zone(&[control], Zone::Battlefield);
        assert_eq!(
            positive.state().objects[&control]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&2),
            "positive reach guard for {quantity}"
        );

        // Extended text fails closed: no counters, and an honest gap marker.
        let negative = runner.cast(witness).resolve();
        negative.assert_zone(&[witness], Zone::Battlefield);
        assert_eq!(
            negative.state().objects[&witness]
                .counters
                .get(&CounterType::Plus1Plus1),
            None,
            "{quantity}"
        );
        assert!(has_unimplemented_ability(&runner, witness), "{quantity}");
    }
}

/// CR 604.3 + CR 208.2a + CR 613.4a: with two creature cards in P1's graveyard
/// and a nonbasic land card in P0's own graveyard, Consuming Aberration is 2/2
/// (the count includes every card in opponents' graveyards) while Detritivore
/// is 0/0 (there are no nonbasic land cards in opponents' graveyards). A reading
/// that drops the "nonbasic land" qualifier would give 2/2 for Detritivore. The
/// CDA line must not become a dynamic P/T static when it cannot be read.
#[test]
fn issue_10088_detritivore_does_not_count_unqualified_opponent_graveyard_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_graveyards(&mut scenario);
    let aberration = scenario
        .add_creature_from_oracle(P0, "Consuming Aberration", 0, 0, CONSUMING_ABERRATION)
        .id();
    let detritivore = scenario
        .add_creature_from_oracle(P0, "Detritivore", 0, 0, DETRITIVORE_CDA)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let pt = |id: ObjectId| {
        let obj = &runner.state().objects[&id];
        (obj.power, obj.toughness)
    };

    // Positive reach guard for the layer evaluation.
    assert_eq!(pt(aberration), (Some(2), Some(2)));
    // Derived from the Oracle text: no nonbasic land cards in opponents' graveyards -> 0.
    assert_eq!(pt(detritivore), (Some(0), Some(0)));
    // Strict-failure guard: the unread CDA line must not be rebuilt as a
    // dynamic P/T static.
    assert!(!has_dynamic_pt_static(&runner, detritivore));
    // ... and it is reported as an honest gap (an `Unimplemented` ability that
    // carries the line), not silently dropped.
    assert!(has_unimplemented_ability(&runner, detritivore));
}
