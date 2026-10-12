//! Condition gates fail closed: a printed gate the parser can type is carried on
//! the definition and enforced at runtime, instead of being dropped so the
//! ungated effect ships.
//!
//! Cards (Oracle text verbatim from card-data / Scryfall):
//!   * Bronze Horse — "As long as you control another creature, prevent all
//!     damage that would be dealt to this creature by spells that target it."
//!   * Woolly Razorback — "As long as this creature has an ice counter on it,
//!     prevent all combat damage it would deal and it has defender."
//!   * Ascendant Packleader — "This creature enters with a +1/+1 counter on it if
//!     you control a permanent with mana value 4 or greater."
//!   * Hagra Mauling — "This spell costs {1} less to cast if an opponent controls
//!     no basic lands."
//!
//! Every test drives the production pipeline (cast, combat, castability).

use engine::game::casting::can_cast_object_now;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRONZE_HORSE: &str = "Trample\nAs long as you control another creature, prevent all damage that would be dealt to this creature by spells that target it.";

const WOOLLY_RAZORBACK: &str = "This creature enters with three ice counters on it.\nAs long as this creature has an ice counter on it, prevent all combat damage it would deal and it has defender.\nWhenever this creature blocks, remove an ice counter from it.";

const ASCENDANT_PACKLEADER: &str = "This creature enters with a +1/+1 counter on it if you control a permanent with mana value 4 or greater.\nWhenever you cast a spell with mana value 4 or greater, put a +1/+1 counter on this creature.";

const HAGRA_MAULING: &str =
    "This spell costs {1} less to cast if an opponent controls no basic lands.\nDestroy target creature.";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

// ── Bronze Horse (CR 604.2 + CR 615.1a + CR 611.3a) ──────────────────────────

/// Which other permanent the Bronze Horse board carries.
enum HorseCompanion {
    None,
    OwnCreature,
    OpponentCreature,
    OwnArtifact,
}

/// Bolt Bronze Horse with the given companion on board; returns damage marked.
fn bolt_bronze_horse(companion: HorseCompanion) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario
        .add_creature(P0, "Bronze Horse", 4, 4)
        .from_oracle_text_with_keywords(&["Trample"], BRONZE_HORSE)
        .id();
    match companion {
        HorseCompanion::None => {}
        HorseCompanion::OwnCreature => {
            scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        }
        HorseCompanion::OpponentCreature => {
            scenario.add_creature(P1, "Grizzly Bears", 2, 2);
        }
        HorseCompanion::OwnArtifact => {
            scenario
                .add_creature(P0, "Ornithopter Husk", 0, 2)
                .as_artifact();
        }
    }
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    runner
        .cast(bolt)
        .target_object(horse)
        .resolve()
        .damage_marked(horse)
}

/// CR 604.2 + CR 611.3a: the shield applies only while its controller controls
/// another creature. Alone, the Horse takes the Bolt's 3 damage (red at base:
/// the ungated shield prevented it).
#[test]
fn bronze_horse_shield_applies_only_with_another_creature() {
    assert_eq!(
        bolt_bronze_horse(HorseCompanion::None),
        3,
        "with no other creature the gate is false — the Bolt's damage is dealt"
    );
    // Positive control: another creature you control turns the shield on.
    assert_eq!(bolt_bronze_horse(HorseCompanion::OwnCreature), 0);
    // Hostile: a creature an opponent controls is not "you control".
    assert_eq!(bolt_bronze_horse(HorseCompanion::OpponentCreature), 3);
    // Hostile: another permanent you control that is not a creature.
    assert_eq!(bolt_bronze_horse(HorseCompanion::OwnArtifact), 3);
}

// ── Woolly Razorback (CR 604.2 + CR 122.1 + CR 510.1a) ───────────────────────

fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..12 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "never reached declare blockers; stuck at {:?}",
        runner.state().waiting_for
    );
}

/// Woolly blocks an attacking Grizzly Bears with `ice` counters on it; its block
/// trigger removes one before combat damage. Returns (bear zone, bear damage).
fn woolly_blocks_bears(ice: u32) -> (Zone, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A"]);
    scenario.with_library_top(P1, &["Filler B"]);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let woolly = scenario
        .add_creature(P1, "Woolly Razorback", 7, 7)
        .from_oracle_text(WOOLLY_RAZORBACK)
        .id();
    scenario.with_counter(woolly, CounterType::Generic("ice".to_string()), ice);
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("Grizzly Bears may attack");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(woolly, bear)])
        .expect("Woolly Razorback may block");
    let outcome = runner.combat_damage();
    (outcome.zone_of(bear), outcome.damage_marked(bear))
}

/// CR 604.2 + CR 122.1: the combat-damage shield is gated on an ice counter. The
/// block trigger removes the last one before damage (CR 510.1a), so Woolly deals
/// its damage and the Bears die (red at base: the ungated shield kept them alive).
#[test]
fn woolly_razorback_deals_combat_damage_once_last_ice_counter_removed() {
    let (zone, _) = woolly_blocks_bears(1);
    assert_eq!(
        zone,
        Zone::Graveyard,
        "with its last ice counter removed, Woolly's combat damage is dealt"
    );
    // Positive control: a counter remains after the block trigger → shield on.
    let (zone, damage) = woolly_blocks_bears(2);
    assert_eq!(zone, Zone::Battlefield);
    assert_eq!(damage, 0);
}

// ── Ascendant Packleader (CR 614.1c + CR 614.1d) ─────────────────────────────

/// Which permanent the Packleader board carries besides the Packleader.
enum PackleaderBoard {
    Empty,
    OwnManaValueFour,
    OpponentManaValueFour,
    OwnManaValueThree,
}

/// Cast Ascendant Packleader; returns its +1/+1 counters once it resolves.
fn cast_packleader(board: PackleaderBoard) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mana_value = |generic: u32| ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic,
    };
    match board {
        PackleaderBoard::Empty => {}
        PackleaderBoard::OwnManaValueFour => {
            scenario
                .add_creature(P0, "Four Drop", 4, 4)
                .with_mana_cost(mana_value(3));
        }
        PackleaderBoard::OpponentManaValueFour => {
            scenario
                .add_creature(P1, "Four Drop", 4, 4)
                .with_mana_cost(mana_value(3));
        }
        PackleaderBoard::OwnManaValueThree => {
            scenario
                .add_creature(P0, "Three Drop", 3, 3)
                .with_mana_cost(mana_value(2));
        }
    }
    let packleader = scenario
        .add_creature_to_hand_from_oracle(P0, "Ascendant Packleader", 2, 1, ASCENDANT_PACKLEADER)
        .with_mana_cost(mana_value(0))
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Green]));
    let mut runner = scenario.build();
    let outcome = runner.cast(packleader).resolve();
    assert_eq!(outcome.zone_of(packleader), Zone::Battlefield);
    outcome.counters(packleader, CounterType::Plus1Plus1)
}

/// CR 614.1c + CR 614.1d: the enters-with counter is gated on controlling a
/// permanent with mana value 4 or greater (red at base: the ungated replacement
/// always placed it).
#[test]
fn ascendant_packleader_enters_without_counter_when_no_mv4_permanent() {
    assert_eq!(cast_packleader(PackleaderBoard::Empty), 0);
    // Positive control: you control a mana value 4 permanent.
    assert_eq!(cast_packleader(PackleaderBoard::OwnManaValueFour), 1);
    // Hostile: the mana value 4 permanent is an opponent's.
    assert_eq!(cast_packleader(PackleaderBoard::OpponentManaValueFour), 0);
    // Hostile: your permanent's mana value is only 3.
    assert_eq!(cast_packleader(PackleaderBoard::OwnManaValueThree), 0);
}

// ── Hagra Mauling (CR 601.2f + CR 109.4) ─────────────────────────────────────

/// Which land, if any, the Hagra board carries.
enum HagraLand {
    None,
    OpponentBasic,
    OwnBasic,
    OpponentNonbasic,
}

/// Can P0 cast Hagra Mauling ({2}{B}{B}) with exactly B, B, C in pool?
fn hagra_castable_with_three_mana(land: HagraLand) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hagra = scenario
        .add_spell_to_hand_from_oracle(P0, "Hagra Mauling", true, HAGRA_MAULING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        })
        .id();
    // The spell's legal target — `can_cast_object_now` requires one, so without
    // it every board would read "uncastable" vacuously.
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    match land {
        HagraLand::None => {}
        HagraLand::OpponentBasic => {
            scenario.add_basic_land(P1, ManaColor::Black);
        }
        HagraLand::OwnBasic => {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        HagraLand::OpponentNonbasic => {
            scenario.add_land_from_oracle(P1, "Hagra Broodpit", "");
        }
    }
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Black, ManaType::Black, ManaType::Colorless]),
    );
    let runner = scenario.build();
    can_cast_object_now(runner.state(), P0, hagra)
}

/// CR 601.2f + CR 109.4: the {1} reduction applies only while an opponent
/// controls no basic lands; with an opposing basic Hagra costs full and three
/// mana is not enough (red at base: the ungated reduction made it castable).
#[test]
fn hagra_mauling_costs_full_when_an_opponent_controls_a_basic_land() {
    assert!(
        !hagra_castable_with_three_mana(HagraLand::OpponentBasic),
        "an opposing basic land turns the reduction off"
    );
    // Positive control: no opposing basic → reduced to three mana.
    assert!(hagra_castable_with_three_mana(HagraLand::None));
    // Hostile: the caster's own basic land does not count.
    assert!(hagra_castable_with_three_mana(HagraLand::OwnBasic));
    // Hostile: an opposing nonbasic land is not a basic land.
    assert!(hagra_castable_with_three_mana(HagraLand::OpponentNonbasic));
}
