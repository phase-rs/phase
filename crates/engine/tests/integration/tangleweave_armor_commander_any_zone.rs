//! CR 903.3e: "If an effect refers to a characteristic of “your commander,” it can
//! find the appropriate player’s commander and see its current characteristics, as
//! modified by continuous effects and other rules, in all zones, including that
//! player’s library and hand."
//!
//! Two printed cards read the greatest mana value among "your commanders", and both
//! carry a ruling that says "It doesn't matter where your commander is":
//!
//! - Tangleweave Armor: "Living weapon (When this Equipment enters, create a 0/0
//!   black Phyrexian Germ creature token, then attach this to it.)\nEquipped creature
//!   gets +X/+X, where X is the greatest mana value among your commanders.\nEquip {4}"
//! - Cactus Preserve: "This land enters tapped.\n{T}: Add one mana of any type that a
//!   land you control could produce.\n{3}: Until end of turn, this land becomes an X/X
//!   green Plant creature with reach, where X is the greatest mana value among your
//!   commanders. It's still a land."
//!
//! Neither card is in the committed integration fixture, so both are parsed here from
//! that verbatim text through the production parser. Every other card comes from the
//! fixture. The Armor is built with no mana cost: its printed {2}{G}{G} is not what
//! these tests exercise, and leaving it out lets each mana pool hold exactly what the
//! spell under test pays (Ghalta's cast below pays with exactly {G}{G}).

use crate::support::shared_card_db;
use engine::database::card_db::CardDatabase;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const TANGLEWEAVE_ARMOR: &str = "Living weapon (When this Equipment enters, create a 0/0 black Phyrexian Germ creature token, then attach this to it.)\nEquipped creature gets +X/+X, where X is the greatest mana value among your commanders.\nEquip {4}";
const CACTUS_PRESERVE: &str = "This land enters tapped.\n{T}: Add one mana of any type that a land you control could produce.\n{3}: Until end of turn, this land becomes an X/X green Plant creature with reach, where X is the greatest mana value among your commanders. It's still a land.";
/// Mana value 12.
const GHALTA: &str = "Ghalta, Primal Hunger";
/// Mana value 4.
const KRENKO: &str = "Krenko, Mob Boss";

/// How a test answers the prompt CR 903.9 raises when the commander moves.
#[derive(Clone, Copy)]
enum CommanderPrompt {
    /// CR 903.9a: the state-based choice for a commander in a graveyard or in exile.
    ZoneChoice { accept: bool },
    /// CR 903.9b: the replacement for a commander going to a hand or a library.
    /// Index 0 moves it to the command zone and index 1 lets the move proceed.
    Replacement { index: usize },
}

/// A real spell that moves a commander off the battlefield, and where the commander
/// ends up once the CR 903.9 prompt is answered.
struct CommanderMove {
    spell: &'static str,
    mana: &'static [ManaType],
    prompt: CommanderPrompt,
    zone: Zone,
}

/// One move each to exile, the graveyard, the hand and the library, with the owner
/// keeping the commander out of the command zone. The rows that cast a commander
/// cover the stack.
const MOVES_OFF_THE_BATTLEFIELD: [CommanderMove; 4] = [
    CommanderMove {
        spell: "Path to Exile",
        mana: &[ManaType::White],
        prompt: CommanderPrompt::ZoneChoice { accept: false },
        zone: Zone::Exile,
    },
    CommanderMove {
        spell: "Murder",
        mana: &[ManaType::Black, ManaType::Black, ManaType::Black],
        prompt: CommanderPrompt::ZoneChoice { accept: false },
        zone: Zone::Graveyard,
    },
    CommanderMove {
        spell: "Unsummon",
        mana: &[ManaType::Blue],
        prompt: CommanderPrompt::Replacement { index: 1 },
        zone: Zone::Hand,
    },
    CommanderMove {
        spell: "Time Ebb",
        mana: &[ManaType::Blue, ManaType::Blue, ManaType::Blue],
        prompt: CommanderPrompt::Replacement { index: 1 },
        zone: Zone::Library,
    },
];

/// Both CR 903.9 routes, with the owner taking the commander to the command zone.
const MOVES_TO_THE_COMMAND_ZONE: [CommanderMove; 2] = [
    CommanderMove {
        spell: "Murder",
        mana: &[ManaType::Black, ManaType::Black, ManaType::Black],
        prompt: CommanderPrompt::ZoneChoice { accept: true },
        zone: Zone::Command,
    },
    CommanderMove {
        spell: "Unsummon",
        mana: &[ManaType::Blue],
        prompt: CommanderPrompt::Replacement { index: 0 },
        zone: Zone::Command,
    },
];

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn commander_game() -> GameScenario {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add_armor(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_artifact_to_hand_from_oracle(player, "Tangleweave Armor", TANGLEWEAVE_ARMOR)
        .with_subtypes(vec!["Equipment"])
        .id()
}

/// CR 903.3: the designation belongs to the card, so a commander placed outside the
/// command zone is designated directly, with no zone write.
fn designate(runner: &mut GameRunner, commander: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .unwrap()
        .is_commander = true;
}

/// Hands `player` the turn and priority so they can cast Hijack, a sorcery.
fn give_sorcery_timing(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn germ(state: &GameState) -> Option<ObjectId> {
    state
        .objects
        .values()
        .find(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name.contains("Germ")
        })
        .map(|object| object.id)
}

#[track_caller]
fn assert_germ(state: &GameState, armor: ObjectId, power_and_toughness: i32) {
    let germ = germ(state).expect("the Germ survives");
    assert_eq!(
        state.objects[&armor].attached_to,
        Some(AttachTarget::Object(germ))
    );
    assert_eq!(
        (state.objects[&germ].power, state.objects[&germ].toughness),
        (Some(power_and_toughness), Some(power_and_toughness))
    );
}

#[track_caller]
fn assert_priority(runner: &GameRunner, player: PlayerId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: waiting } if waiting == player),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

/// Casts `spell` at `commander` through the real pipeline and answers the CR 903.9
/// prompt the move raises. The prompt is asserted, so a row can't pass without it.
fn move_commander(
    runner: &mut GameRunner,
    spell: ObjectId,
    commander: ObjectId,
    prompt: CommanderPrompt,
) {
    // Declining answers Path to Exile's "Its controller may search their library";
    // the other spells ask nothing.
    runner
        .cast(spell)
        .target_object(commander)
        .decline_optional()
        .resolve();
    match prompt {
        CommanderPrompt::ZoneChoice { accept } => {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::CommanderZoneChoice { commander_id, .. } if commander_id == commander),
                "{:?}",
                runner.state().waiting_for
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap();
        }
        CommanderPrompt::Replacement { index } => {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "{:?}",
                runner.state().waiting_for
            );
            runner.act(GameAction::ChooseReplacement { index }).unwrap();
        }
    }
}

/// Ghalta starts on the battlefield as P0's commander, with the Armor and the move's
/// spell in P0's hand and exactly that spell's mana in P0's pool. Returns the runner,
/// Ghalta, the Armor and the spell.
fn ghalta_on_the_battlefield_with(
    mv: &CommanderMove,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Battlefield, db());
    let armor = add_armor(&mut scenario, P0);
    let spell = scenario.add_real_card(P0, mv.spell, Zone::Hand, db());
    scenario.with_mana_pool(P0, pool(mv.mana));
    let mut runner = scenario.build();
    designate(&mut runner, ghalta);
    (runner, ghalta, armor, spell)
}

/// CR 903.3e: the commander in the command zone counts, so the Germ is 12/12 rather
/// than a 0/0 that dies (CR 704.5f).
#[test]
fn commander_in_the_command_zone() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Hand, db());
    scenario.with_commander(ghalta);
    let armor = add_armor(&mut scenario, P0);
    let mut runner = scenario.build();

    runner.cast(armor).resolve();

    assert_eq!(runner.state().objects[&ghalta].zone, Zone::Command);
    assert_germ(runner.state(), armor, 12);
    assert_priority(&runner, P0);
}

/// CR 903.3e: "all zones" includes the battlefield.
#[test]
fn commander_on_the_battlefield() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Battlefield, db());
    let armor = add_armor(&mut scenario, P0);
    let mut runner = scenario.build();
    designate(&mut runner, ghalta);

    runner.cast(armor).resolve();

    assert_germ(runner.state(), armor, 12);
    assert_priority(&runner, P0);
}

/// CR 903.3e: exile, graveyard, hand and library all count when the Armor enters.
#[test]
fn commander_moved_off_the_battlefield_before_the_armor_is_cast() {
    for mv in &MOVES_OFF_THE_BATTLEFIELD {
        let (mut runner, ghalta, armor, spell) = ghalta_on_the_battlefield_with(mv);

        move_commander(&mut runner, spell, ghalta, mv.prompt);
        assert_eq!(
            runner.state().objects[&ghalta].zone,
            mv.zone,
            "{}",
            mv.spell
        );

        runner.cast(armor).resolve();
        assert_germ(runner.state(), armor, 12);
        assert_priority(&runner, P0);
    }
}

/// CR 611.3a + CR 903.3e: a live Germ's bonus keeps reading the commander as it moves,
/// and state-based actions don't see a 0/0.
#[test]
fn live_germ_keeps_its_bonus_as_the_commander_moves() {
    for mv in &MOVES_OFF_THE_BATTLEFIELD {
        let (mut runner, ghalta, armor, spell) = ghalta_on_the_battlefield_with(mv);
        runner.cast(armor).resolve();
        assert_germ(runner.state(), armor, 12);

        move_commander(&mut runner, spell, ghalta, mv.prompt);

        assert_eq!(
            runner.state().objects[&ghalta].zone,
            mv.zone,
            "{}",
            mv.spell
        );
        assert_germ(runner.state(), armor, 12);
        assert_priority(&runner, P0);
    }
}

/// CR 903.9a + CR 903.9b: both routes back to the command zone keep the bonus.
#[test]
fn live_germ_keeps_its_bonus_when_the_commander_returns_to_the_command_zone() {
    for mv in &MOVES_TO_THE_COMMAND_ZONE {
        let (mut runner, ghalta, armor, spell) = ghalta_on_the_battlefield_with(mv);
        runner.cast(armor).resolve();

        move_commander(&mut runner, spell, ghalta, mv.prompt);

        assert_eq!(
            runner.state().objects[&ghalta].zone,
            mv.zone,
            "{}",
            mv.spell
        );
        assert_germ(runner.state(), armor, 12);
        assert_priority(&runner, P0);
    }
}

/// CR 903.3e: the commander counts on the stack, and state-based actions run before
/// its controller receives priority with the spell on the stack. Ghalta: "This spell
/// costs {X} less to cast, where X is the total power of creatures you control." The
/// 12/12 Germ pays for {10}, so an exact {G}{G} pool also checks the Germ's power
/// during the cast.
#[test]
fn live_germ_keeps_its_bonus_while_the_commander_is_cast() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Hand, db());
    scenario.with_commander(ghalta);
    let armor = add_armor(&mut scenario, P0);
    scenario.with_mana_pool(P0, pool(&[ManaType::Green, ManaType::Green]));
    let mut runner = scenario.build();
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 12);

    let commit = runner.cast(ghalta).commit();
    let state = commit.state();
    assert_eq!(state.objects[&ghalta].zone, Zone::Stack);
    assert!(state.stack.iter().any(|entry| entry.id == ghalta));
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: P0 }),
        "{:?}",
        state.waiting_for
    );
    assert_germ(state, armor, 12);
    commit.resolve();

    assert_eq!(runner.state().objects[&ghalta].zone, Zone::Battlefield);
    assert_germ(runner.state(), armor, 12);
    assert_priority(&runner, P0);
}

/// Kurbis, Harvest Celebrant ({X}{G}{G}) as P0's commander, the Armor resolved, and
/// Counterspell in P1's hand. CR 202.3e: X is 0 off the stack, so the Germ starts 2/2.
fn kurbis_game() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = commander_game();
    let kurbis = scenario.add_real_card(P0, "Kurbis, Harvest Celebrant", Zone::Hand, db());
    scenario.with_commander(kurbis);
    let armor = add_armor(&mut scenario, P0);
    let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db());
    scenario.with_mana_pool(P0, pool(&[ManaType::Green; 5]));
    scenario.with_mana_pool(P1, pool(&[ManaType::Blue; 2]));
    let mut runner = scenario.build();
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 2);
    (runner, kurbis, armor, counterspell)
}

/// CR 202.3e + CR 903.3e: X counts in Kurbis's mana value only while it is on the
/// stack, so the Germ is 5/5 there and 2/2 after Kurbis resolves.
#[test]
fn x_counts_on_the_stack_and_not_after_the_commander_resolves() {
    let (mut runner, kurbis, armor, _) = kurbis_game();

    let commit = runner.cast(kurbis).x(3).commit();
    assert_eq!(commit.state().objects[&kurbis].zone, Zone::Stack);
    assert_germ(commit.state(), armor, 5);
    commit.resolve();

    assert_eq!(runner.state().objects[&kurbis].zone, Zone::Battlefield);
    assert_germ(runner.state(), armor, 2);
}

/// CR 202.3e: when Counterspell moves Kurbis from the stack to the graveyard, X stops
/// counting at once. The layers must re-read the population on that zone change.
#[test]
fn x_stops_counting_when_the_commander_spell_is_countered() {
    let (mut runner, kurbis, armor, counterspell) = kurbis_game();

    let mut commit = runner.cast(kurbis).x(3).commit();
    assert_germ(commit.state(), armor, 5);
    commit.act(GameAction::PassPriority).unwrap();
    commit.cast(counterspell).target_object(kurbis).resolve();

    assert_eq!(runner.state().objects[&kurbis].zone, Zone::Graveyard);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_germ(runner.state(), armor, 2);

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_germ(runner.state(), armor, 2);
}

/// Puts a P0-owned Cactus Preserve on the battlefield under `activator`'s control,
/// has `activator` activate its {3} ability during P0's main phase, then asserts
/// that once the ability resolves the land has `expected` power and toughness and
/// is still on the battlefield.
#[track_caller]
fn assert_cactus_preserve_becomes(
    mut scenario: GameScenario,
    activator: PlayerId,
    designated: Option<ObjectId>,
    expected: i32,
) {
    let land = scenario
        .add_land_from_oracle(P0, "Cactus Preserve", CACTUS_PRESERVE)
        .controlled_by(activator)
        .id();
    scenario.with_mana_pool(activator, pool(&[ManaType::Colorless; 3]));
    let mut runner = scenario.build();
    if let Some(commander) = designated {
        designate(&mut runner, commander);
    }
    if activator != P0 {
        // CR 117.3d: P0, the active player, passes, and the next player in turn
        // order receives priority.
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == activator),
        "{:?}",
        runner.state().waiting_for
    );

    runner.activate(land, 1).resolve();

    let object = &runner.state().objects[&land];
    assert_eq!((object.owner, object.controller), (P0, activator));
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(
        (object.power, object.toughness),
        (Some(expected), Some(expected))
    );
    assert_priority(&runner, P0);
}

/// CR 903.3e + CR 109.5: "your" on an activated ability is the player who activated
/// it, and that player's commander counts wherever it is. With P0's Krenko (4) and
/// P1's Ghalta (12) in the command zone, X is 4 when P0 activates the land and 12 when
/// P1 controls and activates the land P0 owns.
#[test]
fn cactus_preserve_reads_the_activators_commander() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Hand, db());
    scenario.with_commander(ghalta);
    assert_cactus_preserve_becomes(scenario, P0, None, 12);

    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Battlefield, db());
    assert_cactus_preserve_becomes(scenario, P0, Some(ghalta), 12);

    for (activator, expected) in [(P0, 4), (P1, 12)] {
        let mut scenario = commander_game();
        let krenko = scenario.add_real_card(P0, KRENKO, Zone::Hand, db());
        scenario.with_commander(krenko);
        let ghalta = scenario.add_real_card(P1, GHALTA, Zone::Hand, db());
        scenario.with_commander(ghalta);
        assert_cactus_preserve_becomes(scenario, activator, None, expected);
    }
}

/// CR 903.3e: both partners are read, each in its own zone, and "greatest" takes the
/// larger. Kydele, Chosen of Kruphix is 4 and Tymna the Weaver is 3, so a
/// battlefield-only misread gives 3 in the first row and a sum gives 7 in both.
#[test]
fn the_greatest_of_two_commanders_in_different_zones_counts() {
    let mut scenario = commander_game();
    let kydele = scenario.add_real_card(P0, "Kydele, Chosen of Kruphix", Zone::Hand, db());
    scenario.with_commander(kydele);
    let tymna = scenario.add_real_card(P0, "Tymna the Weaver", Zone::Battlefield, db());
    let armor = add_armor(&mut scenario, P0);
    let mut runner = scenario.build();
    designate(&mut runner, tymna);
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 4);

    let mut scenario = commander_game();
    let kydele = scenario.add_real_card(P0, "Kydele, Chosen of Kruphix", Zone::Battlefield, db());
    let tymna = scenario.add_real_card(P0, "Tymna the Weaver", Zone::Library, db());
    let armor = add_armor(&mut scenario, P0);
    let mut runner = scenario.build();
    designate(&mut runner, kydele);
    designate(&mut runner, tymna);
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 4);
}

/// CR 903.3e + CR 704.5f: P1's commander is not P0's, so X is 0 and the Germ dies,
/// leaving the Armor unattached. `TokenCreated` proves the Germ existed.
#[test]
fn only_an_opponents_commander_reads_zero() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P1, GHALTA, Zone::Hand, db());
    scenario.with_commander(ghalta);
    let armor = add_armor(&mut scenario, P0);
    let mut runner = scenario.build();

    let outcome = runner.cast(armor).resolve();

    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::TokenCreated { .. })));
    assert_eq!(germ(runner.state()), None);
    assert_eq!(runner.state().objects[&armor].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&armor].attached_to, None);
}

/// CR 903.3 + CR 108.3: the designation belongs to the owner's card, so P0's commander
/// still counts while P1 controls it.
#[test]
fn a_commander_you_own_that_an_opponent_controls_counts() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Battlefield, db());
    let armor = add_armor(&mut scenario, P0);
    let hijack = scenario.add_real_card(P1, "Hijack", Zone::Hand, db());
    scenario.with_mana_pool(P1, pool(&[ManaType::Red; 3]));
    let mut runner = scenario.build();
    designate(&mut runner, ghalta);
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 12);

    give_sorcery_timing(&mut runner, P1);
    runner.cast(hijack).target_object(ghalta).resolve();

    assert_eq!(runner.state().objects[&ghalta].controller, P1);
    assert_germ(runner.state(), armor, 12);
}

/// CR 903.3 + CR 108.3: controlling P1's commander doesn't make it P0's commander, so
/// X is 0 and the Germ dies (CR 704.5f). `TokenCreated` proves the Germ existed.
#[test]
fn a_commander_you_control_but_dont_own_does_not_count() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P1, GHALTA, Zone::Battlefield, db());
    let armor = add_armor(&mut scenario, P0);
    let hijack = scenario.add_real_card(P0, "Hijack", Zone::Hand, db());
    scenario.with_mana_pool(P0, pool(&[ManaType::Red; 3]));
    let mut runner = scenario.build();
    designate(&mut runner, ghalta);

    runner.cast(hijack).target_object(ghalta).resolve();
    assert_eq!(runner.state().objects[&ghalta].controller, P0);
    assert_eq!(runner.state().objects[&ghalta].owner, P1);

    let outcome = runner.cast(armor).resolve();

    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::TokenCreated { .. })));
    assert_eq!(germ(runner.state()), None);
}

/// CR 109.5: for a static ability, "your" is the current controller of the object it's
/// on, so when P1 takes the Armor, X goes from P0's Ghalta (12) to P1's Krenko (4).
#[test]
fn the_reading_follows_the_armors_controller() {
    let mut scenario = commander_game();
    let ghalta = scenario.add_real_card(P0, GHALTA, Zone::Hand, db());
    scenario.with_commander(ghalta);
    let krenko = scenario.add_real_card(P1, KRENKO, Zone::Hand, db());
    scenario.with_commander(krenko);
    let armor = add_armor(&mut scenario, P0);
    let hijack = scenario.add_real_card(P1, "Hijack", Zone::Hand, db());
    scenario.with_mana_pool(P1, pool(&[ManaType::Red; 3]));
    let mut runner = scenario.build();
    runner.cast(armor).resolve();
    assert_germ(runner.state(), armor, 12);

    give_sorcery_timing(&mut runner, P1);
    runner.cast(hijack).target_object(armor).resolve();

    assert_eq!(runner.state().objects[&armor].controller, P1);
    assert_germ(runner.state(), armor, 4);
}
