//! Regression for phase-rs/phase#10095: a compound Ward cost charges every
//! part, not only the first one.
//!
//! The three printed cards whose Ward is a compound cost (Oracle text verbatim
//! from MTGJSON):
//!   - Captain Howler, Sea Scourge: "Ward—{2}, Pay 2 life."
//!   - Gisa, the Hellraiser:        "Ward—{2}, Pay 2 life."
//!   - Ovika, Enigma Goliath:       "Ward—{3}, Pay 3 life."
//!
//! Before the fix `ward_cost_to_ability_cost` kept only `costs.first()`, so the
//! targeting player's mana alone paid the Ward and the life leg was never
//! charged; and the unless-payment adapter refused a mixed composite outright.
//!
//! CR references:
//!   - CR 702.21a: Ward counters the targeting spell or ability unless that
//!     player pays [cost].
//!   - CR 118.3: a player can't pay a cost without the resources to pay it
//!     fully, so no part of a cost is charged when another part is unpayable.
//!   - CR 118.12a: "[do something] unless [a player does something else]" is
//!     an optional cost paid when the spell or ability resolves.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CAPTAIN_HOWLER: &str = "Ward—{2}, Pay 2 life.\nWhenever you discard one or more cards, target creature gets +2/+0 until end of turn for each card discarded this way. Whenever that creature deals combat damage to a player this turn, you draw a card.";
const GISA: &str = "Ward—{2}, Pay 2 life.\nSkeletons and Zombies you control get +1/+1 and have menace.\nWhenever you commit a crime, create two tapped 2/2 blue and black Zombie Rogue creature tokens. This ability triggers only once each turn.";
const OVIKA: &str = "Flying\nWard—{3}, Pay 3 life.\nWhenever you cast a noncreature spell, create X 1/1 red Phyrexian Goblin creature tokens, where X is the mana value of that spell. They gain haste until end of turn.";
const MANA_CONFLUENCE: &str = "{T}, Pay 1 life: Add one mana of any color.";
const PRISTINE_TALISMAN: &str = "{T}: Add {C}. You gain 1 life.";
const ANGEL_OF_JUBILATION: &str = "Flying\nOther nonblack creatures you control get +1/+1.\nPlayers can't pay life or sacrifice creatures to cast spells or activate abilities.";

/// A warded creature under P0 that P1 has just targeted with a destroy spell,
/// with the ward trigger resolved so the game is waiting on P1's payment.
struct WardBoard {
    runner: GameRunner,
    warded: ObjectId,
    destroy: ObjectId,
    lands: Vec<ObjectId>,
}

/// `lands` seeded untapped Plains under P1, the ward's payer.
fn target_warded_creature(name: &str, oracle: &str, lands: usize) -> WardBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = scenario
        .add_creature(P0, name, 3, 4)
        .from_oracle_text_with_keywords(&["Ward"], oracle)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let lands: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P1, ManaColor::White))
        .collect();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(destroy).target_objects(&[warded]).commit();
    runner.advance_until_stack_empty();
    WardBoard {
        runner,
        warded,
        destroy,
        lands,
    }
}

/// Reach guard: the ward trigger asked P1 to pay, and the warded permanent is
/// still on the battlefield. Returns the offered cost.
fn assert_payment_prompt(runner: &GameRunner, warded: ObjectId) -> AbilityCost {
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Battlefield,
        "reach: the warded permanent must be on the battlefield when Ward triggers"
    );
    let WaitingFor::UnlessPayment { player, cost, .. } = &runner.state().waiting_for else {
        panic!(
            "Ward must prompt the targeting player to pay its cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the targeting spell's controller pays Ward");
    cost.clone()
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn assert_lands_tapped(board: &WardBoard) {
    for land in &board.lands {
        assert!(
            board.runner.state().objects[land].tapped,
            "the mana leg of the Ward cost must be paid from P1's land {land:?}"
        );
    }
}

fn assert_lands_untapped(board: &WardBoard) {
    for land in &board.lands {
        assert!(
            !board.runner.state().objects[land].tapped,
            "no mana may be spent when the Ward cost is declined or unpayable ({land:?})"
        );
    }
}

/// CR 702.21a + CR 118.12a: every printed compound Ward offers BOTH legs as one
/// cost. Before the fix only the mana leg reached the prompt.
#[test]
fn every_printed_compound_ward_prompts_for_all_of_its_legs() {
    for (name, oracle, mana, life) in [
        ("Captain Howler, Sea Scourge", CAPTAIN_HOWLER, 2u32, 2i32),
        ("Gisa, the Hellraiser", GISA, 2, 2),
        ("Ovika, Enigma Goliath", OVIKA, 3, 3),
    ] {
        let board = target_warded_creature(name, oracle, 0);
        let cost = assert_payment_prompt(&board.runner, board.warded);
        let AbilityCost::Composite { costs } = cost else {
            panic!("{name}: a compound Ward cost must be one composite cost, got {cost:?}");
        };
        assert_eq!(
            costs.len(),
            2,
            "{name}: both printed legs must be part of the Ward cost, got {costs:?}"
        );
        assert!(
            matches!(&costs[0], AbilityCost::Mana { cost } if *cost == ManaCost::generic(mana)),
            "{name}: the mana leg must be {mana} generic, got {:?}",
            costs[0]
        );
        assert!(
            matches!(
                &costs[1],
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value }
                } if *value == life
            ),
            "{name}: the life leg must be {life} life, got {:?}",
            costs[1]
        );
    }
}

/// CR 702.21a + CR 118.12a: paying a compound Ward charges the mana AND the
/// life; the countered effect is suppressed, so the destroy spell then resolves.
#[test]
fn captain_howler_ward_charges_mana_and_life_then_the_spell_resolves() {
    let mut board = target_warded_creature("Captain Howler, Sea Scourge", CAPTAIN_HOWLER, 2);
    assert_payment_prompt(&board.runner, board.warded);
    let life_before = life_of(&board.runner, P1);

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 can pay {2} and 2 life");
    board.runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&board.runner, P1),
        life_before - 2,
        "paying the Ward must charge the life leg"
    );
    assert_lands_tapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Graveyard,
        "a paid Ward does not counter the destroy spell, so it resolves"
    );
    assert_eq!(
        board.runner.state().objects[&board.destroy].zone,
        Zone::Graveyard
    );
    assert!(
        board.runner.state().stack.is_empty(),
        "the stack must empty after the ward payment and the spell, got {:?}",
        board.runner.state().stack
    );
}

/// Ovika's printed numbers are {3} and 3 life — the composite keeps each leg's
/// own amount rather than a fixed pair.
#[test]
fn ovika_ward_charges_three_mana_and_three_life() {
    let mut board = target_warded_creature("Ovika, Enigma Goliath", OVIKA, 3);
    assert_payment_prompt(&board.runner, board.warded);
    let life_before = life_of(&board.runner, P1);

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 can pay {3} and 3 life");
    board.runner.advance_until_stack_empty();

    assert_eq!(life_of(&board.runner, P1), life_before - 3);
    assert_lands_tapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Graveyard,
        "a paid Ward lets the destroy spell resolve"
    );
}

/// CR 702.21a: declining the compound Ward counters the targeting spell and
/// charges no part of the cost.
#[test]
fn declining_the_compound_ward_counters_the_spell_and_pays_nothing() {
    let mut board = target_warded_creature("Captain Howler, Sea Scourge", CAPTAIN_HOWLER, 2);
    assert_payment_prompt(&board.runner, board.warded);
    let life_before = life_of(&board.runner, P1);

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining Ward is always legal");
    board.runner.advance_until_stack_empty();

    assert_eq!(life_of(&board.runner, P1), life_before);
    assert_lands_untapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Battlefield,
        "an unpaid Ward counters the destroy spell"
    );
    assert_eq!(
        board.runner.state().objects[&board.destroy].zone,
        Zone::Graveyard
    );
}

/// CR 702.21a + CR 118.12a: paying life for Ward is a resolution-time cost,
/// not a cost to cast a spell or activate an ability, so Angel of Jubilation's
/// "players can't pay life ... to cast spells or activate abilities" does not
/// block it. A cast/activation-scoped affordability gate would wrongly refuse
/// this legal payment.
#[test]
fn ward_still_pays_life_beside_angel_of_jubilation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Angel of Jubilation", 4, 5)
        .from_oracle_text_with_keywords(&["Flying"], ANGEL_OF_JUBILATION);
    let warded = scenario
        .add_creature(P0, "Captain Howler, Sea Scourge", 3, 4)
        .from_oracle_text_with_keywords(&["Ward"], CAPTAIN_HOWLER)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let lands: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P1, ManaColor::White))
        .collect();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(destroy).target_objects(&[warded]).commit();
    runner.advance_until_stack_empty();

    let board = WardBoard {
        runner,
        warded,
        destroy,
        lands,
    };
    assert_payment_prompt(&board.runner, board.warded);
    let life_before = life_of(&board.runner, P1);
    let mut board = board;

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("Angel of Jubilation does not prohibit a resolution-time life payment");
    board.runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&board.runner, P1),
        life_before - 2,
        "the Ward's life leg is payable at resolution despite Angel of Jubilation"
    );
    assert_lands_tapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Graveyard,
        "the paid Ward lets the destroy spell resolve"
    );
}

/// CR 118.3 + CR 702.21a: when the mana leg is unaffordable the whole cost is
/// unpaid — the life leg must not be paid either, and the Ward counters the
/// spell.
#[test]
fn a_payer_who_lacks_the_mana_leg_pays_no_life() {
    let mut board = target_warded_creature("Captain Howler, Sea Scourge", CAPTAIN_HOWLER, 1);
    assert_payment_prompt(&board.runner, board.warded);
    let life_before = life_of(&board.runner, P1);

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the pay choice is legal; the payment itself must fail cleanly");
    board.runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&board.runner, P1),
        life_before,
        "no life may be paid when the mana leg cannot be paid"
    );
    assert_lands_untapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Battlefield,
        "an unpayable Ward counters the targeting spell"
    );
}

/// CR 118.3 + CR 702.21a: when one leg is unpayable the whole cost is unpaid —
/// the affordable mana leg must not be spent, and the Ward counters the spell.
#[test]
fn a_payer_who_lacks_the_life_leg_spends_no_mana() {
    let mut board = target_warded_creature("Captain Howler, Sea Scourge", CAPTAIN_HOWLER, 2);
    assert_payment_prompt(&board.runner, board.warded);
    board.runner.state_mut().players[P1.0 as usize].life = 1;

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the pay choice is legal; the payment itself must fail cleanly");
    board.runner.advance_until_stack_empty();

    assert_eq!(life_of(&board.runner, P1), 1, "life cannot go negative");
    assert_lands_untapped(&board);
    assert_eq!(
        board.runner.state().objects[&board.warded].zone,
        Zone::Battlefield,
        "an unpayable Ward counters the targeting spell"
    );
    assert_eq!(
        board.runner.state().objects[&board.destroy].zone,
        Zone::Graveyard
    );
}

/// CR 118.3 + CR 702.21a: the mana leg and the life leg are one cost. Two Mana
/// Confluences fund {2} by paying 2 life, so at 3 life the whole {2}+2-life Ward
/// is unpayable even though the mana component alone is affordable. The whole
/// payment is refused before any resource moves, and the Ward counters the
/// spell.
#[test]
fn compound_ward_whose_mana_source_costs_life_is_refused_jointly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = scenario
        .add_creature(P0, "Captain Howler, Sea Scourge", 3, 4)
        .from_oracle_text_with_keywords(&["Ward"], CAPTAIN_HOWLER)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let confluences: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_land_from_oracle(P1, "Mana Confluence", MANA_CONFLUENCE)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for land in &confluences {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = false;
    }
    runner.state_mut().players[P1.0 as usize].life = 3;
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    assert!(
        !engine::game::mana_sources::activatable_mana_actions_for_player(runner.state(), P1)
            .is_empty(),
        "reach: Mana Confluence offers a mana ability the payer can activate"
    );
    runner.cast(destroy).target_objects(&[warded]).commit();
    runner.advance_until_stack_empty();
    assert_payment_prompt(&runner, warded);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the pay choice is legal; the joint payment must fail cleanly");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&runner, P1),
        3,
        "no life may be paid when the whole cost cannot be completed"
    );
    for land in &confluences {
        assert!(
            !runner.state().objects[land].tapped,
            "no mana may be produced when the whole cost cannot be completed"
        );
    }
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Battlefield,
        "an unpayable Ward counters the targeting spell"
    );
    assert_eq!(runner.state().objects[&destroy].zone, Zone::Graveyard);
}

/// CR 118.3 + CR 702.21a: the shared budget runs both ways. At 1 life a
/// {2}+2-life Ward looks unpayable, but two Pristine Talismans fund the mana and
/// gain the life the life leg needs, so the payment completes: both tap, life
/// ends at 1, and the paid Ward lets the destroy spell resolve.
#[test]
fn compound_ward_completes_when_mana_sources_supply_the_life_leg() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = scenario
        .add_creature(P0, "Captain Howler, Sea Scourge", 3, 4)
        .from_oracle_text_with_keywords(&["Ward"], CAPTAIN_HOWLER)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let talismans: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_artifact_from_oracle(P1, "Pristine Talisman", PRISTINE_TALISMAN)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for talisman in &talismans {
        runner.state_mut().objects.get_mut(talisman).unwrap().tapped = false;
    }
    runner.state_mut().players[P1.0 as usize].life = 1;
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(destroy).target_objects(&[warded]).commit();
    runner.advance_until_stack_empty();
    assert_payment_prompt(&runner, warded);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the mana sources supply the life the life leg needs");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&runner, P1),
        1,
        "two Talismans gain 2 life, then the Ward's 2-life leg is paid"
    );
    for talisman in &talismans {
        assert!(
            runner.state().objects[talisman].tapped,
            "both Talismans fund the mana leg"
        );
    }
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Graveyard,
        "the paid Ward lets the destroy spell resolve"
    );
}
