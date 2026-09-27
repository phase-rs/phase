//! Paying life down to 0 while casting a spell must not end the game mid-cast.
//!
//! CR 119.4: paying life equal to your life total is a legal payment. CR 104.3b:
//! a player at 0 or less life loses "the next time a player would receive
//! priority", and CR 704.3 checks state-based actions only then. No player
//! receives priority while a spell is being cast (CR 601.2h) or an ability
//! activated (CR 602.2b), so the caster finishes the cast and only then loses.
//!
//! The Platinum Angel and Yawgmoth tests are controls: the "can't lose"
//! exception was already honoured, and an activation's life cost is paid as
//! its last cost component, in the same action that puts it on the stack.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TOXIC_DELUGE: &str = "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.";
const MANA_CONFLUENCE: &str = "{T}, Pay 1 life: Add one mana of any color.";
const PLATINUM_ANGEL: &str =
    "Flying\nYou can't lose the game and your opponents can't win the game.";
const YAWGMOTH: &str = "Protection from Humans\nPay 1 life, Sacrifice another creature: Put a -1/-1 counter on up to one target creature and draw a card.\n{B}{B}, Discard a card: Proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn eliminated(runner: &GameRunner) -> bool {
    runner.state().players[P0.0 as usize].is_eliminated
}

fn is_mana_payment(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. })
}

fn p1_won(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(winner) } if winner == P1
    )
}

struct DelugeBoard {
    runner: GameRunner,
    deluge: ObjectId,
    victim: ObjectId,
    confluence: ObjectId,
}

/// Announces Toxic Deluge with X = `x`, paying the X life as the additional
/// cost. Manual payment keeps the {2}{B} window open afterwards.
fn cast_toxic_deluge(
    life: i32,
    x: u32,
    floating: &[ManaType],
    platinum_angel: bool,
) -> DelugeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
    let deluge = scenario
        .add_spell_to_hand_from_oracle(P0, "Toxic Deluge", false, TOXIC_DELUGE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    let confluence = scenario
        .add_land_from_oracle(P0, "Mana Confluence", MANA_CONFLUENCE)
        .id();
    if platinum_angel {
        scenario.add_creature_from_oracle(P0, "Platinum Angel", 4, 4, PLATINUM_ANGEL);
    }
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.with_mana_pool(P0, pool(floating));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&deluge].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: deluge,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Toxic Deluge");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "Toxic Deluge announces X first, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseX { value: x })
        .expect("announce X, paying X life");
    DelugeBoard {
        runner,
        deluge,
        victim,
        confluence,
    }
}

fn spell_cast(events: &[GameEvent], spell: ObjectId) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
}

fn pays_life(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PayLife { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(pays_life),
        _ => false,
    }
}

#[test]
fn spell_paid_down_to_zero_life_is_cast_before_the_player_loses() {
    let DelugeBoard {
        mut runner,
        deluge,
        victim,
        ..
    } = cast_toxic_deluge(
        3,
        3,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Black],
        false,
    );

    // CR 601.2h: Toxic Deluge is still being cast; its {2}{B} is owed.
    assert_eq!(runner.life(P0), 0);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must still be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );

    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B} from the pool")
        .events;

    // CR 601.2i: the spell became cast...
    assert!(
        spell_cast(&events, deluge),
        "Toxic Deluge must become cast: {events:?}"
    );
    // ...then CR 104.3b + CR 704.5a: P0 loses before it can resolve.
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
}

#[test]
fn mana_ability_paying_the_last_life_mid_cast_does_not_end_the_game_yet() {
    let DelugeBoard {
        mut runner,
        deluge,
        confluence,
        ..
    } = cast_toxic_deluge(4, 3, &[ManaType::Colorless, ManaType::Colorless], false);
    assert_eq!(runner.life(P0), 1);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );

    // CR 117.1d: mana abilities may be activated while paying a spell's cost.
    // Tap Confluence for the {B} the pool is missing, paying the last life.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&confluence)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } if selection.mana_type == ManaType::Black => {
                Some(selection.clone())
            }
            _ => None,
        })
        .expect("Mana Confluence's {B} is offered mid-cast");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap Mana Confluence for {B}");

    // CR 601.2h: still casting; the life payment ended no one's game.
    assert_eq!(runner.life(P0), 0);
    assert!(
        is_mana_payment(&runner),
        "Toxic Deluge must still be paying its mana cost, got {:?}",
        runner.state().waiting_for
    );
    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B}")
        .events;

    // CR 601.2i, then CR 104.3b + CR 704.5a.
    assert!(
        spell_cast(&events, deluge),
        "Toxic Deluge must become cast: {events:?}"
    );
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
}

#[test]
fn cant_lose_player_keeps_playing_after_paying_down_to_zero_life() {
    let DelugeBoard {
        mut runner, deluge, ..
    } = cast_toxic_deluge(
        3,
        3,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Black],
        true,
    );
    assert!(is_mana_payment(&runner));
    let events = runner
        .act(GameAction::PassPriority)
        .expect("finish paying {2}{B} from the pool")
        .events;
    assert!(spell_cast(&events, deluge));

    // CR 101.2 + CR 704.5a: Platinum Angel's "can't lose" beats the 0-life SBA,
    // so the priority check after the cast eliminates no one.
    assert_eq!(runner.life(P0), 0);
    assert!(!eliminated(&runner));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&deluge].zone, Zone::Stack);
}

#[test]
fn activation_paid_down_to_zero_life_goes_on_the_stack_before_the_player_loses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 1);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario.add_creature(P0, "Other Fodder", 1, 1);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_card_to_library_top(P0, "Yawgmoth Draw");
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&yawgmoth]
        .abilities
        .iter()
        .position(|ability| ability.cost.as_ref().is_some_and(pays_life))
        .expect("Yawgmoth's pay-life ability parses");
    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("activate Yawgmoth");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(victim)],
        })
        .expect("target the Bears");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));
    assert_eq!(runner.life(P0), 1);
    let events = runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice the Fodder, completing the cost")
        .events;

    // CR 602.2b + CR 601.2i: the ability was activated and put on the stack...
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })),
        "Yawgmoth's ability must reach the stack: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == fodder
        )),
        "the sacrifice cost was paid: {events:?}"
    );
    // ...then CR 104.3b + CR 704.5a: P0 loses as they would receive priority,
    // before the ability can resolve.
    assert_eq!(runner.life(P0), 0);
    assert!(eliminated(&runner));
    assert!(p1_won(&runner), "got {:?}", runner.state().waiting_for);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
}
