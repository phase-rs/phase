//! #9657: an "enters with … counters" clause publishes a counter only for a
//! real count and a whole counter name, and a created token's entry-counter
//! amount reads "other" relative to the entering token.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AggregateFunction, Effect, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PRINTLIFTER: &str = "Deathtouch\nWhenever this creature or another creature you control \
                           is turned face up, create a 0/0 green Ooze creature token with \
                           trample. The token enters with X +1/+1 counters on it, where X is the \
                           number of other creatures you control.\nDisguise {3}{G}";
/// Synthetic control: a disguise creature with no other text, so the creature
/// turned face up is neither Printlifter Ooze nor its token.
const VEILED_BEAR: &str = "Disguise {G}";
const SHOE_TREE: &str = "This creature enters with up to two shoe counters on it. Use your \
                         shoes as counters.\nThis creature gets +1/+1 for each shoe counter on \
                         it.";
/// Synthetic control: a printed count and a whole custom counter name.
const SHOE_RACK: &str = "This creature enters with two shoe counters on it.";
/// Thought Sponge's verbatim Oracle text; Flash is also supplied as a keyword hint.
const THOUGHT_SPONGE: &str =
    "Flash\nThis creature enters with a number of +1/+1 counters on it equal \
                              to the greatest number of cards an opponent has drawn this \
                              turn.\nWhen this creature dies, draw cards equal to its power.";
/// Synthetic neighbour whose discarded-card extremum remains unsupported.
const UNREAD_DRAW_EXTREMUM: &str = "This creature enters with a number of +1/+1 counters on it \
                                  equal to the greatest number of cards an opponent has \
                                  discarded this turn.";
const CENTAUR_VINECRASHER_ENTERS: &str = "This creature enters with a number of +1/+1 counters \
                                          on it equal to the number of land cards in all \
                                          graveyards.";
/// The #9657 issue comment's synthetic line: a conjoined counter list whose
/// second placement is a dynamic count the list reader cannot read.
const CHARGE_LIST_WITNESS: &str = "This creature enters with a +1/+1 counter and a number of \
                                   charge counters on it equal to the number of creatures you \
                                   control.";
/// Synthetic companion: the second placement carries no count at all — a bare
/// plural counter name read through the "equal to" quantity.
const BARE_CHARGE_LIST_WITNESS: &str = "This creature enters with a +1/+1 counter and charge \
                                        counters on it equal to the number of creatures you \
                                        control.";
/// Synthetic companion: the second placement opens with a bare "additional"
/// qualifier and takes its count from the "equal to" quantity.
const ADDITIONAL_CHARGE_LIST_WITNESS: &str = "This creature enters with a +1/+1 counter and \
                                              additional charge counters on it equal to the \
                                              number of creatures you control.";
/// Dust Animus's enters line (its Flying and Plot lines omitted).
const DUST_ANIMUS_ENTERS: &str = "If you control five or more untapped lands, this creature \
                                  enters with two +1/+1 counters and a lifelink counter on it.";

fn add_green(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        let unit = ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Ooze tokens P0 controls on the battlefield: (id, +1/+1 counters, toughness).
fn oozes_on_battlefield(runner: &GameRunner) -> Vec<(ObjectId, u32, Option<i32>)> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Ooze")
        })
        .map(|o| {
            (
                o.id,
                o.counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                o.toughness,
            )
        })
        .collect()
}

/// Play `id` face down (CR 702.168a), then turn it face up (CR 702.168d) and
/// resolve the turned-face-up trigger.
fn flip(runner: &mut GameRunner, id: ObjectId) {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::PlayFaceDown {
            object_id: id,
            card_id,
        })
        .expect("play face down");
    runner.advance_until_stack_empty();
    let obj = &runner.state().objects[&id];
    assert!(
        obj.zone == Zone::Battlefield && obj.face_down,
        "reach guard: the creature is face down on the battlefield"
    );
    runner
        .act(GameAction::TurnFaceUp {
            object_id: id,
            x: 0,
        })
        .expect("turn face up");
    assert!(
        !runner.state().objects[&id].face_down,
        "reach guard: the creature was turned face up"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: Printlifter Ooze's turned-face-up trigger is on the stack"
    );
    runner.advance_until_stack_empty();
}

fn has_unimplemented_ability(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .abilities
        .iter()
        .any(|a| matches!(*a.effect, Effect::Unimplemented { .. }))
}

/// CR 614.1c + CR 122.6 + CR 608.2c + CR 608.2h: "The token enters with X
/// +1/+1 counters on it, where X is the number of other creatures you control."
/// "Other" contrasts with the token, which is not yet on the battlefield when X
/// is determined, so Printlifter Ooze itself counts: X = 1 and the 0/0 token
/// survives as a 1/1. The opponent's creature is not one "you control"
/// (CR 109.5). Reading "other" relative to Printlifter gave X = 0, and the
/// token died (CR 704.5f).
#[test]
fn printlifter_alone_token_counts_printlifter_and_survives() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Opp Bear", 2, 2);
    let printlifter = scenario
        .add_creature_to_hand(P0, "Printlifter Ooze", 2, 2)
        .from_oracle_text_with_keywords(&["Deathtouch", "Disguise"], PRINTLIFTER)
        .id();
    let mut runner = scenario.build();
    add_green(&mut runner, 12);

    flip(&mut runner, printlifter);

    let oozes = oozes_on_battlefield(&runner);
    assert_eq!(oozes.len(), 1, "exactly one Ooze token, got {oozes:?}");
    let (_, plus_counters, toughness) = oozes[0];
    assert_eq!(plus_counters, 1, "X counts Printlifter Ooze");
    assert_eq!(toughness, Some(1));
}

/// Same derivation with another creature you control: X = 2 (Printlifter Ooze
/// and Bear A); the opponent's creature is excluded. Source-relative "other"
/// gave 1.
#[test]
fn printlifter_token_counts_other_own_creatures_not_opponents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear A", 2, 2);
    scenario.add_creature(P1, "Opp Bear", 2, 2);
    let printlifter = scenario
        .add_creature_to_hand(P0, "Printlifter Ooze", 2, 2)
        .from_oracle_text_with_keywords(&["Deathtouch", "Disguise"], PRINTLIFTER)
        .id();
    let mut runner = scenario.build();
    add_green(&mut runner, 12);

    flip(&mut runner, printlifter);

    let oozes = oozes_on_battlefield(&runner);
    assert_eq!(oozes.len(), 1, "exactly one Ooze token, got {oozes:?}");
    assert_eq!(oozes[0].1, 2, "X counts Printlifter Ooze and Bear A");
}

/// "… or another creature you control is turned face up": the trigger object
/// (Veiled Bear), the source (Printlifter Ooze) and the recipient (the token)
/// are three different objects. X = 2 (Printlifter Ooze and Veiled Bear).
/// Source-relative "other" gave 1.
#[test]
fn another_creature_turned_face_up_token_counts_printlifter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Opp Bear", 2, 2);
    scenario.add_creature_from_oracle(P0, "Printlifter Ooze", 2, 2, PRINTLIFTER);
    let veiled = scenario
        .add_creature_to_hand(P0, "Veiled Bear", 2, 2)
        .from_oracle_text_with_keywords(&["Disguise"], VEILED_BEAR)
        .id();
    let mut runner = scenario.build();
    add_green(&mut runner, 12);

    flip(&mut runner, veiled);

    let oozes = oozes_on_battlefield(&runner);
    assert_eq!(oozes.len(), 1, "exactly one Ooze token, got {oozes:?}");
    assert_eq!(oozes[0].1, 2, "X counts Printlifter Ooze and Veiled Bear");
}

/// CR 122.1: Shoe Tree's "up to two shoe counters" has no whole counter name
/// after a count, so it places nothing and surfaces as an unsupported gap
/// rather than one `Generic("up to two shoe")` counter.
#[test]
fn prose_counter_clause_places_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shoe_tree = scenario
        .add_creature_to_hand_from_oracle(P0, "Shoe Tree", 3, 3, SHOE_TREE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let shoe_rack = scenario
        .add_creature_to_hand_from_oracle(P0, "Shoe Rack", 1, 1, SHOE_RACK)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    for id in [shoe_tree, shoe_rack] {
        runner.cast(id).resolve();
        runner.advance_until_stack_empty();
    }

    // Reach guards: both resolved onto the battlefield, and the control's
    // whole custom counter name still places its counters.
    let state = runner.state();
    assert_eq!(state.objects[&shoe_tree].zone, Zone::Battlefield);
    assert_eq!(state.objects[&shoe_rack].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&shoe_rack]
            .counters
            .get(&CounterType::Generic("shoe".to_string()))
            .copied(),
        Some(2)
    );

    assert!(
        state.objects[&shoe_tree].counters.is_empty(),
        "no prose counter, got {:?}",
        state.objects[&shoe_tree].counters
    );
    assert!(has_unimplemented_ability(&runner, shoe_tree));
}

/// CR 122.6: an unreadable discarded-card extremum must fail closed instead
/// of publishing a default single counter; the readable Vinecrasher is the control.
#[test]
fn unread_equal_to_count_places_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sponge = scenario
        .add_creature_to_hand_from_oracle(P0, "Unread Draw Extremum", 1, 1, UNREAD_DRAW_EXTREMUM)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let vinecrasher = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Centaur Vinecrasher",
            1,
            1,
            CENTAUR_VINECRASHER_ENTERS,
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_land_to_graveyard(P0, "Forest");
    scenario.add_land_to_graveyard(P1, "Island");
    let mut runner = scenario.build();
    for id in [sponge, vinecrasher] {
        runner.cast(id).resolve();
        runner.advance_until_stack_empty();
    }

    // Reach guards: both resolved, and a readable "equal to" count still
    // places its counters.
    let state = runner.state();
    assert_eq!(state.objects[&sponge].zone, Zone::Battlefield);
    assert_eq!(state.objects[&vinecrasher].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&vinecrasher]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2)
    );

    assert_eq!(
        state.objects[&sponge]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        None,
        "no Fixed 1 count published"
    );
    assert!(has_unimplemented_ability(&runner, sponge));
}

/// CR 614.1c + CR 122.6: cast the actual replacement through the normal stack.
/// CR 121.1 + CR 800.4i + CR 102.3: the magnitude is a per-player history
/// extremum, retaining departures but excluding teammates for opponent scopes.
fn assert_draw_extremum_entry(
    mut scenario: GameScenario,
    caster: PlayerId,
    card: (&str, &str),
    scope: PlayerScope,
    draws: &[u32],
    departed: Option<PlayerId>,
    expected: u32,
) {
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(caster, ManaColor::Blue);
    }
    let sponge = scenario
        .add_creature_to_hand(caster, card.0, 1, 1)
        .from_oracle_text_with_keywords(&["Flash"], card.1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().players.len(), draws.len());
    for (player, &drawn) in runner.state_mut().players.iter_mut().zip(draws) {
        player.cards_drawn_this_turn = drawn;
        player.is_eliminated = Some(player.id) == departed;
    }
    runner.state_mut().active_player = caster;
    runner.state_mut().priority_player = caster;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };

    assert!(
        !has_unimplemented_ability(&runner, sponge),
        "positive parse guard"
    );
    let count = QuantityExpr::Ref {
        qty: QuantityRef::CardsDrawnThisTurn { player: scope },
    };
    assert!(
        runner.state().objects[&sponge]
            .replacement_definitions
            .iter_unchecked()
            .any(
                |definition| definition.execute.as_deref().is_some_and(|execute| {
                    matches!(
                        &*execute.effect,
                        Effect::PutCounter {
                            counter_type: CounterType::Plus1Plus1,
                            count: actual,
                            target: TargetFilter::SelfRef,
                        } if actual == &count
                    )
                })
            ),
        "SHAPE reach guard: the replacement reads the exact draw extremum"
    );
    let outcome = runner.cast(sponge).resolve();
    outcome.assert_zone(&[sponge], Zone::Battlefield);
    assert_eq!(
        outcome.state().objects[&sponge]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        expected,
        "the entering permanent reads the caster's history population"
    );
}

#[test]
fn issue_9887_thought_sponge_counts_opponent_draws_not_own() {
    assert_draw_extremum_entry(
        GameScenario::new(),
        P0,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[5, 3],
        None,
        3,
    );
}

#[test]
fn issue_9887_thought_sponge_uses_max_not_sum_first_or_last() {
    assert_draw_extremum_entry(
        GameScenario::new_n_player(4, 42),
        P0,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[7, 2, 5, 3],
        None,
        5,
    );
}

#[test]
fn issue_9887_thought_sponge_keeps_departed_opponents_draws() {
    assert_draw_extremum_entry(
        GameScenario::new_n_player(3, 42),
        P0,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[9, 7, 2],
        Some(P1),
        7,
    );
}

#[test]
fn issue_9887_thought_sponge_excludes_two_headed_giant_teammate() {
    assert_draw_extremum_entry(
        GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42),
        P0,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[1, 8, 2, 3],
        None,
        3,
    );
}

#[test]
fn issue_9887_thought_sponge_p1_cast_reads_p0_as_opponent() {
    assert_draw_extremum_entry(
        GameScenario::new_n_player(4, 42),
        P1,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[5, 7, 2, 3],
        None,
        5,
    );
}

#[test]
fn issue_9887_synthetic_all_player_draw_extremum_includes_controller() {
    let oracle = "This creature enters with a number of +1/+1 counters on it equal to the greatest number of cards a player has drawn this turn.";
    assert_draw_extremum_entry(
        GameScenario::new(),
        P0,
        ("Synthetic All-player Extremum", oracle),
        PlayerScope::AllPlayers {
            aggregate: AggregateFunction::Max,
            exclude: None,
        },
        &[5, 3],
        None,
        5,
    );
}

#[test]
fn issue_9887_thought_sponge_zero_draws_is_supported_zero() {
    assert_draw_extremum_entry(
        GameScenario::new(),
        P0,
        ("Thought Sponge", THOUGHT_SPONGE),
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Max,
        },
        &[0, 0],
        None,
        0,
    );
}

/// CR 122.6: a one-word-away unreadable amount must not publish a partial
/// counter placement. Every row casts the positive real-card control too.
#[test]
fn issue_9887_extremum_near_misses_remain_pipeline_gaps() {
    for quantity in [
        "the least number of cards an opponent has drawn this turn",
        "the fewest number of cards an opponent has drawn this turn",
        "the greatest number of cards an opponent has discarded this turn",
        "the greatest number of cards an opponent drew last turn",
        "the greatest number of cards each opponent has drawn this turn",
        "the greatest number of cards target opponent has drawn this turn",
        "the greatest number of cards you have drawn this turn",
        "the greatest number of cards your opponents have drawn this turn",
        "the greatest number of cards any player has drawn this turn",
        "the greatest number of card an opponent has drawn this turn",
        "the greatest number of cards an opponent has drawn this turn ever",
        "the greatest number of cards an opponent has drawn this turn among your opponents",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let control = scenario
            .add_creature_to_hand(P0, "Thought Sponge", 1, 1)
            .from_oracle_text_with_keywords(&["Flash"], THOUGHT_SPONGE)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let oracle = format!(
            "This creature enters with a number of +1/+1 counters on it equal to {quantity}."
        );
        let witness = scenario
            .add_creature_to_hand_from_oracle(P0, "Synthetic Unread Extremum", 1, 1, &oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().players[0].cards_drawn_this_turn = 5;
        runner.state_mut().players[1].cards_drawn_this_turn = 3;
        assert!(!has_unimplemented_ability(&runner, control), "{quantity}");
        let positive = runner.cast(control).resolve();
        positive.assert_zone(&[control], Zone::Battlefield);
        assert_eq!(
            positive.state().objects[&control]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&3),
            "positive reach guard for {quantity}"
        );
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

/// CR 614.1c + CR 122.6 + CR 608.2c: an "enters with" counter list whose second
/// placement the list reader cannot read ("a number of charge counters … equal
/// to …", or the count-less "charge counters … equal to …" and "additional
/// charge counters … equal to …") must not publish its first placement alone. Doing so dropped the
/// charge counters and gave the +1/+1 placement the charge counters' count
/// (creatures you control). The line places nothing and surfaces as an
/// unsupported gap; Dust Animus's readable conjoined list still places both
/// counters in the same game.
#[test]
fn unread_second_counter_placement_places_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A creature P0 controls, so the dropped placement's "equal to" count is
    // nonzero when it is wrongly handed to the +1/+1 placement.
    scenario.add_creature(P0, "Bear A", 2, 2);
    // CR 614.1c: Dust Animus's gate — five untapped lands.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let witness = scenario
        .add_creature_to_hand_from_oracle(P0, "Charge Witness", 1, 1, CHARGE_LIST_WITNESS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let bare_witness = scenario
        .add_creature_to_hand_from_oracle(P0, "Bare Charge Witness", 1, 1, BARE_CHARGE_LIST_WITNESS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let additional_witness = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Additional Charge Witness",
            1,
            1,
            ADDITIONAL_CHARGE_LIST_WITNESS,
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let dust_animus = scenario
        .add_creature_to_hand_from_oracle(P0, "Dust Animus", 2, 3, DUST_ANIMUS_ENTERS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    for id in [witness, bare_witness, additional_witness, dust_animus] {
        runner.cast(id).resolve();
        runner.advance_until_stack_empty();
    }

    // Reach guards: all resolved onto the battlefield, and the readable
    // conjoined list places both of its counters.
    let state = runner.state();
    assert_eq!(state.objects[&witness].zone, Zone::Battlefield);
    assert_eq!(state.objects[&bare_witness].zone, Zone::Battlefield);
    assert_eq!(state.objects[&additional_witness].zone, Zone::Battlefield);
    assert_eq!(state.objects[&dust_animus].zone, Zone::Battlefield);
    let animus_counters = &state.objects[&dust_animus].counters;
    assert_eq!(
        animus_counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "got {animus_counters:?}"
    );
    assert_eq!(
        animus_counters
            .get(&CounterType::Keyword(KeywordKind::Lifelink))
            .copied(),
        Some(1),
        "got {animus_counters:?}"
    );

    for id in [witness, bare_witness, additional_witness] {
        assert!(
            state.objects[&id].counters.is_empty(),
            "no partial counter list, got {:?}",
            state.objects[&id].counters
        );
        assert!(has_unimplemented_ability(&runner, id));
    }
}
