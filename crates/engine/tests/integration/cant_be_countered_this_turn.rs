//! Runtime probes for turn-long rules effects that prevent countering spells.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text verified against data/mtgjson/AtomicCards.json.
// Determined is the Determined face of Bound // Determined; Grizzly Bears has no text.
const VEIL: &str = "Draw a card if an opponent has cast a blue or black spell this turn. Spells you control can't be countered this turn. You and permanents you control gain hexproof from blue and from black until end of turn. (You and they can't be the targets of blue or black spells or abilities your opponents control.)";
const DETERMINED: &str = "Other spells you control can't be countered this turn.\nDraw a card.";
const COUNTERSPELL: &str = "Counter target spell.";

fn fixture(
    protection: Option<(&str, &str, Vec<ManaCostShard>)>,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    // Determined draws unconditionally; keep the draw from ending the game.
    scenario.with_library_top(P0, &["Forest", "Island"]);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let counter = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let protection = protection.map(|(name, text, shards)| {
        scenario
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .with_mana_cost(ManaCost::Cost { generic: 0, shards })
            .id()
    });
    (scenario.build(), creature, counter, protection)
}

fn assert_priority(runner: &GameRunner, player: engine::types::player::PlayerId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: actual } if actual == player),
        "expected priority for {player:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn counter_creature(
    runner: &mut GameRunner,
    creature: ObjectId,
    counter: ObjectId,
    expected_zone: Zone,
) {
    assert_priority(runner, P0);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes to P1");
    assert_priority(runner, P1);
    runner.cast(counter).target_object(creature).commit();
    assert_eq!(runner.state().objects[&counter].zone, Zone::Stack);
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == counter)
        .expect("Counterspell on stack");
    assert_eq!(
        entry.ability().expect("Counterspell ability").targets,
        vec![TargetRef::Object(creature)]
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&counter].zone, Zone::Graveyard);
    // CR 101.2: a prohibition on countering takes precedence over Counterspell.
    // CR 701.6a: a successfully countered spell leaves the stack for its owner's graveyard.
    let intermediate_zone = if expected_zone == Zone::Battlefield {
        Zone::Stack
    } else {
        Zone::Graveyard
    };
    assert_eq!(runner.state().objects[&creature].zone, intermediate_zone);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&creature].zone, expected_zone);
    assert_eq!(
        runner.state().battlefield.contains(&creature),
        expected_zone == Zone::Battlefield
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn veil_of_summer_protects_a_creature_spell_cast_afterward() {
    let (mut runner, creature, counter, veil) =
        fixture(Some(("Veil of Summer", VEIL, vec![ManaCostShard::Green])));
    let veil = veil.unwrap();
    let outcome = runner.cast(veil).resolve();
    outcome.assert_zone(&[veil], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    // CR 611.2c: a rules-changing continuous effect can affect later objects.
    runner.cast(creature).commit();
    counter_creature(&mut runner, creature, counter, Zone::Battlefield);
}

#[test]
fn veil_of_summer_protects_a_spell_already_on_the_stack() {
    let (mut runner, creature, counter, veil) =
        fixture(Some(("Veil of Summer", VEIL, vec![ManaCostShard::Green])));
    runner.cast(creature).commit();
    // CR 117.3c: the caster retains priority and can cast Veil above the creature.
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    let veil = veil.unwrap();
    runner.cast(veil).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&veil].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
    counter_creature(&mut runner, creature, counter, Zone::Battlefield);
}

#[test]
fn determined_protects_other_spells() {
    let (mut runner, creature, counter, determined) = fixture(Some((
        "Determined",
        DETERMINED,
        vec![ManaCostShard::Green, ManaCostShard::Blue],
    )));
    runner.cast(creature).commit();
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    let determined = determined.unwrap();
    runner.cast(determined).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&determined].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
    counter_creature(&mut runner, creature, counter, Zone::Battlefield);
}

#[test]
fn counterspell_counters_a_creature_without_protection() {
    let (mut runner, creature, counter, _) = fixture(None);
    runner.cast(creature).commit();
    counter_creature(&mut runner, creature, counter, Zone::Graveyard);
}

const SHUSHER: &str = "This spell can't be countered.\n{R/G}: Target spell can't be countered.";

#[test]
fn vexing_shusher_protects_its_target_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let lands = [scenario.add_basic_land(P0, ManaColor::Red)];
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let shusher = scenario
        .add_creature_from_oracle(P0, "Vexing Shusher", 2, 2, SHUSHER)
        .id();
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let counter = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes to P1");
    assert_priority(&runner, P1);
    runner.cast(counter).target_object(creature).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes to P0");
    assert_priority(&runner, P0);
    // CR 101.2: the activation resolves above Counterspell, so Counterspell
    // then fails to counter its target, and the creature resolves.
    runner
        .activate(shusher, 0)
        .target_object(creature)
        .pay_with(&lands)
        .resolve();
    assert!(
        runner.state().objects[&lands[0]].tapped,
        "Shusher activation paid"
    );
    assert_eq!(runner.state().objects[&counter].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    // CR 400.7: the targeted rule retains the STACK incarnation, even though
    // characteristic grants can follow a permanent spell onto the battlefield.
    let pin = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find_map(|effect| {
            effect
                .affected_recipient
                .filter(|pin| pin.object_id == creature)
        })
        .expect("Shusher's rule was bound to its target incarnation");
    assert_ne!(
        pin.incarnation,
        runner.state().objects[&creature].incarnation
    );
}

const DOMRI: &str = "Creatures you control get +1/+0.\n[+1]: Add {R} or {G}. Creature spells you cast this turn can't be countered.\n[−2]: Target creature you control fights target creature you don't control.";

const SWIFT_SILENCE: &str =
    "Counter all other spells. Draw a card for each spell countered this way.";

const MANA_LEAK: &str = "Counter target spell unless its controller pays {3}.";

const ARCHMAGE: &str = "Flying\n{U}, Sacrifice this creature: Counter target noncreature spell.\nPersist (When this creature dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";

const CONTRADICT: &str = "Counter target spell.\nDraw a card.";

const TYRANT: &str = "This spell can't be countered.\nTrample, hexproof";

const SHEPHERD: &str = "This spell can't be countered.\nGreen spells you control can't be countered.\n{4}{G}{G}: Until end of turn, each Elf creature you control has base power and toughness 5/5 and becomes a Dinosaur in addition to its other creature types.";

const INSIST: &str = "The next creature spell you cast this turn can't be countered.\nDraw a card.";

const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";

const AETHERSNATCH: &str = "Gain control of target spell. You may choose new targets for it. (If that spell becomes a permanent, it enters under your control.)";

const DOWNFALL: &str = "Destroy target creature or planeswalker.";

const DOUBLE_MAJOR: &str = "Copy target creature spell you control, except it isn't legendary if the spell is legendary. (A copy of a creature spell becomes a token.)";

fn matrix_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for color in [
            ManaColor::Blue,
            ManaColor::Green,
            ManaColor::Red,
            ManaColor::White,
            ManaColor::Black,
        ] {
            for _ in 0..8 {
                scenario.add_basic_land(player, color);
            }
        }
        scenario.with_library_top(
            player,
            &["Forest", "Island", "Forest", "Island", "Forest", "Island"],
        );
    }
    scenario
}

fn spell(
    scenario: &mut GameScenario,
    player: engine::types::player::PlayerId,
    name: &str,
    text: &str,
    generic: u32,
    shards: Vec<ManaCostShard>,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(ManaCost::Cost { generic, shards })
        .id()
}

fn bears(scenario: &mut GameScenario, player: engine::types::player::PlayerId) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(player, "Grizzly Bears", 2, 2, "")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id()
}

fn veil(scenario: &mut GameScenario) -> ObjectId {
    spell(
        scenario,
        P0,
        "Veil of Summer",
        VEIL,
        0,
        vec![ManaCostShard::Green],
    )
}

fn counterspell(scenario: &mut GameScenario) -> ObjectId {
    spell(
        scenario,
        P1,
        "Counterspell",
        COUNTERSPELL,
        0,
        vec![ManaCostShard::Blue, ManaCostShard::Blue],
    )
}

fn domri(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(P0, "Domri, Anarch of Bolas", "Domri", 3, DOMRI)
        .id()
}

fn activate_domri(runner: &mut GameRunner, domri: ObjectId) {
    let outcome = runner.activate(domri, 0).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&domri].loyalty, Some(4));
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| effect.modifications.iter().any(|m| matches!(
            m,
            engine::types::ability::ContinuousModification::AddStaticMode {
                mode: engine::types::statics::StaticMode::CantBeCountered
            }
        ))));
}

fn pass_to(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    if !matches!(runner.state().waiting_for, WaitingFor::Priority { player: actual } if actual == player)
    {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_priority(runner, player);
}

fn assert_complete(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn swift_silence_counts_only_spells_actually_countered_under_veil() {
    let mut scenario = matrix_scenario();
    let veil = veil(&mut scenario);
    let creature = bears(&mut scenario, P0);
    let own_spell = spell(&mut scenario, P1, "Opt", OPT, 0, vec![ManaCostShard::Blue]);
    let silence = spell(
        &mut scenario,
        P1,
        "Swift Silence",
        SWIFT_SILENCE,
        2,
        vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
        ],
    );
    let mut runner = scenario.build();
    runner.cast(veil).resolve();
    runner.cast(creature).commit();
    pass_to(&mut runner, P1);
    runner.cast(own_spell).commit();
    let hand_before = runner.state().players[1].hand.len();
    runner.cast(silence).commit();
    runner.resolve_top();
    // CR 101.2 + CR 608.2c: only successfully countered spells count for the draw.
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&own_spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&silence].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[1].hand.len(), hand_before);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_complete(&runner);
}

#[test]
fn mana_leak_declined_payment_does_not_counter_under_veil() {
    let mut scenario = matrix_scenario();
    let veil = veil(&mut scenario);
    let creature = bears(&mut scenario, P0);
    let leak = spell(
        &mut scenario,
        P1,
        "Mana Leak",
        MANA_LEAK,
        1,
        vec![ManaCostShard::Blue],
    );
    let mut runner = scenario.build();
    runner.cast(veil).resolve();
    runner.cast(creature).commit();
    pass_to(&mut runner, P1);
    runner.cast(leak).target_object(creature).commit();
    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P0, .. }
    ));
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    // CR 101.2: declining payment cannot overcome the countering prohibition.
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_complete(&runner);
}

#[test]
fn archmage_pays_sacrifice_but_cannot_counter_a_veil_protected_noncreature_spell() {
    let mut scenario = matrix_scenario();
    let veil = veil(&mut scenario);
    let opt = spell(&mut scenario, P0, "Opt", OPT, 0, vec![ManaCostShard::Blue]);
    let archmage = scenario
        .add_creature_from_oracle(P1, "Glen Elendra Archmage", 2, 2, ARCHMAGE)
        .id();
    let mut runner = scenario.build();
    runner.cast(veil).resolve();
    runner.cast(opt).commit();
    pass_to(&mut runner, P1);
    let outcome = runner.activate(archmage, 0).target_object(opt).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event,
        engine::types::events::GameEvent::PermanentSacrificed { object_id, .. } if *object_id == archmage)));
    assert!(
        outcome.events().iter().any(|event| matches!(event,
        engine::types::events::GameEvent::CardDrawn { player_id, .. } if *player_id == P0)),
        "Opt resolved and drew, rather than being countered"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_complete(&runner);
}

#[test]
fn contradict_still_draws_when_countering_is_prohibited() {
    let mut scenario = matrix_scenario();
    let veil = veil(&mut scenario);
    let creature = bears(&mut scenario, P0);
    let contradict = spell(
        &mut scenario,
        P1,
        "Contradict",
        CONTRADICT,
        3,
        vec![ManaCostShard::Blue, ManaCostShard::Blue],
    );
    let mut runner = scenario.build();
    runner.cast(veil).resolve();
    runner.cast(creature).commit();
    pass_to(&mut runner, P1);
    let outcome = runner.cast(contradict).target_object(creature).resolve();
    // CR 608.2c: Counterspell's failed instruction does not cancel a later draw.
    outcome.assert_hand_drawn(P1, 1);
    outcome.assert_zone(&[creature], Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_complete(&runner);
}

#[test]
fn domri_protects_a_later_creature_spell() {
    let mut scenario = matrix_scenario();
    let domri = domri(&mut scenario);
    let creature = bears(&mut scenario, P0);
    let counter = counterspell(&mut scenario);
    let mut runner = scenario.build();
    activate_domri(&mut runner, domri);
    runner.cast(creature).commit();
    counter_creature(&mut runner, creature, counter, Zone::Battlefield);
}

#[test]
fn domri_does_not_protect_noncreature_spells() {
    let mut scenario = matrix_scenario();
    let domri = domri(&mut scenario);
    let opt = spell(&mut scenario, P0, "Opt", OPT, 0, vec![ManaCostShard::Blue]);
    let counter = counterspell(&mut scenario);
    let mut runner = scenario.build();
    activate_domri(&mut runner, domri);
    runner.cast(opt).commit();
    pass_to(&mut runner, P1);
    let outcome = runner.cast(counter).target_object(opt).resolve();
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_zone(&[opt], Zone::Graveyard);
    assert_complete(&runner);
}

#[test]
fn domri_protection_expires_next_turn() {
    let mut scenario = matrix_scenario();
    let domri = domri(&mut scenario);
    let creature = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Ashcoat Bear",
            2,
            2,
            "Flash (You may cast this spell any time you could cast an instant.)",
        )
        .with_keyword(engine::types::keywords::Keyword::Flash)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let counter = counterspell(&mut scenario);
    let mut runner = scenario.build();
    activate_domri(&mut runner, domri);
    let turn = runner.state().turn_number;
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(runner.state().turn_number > turn);
    assert_eq!(runner.state().active_player, P1);
    pass_to(&mut runner, P0);
    runner.cast(creature).commit();
    counter_creature(&mut runner, creature, counter, Zone::Graveyard);
}

#[test]
fn domri_rule_outlives_source_and_only_protects_its_activator() {
    let mut scenario = matrix_scenario();
    let domri = domri(&mut scenario);
    let ours = bears(&mut scenario, P0);
    let theirs = scenario
        .add_creature_to_hand_from_oracle(
            P1,
            "Ashcoat Bear",
            2,
            2,
            "Flash (You may cast this spell any time you could cast an instant.)",
        )
        .with_keyword(engine::types::keywords::Keyword::Flash)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let counter_ours = counterspell(&mut scenario);
    let counter_theirs = spell(
        &mut scenario,
        P0,
        "Counterspell",
        COUNTERSPELL,
        0,
        vec![ManaCostShard::Blue, ManaCostShard::Blue],
    );
    let downfall = spell(
        &mut scenario,
        P1,
        "Hero's Downfall",
        DOWNFALL,
        1,
        vec![ManaCostShard::Black, ManaCostShard::Black],
    );
    let mut runner = scenario.build();
    activate_domri(&mut runner, domri);
    pass_to(&mut runner, P1);
    runner.cast(downfall).target_object(domri).resolve();
    assert_eq!(runner.state().objects[&domri].zone, Zone::Graveyard);
    pass_to(&mut runner, P0);
    runner.cast(ours).commit();
    counter_creature(&mut runner, ours, counter_ours, Zone::Battlefield);
    pass_to(&mut runner, P1);
    runner.cast(theirs).commit();
    pass_to(&mut runner, P0);
    runner.cast(counter_theirs).target_object(theirs).resolve();
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Graveyard);
    assert_complete(&runner);
}

#[test]
fn domri_follows_caster_while_veil_follows_live_controller() {
    for use_domri in [false, true] {
        let mut scenario = matrix_scenario();
        let source = if use_domri {
            domri(&mut scenario)
        } else {
            veil(&mut scenario)
        };
        let creature = bears(&mut scenario, P0);
        let snatch = spell(
            &mut scenario,
            P1,
            "Aethersnatch",
            AETHERSNATCH,
            4,
            vec![ManaCostShard::Blue, ManaCostShard::Blue],
        );
        let counter = counterspell(&mut scenario);
        let mut runner = scenario.build();
        if use_domri {
            activate_domri(&mut runner, source);
        } else {
            runner.cast(source).resolve();
        }
        runner.cast(creature).commit();
        pass_to(&mut runner, P1);
        runner.cast(snatch).target_object(creature).commit();
        runner.resolve_top();
        // CR 115.7: "you may choose new targets" is optional, and a creature
        // spell has no targets to change, so decline it.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("P1 declines choosing new targets");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().objects[&creature].controller, P1);
        assert_eq!(
            runner.state().objects[&creature]
                .cast_occurrence
                .unwrap()
                .caster,
            P0
        );
        pass_to(&mut runner, P1);
        runner.cast(counter).target_object(creature).commit();
        runner.resolve_top();
        assert_eq!(
            runner.state().objects[&creature].zone,
            if use_domri {
                Zone::Stack
            } else {
                Zone::Graveyard
            }
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&creature].zone,
            if use_domri {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_complete(&runner);
    }
}

#[test]
fn domri_does_not_protect_an_uncast_creature_spell_copy() {
    let mut scenario = matrix_scenario();
    let domri = domri(&mut scenario);
    let creature = bears(&mut scenario, P0);
    let double = spell(
        &mut scenario,
        P0,
        "Double Major",
        DOUBLE_MAJOR,
        0,
        vec![ManaCostShard::Green, ManaCostShard::Blue],
    );
    let counter = counterspell(&mut scenario);
    let counter_original = counterspell(&mut scenario);
    let mut runner = scenario.build();
    activate_domri(&mut runner, domri);
    runner.cast(creature).commit();
    runner.cast(double).target_object(creature).commit();
    // Resolve exactly Double Major: replacing it with a copy keeps stack length
    // unchanged, so the runner's shrink-based resolve_top would run too far.
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    let copy = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id != creature)
        .expect("Double Major created a copy")
        .id;
    // CR 707.10: copying puts a spell on the stack without casting it.
    assert!(runner.state().objects[&copy].cast_occurrence.is_none());
    assert!(runner.state().objects[&creature].cast_occurrence.is_some());
    pass_to(&mut runner, P1);
    runner.cast(counter).target_object(copy).commit();
    runner.resolve_top();
    assert!(!runner.state().stack.iter().any(|entry| entry.id == copy));
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == creature));
    pass_to(&mut runner, P1);
    runner
        .cast(counter_original)
        .target_object(creature)
        .commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_complete(&runner);
}

/// The three pre-existing protection routes that this change must preserve.
#[derive(Clone, Copy, Debug)]
enum ProtectionRoute {
    /// "This spell can't be countered." (Carnage Tyrant)
    Intrinsic,
    /// A permanent's static grant (Allosaurus Shepherd).
    Permanent,
    /// A next-spell modifier (Insist).
    NextSpell,
}

#[test]
fn existing_intrinsic_permanent_and_next_spell_protections_still_work() {
    for route in [
        ProtectionRoute::Intrinsic,
        ProtectionRoute::Permanent,
        ProtectionRoute::NextSpell,
    ] {
        let mut scenario = matrix_scenario();
        let protection = match route {
            ProtectionRoute::Intrinsic => None,
            ProtectionRoute::Permanent => {
                scenario.add_creature_from_oracle(P0, "Allosaurus Shepherd", 1, 1, SHEPHERD);
                None
            }
            ProtectionRoute::NextSpell => Some(spell(
                &mut scenario,
                P0,
                "Insist",
                INSIST,
                0,
                vec![ManaCostShard::Green],
            )),
        };
        let creature = match route {
            ProtectionRoute::Intrinsic => scenario
                .add_creature_to_hand_from_oracle(P0, "Carnage Tyrant", 7, 6, TYRANT)
                .with_mana_cost(ManaCost::Cost {
                    generic: 4,
                    shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                })
                .id(),
            ProtectionRoute::Permanent | ProtectionRoute::NextSpell => bears(&mut scenario, P0),
        };
        let counter = counterspell(&mut scenario);
        let mut runner = scenario.build();
        if let Some(protection) = protection {
            runner.cast(protection).resolve();
        }
        runner.cast(creature).commit();
        counter_creature(&mut runner, creature, counter, Zone::Battlefield);
    }
}

#[test]
fn shusher_prohibition_is_bound_to_the_target_spell_incarnation() {
    let mut scenario = matrix_scenario();
    let shusher = scenario
        .add_creature_from_oracle(P0, "Vexing Shusher", 2, 2, SHUSHER)
        .id();
    let red = scenario.add_basic_land(P0, ManaColor::Red);
    let creature = bears(&mut scenario, P0);
    let bounce = spell(
        &mut scenario,
        P1,
        "Unsubstantiate",
        "Return target spell or creature to its owner's hand.",
        1,
        vec![ManaCostShard::Blue],
    );
    let counter = counterspell(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    pass_to(&mut runner, P1);
    runner.cast(bounce).target_object(creature).commit();
    pass_to(&mut runner, P0);
    runner
        .activate(shusher, 0)
        .target_object(creature)
        .pay_with(&[red])
        .resolve();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Hand);
    // CR 400.7: returning and recasting the same stored card creates a new spell.
    pass_to(&mut runner, P0);
    runner.cast(creature).commit();
    counter_creature(&mut runner, creature, counter, Zone::Graveyard);
}
