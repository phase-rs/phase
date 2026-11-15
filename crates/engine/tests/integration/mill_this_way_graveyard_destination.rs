//! CR 614.6 + CR 701.17c: a "put into a graveyard this way" consumer counts
//! only the milled cards that ACTUALLY arrived in a graveyard.
//!
//! A mill batch is published to the tracked set including the cards a
//! replacement (Rest in Peace, Leyline of the Void) sent to public exile
//! instead, because an effect that refers to a milled card finds it in the
//! public zone it went to (CR 701.17c — Locke's "cast a spell from among those
//! cards"). The destination-NAMED consumers are different: CR 614.6 says a
//! replaced event never happens, so a card exiled instead was never "put into a
//! graveyard", and the count must not include it.
//!
//! Oracle text below is verbatim from Scryfall `cards/named`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DREAD_SUMMONS: &str = "Each player mills X cards. For each creature card put into a graveyard this way, you create a tapped 2/2 black Zombie creature token. (To mill a card, a player puts the top card of their library into their graveyard.)";
const COERCED_CONFESSION: &str = "Target player mills four cards. You draw a card for each creature card put into their graveyard this way.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

fn pool(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// Stage a creature card on top of `player`'s library.
fn stage_creature(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, "Milled Creature", false)
        .as_creature()
        .id()
}

/// Stage a noncreature card on top of `player`'s library.
fn stage_spell(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, "Milled Spell", false)
        .id()
}

fn zombie_tokens(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.is_token
                && obj.controller == P0
                && obj.zone == Zone::Battlefield
                && obj.card_types.subtypes.iter().any(|s| s == "Zombie")
        })
        .count()
}

/// Dread Summons for X=2: each library holds a creature over a noncreature card,
/// so the batch is two creature cards milled. Returns the Zombie count and where
/// the milled creatures ended up.
fn dread_summons(rest_in_peace: bool) -> (usize, Vec<Zone>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dread Summons", false, DREAD_SUMMONS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    if rest_in_peace {
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    let mut creatures = Vec::new();
    for player in [P0, P1] {
        stage_spell(&mut scenario, player);
        creatures.push(stage_creature(&mut scenario, player));
    }
    scenario.with_mana_pool(P0, pool(ManaType::Black, 4));
    let mut runner = scenario.build();
    runner.cast(spell).x(2).resolve();
    let zones = creatures
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    (zombie_tokens(&runner), zones)
}

/// CR 701.17a + CR 608.2c: control — with no replacement every milled creature
/// card is put into a graveyard, so each makes a Zombie.
#[test]
fn dread_summons_makes_a_zombie_per_creature_put_into_a_graveyard() {
    let (zombies, zones) = dread_summons(false);
    assert_eq!(
        zones,
        vec![Zone::Graveyard, Zone::Graveyard],
        "reach guard: both milled creatures reached a graveyard"
    );
    assert_eq!(zombies, 2);
}

/// CR 614.6: with Rest in Peace the milled creature cards are exiled instead, so
/// none was "put into a graveyard this way" and no Zombie is created.
#[test]
fn dread_summons_under_rest_in_peace_makes_no_zombies() {
    let (zombies, zones) = dread_summons(true);
    assert_eq!(
        zones,
        vec![Zone::Exile, Zone::Exile],
        "reach guard: Rest in Peace exiled both milled creatures"
    );
    assert_eq!(zombies, 0);
}

/// Coerced Confession: the target (P1) mills four with two creatures among them.
/// Returns the cards P0 drew and where the creatures ended up.
fn coerced_confession(rest_in_peace: bool) -> (usize, Vec<Zone>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coerced Confession", false, COERCED_CONFESSION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::BlueBlack],
            generic: 4,
        })
        .id();
    if rest_in_peace {
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    // P0's library must hold cards to draw.
    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "Draw Fodder");
    }
    let mut creatures = Vec::new();
    for _ in 0..2 {
        stage_spell(&mut scenario, P1);
        creatures.push(stage_creature(&mut scenario, P1));
    }
    scenario.with_mana_pool(P0, pool(ManaType::Black, 5));
    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.cast(spell).target_player(P1).resolve();
    // The spell left the hand when cast; everything else in hand was drawn.
    let drawn = runner.state().players[P0.0 as usize].hand.len() + 1 - hand_before;
    let zones = creatures
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    (drawn, zones)
}

/// CR 701.17a + CR 608.2c: control — both milled creature cards reach the
/// target's graveyard, so the controller draws two.
#[test]
fn coerced_confession_draws_per_creature_put_into_their_graveyard() {
    let (drawn, zones) = coerced_confession(false);
    assert_eq!(
        zones,
        vec![Zone::Graveyard, Zone::Graveyard],
        "reach guard: both milled creatures reached the graveyard"
    );
    assert_eq!(drawn, 2);
}

/// CR 614.6: with Rest in Peace none of the milled creature cards was put into
/// the target's graveyard, so nothing is drawn.
#[test]
fn coerced_confession_under_rest_in_peace_draws_nothing() {
    let (drawn, zones) = coerced_confession(true);
    assert_eq!(
        zones,
        vec![Zone::Exile, Zone::Exile],
        "reach guard: Rest in Peace exiled both milled creatures"
    );
    assert_eq!(drawn, 0);
}

const BRUVAC: &str = "If an opponent would mill one or more cards, they mill twice that many cards instead. (To mill a card, a player puts the top card of their library into their graveyard.)";
const WATER_CRYSTAL: &str = "If an opponent would mill one or more cards, they mill that many cards plus four instead.\nBlue spells you cast cost {1} less to cast.\n{4}{U}{U}, {T}: Each opponent mills cards equal to the number of cards in your hand.";

/// CR 616.1 + CR 701.17a: P1 controls Bruvac and The Water Crystal, so P0's mill
/// is modified by two replacements of the same event and P0 chooses the order
/// (either order mills at least X + 4 cards). P0's library holds one creature
/// card on top of otherwise noncreature cards deep enough for either order.
/// Returns the staged creature card's id.
fn stage_order_prompt_board(scenario: &mut GameScenario, below_mill_range: usize) -> ObjectId {
    scenario.add_creature_from_oracle(P1, "Bruvac the Grandiloquent", 1, 4, BRUVAC);
    scenario.add_artifact_from_oracle(P1, "The Water Crystal", WATER_CRYSTAL);
    // Cards below the mill range, for a spell that draws afterwards.
    for _ in 0..below_mill_range {
        scenario.add_card_to_library_top(P0, "Draw Fodder");
    }
    for _ in 0..24 {
        stage_spell(scenario, P0);
    }
    let creature = stage_creature(scenario, P0);
    stage_spell(scenario, P1);
    creature
}

/// Answer the CR 616.1 order prompt, then settle to an empty stack.
fn answer_order_prompt_and_settle(runner: &mut GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice {
                player: P0,
                kind: ReplacementChoiceKind::Order,
                ..
            }
        ),
        "reach guard: P0 is asked to order Bruvac and The Water Crystal, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the order choice is accepted");
    for _ in 0..50 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            ref other => panic!("unexpected decision after the order choice: {other:?}"),
        }
    }
    panic!("the spell never finished resolving");
}

/// CR 616.1 + CR 701.17a + CR 608.2c: Dread Summons X=1 with P0's mill paused on
/// the replacement order. The creature card delivered after the answer was put
/// into a graveyard this way, so exactly one Zombie is created.
#[test]
fn dread_summons_counts_a_creature_delivered_after_the_replacement_order_answer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Dread Summons", false, DREAD_SUMMONS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let creature = stage_order_prompt_board(&mut scenario, 0);
    scenario.with_mana_pool(P0, pool(ManaType::Black, 3));
    let mut runner = scenario.build();
    runner.cast(spell).x(1).resolve();
    answer_order_prompt_and_settle(&mut runner);
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the creature card reached P0's graveyard"
    );
    assert_eq!(zombie_tokens(&runner), 1);
}

/// CR 616.1 + CR 701.17a + CR 608.2c: Coerced Confession targeting P0 with the
/// mill paused on the replacement order. The creature card put into P0's
/// graveyard after the answer yields exactly one draw.
#[test]
fn coerced_confession_counts_a_creature_delivered_after_the_replacement_order_answer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coerced Confession", false, COERCED_CONFESSION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::BlueBlack],
            generic: 4,
        })
        .id();
    let creature = stage_order_prompt_board(&mut scenario, 4);
    scenario.with_mana_pool(P0, pool(ManaType::Black, 5));
    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.cast(spell).target_player(P0).resolve();
    answer_order_prompt_and_settle(&mut runner);
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the creature card reached P0's graveyard"
    );
    // The spell left the hand when cast; everything else in hand was drawn.
    let drawn = runner.state().players[P0.0 as usize].hand.len() + 1 - hand_before;
    assert_eq!(drawn, 1);
}
