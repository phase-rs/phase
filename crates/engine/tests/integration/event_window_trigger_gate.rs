//! CR 603.2 (#9179 A, #8124, #6921): the "during <window>" qualifier of a
//! dies/enters event-trigger head is part of the trigger event. It gates the
//! trigger when the event happens and is never rechecked on resolution
//! (CR 603.4 rechecks only an intervening "if").
//!
//! Cards are built from their verbatim MTGJSON `AtomicCards` Oracle text and
//! driven through `GameScenario` / `GameRunner` (`apply()`), so every assertion
//! is a game-state delta, not a re-read of the parse.
//!
//! Base behavior that flips: the window was dropped (`condition: null`), so
//! Mongrel Pack made Dogs on a main-phase death, Vogar counted deaths on a
//! turn that was not its controller's, The Blue Spirit drew for main-phase
//! entries, Noctis attached outside combat, Foe-liage counted lands on an
//! opponent's turn and Jenova drew on an opponent's turn.
//!
//! "Not your turn" is staged by splitting control — the gated permanent is
//! controlled by P1 while P0 is the active player — so no test writes
//! `active_player` or `waiting_for` through `state_mut()`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MURDER: &str = "Destroy target creature.";
const MONGREL_PACK: &str =
    "When this creature dies during combat, create four 1/1 green Dog creature tokens.";
const VOGAR: &str = "Menace\nWhenever another creature dies during your turn, put a +1/+1 counter on Vogar.\nWhen Vogar dies, draw a card for each +1/+1 counter on it.";
const BLUE_SPIRIT: &str = "You may cast the first creature spell you cast each turn as though it had flash.\nWhenever a nontoken creature you control enters during combat, draw a card.";
const NOCTIS: &str = "Whenever a creature you control enters during combat, you may attach target Equipment you control to target creature you control.\nWarp-Strike — {3}: Exile Noctis. Return it to the battlefield under its owner's control tapped and attacking at the beginning of that player's next declare attackers step. It can't be blocked that combat.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";
const FOE_LIAGE: &str =
    "Whenever a land enters during your turn, put a +1/+1 counter on this creature.";
const JENOVA: &str = "At the beginning of combat on your turn, put a number of +1/+1 counters equal to Jenova's power on up to one other target creature. That creature becomes a Mutant in addition to its other types.\nWhenever a Mutant you control dies during your turn, you draw cards equal to its power.";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";

const LIBRARY: [&str; 6] = ["Island", "Island", "Island", "Island", "Island", "Island"];

fn dogs_controlled_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.name == "Dog" && obj.controller == player)
        .count()
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Move from precombat main to the declare-attackers step through the real
/// turn structure and declare `attacks` (CR 507 → CR 508.1).
fn declare_attackers(runner: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    runner.pass_both_players();
    runner
        .declare_attackers(attacks)
        .expect("the attack declaration is legal");
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
}

/// Pass priority until the stack holds exactly `names` (bounded).
fn pass_until_stack_is(runner: &mut GameRunner, names: &[&str]) {
    for _ in 0..10 {
        if runner.stack_names() == names {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    assert_eq!(runner.stack_names(), names);
}

// ---------------------------------------------------------------------------
// Mongrel Pack — "dies during combat"
// ---------------------------------------------------------------------------

/// R1: a main-phase death is outside the window — no Dogs.
#[test]
fn mongrel_pack_destroyed_in_main_phase_makes_no_dogs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pack = scenario
        .add_creature_from_oracle(P0, "Mongrel Pack", 4, 1, MONGREL_PACK)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(pack).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the Pack really died (the trigger event happened).
    assert_eq!(zone_of(&runner, pack), Zone::Graveyard);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        dogs_controlled_by(&runner, P0),
        0,
        "CR 603.2: a main-phase death does not match 'dies during combat'"
    );
}

/// R2: a combat-damage death is inside the window — four Dogs.
#[test]
fn mongrel_pack_dying_to_combat_damage_makes_four_dogs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pack = scenario
        .add_creature_from_oracle(P0, "Mongrel Pack", 4, 1, MONGREL_PACK)
        .id();
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    declare_attackers(&mut runner, &[(pack, AttackTarget::Player(P1))]);
    runner.pass_both_players();
    runner
        .declare_blockers(&[(blocker, pack)])
        .expect("the block is legal");
    runner.combat_damage();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, pack), Zone::Graveyard);
    assert_eq!(dogs_controlled_by(&runner, P0), 4);
}

/// R3 (ruling 2011-06-01 / 2004-10-04): the gate is the phase, not the
/// cause — an instant that destroys the Pack during the declare attackers
/// step triggers it.
#[test]
fn mongrel_pack_destroyed_by_a_spell_during_combat_makes_four_dogs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pack = scenario
        .add_creature_from_oracle(P0, "Mongrel Pack", 4, 1, MONGREL_PACK)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    declare_attackers(&mut runner, &[(pack, AttackTarget::Player(P1))]);
    runner.cast(murder).target_object(pack).commit();
    pass_until_stack_is(&mut runner, &["Mongrel Pack"]);
    assert_eq!(zone_of(&runner, pack), Zone::Graveyard);

    runner.resolve_top();
    runner.advance_until_stack_empty();
    assert_eq!(dogs_controlled_by(&runner, P0), 4);
}

/// R4 (CR 603.2 + CR 603.4): the window is read at the event and never
/// rechecked — the stacked trigger carries no resolution condition, and it
/// still resolves when the game has left combat by the time it resolves.
///
/// SYNTHETIC: the phase write models CR 724.2f. Mandate of Peace is
/// implemented (`Effect::EndCombatPhase`), but it exiles every stack object
/// (CR 724.2b). Only a death during the 724.2 process itself would put a
/// "during combat" trigger on the stack in postcombat main, and no fixture
/// produces one, so the mutation is labelled synthetic. Green at base by
/// construction (base had no gate at all); it guards the design — a bare,
/// rechecked window condition fails it.
#[test]
fn mongrel_pack_trigger_is_not_rechecked_after_combat_ends() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pack = scenario
        .add_creature_from_oracle(P0, "Mongrel Pack", 4, 1, MONGREL_PACK)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    declare_attackers(&mut runner, &[(pack, AttackTarget::Player(P1))]);
    runner.cast(murder).target_object(pack).commit();
    pass_until_stack_is(&mut runner, &["Mongrel Pack"]);

    let entry = runner.state().stack.last().expect("the dies trigger");
    assert!(
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility {
                condition: None,
                ..
            }
        ),
        "CR 603.4: the event window must not become a resolution recheck: {:?}",
        entry.kind
    );

    // SYNTHETIC (CR 724.2f model, see doc comment).
    runner.state_mut().phase = Phase::PostCombatMain;
    runner.resolve_top();
    runner.advance_until_stack_empty();
    assert_eq!(
        dogs_controlled_by(&runner, P0),
        4,
        "a trigger that fired during combat resolves after combat"
    );
}

/// R5 (ruling 2004-10-04): the Pack need not be an attacker or blocker, and
/// "during combat" is not scoped to its controller's turn — P1's Pack dies to
/// P0's instant during P0's combat while a different creature attacks.
#[test]
fn mongrel_pack_outside_the_fight_on_another_players_combat_makes_dogs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let pack = scenario
        .add_creature_from_oracle(P1, "Mongrel Pack", 4, 1, MONGREL_PACK)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    declare_attackers(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    runner.cast(murder).target_object(pack).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, pack), Zone::Graveyard);
    assert_eq!(dogs_controlled_by(&runner, P1), 4);
}

// ---------------------------------------------------------------------------
// Vogar, Necropolis Tyrant — "dies during your turn"
// ---------------------------------------------------------------------------

/// R6 (CR 102.1): "your turn" is relative to the trigger's controller. On
/// P0's turn a death counts for P0's Vogar but not for P1's Vogar.
#[test]
fn vogar_counts_deaths_only_on_its_controllers_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario
        .add_creature_from_oracle(P0, "Vogar, Necropolis Tyrant", 4, 4, VOGAR)
        .id();
    let theirs = scenario
        .add_creature_from_oracle(P1, "Vogar, Necropolis Tyrant", 4, 4, VOGAR)
        .id();
    let victim = scenario.add_vanilla(P1, 2, 2);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(victim).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, victim), Zone::Graveyard);
    assert_eq!(
        plus_one_counters(&runner, mine),
        1,
        "positive reach guard: the active player's Vogar counts the death"
    );
    assert_eq!(
        plus_one_counters(&runner, theirs),
        0,
        "CR 102.1: it is not P1's turn, so P1's Vogar does not count it"
    );
}

// ---------------------------------------------------------------------------
// The Blue Spirit — "enters during combat"
// ---------------------------------------------------------------------------

fn blue_spirit_draws_for_flash_creature(phase: Phase) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.add_creature_from_oracle(P0, "The Blue Spirit", 2, 4, BLUE_SPIRIT);
    let bear = scenario.add_creature_to_hand(P0, "Bear", 2, 2).flash().id();
    scenario.with_library_top(P0, &LIBRARY);
    let mut runner = scenario.build();

    let hand_before = hand_size(&runner, P0);
    runner.cast(bear).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, bear), Zone::Battlefield);
    // The Bear left the hand; anything above that is a draw.
    hand_size(&runner, P0) + 1 - hand_before
}

/// R7: a nontoken creature entering in the main phase draws nothing; the same
/// creature entering during combat draws one card.
#[test]
fn blue_spirit_draws_only_for_entries_during_combat() {
    assert_eq!(
        blue_spirit_draws_for_flash_creature(Phase::PreCombatMain),
        0,
        "CR 603.2: a main-phase entry does not match 'enters during combat'"
    );
    assert_eq!(
        blue_spirit_draws_for_flash_creature(Phase::BeginCombat),
        1,
        "positive reach guard: a combat entry draws"
    );
}

/// R7 sibling: tokens entering during combat do not draw (the existing
/// nontoken subject filter still composes with the window).
#[test]
fn blue_spirit_ignores_tokens_entering_during_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    scenario.add_creature_from_oracle(P0, "The Blue Spirit", 2, 4, BLUE_SPIRIT);
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    scenario.with_library_top(P0, &LIBRARY);
    let mut runner = scenario.build();

    let hand_before = hand_size(&runner, P0);
    runner.cast(alarm).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner
            .battlefield_names()
            .iter()
            .filter(|name| name.as_str() == "Soldier")
            .count(),
        2,
        "reach guard: the tokens entered during combat"
    );
    assert_eq!(
        hand_size(&runner, P0) + 1,
        hand_before,
        "no draw for tokens"
    );
}

// ---------------------------------------------------------------------------
// Noctis, Heir Apparent — "enters during combat"
// ---------------------------------------------------------------------------

/// Cast a flash creature in `phase` with Noctis and an unattached Equipment
/// on the battlefield, declaring the Equipment and `host` for the trigger's
/// targets and accepting the optional attach. Returns `(host, the object the
/// Equipment is attached to, final stack size)`.
fn noctis_attach_after_entry(phase: Phase) -> (ObjectId, Option<ObjectId>, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.add_creature_from_oracle(P0, "Noctis, Heir Apparent", 2, 3, NOCTIS);
    let host = scenario.add_vanilla(P0, 2, 2);
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let bear = scenario.add_creature_to_hand(P0, "Bear", 2, 2).flash().id();
    let mut runner = scenario.build();

    runner
        .cast(bear)
        .target_objects(&[equipment, host])
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, bear), Zone::Battlefield);
    let attached = runner.state().objects[&equipment]
        .attached_to
        .as_ref()
        .and_then(|target| target.as_object());
    (host, attached, runner.state().stack.len())
}

/// R8: the optional attach happens for a combat entry and not for a
/// main-phase entry.
#[test]
fn noctis_attaches_only_for_entries_during_combat() {
    let (host, attached, stack_len) = noctis_attach_after_entry(Phase::BeginCombat);
    assert_eq!(
        attached,
        Some(host),
        "positive reach guard: a combat entry attaches the Equipment"
    );
    assert_eq!(stack_len, 0);

    let (_, attached, stack_len) = noctis_attach_after_entry(Phase::PreCombatMain);
    assert_eq!(
        attached, None,
        "CR 603.2: a main-phase entry does not trigger Noctis"
    );
    assert_eq!(stack_len, 0);
}

// ---------------------------------------------------------------------------
// Foe-liage — "a land enters during your turn"
// ---------------------------------------------------------------------------

/// R9 (CR 102.1): P0's land play on P0's turn grows P0's Foe-liage and not
/// P1's Foe-liage.
#[test]
fn foe_liage_counts_lands_only_on_its_controllers_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario
        .add_creature_from_oracle(P0, "Foe-liage", 3, 3, FOE_LIAGE)
        .id();
    let theirs = scenario
        .add_creature_from_oracle(P1, "Foe-liage", 3, 3, FOE_LIAGE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("the land is playable");
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, forest), Zone::Battlefield);
    assert_eq!(
        plus_one_counters(&runner, mine),
        1,
        "positive reach guard: the active player's Foe-liage grows"
    );
    assert_eq!(
        plus_one_counters(&runner, theirs),
        0,
        "CR 102.1: it is not P1's turn, so P1's Foe-liage does not grow"
    );
}

// ---------------------------------------------------------------------------
// Jenova, Ancient Calamity — "a Mutant you control dies during your turn"
// ---------------------------------------------------------------------------

/// R10 (CR 102.1): P1's Jenova does not draw for P1's Mutant dying on P0's
/// turn; P0's Jenova draws the dying Mutant's power on P0's turn.
#[test]
fn jenova_draws_only_for_mutants_dying_on_its_controllers_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Jenova, Ancient Calamity", 1, 5, JENOVA);
    scenario.add_creature_from_oracle(P1, "Jenova, Ancient Calamity", 1, 5, JENOVA);
    let my_mutant = scenario
        .add_creature(P0, "Mutant Brute", 3, 3)
        .with_subtypes(vec!["Mutant"])
        .id();
    let their_mutant = scenario
        .add_creature(P1, "Mutant Grunt", 2, 2)
        .with_subtypes(vec!["Mutant"])
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_library_top(P1, &LIBRARY);
    let mut runner = scenario.build();

    let p1_hand = hand_size(&runner, P1);
    runner.cast(first).target_object(their_mutant).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, their_mutant), Zone::Graveyard);
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand,
        "CR 102.1: it is not P1's turn, so P1's Jenova draws nothing"
    );

    let p0_hand = hand_size(&runner, P0);
    runner.cast(second).target_object(my_mutant).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, my_mutant), Zone::Graveyard);
    assert_eq!(
        hand_size(&runner, P0) + 1,
        p0_hand + 3,
        "positive reach guard: P0's Jenova draws the Mutant's power (3)"
    );
    assert_eq!(hand_size(&runner, P1), p1_hand);
}
