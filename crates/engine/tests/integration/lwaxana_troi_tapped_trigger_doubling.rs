//! Lwaxana Troi, Charming Guest — an object-scoped trigger doubler that lasts
//! for as long as its source remains tapped.
//!
//! Oracle text:
//! "You may choose not to untap Lwaxana Troi during your untap step.
//! {2}, {T}: Choose another target creature you control. For as long as
//! Lwaxana Troi remains tapped, if a triggered ability of that creature
//! triggers, that ability triggers an additional time."
//!
//! CR 603.2d: the chosen creature's triggered abilities trigger an additional
//! time. CR 611.2b: the effect ends as soon as Lwaxana is untapped. CR 611.2c:
//! the effect modifies the rules rather than the creature, so each resolution
//! is its own effect, and CR 603.10a: the chosen creature's own dies trigger
//! looks back to the effect that applied to it as it died.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LWAXANA: &str = "Lwaxana Troi, Charming Guest";
const LWAXANA_ORACLE: &str = "You may choose not to untap Lwaxana Troi during your untap step.\n{2}, {T}: Choose another target creature you control. For as long as Lwaxana Troi remains tapped, if a triggered ability of that creature triggers, that ability triggers an additional time.";
const MOONRISE_CLERIC_ORACLE: &str = "Flying\nWhenever this creature attacks, you gain 1 life.";
const VITALIZE_ORACLE: &str = "Untap all creatures you control.";
const ILLUSIONISTS_BRACERS_ORACLE: &str = "Whenever an ability of equipped creature is activated, if it isn't a mana ability, copy that ability. You may choose new targets for the copy.\nEquip {3}";
const SOLEMN_SIMULACRUM_ORACLE: &str = "When this creature enters, you may search your library for a basic land card, put that card onto the battlefield tapped, then shuffle.\nWhen this creature dies, you may draw a card.";
const MURDER_ORACLE: &str = "Destroy target creature.";
const WRATH_OF_GOD_ORACLE: &str = "Destroy all creatures. They can't be regenerated.";
const PYROCLASM_ORACLE: &str = "Pyroclasm deals 2 damage to each creature.";
const KORVOLD_ORACLE: &str = "Flying\nWhenever Korvold enters or attacks, sacrifice another permanent.\nWhenever you sacrifice a permanent, put a +1/+1 counter on Korvold and draw a card.";
const BARTER_IN_BLOOD_ORACLE: &str = "Each player sacrifices two creatures of their choice.";
const HARMONIC_PRODIGY_ORACLE: &str = "Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nIf a triggered ability of a Shaman or another Wizard you control triggers, that ability triggers an additional time.";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn main_phase(mana: ManaType, amount: usize) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lwaxana = scenario
        .add_creature_from_oracle(P0, LWAXANA, 2, 2, LWAXANA_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(mana, amount)
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Plains"; 5]);
    scenario.with_library_top(P1, &["Plains"; 5]);
    (scenario, lwaxana)
}

fn add_moonrise_cleric(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Moonrise Cleric", 2, 3)
        .with_subtypes(vec!["Bat", "Cleric"])
        .from_oracle_text_with_keywords(&["Flying"], MOONRISE_CLERIC_ORACLE)
        .id()
}

struct Board {
    runner: GameRunner,
    lwaxana: ObjectId,
    chosen: ObjectId,
    other: ObjectId,
    vitalize: ObjectId,
}

fn board() -> Board {
    board_with(|_, _| {})
}

/// Lwaxana, two Moonrise Clerics and Vitalize; `extra` stages anything more
/// (given the scenario and Lwaxana) before the game is built.
fn board_with(extra: impl FnOnce(&mut GameScenario, ObjectId)) -> Board {
    let (mut scenario, lwaxana) = main_phase(ManaType::Green, 3);
    let chosen = add_moonrise_cleric(&mut scenario);
    let other = add_moonrise_cleric(&mut scenario);
    let vitalize = scenario
        .add_spell_to_hand_from_oracle(P0, "Vitalize", true, VITALIZE_ORACLE)
        .id();
    extra(&mut scenario, lwaxana);
    Board {
        runner: scenario.build(),
        lwaxana,
        chosen,
        other,
        vitalize,
    }
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

fn activate_on(board: &mut Board) {
    let (lwaxana, chosen) = (board.lwaxana, board.chosen);
    board
        .runner
        .activate(lwaxana, 0)
        .target_objects(&[chosen])
        .resolve();
    let state = board.runner.state();
    assert!(state.objects[&lwaxana].tapped, "Lwaxana taps to activate");
    assert!(state.stack.is_empty(), "the activation resolves");
}

/// Order and resolve every pending trigger, accepting each "you may", until
/// the stack is empty with priority.
fn resolve_all(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional effect");
            }
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the copy's targets");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert!(runner.state().stack.is_empty(), "every trigger resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Attack with both Clerics and resolve every attack trigger; returns the life
/// gained.
fn attack_with_both(board: &mut Board) -> i32 {
    let before = life(&board.runner);
    board.runner.advance_to_combat();
    board
        .runner
        .declare_attackers(&[
            (board.chosen, AttackTarget::Player(P1)),
            (board.other, AttackTarget::Player(P1)),
        ])
        .expect("both Clerics attack");
    resolve_all(&mut board.runner);
    life(&board.runner) - before
}

#[test]
fn only_the_chosen_creatures_triggers_are_doubled_while_tapped() {
    let mut board = board();
    activate_on(&mut board);
    // CR 603.2d: the chosen Cleric's trigger fires twice (+2); the other
    // Cleric's trigger is not doubled (+1).
    assert_eq!(attack_with_both(&mut board), 3);
}

#[test]
fn untapping_lwaxana_ends_the_doubling() {
    let mut board = board();
    activate_on(&mut board);
    let vitalize = board.vitalize;
    board.runner.cast(vitalize).resolve();
    assert!(
        !board.runner.state().objects[&board.lwaxana].tapped,
        "Vitalize untaps Lwaxana"
    );
    // CR 611.2b: once Lwaxana is untapped the effect is over — each Cleric's
    // trigger fires once.
    assert_eq!(attack_with_both(&mut board), 2);
}

#[test]
fn without_the_activation_nothing_is_doubled() {
    let mut board = board();
    assert_eq!(attack_with_both(&mut board), 2);
}

/// CR 603.2d + CR 707.10: Illusionist's Bracers copies Lwaxana's activation,
/// so two abilities resolve against the same Cleric. Each creates its own
/// effect, and each makes the Cleric's trigger trigger an additional time.
#[test]
fn a_copied_activation_is_a_second_independent_doubling_effect() {
    let mut bracers = None;
    let mut board = board_with(|scenario, _| {
        bracers = Some(
            scenario
                .add_creature(P0, "Illusionist's Bracers", 0, 0)
                .as_artifact()
                .with_subtypes(vec!["Equipment"])
                .from_oracle_text(ILLUSIONISTS_BRACERS_ORACLE)
                .id(),
        );
    });
    let bracers = bracers.expect("Bracers staged");
    // Fixture: the Equipment starts attached to Lwaxana.
    attach_to(board.runner.state_mut(), bracers, board.lwaxana);

    let (lwaxana, chosen) = (board.lwaxana, board.chosen);
    board
        .runner
        .activate(lwaxana, 0)
        .target_objects(&[chosen])
        .resolve();
    resolve_all(&mut board.runner);
    assert!(
        board.runner.state().objects[&lwaxana].tapped,
        "Lwaxana stays tapped"
    );

    // CR 603.2d: the chosen Cleric's trigger fires once plus once per effect
    // (+3); the other Cleric's fires once (+1).
    assert_eq!(attack_with_both(&mut board), 4);
}

/// Whether Lwaxana comes before or after the creature she chose on the
/// battlefield — the order a sweeper or sacrifice moves them in.
#[derive(Clone, Copy)]
enum BattlefieldOrder {
    LwaxanaFirst,
    LwaxanaLast,
}

struct SolemnBoard {
    runner: GameRunner,
    lwaxana: ObjectId,
    solemn: ObjectId,
    spell: ObjectId,
}

/// Lwaxana and Solemn Simulacrum (in `order`), the given sorcery or instant
/// in hand, and `mana` of one type; Lwaxana's ability has resolved on Solemn.
fn solemn_board(
    order: BattlefieldOrder,
    mana: ManaType,
    amount: usize,
    spell: (&str, bool, &str),
) -> SolemnBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let add_solemn = |scenario: &mut GameScenario| {
        scenario
            .add_creature(P0, "Solemn Simulacrum", 2, 2)
            .as_artifact_creature()
            .with_subtypes(vec!["Golem"])
            .from_oracle_text(SOLEMN_SIMULACRUM_ORACLE)
            .id()
    };
    let add_lwaxana = |scenario: &mut GameScenario| {
        scenario
            .add_creature_from_oracle(P0, LWAXANA, 2, 2, LWAXANA_ORACLE)
            .id()
    };
    let (lwaxana, solemn) = match order {
        BattlefieldOrder::LwaxanaFirst => {
            let lwaxana = add_lwaxana(&mut scenario);
            (lwaxana, add_solemn(&mut scenario))
        }
        BattlefieldOrder::LwaxanaLast => {
            let solemn = add_solemn(&mut scenario);
            (add_lwaxana(&mut scenario), solemn)
        }
    };
    let (name, is_instant, oracle) = spell;
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
        .id();
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(mana, amount)
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Plains"; 5]);
    scenario.with_library_top(P1, &["Plains"; 5]);
    let mut runner = scenario.build();
    runner
        .activate(lwaxana, 0)
        .target_objects(&[solemn])
        .resolve();
    assert!(runner.state().objects[&lwaxana].tapped, "Lwaxana taps");
    assert!(runner.state().stack.is_empty(), "the activation resolves");
    SolemnBoard {
        runner,
        lwaxana,
        solemn,
        spell,
    }
}

/// Triggered abilities of `source` on the stack.
fn triggered_abilities_of(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

/// Resolves the committed kill spell until Solemn's dies triggers are on the
/// stack, then asserts how many there are and that each draws a card.
fn assert_solemn_dies_twice(board: &mut SolemnBoard) {
    let library_before = board.runner.state().players[0].library.len();
    for _ in 0..16 {
        if triggered_abilities_of(&board.runner, board.solemn) > 0
            || matches!(
                board.runner.state().waiting_for,
                WaitingFor::OrderTriggers { .. }
            )
        {
            break;
        }
        board.runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        board.runner.state().objects[&board.solemn].zone,
        Zone::Graveyard,
        "Solemn died"
    );
    if matches!(
        board.runner.state().waiting_for,
        WaitingFor::OrderTriggers { .. }
    ) {
        drain_order_triggers_with_identity(board.runner.state_mut());
    }
    assert_eq!(
        triggered_abilities_of(&board.runner, board.solemn),
        2,
        "Solemn's dies trigger triggers an additional time"
    );
    resolve_all(&mut board.runner);
    assert_eq!(
        library_before - board.runner.state().players[0].library.len(),
        2,
        "each instance offers a draw, and both are accepted"
    );
}

/// CR 603.10a + CR 603.2d: the chosen creature's own dies trigger looks back
/// to the moment it died, when Lwaxana's effect still applied to it.
#[test]
fn the_chosen_creatures_dies_trigger_triggers_an_additional_time() {
    let mut board = solemn_board(
        BattlefieldOrder::LwaxanaFirst,
        ManaType::Black,
        5,
        ("Murder", true, MURDER_ORACLE),
    );
    let (murder, solemn) = (board.spell, board.solemn);
    board.runner.cast(murder).target_object(solemn).commit();
    assert_solemn_dies_twice(&mut board);
    assert!(
        board.runner.state().objects[&board.lwaxana].tapped,
        "Lwaxana survives, still tapped"
    );
}

/// CR 603.10a: Lwaxana dying in the same event as the chosen creature still
/// applied immediately before it (cf. the Teysa Karlov ruling: a creature
/// dying at the same time as the doubler still triggers an additional time),
/// whichever of the two the sweeper moves first.
fn wrath_kills_both(order: BattlefieldOrder) {
    let mut board = solemn_board(
        order,
        ManaType::White,
        6,
        ("Wrath of God", false, WRATH_OF_GOD_ORACLE),
    );
    let wrath = board.spell;
    board.runner.cast(wrath).commit();
    assert_solemn_dies_twice(&mut board);
    assert_eq!(
        board.runner.state().objects[&board.lwaxana].zone,
        Zone::Graveyard,
        "Lwaxana died in the same event"
    );
}

#[test]
fn wrath_of_god_lwaxana_first_still_doubles_the_dies_trigger() {
    wrath_kills_both(BattlefieldOrder::LwaxanaFirst);
}

#[test]
fn wrath_of_god_solemn_first_still_doubles_the_dies_trigger() {
    wrath_kills_both(BattlefieldOrder::LwaxanaLast);
}

/// CR 704.3 + CR 603.10a: the same look-back for a state-based-action death
/// batch — Pyroclasm's 2 damage kills both 2-toughness creatures at once.
fn pyroclasm_kills_both(order: BattlefieldOrder) {
    let mut board = solemn_board(
        order,
        ManaType::Red,
        4,
        ("Pyroclasm", false, PYROCLASM_ORACLE),
    );
    let pyroclasm = board.spell;
    board.runner.cast(pyroclasm).commit();
    assert_solemn_dies_twice(&mut board);
    assert_eq!(
        board.runner.state().objects[&board.lwaxana].zone,
        Zone::Graveyard,
        "Lwaxana died in the same state-based-action pass"
    );
}

#[test]
fn pyroclasm_lwaxana_first_still_doubles_the_dies_trigger() {
    pyroclasm_kills_both(BattlefieldOrder::LwaxanaFirst);
}

#[test]
fn pyroclasm_solemn_first_still_doubles_the_dies_trigger() {
    pyroclasm_kills_both(BattlefieldOrder::LwaxanaLast);
}

/// CR 603.10a: "abilities that trigger when a player sacrifices a permanent"
/// look back in time. Barter in Blood sacrifices Lwaxana and Korvold, Fae-
/// Cursed King together. Korvold's trigger for its own sacrifice fires on the
/// sacrifice event, not a zone change, and Lwaxana's effect, which applied to
/// Korvold immediately before, still doubles it — whichever of the two is
/// sacrificed first.
fn barter_sacrifices_both(order: BattlefieldOrder) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let add_korvold = |scenario: &mut GameScenario| {
        scenario
            .add_creature(P0, "Korvold, Fae-Cursed King", 4, 4)
            .with_subtypes(vec!["Dragon", "Noble"])
            .from_oracle_text_with_keywords(&["Flying"], KORVOLD_ORACLE)
            .id()
    };
    let add_lwaxana = |scenario: &mut GameScenario| {
        scenario
            .add_creature_from_oracle(P0, LWAXANA, 2, 2, LWAXANA_ORACLE)
            .id()
    };
    let (lwaxana, korvold) = match order {
        BattlefieldOrder::LwaxanaFirst => {
            let lwaxana = add_lwaxana(&mut scenario);
            (lwaxana, add_korvold(&mut scenario))
        }
        BattlefieldOrder::LwaxanaLast => {
            let korvold = add_korvold(&mut scenario);
            (add_lwaxana(&mut scenario), korvold)
        }
    };
    let barter = scenario
        .add_spell_to_hand_from_oracle(P0, "Barter in Blood", false, BARTER_IN_BLOOD_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Black, 6)
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Plains"; 8]);
    scenario.with_library_top(P1, &["Plains"; 5]);
    let mut runner = scenario.build();
    runner
        .activate(lwaxana, 0)
        .target_objects(&[korvold])
        .resolve();
    assert!(runner.state().objects[&lwaxana].tapped, "Lwaxana taps");
    let library_before = runner.state().players[0].library.len();

    runner.cast(barter).commit();
    for _ in 0..16 {
        if triggered_abilities_of(&runner, korvold) > 0
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&lwaxana].zone,
        Zone::Graveyard,
        "Lwaxana sacrificed"
    );
    assert_eq!(
        state.objects[&korvold].zone,
        Zone::Graveyard,
        "Korvold sacrificed"
    );
    let for_own_sacrifice = state
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == korvold
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility {
                        trigger_event: Some(GameEvent::PermanentSacrificed { object_id, .. }),
                        ..
                    } if *object_id == korvold
                )
        })
        .count();
    assert_eq!(
        for_own_sacrifice, 2,
        "Korvold's trigger for its own sacrifice is doubled by Lwaxana"
    );
    let korvold_triggers = triggered_abilities_of(&runner, korvold);
    resolve_all(&mut runner);
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        korvold_triggers,
        "each of Korvold's triggers draws a card"
    );
}

#[test]
fn barter_in_blood_lwaxana_first_doubles_korvolds_sacrifice_triggers() {
    barter_sacrifices_both(BattlefieldOrder::LwaxanaFirst);
}

#[test]
fn barter_in_blood_korvold_first_doubles_korvolds_sacrifice_triggers() {
    barter_sacrifices_both(BattlefieldOrder::LwaxanaLast);
}

/// CR 611.2b + CR 603.10a: Lwaxana dying in an EARLIER event ended the
/// effect, so the chosen creature's later death looks back to no doubler.
#[test]
fn lwaxana_dying_first_in_a_separate_event_ends_the_doubling() {
    let (mut scenario, lwaxana) = main_phase(ManaType::Black, 8);
    let solemn = scenario
        .add_creature(P0, "Solemn Simulacrum", 2, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .from_oracle_text(SOLEMN_SIMULACRUM_ORACLE)
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .activate(lwaxana, 0)
        .target_objects(&[solemn])
        .resolve();
    runner.cast(first).target_object(lwaxana).resolve();
    assert_eq!(runner.state().objects[&lwaxana].zone, Zone::Graveyard);

    runner.cast(second).target_object(solemn).commit();
    for _ in 0..16 {
        if triggered_abilities_of(&runner, solemn) > 0 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(runner.state().objects[&solemn].zone, Zone::Graveyard);
    assert_eq!(
        triggered_abilities_of(&runner, solemn),
        1,
        "the effect ended with Lwaxana's earlier death"
    );
}

/// CR 603.2d: Harmonic Prodigy's own doubler covers "a Shaman or another
/// Wizard", never itself; Lwaxana's effect on it still counts, so its prowess
/// triggers twice — once more, not twice more.
#[test]
fn a_chosen_doubler_gains_one_occurrence_from_lwaxana_not_from_itself() {
    let (mut scenario, lwaxana) = main_phase(ManaType::Red, 3);
    let prodigy = scenario
        .add_creature(P0, "Harmonic Prodigy", 1, 3)
        .with_subtypes(vec!["Human", "Wizard"])
        .from_oracle_text_with_keywords(&["Prowess"], HARMONIC_PRODIGY_ORACLE)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner
        .activate(lwaxana, 0)
        .target_objects(&[prodigy])
        .resolve();
    assert!(runner.state().objects[&lwaxana].tapped);

    runner.cast(shock).target_player(P1).commit();
    let prowess = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == prodigy
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();
    assert_eq!(prowess, 2, "prowess triggers once more for Lwaxana");

    resolve_all(&mut runner);
    let state = runner.state();
    assert_eq!(state.players[1].life, 18, "Shock resolves");
    assert_eq!(
        (
            state.objects[&prodigy].power,
            state.objects[&prodigy].toughness
        ),
        (Some(3), Some(5)),
        "two prowess resolutions: +2/+2"
    );
}
