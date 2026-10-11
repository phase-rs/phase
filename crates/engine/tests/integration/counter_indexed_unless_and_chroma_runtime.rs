//! Counter-indexed unless costs and the Chroma "for each" count, driven through
//! the real trigger pipeline from verbatim Oracle text (Scryfall).
//!
//! - Cyclone / Phantasmal Sphere: "put a <kind> counter on this <type>, then
//!   sacrifice this <type> unless you pay {cost} for each <kind> counter on it".
//!   CR 608.2c orders the instructions as written and CR 118.12a binds the unless
//!   to the sacrifice, so the prompt follows the counter; CR 608.2h reads the
//!   counter count when the unless resolves, so the new counter is included.
//! - Springjack Shepherd: "create a 0/1 white Goat creature token for each white
//!   mana symbol in the mana costs of permanents you control" counts the white
//!   symbols devotion counts (CR 700.5): each symbol once, hybrid once, only the
//!   controller's permanents, the Shepherd itself included.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CYCLONE: &str = "At the beginning of your upkeep, put a wind counter on this enchantment, \
     then sacrifice this enchantment unless you pay {G} for each wind counter on it. If you \
     pay, this enchantment deals damage equal to the number of wind counters on it to each \
     creature and each player.";

const PHANTASMAL_SPHERE: &str = "Flying\nAt the beginning of your upkeep, put a +1/+1 counter \
     on this creature, then sacrifice this creature unless you pay {1} for each +1/+1 counter \
     on it.\nWhen this creature leaves the battlefield, target opponent creates an X/X blue \
     Orb creature token with flying, where X is the number of +1/+1 counters on this \
     creature.";

const SPRINGJACK_SHEPHERD: &str = "Chroma — When this creature enters, create a 0/1 white Goat \
     creature token for each white mana symbol in the mana costs of permanents you control.";

fn wind() -> CounterType {
    CounterType::Generic("wind".to_string())
}

fn counters(runner: &GameRunner, id: ObjectId, counter: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(counter)
        .copied()
        .unwrap_or(0)
}

/// Advance P0's turn into its upkeep, then pass priority until the upkeep
/// trigger's resolution surfaces its unless-payment prompt.
fn advance_to_upkeep_unless_prompt(runner: &mut GameRunner) {
    runner.advance_to_upkeep();
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "the upkeep trigger never surfaced an unless-payment prompt: {:?}",
        runner.state().waiting_for
    );
}

/// The payer and cost of the pending unless-payment prompt.
fn unless_prompt(runner: &GameRunner) -> (PlayerId, AbilityCost) {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => (*player, cost.clone()),
        other => panic!("expected an unless-payment prompt, got {other:?}"),
    }
}

fn green(count: usize) -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Green; count],
            generic: 0,
        },
    }
}

/// Cyclone on P0's battlefield with `seeded` wind counters, with a P1 creature
/// on the battlefield as a damage witness, driven to the upkeep unless prompt.
fn cyclone_at_unless_prompt(seeded: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let cyclone = scenario
        .add_enchantment_from_oracle(P0, "Cyclone", CYCLONE)
        .id();
    scenario.with_counter(cyclone, wind(), seeded);
    let witness = scenario.add_vanilla(P1, 3, 3);
    let mut runner = scenario.build();
    advance_to_upkeep_unless_prompt(&mut runner);
    (runner, cyclone, witness)
}

/// CR 608.2c + CR 118.12a + CR 608.2h: Cyclone's prompt comes after the wind
/// counter is put and charges {G} per wind counter, the new one included.
#[test]
fn cyclone_prompts_g_per_wind_counter_after_the_counter() {
    let (runner, cyclone, _) = cyclone_at_unless_prompt(0);
    let (payer, cost) = unless_prompt(&runner);
    assert_eq!(payer, P0, "Cyclone's controller is asked to pay");
    assert_eq!(
        counters(&runner, cyclone, &wind()),
        1,
        "the wind counter is put before the payment is asked for"
    );
    assert_eq!(cost, green(1), "one wind counter → {{G}}");

    let (runner, cyclone, _) = cyclone_at_unless_prompt(1);
    let (payer, cost) = unless_prompt(&runner);
    assert_eq!(payer, P0);
    assert_eq!(counters(&runner, cyclone, &wind()), 2);
    assert_eq!(cost, green(2), "two wind counters → {{G}}{{G}}");
}

/// CR 118.12a: declining sacrifices Cyclone. The "If you pay, …" continuation is
/// honestly unrepresented, so nothing deals damage on the decline either.
#[test]
fn cyclone_declined_is_sacrificed_without_damage() {
    let (mut runner, cyclone, witness) = cyclone_at_unless_prompt(0);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    // Reach guard for the negatives below: the decline took effect.
    assert_eq!(runner.state().objects[&cyclone].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&witness].damage_marked, 0);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 20);
}

/// CR 118.12a: paying {G} keeps Cyclone on the battlefield. The paid-branch
/// damage is out of scope (the continuation fails closed) and is not asserted.
#[test]
fn cyclone_paid_stays_on_the_battlefield() {
    let (mut runner, cyclone, _) = cyclone_at_unless_prompt(0);
    let _ = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying must be accepted");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&cyclone].zone, Zone::Battlefield);
}

/// Phantasmal Sphere on P0's battlefield with `seeded` +1/+1 counters, driven to
/// the upkeep unless prompt.
fn sphere_at_unless_prompt(seeded: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let sphere = scenario
        .add_creature(P0, "Phantasmal Sphere", 0, 1)
        .from_oracle_text_with_keywords(&["Flying"], PHANTASMAL_SPHERE)
        .with_plus_counters(seeded)
        .id();
    let mut runner = scenario.build();
    advance_to_upkeep_unless_prompt(&mut runner);
    (runner, sphere)
}

/// CR 608.2c + CR 118.12a + CR 608.2h: Phantasmal Sphere asks for {1} per +1/+1
/// counter after the counter is put; declining sacrifices it.
#[test]
fn phantasmal_sphere_pays_one_per_counter_after_the_counter() {
    let (runner, sphere) = sphere_at_unless_prompt(0);
    let (payer, cost) = unless_prompt(&runner);
    assert_eq!(payer, P0);
    assert_eq!(counters(&runner, sphere, &CounterType::Plus1Plus1), 1);
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(1)
        }
    );

    let (mut runner, sphere) = sphere_at_unless_prompt(1);
    let (payer, cost) = unless_prompt(&runner);
    assert_eq!(payer, P0);
    assert_eq!(
        counters(&runner, sphere, &CounterType::Plus1Plus1),
        2,
        "the new counter is put before the payment is asked for"
    );
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(2)
        },
        "two counters → {{2}}"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&sphere].zone, Zone::Graveyard);
}

fn printed_cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// CR 700.5: Springjack Shepherd creates one Goat per white mana symbol among
/// its controller's permanents — {W}{W} (2) + {1}{W} (1) + {W/U} (1, hybrid once)
/// + the Shepherd's own {3}{W} (1) = 5. The opponent's {W}{W} does not count.
#[test]
fn springjack_shepherd_creates_a_goat_per_white_symbol() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario
        .add_creature(P0, "Double White", 1, 1)
        .with_mana_cost(printed_cost(
            0,
            vec![ManaCostShard::White, ManaCostShard::White],
        ));
    scenario
        .add_creature(P0, "Single White", 1, 1)
        .with_mana_cost(printed_cost(1, vec![ManaCostShard::White]));
    scenario
        .add_creature(P0, "Hybrid White Blue", 1, 1)
        .with_mana_cost(printed_cost(0, vec![ManaCostShard::WhiteBlue]));
    scenario
        .add_creature(P1, "Opponent Double White", 1, 1)
        .with_mana_cost(printed_cost(
            0,
            vec![ManaCostShard::White, ManaCostShard::White],
        ));
    let shepherd = scenario
        .add_creature_to_hand_from_oracle(P0, "Springjack Shepherd", 1, 2, SPRINGJACK_SHEPHERD)
        .with_mana_cost(printed_cost(3, vec![ManaCostShard::White]))
        .id();

    let mut runner = scenario.build();
    runner.cast(shepherd).resolve();
    runner.advance_until_stack_empty();

    let goats: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|obj| obj.is_token && obj.name == "Goat")
        .collect();
    // Reach guard: the trigger resolved into real Goat tokens.
    assert!(!goats.is_empty(), "the ETB trigger must create Goats");
    for goat in &goats {
        assert_eq!(goat.controller, P0);
        assert_eq!((goat.power, goat.toughness), (Some(0), Some(1)));
        assert_eq!(goat.color, vec![ManaColor::White]);
    }
    assert_eq!(
        goats.len(),
        5,
        "one Goat per white mana symbol among P0's permanents (CR 700.5)"
    );
}
