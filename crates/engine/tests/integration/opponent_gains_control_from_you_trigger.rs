//! CR 603.2 + CR 603.10d: "Whenever an opponent gains control of a permanent
//! from you" (Zidane, Tantalus Thief) — the opponent-gains half of the
//! look-back control-change triggers, driven through real control-changing
//! spells and the cleanup reversion.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{filter_events_for_viewer, filter_state_for_viewer};
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{ControlChangeEvidence, GameEvent};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Scryfall).
const ZIDANE: &str = "When Zidane enters, gain control of target creature an opponent \
    controls until end of turn. Untap it. It gains lifelink and haste until end of turn.\n\
    Whenever an opponent gains control of a permanent from you, you create a Treasure token.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
    creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const MOB_RULE: &str = "Choose one \u{2014}\n\u{2022} Gain control of all creatures with power \
    4 or greater until end of turn. Untap those creatures. They gain haste until end of turn.\n\
    \u{2022} Gain control of all creatures with power 3 or less until end of turn. Untap those \
    creatures. They gain haste until end of turn.";
const SWITCHEROO: &str = "Exchange control of two target creatures.";
const PERPLEXING_CHIMERA: &str = "Whenever an opponent casts a spell, you may exchange control \
    of this creature and that spell. If you do, you may choose new targets for the spell. (If \
    the spell becomes a permanent, you control that permanent.)";

fn treasures(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[*id];
            obj.name == "Treasure" && obj.controller == player
        })
        .count()
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Zidane with its printed mana cost, {3}{R}{W} (mana value 5).
fn zidane_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::White],
        generic: 3,
    }
}

/// Zidane as printed: Legendary Creature — Human Mutant Scout, 3/3.
fn add_zidane(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Zidane, Tantalus Thief", 3, 3, ZIDANE)
        .with_mana_cost(zidane_cost())
        .with_subtypes(vec!["Human", "Mutant", "Scout"])
        .as_legendary()
        .id()
}

fn add_free_spell(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, false, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// An opponent stealing your permanent fires once for you; the same permanent
/// reverting back to you at cleanup is a gain TO you and must not fire again.
#[test]
fn opponent_steal_fires_and_revert_back_to_you_does_not() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    add_zidane(&mut scenario, P0);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(steal).target_object(victim).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, victim),
        P1,
        "REACH: P1 stole the creature"
    );
    assert_eq!(
        treasures(&runner, P0),
        1,
        "the steal creates one Treasure for P0"
    );
    assert_eq!(treasures(&runner, P1), 0);

    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, victim),
        P0,
        "REACH: control reverted to P0 at cleanup"
    );
    assert_eq!(
        treasures(&runner, P0),
        1,
        "regaining control is not an opponent gaining control from you"
    );
}

/// Zidane's own steal is a gain TO you (no Treasure); when it ends at cleanup
/// the opponent regains control from you, which does fire (CR 514.3a: the
/// trigger is handled in an extra cleanup step).
#[test]
fn zidane_steal_returning_at_end_of_turn_fires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let zidane = scenario
        .add_creature_to_hand_from_oracle(P0, "Zidane, Tantalus Thief", 3, 3, ZIDANE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let bear = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(zidane).target_object(bear).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, bear),
        P0,
        "REACH: Zidane's ETB stole the bear"
    );
    assert_eq!(
        treasures(&runner, P0),
        0,
        "gaining control yourself does not fire"
    );

    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, bear),
        P1,
        "REACH: the bear went back to P1"
    );
    assert_eq!(
        treasures(&runner, P0),
        1,
        "the opponent regaining control from you creates a Treasure"
    );
    assert_eq!(treasures(&runner, P1), 0);
}

/// CR 603.10d + CR 603.3a: the trigger looks back in time, so stealing Zidane
/// itself fires, controlled by its previous controller (Scryfall ruling).
#[test]
fn stealing_zidane_itself_fires_for_its_previous_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(steal).target_object(zidane).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: P1 stole Zidane");
    assert_eq!(
        treasures(&runner, P0),
        1,
        "P0 (the player who lost Zidane) gets the Treasure"
    );
    assert_eq!(treasures(&runner, P1), 0, "the new controller gets nothing");
}

/// CR 603.2c: a mass steal is one occurrence per permanent. Mob Rule's first
/// mode takes every creature with power 4 or greater: both 4/4s, not Zidane
/// (3/3).
#[test]
fn mass_steal_fires_once_per_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let giants = [
        scenario.add_vanilla(P0, 4, 4),
        scenario.add_vanilla(P0, 4, 4),
    ];
    let mob_rule = add_free_spell(&mut scenario, P1, "Mob Rule", MOB_RULE);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(mob_rule).modes(&[0]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: Zidane stayed with P0"
    );
    for giant in giants {
        assert_eq!(controller(&runner, giant), P1, "REACH: each 4/4 stolen");
    }
    assert_eq!(
        treasures(&runner, P0),
        2,
        "one Treasure per stolen permanent"
    );
    assert_eq!(treasures(&runner, P1), 0);
}

/// CR 603.2c + CR 603.10d (Scryfall ruling: "If an opponent gains control of
/// Zidane and one or more other permanents from you simultaneously, Zidane's
/// last ability will trigger for each of those permanents, including itself.")
/// Mob Rule's second mode takes every creature with power 3 or less: Zidane
/// (3/3, printed mana value 5) and the bear, in one simultaneous change. Both
/// look back to P0's Zidane. CodeRabbit 4254189582: exactly 2, not "at least".
#[test]
fn mob_rule_taking_zidane_and_a_bear_triggers_twice_for_p0() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear = scenario.add_vanilla(P0, 2, 2);
    let mob_rule = add_free_spell(&mut scenario, P1, "Mob Rule", MOB_RULE);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    assert_eq!(
        runner.state().objects[&zidane].mana_cost.mana_value(),
        5,
        "REACH: Zidane keeps its printed mana value"
    );

    runner.cast(mob_rule).modes(&[1]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane stolen");
    assert_eq!(controller(&runner, bear), P1, "REACH: bear stolen");
    assert_eq!(
        treasures(&runner, P0),
        2,
        "one Treasure each for Zidane and the bear, both P0's"
    );
    assert_eq!(
        treasures(&runner, P1),
        0,
        "nothing for Zidane's new controller"
    );
}

/// CR 110.1 + CR 109.4: a spell changing control on the stack is not a
/// permanent. Zidane is on P1's side; P0's Perplexing Chimera exchanges itself
/// (P0 → P1, not from P1) for P1's creature spell (P1 → P0, a spell).
/// Paired positive on the same board (F3, CodeRabbit 4254189598): P0 steals a
/// battlefield creature from P1 with Act of Treason, and P1's Zidane fires.
#[test]
fn spell_control_exchange_is_not_a_permanent() {
    for steal_permanent in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_zidane(&mut scenario, P1);
        let chimera = scenario
            .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA)
            .id();
        let bears = scenario
            .add_creature_to_hand_from_oracle(P1, "Grizzly Bears", 2, 2, "")
            .with_mana_cost(ManaCost::zero())
            .id();
        let p1_creature = scenario.add_vanilla(P1, 2, 2);
        let treason = add_free_spell(&mut scenario, P0, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();

        if steal_permanent {
            give_priority(&mut runner, P0);
            runner.cast(treason).target_object(p1_creature).resolve();
            runner.advance_until_stack_empty();
            assert_eq!(
                controller(&runner, p1_creature),
                P0,
                "REACH: P0 stole P1's creature"
            );
            assert_eq!(
                treasures(&runner, P1),
                1,
                "P0 gained control of a permanent from P1: one Treasure for P1"
            );
            assert_eq!(treasures(&runner, P0), 0);
            continue;
        }

        give_priority(&mut runner, P1);
        runner.cast(bears).accept_optional().resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, chimera),
            P1,
            "REACH: the Chimera swapped to P1"
        );
        assert!(
            runner.state().battlefield.contains(&bears),
            "REACH: the exchanged spell resolved"
        );
        assert_eq!(
            controller(&runner, bears),
            P0,
            "REACH: the spell resolved under P0"
        );
        assert_eq!(
            treasures(&runner, P1),
            0,
            "P0 gaining control of P1's spell is not gaining control of a permanent"
        );
        assert_eq!(treasures(&runner, P0), 0);
    }
}

/// CR 603.10d + CR 603.3a: each control change binds to its own holder of
/// Zidane. P1 steals Zidane (P0's loss: P0 +1); later, in a separate
/// resolution, P0 steals a creature from P1 while P1 controls Zidane (P1's
/// loss: P1 +1).
#[test]
fn separate_resolutions_bind_each_change_to_its_holder() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let p1_creature = scenario.add_vanilla(P1, 2, 2);
    let theirs = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let ours = add_free_spell(&mut scenario, P0, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();

    give_priority(&mut runner, P1);
    runner.cast(theirs).target_object(zidane).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: P1 stole Zidane");
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));

    give_priority(&mut runner, P0);
    runner.cast(ours).target_object(p1_creature).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, p1_creature),
        P0,
        "REACH: P0 stole P1's creature"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (1, 1),
        "the second steal is P1's loss while P1 controls Zidane"
    );
}

/// CR 603.10d + CR 701.12a: exchanging Zidane for an opponent's creature is
/// one simultaneous control change. Zidane leaving you fires for you (its
/// controller before the exchange); the opponent's creature arriving does not,
/// and must not be credited to the opponent who now controls Zidane.
#[test]
fn exchanging_zidane_fires_only_for_its_previous_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear = scenario.add_vanilla(P1, 2, 2);
    let switcheroo = add_free_spell(&mut scenario, P0, "Switcheroo", SWITCHEROO);
    let mut runner = scenario.build();

    runner
        .cast(switcheroo)
        .target_objects(&[zidane, bear])
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane went to P1");
    assert_eq!(controller(&runner, bear), P0, "REACH: the bear came to P0");
    assert_eq!(treasures(&runner, P0), 1, "P0 lost Zidane to an opponent");
    assert_eq!(
        treasures(&runner, P1),
        0,
        "P1 did not control Zidane when P1 lost the bear"
    );
}

/// CR 102.3: in Two-Headed Giant a teammate is not an opponent, so a teammate
/// taking your permanent does not fire; an opponent taking it does.
#[test]
fn teammate_steal_does_not_fire() {
    for (thief, expected) in [(P1, 0), (P2, 1)] {
        let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        add_zidane(&mut scenario, P0);
        let victim = scenario.add_vanilla(P0, 2, 2);
        let steal = add_free_spell(&mut scenario, thief, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();
        give_priority(&mut runner, thief);

        runner.cast(steal).target_object(victim).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, victim),
            thief,
            "REACH: {thief:?} took the creature"
        );
        assert_eq!(
            treasures(&runner, P0),
            expected,
            "thief {thief:?}: only an opponent gaining control fires"
        );
    }
}

/// A steal between two other players is not "from you"; the same steal of
/// your creature in the same three-player game is.
#[test]
fn steal_between_other_players_does_not_fire() {
    for (victim_controller, expected) in [(P2, 0), (P0, 1)] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        add_zidane(&mut scenario, P0);
        let victim = scenario.add_vanilla(victim_controller, 2, 2);
        let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);

        runner.cast(steal).target_object(victim).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, victim),
            P1,
            "REACH: P1 stole {victim_controller:?}'s creature"
        );
        assert_eq!(
            treasures(&runner, P0),
            expected,
            "victim controller {victim_controller:?}: only a steal from you fires"
        );
        assert_eq!(treasures(&runner, P2), 0);
    }
}

/// Synthetic "~" subject through the same parser arm: `valid_card` scopes the
/// changing permanent to the source itself.
const SELF_WATCHER: &str =
    "Whenever an opponent gains control of ~ from you, you create a Treasure token.";

#[test]
fn self_subject_fires_only_for_the_source() {
    for (steal_source, expected) in [(false, 0), (true, 1)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let watcher = scenario
            .add_creature_from_oracle(P0, "Self Watcher", 1, 1, SELF_WATCHER)
            .id();
        let other = scenario.add_vanilla(P0, 2, 2);
        let victim = if steal_source { watcher } else { other };
        let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);

        runner.cast(steal).target_object(victim).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, victim), P1, "REACH: P1 stole it");
        assert_eq!(
            treasures(&runner, P0),
            expected,
            "steal_source {steal_source}: only the source changing hands fires"
        );
        assert_eq!(treasures(&runner, P1), 0);
    }
}

/// Verbatim Oracle text (Scryfall).
const HOMEWARD_PATH: &str =
    "{T}: Add {C}.\n{T}: Each player gains control of all creatures they own.";

/// CR 101.4 + CR 608.2e: Homeward Path returns every creature to its owner in
/// one simultaneous action. After Switcheroo gave P1 Zidane and P0 P1's bear,
/// Homeward Path moves Zidane back from P1 (an opponent of P1 gains control of
/// it from P1: Treasure for P1) and the bear back from P0 (P0 didn't control
/// Zidane immediately before: nothing for P0). The engine applies the action
/// seat by seat, so both APNAP orders are pinned (P0 active, then P1 active).
#[test]
fn homeward_after_switcheroo_is_simultaneous_in_both_apnap_orders() {
    for active in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let bear = scenario.add_vanilla(P1, 2, 2);
        let switcheroo = add_free_spell(&mut scenario, P0, "Switcheroo", SWITCHEROO);
        let path = scenario
            .add_land_from_oracle(P0, "Homeward Path", HOMEWARD_PATH)
            .id();
        let mut runner = scenario.build();

        runner
            .cast(switcheroo)
            .target_objects(&[zidane, bear])
            .resolve();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane went to P1");
        assert_eq!(controller(&runner, bear), P0, "REACH: the bear came to P0");
        let (p0_before, p1_before) = (treasures(&runner, P0), treasures(&runner, P1));
        assert_eq!((p0_before, p1_before), (1, 0), "Switcheroo: P0 lost Zidane");

        {
            let state = runner.state_mut();
            state.active_player = active;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        runner.activate(path, 1).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, zidane),
            P0,
            "REACH ({active:?} active): Zidane returned to P0"
        );
        assert_eq!(
            controller(&runner, bear),
            P1,
            "REACH ({active:?} active): the bear returned to P1"
        );
        assert_eq!(
            (
                treasures(&runner, P0) - p0_before,
                treasures(&runner, P1) - p1_before
            ),
            (0, 1),
            "{active:?} active: P1 lost Zidane; P0 didn't control Zidane when it lost the bear"
        );
        assert!(
            runner.state().control_change_captures.is_empty(),
            "the completed link released its capture"
        );
    }
}

/// Labelled synthetic: Homeward Path's action, optional for each player, so the
/// fan-out pauses on each seat's choice and resumes from a parked
/// continuation (CR 608.2e: the choices are made in APNAP order, then the
/// action is processed for every player at once).
const OPTIONAL_HOMEWARD: &str = "Each player may gain control of all nontoken permanents they own.";

/// DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. An optional
/// scoped control action pauses between seats, so it isn't an allowlisted
/// shape (`fan_out_head_is_allowlisted`): no pre-seat capture is taken, its changes
/// publish `Unrecorded`, and Zidane fires for no player. The same board with
/// Homeward Path's mandatory action is exact
/// (`homeward_after_switcheroo_is_simultaneous_in_both_apnap_orders`); the rules
/// give P1 +1 here too.
#[test]
fn an_optional_scoped_control_fan_out_fails_closed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear = scenario.add_vanilla(P1, 2, 2);
    let switcheroo = add_free_spell(&mut scenario, P0, "Switcheroo", SWITCHEROO);
    let returns = add_free_spell(&mut scenario, P0, "Optional Homeward", OPTIONAL_HOMEWARD);
    let mut runner = scenario.build();

    runner
        .cast(switcheroo)
        .target_objects(&[zidane, bear])
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));

    runner.cast(returns).accept_optional().resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: Zidane returned to P0"
    );
    assert_eq!(
        controller(&runner, bear),
        P1,
        "REACH: the bear returned to P1"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (1, 0),
        "the class fires for no player"
    );
}

/// CodeRabbit 4254189574: control that came back to P0 earlier in the turn
/// doesn't stop a later loss from triggering. Homeward Path returns Zidane to
/// P0 (P1 +1); a later steal of another P0 creature, in a separate resolution,
/// is P0's loss (P0 +1).
#[test]
fn return_then_later_loss_triggers_for_each_holder() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear = scenario.add_vanilla(P1, 2, 2);
    let later = scenario.add_vanilla(P0, 2, 2);
    let switcheroo = add_free_spell(&mut scenario, P0, "Switcheroo", SWITCHEROO);
    let path = scenario
        .add_land_from_oracle(P0, "Homeward Path", HOMEWARD_PATH)
        .id();
    let treason = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();

    runner
        .cast(switcheroo)
        .target_objects(&[zidane, bear])
        .resolve();
    runner.advance_until_stack_empty();
    runner.activate(path, 1).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: Zidane back with P0"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (1, 1),
        "Switcheroo: P0 +1; Homeward Path: P1 +1"
    );

    give_priority(&mut runner, P1);
    runner.cast(treason).target_object(later).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, later), P1, "REACH: the later steal");
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (2, 1),
        "the later loss is P0's, whose Zidane came back earlier this turn"
    );
}

/// Labelled synthetic two-instruction spell: two exchanges in one resolution,
/// one after the other (CR 608.2c).
const DOUBLE_SWITCHEROO: &str =
    "Exchange control of two target creatures. Exchange control of two target creatures.";

/// CR 608.2c + CR 603.10d: each instruction looks back to its own moment. The
/// first exchange gives Zidane to P1 and bear A to P0 (P0's loss of Zidane:
/// P0 +1; P1's loss of bear A came with P1 not yet controlling Zidane:
/// nothing). The second moves bear B from P1 to P2 while P1 controls Zidane
/// (P1's loss: P1 +1) and bear C from P2 to P1 (P2 never controlled Zidane:
/// nothing). CodeRabbit 4254189607: P1's count is asserted too.
#[test]
fn double_exchange_binds_each_instruction_to_its_own_holder() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear_a = scenario.add_vanilla(P1, 2, 2);
    let bear_b = scenario.add_vanilla(P1, 2, 2);
    let bear_c = scenario.add_vanilla(P2, 2, 2);
    let spell = add_free_spell(&mut scenario, P0, "Double Switcheroo", DOUBLE_SWITCHEROO);
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_objects(&[zidane, bear_a, bear_b, bear_c])
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane went to P1");
    assert_eq!(controller(&runner, bear_a), P0, "REACH: bear A came to P0");
    assert_eq!(controller(&runner, bear_b), P2, "REACH: bear B went to P2");
    assert_eq!(controller(&runner, bear_c), P1, "REACH: bear C came to P1");
    assert_eq!(
        (
            treasures(&runner, P0),
            treasures(&runner, P1),
            treasures(&runner, P2)
        ),
        (1, 1, 0)
    );
}

/// Labelled synthetic: two independent "each player" sentences, each its own
/// scoped control-change action (CR 608.2e: each action processed
/// simultaneously, the second after the first), each an allowlisted printed
/// shape (Homeward Path's, then The Fall of Lord Konda's).
const TWO_SCOPED_RETURNS: &str = "Each player gains control of all creatures they own. Each \
    player gains control of all permanents they own.";

/// Q9: each scoped link captures its own boundary. P0 (with Zidane) has taken
/// P1's creature and land; the first sentence returns the creature and the
/// second the land, two losses for P0 while P0 controls Zidane: P0 +2.
#[test]
fn two_scoped_links_each_get_their_own_boundary() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let creature = scenario.add_vanilla(P1, 2, 2);
    let land = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    let steal_creature = add_free_spell(&mut scenario, P0, "Steal", STEAL_ANY_PERMANENT);
    let steal_land = add_free_spell(&mut scenario, P0, "Steal", STEAL_ANY_PERMANENT);
    let returns = add_free_spell(&mut scenario, P0, "Two Returns", TWO_SCOPED_RETURNS);
    let mut runner = scenario.build();

    runner
        .cast(steal_creature)
        .target_object(creature)
        .resolve();
    runner.advance_until_stack_empty();
    runner.cast(steal_land).target_object(land).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, creature),
        P0,
        "REACH: P0 took the creature"
    );
    assert_eq!(controller(&runner, land), P0, "REACH: P0 took the land");
    assert_eq!(treasures(&runner, P0), 0, "P0's own gains don't fire");

    runner.cast(returns).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, creature),
        P1,
        "REACH: first link returned the creature"
    );
    assert_eq!(
        controller(&runner, land),
        P1,
        "REACH: second link returned the land"
    );
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: Zidane stayed with P0"
    );
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (2, 0));
}

/// Verbatim Oracle text (Scryfall).
const GRAB_THE_REINS: &str = "Choose one \u{2014}\n\u{2022} Until end of turn, you gain control \
    of target creature and it gains haste.\n\u{2022} Sacrifice a creature. Grab the Reins deals \
    damage equal to that creature's power to any target.\nEntwine {2}{R} (Choose both if you \
    pay the entwine cost.)";

/// What entwined Grab the Reins steals and then sacrifices in one resolution.
#[derive(Clone, Copy, Debug)]
enum SacrificedVictim {
    /// The stolen bear, P1's only creature: the sacrifice takes it directly.
    BearOnly,
    /// The stolen bear while P1 has another creature: the sacrifice pauses for
    /// P1's choice and resumes.
    BearAfterChoice,
    /// Zidane itself.
    Zidane,
}

/// CR 110.1 + CR 603.10d: a permanent stolen and then sacrificed by the same
/// resolution was a permanent when it changed hands, and Zidane (or the
/// stolen Zidane itself) looks back to that moment: P0 +1, P1 +0 every time,
/// on the immediate sacrifice, the choice-paused sacrifice, and Zidane's own.
#[test]
fn grab_the_reins_steal_then_sacrifice_triggers_for_p0() {
    for victim_kind in [
        SacrificedVictim::BearOnly,
        SacrificedVictim::BearAfterChoice,
        SacrificedVictim::Zidane,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let bear = scenario.add_vanilla(P0, 2, 2);
        let other = matches!(victim_kind, SacrificedVictim::BearAfterChoice)
            .then(|| scenario.add_vanilla(P1, 1, 1));
        let victim = match victim_kind {
            SacrificedVictim::Zidane => zidane,
            SacrificedVictim::BearOnly | SacrificedVictim::BearAfterChoice => bear,
        };
        let reins = scenario
            .add_spell_to_hand_from_oracle(P1, "Grab the Reins", true, GRAB_THE_REINS)
            .from_oracle_text_with_keywords(&["Entwine"], GRAB_THE_REINS)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 3,
            })
            .id();
        scenario.with_mana_pool(
            P1,
            (0..7)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);

        runner
            .cast(reins)
            .modes(&[0, 1])
            .target_object(victim)
            .target_player(P0)
            .sacrifice_with(&[victim])
            .resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&victim].zone,
            Zone::Graveyard,
            "REACH ({victim_kind:?}): the stolen permanent was sacrificed"
        );
        if let Some(other) = other {
            assert!(
                runner.state().battlefield.contains(&other),
                "REACH: P1 chose the stolen bear over its own creature"
            );
        }
        assert_eq!(
            (treasures(&runner, P0), treasures(&runner, P1)),
            (1, 0),
            "{victim_kind:?}: one Treasure for P0, none for the thief"
        );
    }
}

/// Verbatim Oracle text (Scryfall).
const THRAGTUSK: &str = "When this creature enters, you gain 5 life.\nWhen this creature \
    leaves the battlefield, create a 3/3 green Beast creature token.";
/// Labelled synthetic steal-then-exile spell.
const STEAL_THEN_EXILE: &str = "Gain control of target creature until end of turn. Exile it.";

/// What the synthetic steal-then-exile takes.
#[derive(Clone, Copy, Debug)]
enum ExiledVictim {
    /// Has its own leaves-the-battlefield trigger, collected with the exile.
    Thragtusk,
    Vanilla,
    /// A token, which ceases to exist once exiled (CR 111.7).
    Token,
}

/// CR 110.1 + CR 603.10d: steal-then-exile in one resolution. The subject was
/// a permanent when it changed hands, so P0 +1, P1 +0, whether it has its own
/// leaves trigger (Thragtusk's Beast still goes to its last controller, P1),
/// or is a token that ceased to exist.
#[test]
fn steal_then_exile_triggers_for_p0() {
    for victim_kind in [
        ExiledVictim::Thragtusk,
        ExiledVictim::Vanilla,
        ExiledVictim::Token,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_zidane(&mut scenario, P0);
        let victim = match victim_kind {
            ExiledVictim::Thragtusk => scenario
                .add_creature_from_oracle(P0, "Thragtusk", 5, 3, THRAGTUSK)
                .id(),
            ExiledVictim::Vanilla => scenario.add_vanilla(P0, 5, 3),
            ExiledVictim::Token => scenario.add_creature(P0, "Soldier", 1, 1).id(),
        };
        let spell = add_free_spell(&mut scenario, P1, "Steal Then Exile", STEAL_THEN_EXILE);
        let mut runner = scenario.build();
        if matches!(victim_kind, ExiledVictim::Token) {
            runner
                .state_mut()
                .objects
                .get_mut(&victim)
                .unwrap()
                .is_token = true;
        }
        give_priority(&mut runner, P1);

        runner.cast(spell).target_object(victim).resolve();
        runner.advance_until_stack_empty();
        assert!(
            !runner.state().battlefield.contains(&victim),
            "REACH ({victim_kind:?}): the stolen creature left the battlefield"
        );
        let p1_beasts = runner
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                let obj = &runner.state().objects[*id];
                obj.name.contains("Beast") && obj.controller == P1
            })
            .count();
        assert_eq!(
            p1_beasts,
            usize::from(matches!(victim_kind, ExiledVictim::Thragtusk)),
            "REACH: Thragtusk's leaves trigger is controlled by its last controller, P1"
        );
        assert_eq!(
            (treasures(&runner, P0), treasures(&runner, P1)),
            (1, 0),
            "{victim_kind:?}: one Treasure for P0, none for the thief"
        );
    }
}
/// Verbatim Oracle text (Scryfall).
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";
/// Labelled synthetic two-instruction spell.
const STEAL_AND_DESTROY_ENCHANTMENT: &str =
    "Gain control of target creature until end of turn. Destroy target enchantment.";

/// An Aura in P1's hand: Control Magic as an enchantment card with its Enchant
/// keyword, so it is cast and attached through the production pipeline.
fn add_control_magic(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_artifact_to_hand_from_oracle(player, "Control Magic", CONTROL_MAGIC)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn make_enchantment_aura(runner: &mut GameRunner, aura: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&aura).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.base_card_types = obj.card_types.clone();
    let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
    obj.keywords.push(enchant.clone());
    obj.base_keywords.push(enchant);
}

/// CR 603.10d + CR 613.1b: a layer-2 control change publishes no
/// `ControllerChanged` (Control Magic leaving), so "no event for Zidane" is no
/// proof that its controller held. P1's Control Magic gives P1 Zidane (owned by
/// P0). P1 then steals P0's bear and destroys Control Magic in one resolution:
/// when the bear changed hands P1 controlled Zidane, so nothing fires for P0,
/// although Zidane is back with P0 when triggers are collected. Paired control:
/// Control Magic on another P0 creature, Zidane with P0 throughout → 1.
#[test]
fn silent_layer_two_return_of_zidane_never_fires_for_the_wrong_player() {
    for magic_on_zidane in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let bear = scenario.add_vanilla(P0, 2, 2);
        let decoy = scenario.add_vanilla(P0, 1, 1);
        let magic = add_control_magic(&mut scenario, P1);
        let spell = add_free_spell(
            &mut scenario,
            P1,
            "Steal and Destroy Enchantment",
            STEAL_AND_DESTROY_ENCHANTMENT,
        );
        let mut runner = scenario.build();
        make_enchantment_aura(&mut runner, magic);
        give_priority(&mut runner, P1);

        let enchanted = if magic_on_zidane { zidane } else { decoy };
        runner.cast(magic).target_object(enchanted).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, enchanted),
            P1,
            "REACH: Control Magic took its creature"
        );
        let p0_before = treasures(&runner, P0);

        let outcome = runner.cast(spell).target_objects(&[bear, magic]).resolve();
        let zidane_events = outcome
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::ControllerChanged { object_id, .. } if *object_id == zidane
                )
            })
            .count();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, bear), P1, "REACH: the bear went to P1");
        assert_eq!(
            runner.state().objects[&magic].zone,
            Zone::Graveyard,
            "REACH: Control Magic was destroyed"
        );
        assert_eq!(controller(&runner, zidane), P0, "REACH: Zidane is with P0");
        assert_eq!(
            zidane_events, 0,
            "REACH: Zidane's return published no ControllerChanged"
        );
        let expected = if magic_on_zidane { 0 } else { 1 };
        assert_eq!(
            treasures(&runner, P0) - p0_before,
            expected,
            "magic_on_zidane {magic_on_zidane}: Zidane's controller when the bear moved decides"
        );
        assert_eq!(treasures(&runner, P1), 0);
    }
}

/// CR 514.2 + CR 603.10d: the until-end-of-turn reversion is one simultaneous
/// action. P1 steals Zidane (P0 +1); at cleanup Zidane returns from P1 to P0,
/// an opponent gaining control of it from P1, who controlled it immediately
/// before (P1 +1). Zidane coming back is not a loss for P0 (P0 +0).
#[test]
fn cleanup_revert_of_stolen_zidane_credits_the_thief() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(steal).target_object(zidane).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, zidane), P1, "REACH: P1 stole Zidane");
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));

    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: Zidane reverted to P0"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (1, 1),
        "the reversion is P1's loss: one Treasure for P1, none for P0"
    );
}

/// Verbatim Oracle text (Scryfall).
const KHARN: &str = "Berzerker \u{2014} Khârn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption \u{2014} When you lose control of Khârn the Betrayer, draw two \
    cards.\nThe Betrayer \u{2014} If damage would be dealt to Khârn the Betrayer, prevent that \
    damage and an opponent of your choice gains control of it.";

/// CR 603.10d: Khârn's "when you lose control" reads the latched watcher like
/// Zidane: the player who lost it draws exactly two; the thief draws nothing.
#[test]
fn kharn_losing_controller_draws_exactly_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    scenario.with_library_top(P1, &["Card D", "Card E", "Card F"]);
    let kharn = scenario
        .add_creature_from_oracle(P0, "Khârn the Betrayer", 5, 1, KHARN)
        .id();
    let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    let hand = |runner: &GameRunner, player: PlayerId| {
        runner.state().players[player.0 as usize].hand.len()
    };
    let (p0_before, p1_before) = (hand(&runner, P0), hand(&runner, P1));

    runner.cast(steal).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");
    assert_eq!(hand(&runner, P0) - p0_before, 2, "P0 lost Khârn: draws two");
    assert_eq!(
        hand(&runner, P1) + 1,
        p1_before,
        "P1 only cast Act of Treason: draws nothing"
    );
}

/// Verbatim Oracle text (Scryfall).
const DUPLICITY: &str = "When this enchantment enters, exile the top five cards of your \
    library face down.\nAt the beginning of your upkeep, you may exile all cards from your \
    hand face down. If you do, put all other cards you own exiled with this enchantment into \
    your hand.\nAt the beginning of your end step, discard a card.\nWhen you lose control of \
    this enchantment, put all cards exiled with this enchantment into their owner's \
    graveyard.";
/// Labelled synthetic: a steal that can take a noncreature permanent.
const STEAL_PERMANENT: &str = "Gain control of target permanent.";

fn duplicity_board(targeted_watcher: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six"]);
    add_zidane(&mut scenario, P0);
    if targeted_watcher {
        scenario.add_creature_from_oracle(P0, "Targeted Watcher", 1, 1, TARGETED_WATCHER);
    }
    let duplicity = scenario
        .add_spell_to_hand_from_oracle(P0, "Duplicity", false, DUPLICITY)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let steal = add_free_spell(&mut scenario, P1, "Steal Permanent", STEAL_PERMANENT);
    let mut runner = scenario.build();
    runner.cast(duplicity).resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner.state().battlefield.contains(&duplicity),
        "REACH: Duplicity entered"
    );
    (runner, duplicity, bear, steal)
}

fn face_down_exiles(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .exile
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].face_down)
        .collect()
}

/// CR 603.10d: Duplicity's "when you lose control" puts its face-down exiles
/// into their owner's graveyard; the same steal is a loss Zidane sees (P0 +1).
#[test]
fn duplicity_losing_control_puts_its_exiles_into_the_owners_graveyard() {
    let (mut runner, duplicity, _, steal) = duplicity_board(false);
    let exiled = face_down_exiles(&runner);
    assert_eq!(
        exiled.len(),
        5,
        "REACH: Duplicity exiled five cards face down"
    );
    give_priority(&mut runner, P1);

    runner.cast(steal).target_object(duplicity).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, duplicity),
        P1,
        "REACH: P1 took Duplicity"
    );
    for card in exiled {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Graveyard,
            "each card exiled with Duplicity goes to its owner's graveyard"
        );
        assert_eq!(runner.state().objects[&card].owner, P0);
    }
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));
}

/// Labelled synthetic look-back watcher whose trigger targets, so with two
/// legal players its trigger waits in `WaitingFor::TriggerTargetSelection`,
/// a prompt that carries the `ControllerChanged` event.
const TARGETED_WATCHER: &str =
    "Whenever an opponent gains control of a permanent from you, target player draws a card.";

/// Every `ControllerChanged` evidence value anywhere in a serialized state.
fn serialized_evidence(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("ControllerChanged") {
                if let Some(evidence) = map.get("data").and_then(|data| data.get("evidence")) {
                    out.push(evidence.clone());
                }
            }
            map.values()
                .for_each(|child| serialized_evidence(child, out));
        }
        serde_json::Value::Array(items) => items
            .iter()
            .for_each(|child| serialized_evidence(child, out)),
        _ => {}
    }
}

/// CR 400.2 + CR 603.10d: control-change observations are rules authority.
/// They latch Duplicity, whose context records the face-down cards it exiled
/// (ids and mana values). Events and abilities carry only a key into
/// `GameState::control_change_observations`, which no viewer copy keeps, so no
/// carrier (stack entry, pending prompt, live event) can expose an
/// observation. Projected while the event is held, by the two triggers it fired
/// waiting in P0's prompt.
#[test]
fn control_change_evidence_never_reaches_a_viewer() {
    let (mut runner, duplicity, bear, steal) = duplicity_board(true);
    give_priority(&mut runner, P1);

    let mut events = Vec::new();
    runner.cast(steal).target_object(bear).commit();
    for _ in 0..2 {
        if let Ok(result) = runner.act(GameAction::PassPriority) {
            events.extend(result.events);
        }
    }
    // Zidane's and the watcher's triggers fire together for P0, so they wait in
    // P0's ordering prompt, whose pending contexts carry the event.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OrderTriggers { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        "REACH: the triggers wait in a prompt: {:?}",
        runner.state().waiting_for
    );
    let evidence = events
        .iter()
        .find_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                evidence,
                ..
            } if *object_id == bear => Some(*evidence),
            _ => None,
        })
        .expect("REACH: the steal published its control change");
    let authoritative = runner
        .state()
        .control_change_observation(evidence)
        .expect("REACH: the authoritative state holds the observation");
    assert!(
        authoritative
            .watchers
            .iter()
            .any(|watcher| watcher.identity.reference.object_id == duplicity
                && !watcher.linked_exile_snapshot.is_empty()),
        "REACH: the authoritative observation latched Duplicity with its exiles"
    );
    let held = {
        let mut held = Vec::new();
        serialized_evidence(&serde_json::to_value(runner.state()).unwrap(), &mut held);
        held
    };
    assert!(
        !held.is_empty(),
        "REACH: the authoritative state holds the event (a carrier) when projected"
    );

    for viewer in [P0, P1, PlayerId(9)] {
        for event in filter_events_for_viewer(&events, runner.state(), viewer) {
            if let GameEvent::ControllerChanged { evidence, .. } = event {
                assert!(
                    runner
                        .state()
                        .control_change_observation(evidence)
                        .is_some()
                        || evidence == ControlChangeEvidence::Unrecorded,
                    "viewer {viewer:?}: the streamed event carries a key, not an observation"
                );
            }
        }
        let view = filter_state_for_viewer(runner.state(), viewer);
        assert!(
            view.control_change_observations.is_empty() && view.control_change_captures.is_empty(),
            "viewer {viewer:?}: the projection keeps no observation or capture"
        );
        let mut projected = Vec::new();
        serialized_evidence(&serde_json::to_value(&view).unwrap(), &mut projected);
        // The triggers' controller keeps every carrier of the event; an
        // opponent's or spectator's copy may also drop pending trigger events.
        if viewer == P0 {
            assert_eq!(
                projected.len(),
                held.len(),
                "REACH: the controller's projection kept every carrier of the event"
            );
        }
        for value in projected {
            assert!(
                value
                    .as_object()
                    .is_some_and(|map| map.get("Observed").is_some_and(serde_json::Value::is_u64)),
                "viewer {viewer:?}: a projected event's evidence is a bare key: {value}"
            );
        }
        assert!(
            view.control_change_observation(evidence).is_none(),
            "viewer {viewer:?}: the key resolves to nothing in the projection"
        );
    }
}
/// Saved games from before control-change evidence carry `ControllerChanged`
/// with no `evidence` field. It decodes to `Unrecorded` (never rebuilt from the
/// current board). A trigger that had already fired from such an event still
/// resolves: here Zidane's trigger is on the stack, with its event, when the
/// game is saved and restored with every event's evidence stripped.
#[test]
fn a_restored_legacy_controller_changed_decodes_unrecorded() {
    for legacy in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_zidane(&mut scenario, P0);
        let bear = scenario.add_vanilla(P0, 2, 2);
        let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);
        runner.cast(steal).target_object(bear).commit();
        for _ in 0..2 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == steal)
            {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
        assert_eq!(controller(&runner, bear), P1, "REACH: P1 stole the bear");
        assert!(
            runner.state().stack.len() == 1 && treasures(&runner, P0) == 0,
            "REACH: Zidane's trigger is on the stack, unresolved"
        );

        runner.state_mut().capture_rng_word_pos();
        let mut saved = serde_json::to_value(runner.state()).expect("state serializes");
        let mut stripped = 0;
        strip_controller_changed_evidence(&mut saved, legacy, &mut stripped);
        assert!(
            stripped > 0,
            "REACH (legacy {legacy}): the save holds a ControllerChanged with evidence"
        );
        let mut restored: engine::types::game_state::GameState =
            serde_json::from_value(saved).expect("the saved state restores");
        restored.rehydrate_rng();
        // CodeRabbit 4266584501: every restored carrier's evidence, decoded.
        // A stripped (legacy) event is `Unrecorded`, never rebuilt from the
        // restored board; a current one still resolves to its observation.
        let decoded = restored_controller_changed_evidence(&restored);
        assert_eq!(
            decoded.len(),
            stripped,
            "REACH (legacy {legacy}): every saved event is restored"
        );
        for evidence in decoded {
            if legacy {
                assert_eq!(
                    evidence,
                    ControlChangeEvidence::Unrecorded,
                    "a legacy event decodes Unrecorded"
                );
            } else {
                assert!(
                    restored.control_change_observation(evidence).is_some(),
                    "a current event resolves to its observation: {evidence:?}"
                );
            }
        }
        *runner.state_mut() = restored;

        runner.advance_until_stack_empty();
        assert_eq!(
            (treasures(&runner, P0), treasures(&runner, P1)),
            (1, 0),
            "legacy {legacy}: the already-fired trigger resolves"
        );
    }
}

/// The decoded evidence of every `ControllerChanged` the restored state carries,
/// read by decoding each serialized carrier back into a typed `GameEvent`.
fn restored_controller_changed_evidence(
    state: &engine::types::game_state::GameState,
) -> Vec<ControlChangeEvidence> {
    fn walk(value: &serde_json::Value, out: &mut Vec<ControlChangeEvidence>) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|t| t.as_str()) == Some("ControllerChanged") {
                    if let Ok(GameEvent::ControllerChanged { evidence, .. }) =
                        serde_json::from_value::<GameEvent>(value.clone())
                    {
                        out.push(evidence);
                    }
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(
        &serde_json::to_value(state).expect("state serializes"),
        &mut out,
    );
    out
}

/// Counts every saved `ControllerChanged` with evidence and, when `strip`,
/// removes the field, as a pre-evidence save would have it.
fn strip_controller_changed_evidence(
    value: &mut serde_json::Value,
    strip: bool,
    count: &mut usize,
) {
    match value {
        serde_json::Value::Object(map) => {
            let is_event = map.get("type").and_then(|t| t.as_str()) == Some("ControllerChanged");
            if is_event {
                if let Some(data) = map.get_mut("data").and_then(|d| d.as_object_mut()) {
                    if data.contains_key("evidence") {
                        *count += 1;
                        if strip {
                            data.remove("evidence");
                        }
                    }
                }
            }
            for child in map.values_mut() {
                strip_controller_changed_evidence(child, strip, count);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                strip_controller_changed_evidence(child, strip, count);
            }
        }
        _ => {}
    }
}

/// Labelled synthetic, castable at instant speed: Homeward Path's action made
/// optional for each player, so each seat pauses on its own choice.
const OPTIONAL_HOMEWARD_INSTANT: &str =
    "Each player may gain control of all nontoken permanents they own.";

fn cast_and_pass(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    // Every player passes once, so the spell starts resolving.
    for _ in 0..runner.state().players.len() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
        {
            let _ = runner.act(GameAction::PassPriority);
        }
    }
}

fn answer_optional(runner: &mut GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "REACH: a seat is choosing: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the seat accepts");
}

/// R3-2, under the bare-head admission: a fan-out that pauses between seats is
/// never `Head`, so no capture is ever held across a pause; the parked seat
/// carries the fail-closed `Unobservable` carrier instead, and that survives
/// authoritative save/restore. After Switcheroo (Zidane to P1, P1's bear to
/// P0), P1 is active, so P1's seat returns the bear first, and the game is
/// saved while P0's seat is choosing. Restored and resumed, P0's seat returns
/// Zidane from P1, publishing `Unrecorded`: Zidane fires for no player, (1, 0),
/// where an exact capture would give (1, 1).
#[test]
fn a_paused_fail_closed_fan_out_survives_save_and_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let bear = scenario.add_vanilla(P1, 2, 2);
    let switcheroo = add_free_spell(&mut scenario, P0, "Switcheroo", SWITCHEROO);
    let returns = scenario
        .add_spell_to_hand_from_oracle(P0, "Optional Homeward", true, OPTIONAL_HOMEWARD_INSTANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .cast(switcheroo)
        .target_objects(&[zidane, bear])
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    cast_and_pass(&mut runner, returns);
    answer_optional(&mut runner); // P1's seat (active player first)
    assert_eq!(
        controller(&runner, bear),
        P1,
        "REACH: P1's seat returned the bear"
    );
    assert_eq!(
        controller(&runner, zidane),
        P1,
        "REACH: Zidane not yet returned"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "REACH: paused on P0's seat"
    );
    assert!(
        runner.state().control_change_captures.is_empty(),
        "no capture is held across the pause"
    );

    runner.state_mut().capture_rng_word_pos();
    let saved = serde_json::to_value(runner.state()).expect("state serializes");
    assert!(
        saved
            .to_string()
            .contains("\"control_change_instruction\":\"Unobservable\""),
        "the parked seat carries the fail-closed carrier in the save"
    );
    let mut restored: engine::types::game_state::GameState =
        serde_json::from_value(saved).expect("the saved state restores");
    restored.rehydrate_rng();
    *runner.state_mut() = restored;

    answer_optional(&mut runner); // P0's seat, resumed from the restore
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: P0's seat returned Zidane"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (1, 0),
        "the resumed fail-closed seat fires for no player"
    );
}

/// Labelled synthetic instant: Brooding Saurian's mandatory scoped action.
const MANDATORY_RETURN_INSTANT: &str =
    "Each player gains control of all nontoken permanents they own.";

/// Labelled synthetic: steal any permanent, for the probe setups below.
const STEAL_ANY_PERMANENT: &str = "Gain control of target permanent.";

/// The gate's `Head` probe pair (CR 608.2e + CR 603.10 + CR 603.10d + CR
/// 603.2c). Three players, P0 active. P1 owns and controls Zidane; P0 owns
/// Control Magic, which P1 has taken; P1 has also taken a creature P2 owns.
/// "Each player gains control of all nontoken permanents they own" (Brooding
/// Saurian's action, as a labelled synthetic instant): in one simultaneous
/// action P1 loses the Aura to P0 and the creature to P2, so P1 gets exactly 2
/// Treasures, although the engine applies P0's seat (the Aura) before P2's
/// (the creature).
///
/// The gate's hostile variant (Control Magic enchanting P2's creature, so P1
/// controls the creature only through the stolen Aura) isn't reachable here:
/// once P1 takes the Aura, the engine still gives the enchanted creature to
/// the Aura's owner, P0, not to its new controller. That looks like a
/// pre-existing layer-2 dependency defect (CR 613.8a: the static depends on the
/// Aura's own control change), reported separately. The rule the hostile
/// variant targets (a later seat's member whose live controller moved only as
/// a consequence of an earlier seat keeps its captured observation) is pinned
/// directly in `gain_control`'s unit tests.
#[test]
fn head_capture_two_losses_in_one_scoped_action() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P1);
    let p2_creature = scenario.add_vanilla(P2, 2, 2);
    let p1_creature = scenario.add_vanilla(P1, 1, 1);
    let magic = add_control_magic(&mut scenario, P0);
    let steal_aura = add_free_spell(&mut scenario, P1, "Steal Permanent", STEAL_ANY_PERMANENT);
    let steal_creature = add_free_spell(&mut scenario, P1, "Steal Permanent", STEAL_ANY_PERMANENT);
    let returns = scenario
        .add_spell_to_hand_from_oracle(P0, "Mandatory Return", true, MANDATORY_RETURN_INSTANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    make_enchantment_aura(&mut runner, magic);

    give_priority(&mut runner, P0);
    runner.cast(magic).target_object(p1_creature).resolve();
    runner.advance_until_stack_empty();
    give_priority(&mut runner, P1);
    runner.cast(steal_aura).target_object(magic).resolve();
    runner.advance_until_stack_empty();
    runner
        .cast(steal_creature)
        .target_object(p2_creature)
        .resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, magic),
        P1,
        "REACH: P1 controls the Aura"
    );
    assert_eq!(
        controller(&runner, p2_creature),
        P1,
        "REACH: P1 controls P2's creature"
    );
    assert_eq!(controller(&runner, zidane), P1, "REACH: P1 controls Zidane");
    let p1_before = treasures(&runner, P1);

    give_priority(&mut runner, P0);
    cast_and_pass(&mut runner, returns);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, magic),
        P0,
        "REACH: the Aura went home to P0"
    );
    assert_eq!(
        controller(&runner, p2_creature),
        P2,
        "REACH: P2's creature went home to P2"
    );
    assert_eq!(
        treasures(&runner, P1) - p1_before,
        2,
        "P1 lost the Aura and the creature in one action"
    );
    assert_eq!((treasures(&runner, P0), treasures(&runner, P2)), (0, 0));
}

/// Verbatim Oracle text (Scryfall), Trostani Discordant (1/4).
const TROSTANI: &str = "Other creatures you control get +1/+1.\nWhen Trostani enters, create \
    two 1/1 white Soldier creature tokens with lifelink.\nAt the beginning of your end step, \
    each player gains control of all creatures they own.";
/// Labelled synthetic for the R4-1 probe: a power-bounded "each player may".
const RETURN_SMALL_CREATURES: &str =
    "Each player may gain control of all creatures they own with power 3 or less.";

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.unwrap_or_default()
}

/// R4-1 (CR 611.2c + CR 608.2e): DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not
/// rules-correct counts. The affected set of "each player may gain control of
/// all creatures they own with power 3 or less" is fixed when the action
/// begins, but the engine applies it seat by seat and settles between seats
/// (each seat's "may" pauses), so a later seat reads a population an earlier
/// seat's control change altered: P1 controls P0's Trostani and P2's creature;
/// P0's seat takes Trostani back, the anthem leaves, and P2's seat then finds
/// its creature at power 3. Probed at 0376d8d75 (before this class failed
/// closed): `base_power` 3 gave P1 +2 and moved the creature to P2, where the
/// rules give P1 +1 and leave it with P1.
///
/// Such a producer (an optional head, which pauses between seats) is
/// unsupported in coverage, and its changes
/// publish `Unrecorded`, so Zidane never fires for them, for any player. This
/// pins that, with reach guards; the creature's own move is the engine's
/// pre-existing seat-by-seat population (reported separately).
#[test]
fn shifting_population_scoped_producer_fails_closed() {
    for base_power in [3, 2] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P1);
        let trostani = scenario
            .add_creature_from_oracle(P0, "Trostani Discordant", 1, 4, TROSTANI)
            .id();
        let p2_creature = scenario.add_vanilla(P2, base_power, base_power);
        let steal_trostani = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let steal_creature = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let returns = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Return Small Creatures",
                true,
                RETURN_SMALL_CREATURES,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();

        give_priority(&mut runner, P1);
        runner
            .cast(steal_trostani)
            .target_object(trostani)
            .resolve();
        runner.advance_until_stack_empty();
        runner
            .cast(steal_creature)
            .target_object(p2_creature)
            .resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, trostani),
            P1,
            "REACH: P1 controls Trostani"
        );
        assert_eq!(power(&runner, trostani), 1, "REACH: Trostani has power 1");
        assert_eq!(
            controller(&runner, p2_creature),
            P1,
            "REACH: P1 controls P2's creature"
        );
        assert_eq!(
            power(&runner, p2_creature),
            base_power + 1,
            "REACH: P1's anthem pumps P2's creature"
        );
        assert_eq!(power(&runner, zidane), 4, "REACH: P1's anthem pumps Zidane");
        let before = (
            treasures(&runner, P0),
            treasures(&runner, P1),
            treasures(&runner, P2),
        );

        give_priority(&mut runner, P0);
        cast_and_pass(&mut runner, returns);
        assert!(
            runner.state().control_change_captures.is_empty(),
            "the class takes no pre-seat capture: it fails closed"
        );
        answer_optional(&mut runner); // P0's seat: Trostani returns.
        assert_eq!(
            controller(&runner, trostani),
            P0,
            "REACH: P0 took Trostani back"
        );
        assert_eq!(
            power(&runner, p2_creature),
            base_power,
            "REACH: the anthem left P1 before P2's seat"
        );
        for _ in 0..2 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                answer_optional(&mut runner);
            }
        }
        runner.advance_until_stack_empty();
        let after = (
            treasures(&runner, P0),
            treasures(&runner, P1),
            treasures(&runner, P2),
        );
        assert_eq!(
            (after.0 - before.0, after.1 - before.1, after.2 - before.2),
            (0, 0, 0),
            "base_power {base_power}: the class fires for no player"
        );
    }
}

/// R5-1 probe (CR 611.2c + CR 608.2e), hand-built typed AST (labelled
/// synthetic, no printed producer): "Choose target creature. Each opponent
/// may gain control of all nontoken permanents owned by that creature's
/// controller." The ownership reference reads the target's live controller.
fn r5_1_probe_spell(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, PlayerFilter,
        TargetFilter, TypedFilter,
    };
    let steal = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainControlAll {
            target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::NonToken,
                FilterProp::Owned {
                    controller: ControllerRef::ParentTargetController,
                },
            ])),
        },
    )
    .player_scope(PlayerFilter::Opponent)
    .optional();
    let root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    )
    .sub_ability(steal);
    let mut builder = scenario.add_spell_to_hand(owner, "R5-1 Probe", true);
    builder.with_ability_definition(root);
    builder.with_mana_cost(ManaCost::zero()).id()
}

/// DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. Probed at
/// 151bf7c05 (before the class failed closed): P1 +3, because P2's seat
/// re-read `ParentTargetController` as P0 after P0 took Anchor and also took
/// the bear; the rules give P1 +2. An optional head pauses between seats, so
/// whatever its population reads the producer is unsupported, and Zidane
/// fires for no player.
#[test]
fn r5_1_owned_by_a_live_reference_fails_closed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P1);
    let anchor = scenario.add_vanilla(P1, 2, 2);
    let bear = scenario.add_vanilla(P0, 2, 2);
    let steal_bear = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let probe = r5_1_probe_spell(&mut scenario, P1);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    runner.cast(steal_bear).target_object(bear).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, bear),
        P1,
        "REACH: P1 controls P0's bear"
    );
    let before = treasures(&runner, P1);

    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(probe).target_object(anchor).commit();
    for _ in 0..3 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
        {
            let _ = runner.act(GameAction::PassPriority);
        }
    }
    for _ in 0..2 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            answer_optional(&mut runner);
        }
    }
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, anchor),
        P0,
        "REACH: P0's seat took Anchor"
    );
    assert_eq!(
        controller(&runner, zidane),
        P0,
        "REACH: P0's seat took Zidane"
    );
    assert_eq!(
        treasures(&runner, P1) - before,
        0,
        "the class fires for no player"
    );
}

/// R5-2 probe: a retained continuation that removes a later seat's member.
const RETURN_AND_DESTROY: &str = "Each player may gain control of all nontoken permanents they \
    own and destroy target creature.";

/// DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. Probed at
/// 151bf7c05: P1 +1, because P0's seat ran the retained destroy on B before
/// P2's seat could take B back; CR 608.2e processes the return for every player
/// first (P1 +2). Any node retained in the seat template makes the producer
/// unsupported, and Zidane fires for no player.
#[test]
fn r5_2_retained_destroy_fails_closed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P1);
    let a = scenario.add_vanilla(P0, 2, 2);
    let b = scenario.add_vanilla(P2, 2, 2);
    let steal_a = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let steal_b = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Return and Destroy", true, RETURN_AND_DESTROY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    runner.cast(steal_a).target_object(a).resolve();
    runner.advance_until_stack_empty();
    runner.cast(steal_b).target_object(b).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (controller(&runner, a), controller(&runner, b)),
        (P1, P1),
        "REACH"
    );
    let before = treasures(&runner, P1);

    give_priority(&mut runner, P0);
    runner.cast(spell).target_object(b).commit();
    for _ in 0..3 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
        {
            let _ = runner.act(GameAction::PassPriority);
        }
    }
    for _ in 0..3 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            answer_optional(&mut runner);
        }
    }
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, a), P0, "REACH: P0's seat took A back");
    assert_eq!(
        runner.state().objects[&b].zone,
        Zone::Graveyard,
        "REACH: the retained destroy killed B"
    );
    assert_eq!(
        controller(&runner, zidane),
        P1,
        "REACH: Zidane stayed with P1"
    );
    assert_eq!(
        treasures(&runner, P1) - before,
        0,
        "the class fires for no player"
    );
}

/// Labelled synthetic for R6-1: the R4-1 population, mandatory, with an
/// unless-payment per player (CR 118.12a) instead of "may".
const RETURN_SMALL_UNLESS_PAY: &str = "Each player gains control of all creatures they own with \
    power 3 or less unless they pay {1}.";

/// R6-1: DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. An
/// unless-payment is a per-seat pause (CR 118.12a), so the head isn't one bare
/// mass action. Probed at
/// cc30bfe87's classifier (`Head`, capture taken): `base_power` 3 gave P1 +2
/// and moved the creature to P2, where the rules (CR 608.2e + CR 611.2c) give
/// P1 +1 and leave it with P1. The producer is unsupported, and Zidane fires
/// for no player.
#[test]
fn r6_1_unless_payment_head_fails_closed() {
    for base_power in [3, 2] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P1);
        let trostani = scenario
            .add_creature_from_oracle(P0, "Trostani Discordant", 1, 4, TROSTANI)
            .id();
        let p2_creature = scenario.add_vanilla(P2, base_power, base_power);
        let steal_trostani = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let steal_creature = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let returns = scenario
            .add_spell_to_hand_from_oracle(P0, "Return Unless Paid", true, RETURN_SMALL_UNLESS_PAY)
            .with_mana_cost(ManaCost::zero())
            .id();
        for player in [P0, P1, P2] {
            scenario.with_mana_pool(
                player,
                vec![ManaUnit::new(
                    ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
        }
        let mut runner = scenario.build();

        give_priority(&mut runner, P1);
        runner
            .cast(steal_trostani)
            .target_object(trostani)
            .resolve();
        runner.advance_until_stack_empty();
        runner
            .cast(steal_creature)
            .target_object(p2_creature)
            .resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            controller(&runner, trostani),
            P1,
            "REACH: P1 controls Trostani"
        );
        assert_eq!(
            controller(&runner, p2_creature),
            P1,
            "REACH: P1 controls P2's creature"
        );
        assert_eq!(
            power(&runner, p2_creature),
            base_power + 1,
            "REACH: P1's anthem"
        );
        assert_eq!(power(&runner, zidane), 4, "REACH: P1's anthem pumps Zidane");
        let before = treasures(&runner, P1);

        give_priority(&mut runner, P0);
        cast_and_pass(&mut runner, returns);
        let captured = !runner.state().control_change_captures.is_empty();
        let mut seats = 0;
        for _ in 0..3 {
            if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
                if seats == 1 {
                    assert_eq!(
                        controller(&runner, trostani),
                        P0,
                        "REACH: P0 declined first"
                    );
                    assert_eq!(
                        power(&runner, p2_creature),
                        base_power,
                        "REACH: the anthem left P1 before the later seats"
                    );
                }
                seats += 1;
                runner
                    .act(GameAction::PayUnlessCost { pay: false })
                    .expect("the seat declines");
            }
        }
        assert_eq!(seats, 3, "REACH: every seat reached its unless-payment");
        runner.advance_until_stack_empty();
        assert!(
            !captured,
            "the class takes no pre-seat capture: it fails closed"
        );
        assert_eq!(
            treasures(&runner, P1) - before,
            0,
            "base_power {base_power}: the class fires for no player"
        );
    }
}

/// Labelled synthetic for R7-1: a mandatory scoped return with an
/// unless-sacrifice rider (CR 118.12a + CR 701.21a).
const RETURN_UNLESS_SACRIFICE: &str = "Each player gains control of all nontoken permanents they \
    own unless they sacrifice a creature.";

/// R7-1: DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. A
/// payment runs between seats, so a seat can mutate the board before a later
/// seat's control change: P0 (Zidane, a spare bear, and P1's stolen bear) pays
/// by sacrificing Zidane, then P1 declines and takes its bear back. Zidane was
/// gone before that change, so the rules give P0 +0 (CR 603.10d looks back to
/// the change, not to the start of the spell); sacrificing the spare instead
/// gives P0 +1. Probed at 616fe406b's classifier (`Head`, a capture taken
/// before the first payment): +1 in both legs. An unless-payment head is
/// unsupported, and Zidane fires for no player.
#[test]
fn r7_1_unless_sacrifice_head_fails_closed() {
    for sacrifice_zidane in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let spare = scenario.add_vanilla(P0, 2, 2);
        let theirs = scenario.add_vanilla(P1, 2, 2);
        let steal = add_free_spell(&mut scenario, P0, "Act of Treason", ACT_OF_TREASON);
        let returns = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Return Unless Sacrificed",
                true,
                RETURN_UNLESS_SACRIFICE,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        give_priority(&mut runner, P0);
        runner.cast(steal).target_object(theirs).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, theirs), P0, "REACH: P0 stole the bear");
        let before = treasures(&runner, P0);

        give_priority(&mut runner, P0);
        cast_and_pass(&mut runner, returns);
        let mut seats = Vec::new();
        let mut sacrificed = false;
        for _ in 0..4 {
            match runner.state().waiting_for.clone() {
                WaitingFor::UnlessPayment { player, .. } => {
                    assert!(
                        runner.state().control_change_captures.is_empty(),
                        "the class takes no pre-seat capture"
                    );
                    seats.push(player);
                    runner
                        .act(GameAction::PayUnlessCost { pay: player == P0 })
                        .expect("the seat answers");
                }
                WaitingFor::WardSacrificeChoice {
                    player, permanents, ..
                } => {
                    let chosen = if sacrifice_zidane { zidane } else { spare };
                    assert_eq!(player, P0, "REACH: P0 pays");
                    assert!(permanents.contains(&chosen), "REACH: {chosen:?} eligible");
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![chosen],
                        })
                        .expect("P0 sacrifices");
                    sacrificed = true;
                }
                _ => {}
            }
        }
        runner.advance_until_stack_empty();
        assert_eq!(seats, vec![P0, P1], "REACH: each seat reached its payment");
        assert!(sacrificed, "REACH: P0 paid by sacrificing");
        let gone = if sacrifice_zidane { zidane } else { spare };
        assert_eq!(
            runner.state().objects[&gone].zone,
            Zone::Graveyard,
            "REACH: the sacrificed creature left first"
        );
        assert_eq!(
            controller(&runner, theirs),
            P1,
            "REACH: P1 declined and took its bear back"
        );
        assert_eq!(
            treasures(&runner, P0) - before,
            0,
            "sacrifice_zidane {sacrifice_zidane}: the class fires for no player"
        );
    }
}

/// R7-2 synthetic, hand-built (the parsed "if you control …" binds "you" to the
/// seat, a separate parse defect): "If you control three or more creatures,
/// each player may gain control of all nontoken permanents they own."
fn r7_2_conditional_return(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    use engine::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect,
        FilterProp, PlayerFilter, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
    };
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainControlAll {
            target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::NonToken,
                FilterProp::Owned {
                    controller: ControllerRef::ScopedPlayer,
                },
            ])),
        },
    )
    .player_scope(PlayerFilter::All)
    .optional()
    .condition(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 3 },
    });
    let mut builder = scenario.add_spell_to_hand(owner, "R7-2 Conditional Return", true);
    builder.with_ability_definition(def);
    builder.with_mana_cost(ManaCost::zero()).id()
}

/// R7-2: DOCUMENTS A FAIL-CLOSED PRODUCER CLASS, not rules-correct counts. The
/// head's condition is re-checked for each seat on the board an earlier seat
/// changed. Three players, P0 active; P1 controls Zidane and has stolen A (P0's)
/// and B (P2's), exactly three creatures. P0's seat takes A back, and P2's seat
/// then fails the condition, so B stays with P1; the rules (CR 608.2e + CR
/// 611.2c) give P1 +2 and B to P2. With a spare P1 creature the condition holds
/// for every seat (P1 +2 by the rules). Probed at 616fe406b's classifier
/// (`Head`, capture taken): P1 +1 and +2. A conditional head is unsupported,
/// and Zidane fires for no player.
#[test]
fn r7_2_condition_rechecked_per_seat_fails_closed() {
    for spare in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P1);
        let a = scenario.add_vanilla(P0, 2, 2);
        let b = scenario.add_vanilla(P2, 2, 2);
        if spare {
            scenario.add_vanilla(P1, 2, 2);
        }
        let steal_a = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let steal_b = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let spell = r7_2_conditional_return(&mut scenario, P1);
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);
        runner.cast(steal_a).target_object(a).resolve();
        runner.advance_until_stack_empty();
        runner.cast(steal_b).target_object(b).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(
            (controller(&runner, a), controller(&runner, b)),
            (P1, P1),
            "REACH: P1 stole both"
        );
        let before = treasures(&runner, P1);

        give_priority(&mut runner, P1);
        runner.state_mut().active_player = P0;
        cast_and_pass(&mut runner, spell);
        let mut seats = Vec::new();
        for _ in 0..4 {
            if let WaitingFor::OptionalEffectChoice { player, .. } =
                runner.state().waiting_for.clone()
            {
                assert!(
                    runner.state().control_change_captures.is_empty(),
                    "the class takes no pre-seat capture"
                );
                seats.push(player);
                answer_optional(&mut runner);
            }
        }
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, a), P0, "REACH: P0's seat took A back");
        assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane stayed");
        if spare {
            assert_eq!(
                seats,
                vec![P0, P1, P2],
                "REACH: every seat passed the check"
            );
            assert_eq!(controller(&runner, b), P2, "REACH: P2 took B back");
        } else {
            assert_eq!(
                seats,
                vec![P0, P1],
                "REACH: P2's seat failed the re-checked condition"
            );
            assert_eq!(controller(&runner, b), P1, "REACH: B stayed with P1");
        }
        assert_eq!(
            treasures(&runner, P1) - before,
            0,
            "spare {spare}: the class fires for no player"
        );
    }
}

/// R8-1 synthetic, hand-built: "Return all creature cards from all graveyards to
/// the battlefield under your control. Each player gains control of each of
/// those permanents they own." The scoped head is mandatory and bare, but its
/// population (`And(LastZoneChanged, Typed(Permanent, Owned(ScopedPlayer)))`)
/// isn't an allowlisted printed shape.
fn r8_1_return_then_hand_back(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, PlayerFilter,
        SubAbilityLink, TargetFilter, TypedFilter,
    };
    let mut hand_back = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainControlAll {
            target: TargetFilter::And {
                filters: vec![
                    TargetFilter::LastZoneChanged,
                    TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer,
                        },
                    ])),
                ],
            },
        },
    )
    .player_scope(PlayerFilter::All);
    hand_back.sub_link = SubAbilityLink::SequentialSibling;
    let root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZoneAll {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::creature()),
            enters_under: Some(ControllerRef::You),
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            library_shuffle: Default::default(),
            random_order: false,
        },
    )
    .sub_ability(hand_back);
    let mut builder = scenario.add_spell_to_hand(owner, "R8-1 Return Then Hand Back", true);
    builder.with_ability_definition(root);
    builder.with_mana_cost(ManaCost::zero()).id()
}

/// Main's scope stop: a fan-out whose seat template isn't an allowlisted
/// printed shape fails closed, so Zidane fires for no player, even when its
/// head is mandatory and bare. R8-1's board: P1 controls Zidane; P1's spell
/// returns P0's bear A and P2's bear B from their graveyards under P1's
/// control, then each player gains control of those of them they own. The
/// rules (CR 608.2e + CR 603.2c) give P1 +2; the engine's per-seat population
/// re-read (the last-zone-changed set is replaced after the first seat) leaves
/// B with P1. Either way, P1 gets +0 here.
#[test]
fn a_non_allowlisted_fan_out_does_not_fire() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P1);
    let a = scenario.add_creature_to_graveyard(P0, "Bears A", 2, 2).id();
    let b = scenario.add_creature_to_graveyard(P2, "Bears B", 2, 2).id();
    let spell = r8_1_return_then_hand_back(&mut scenario, P1);
    let mut runner = scenario.build();
    let before = treasures(&runner, P1);

    give_priority(&mut runner, P1);
    runner.state_mut().active_player = P0;
    cast_and_pass(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&a].zone,
        Zone::Battlefield,
        "REACH: A returned"
    );
    assert_eq!(
        runner.state().objects[&b].zone,
        Zone::Battlefield,
        "REACH: B returned"
    );
    assert_eq!(
        controller(&runner, a),
        P0,
        "REACH: the scoped head ran and P0 took A back from P1"
    );
    assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane stayed");
    assert_eq!(
        treasures(&runner, P1) - before,
        0,
        "a non-allowlisted fan-out fires for no player"
    );
}

/// Verbatim Oracle text (Scryfall), Annie Joins Up (Legendary Enchantment).
const ANNIE_JOINS_UP: &str = "When Annie Joins Up enters, it deals 5 damage to target \
    creature or planeswalker an opponent controls.\nIf a triggered ability of a legendary \
    creature you control triggers, that ability triggers an additional time.";

fn add_annie(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(player, "Annie Joins Up", ANNIE_JOINS_UP)
        .as_legendary()
        .id()
}

/// F1 (maintainer round 2; CR 603.10d + CR 603.2d + CR 603.3a): an additional
/// trigger is counted as of immediately before the control change. P0 controls
/// Annie Joins Up and legendary Zidane; P1 casts Act of Treason.
/// - On Zidane: Zidane's loss trigger is P0's (the watcher latched under P0),
///   and Annie judged Zidane then: a legendary creature P0 controlled. P0 gets
///   exactly 2, though Zidane is P1's afterwards.
/// - Paired, same board, on P0's bear: Zidane never moves, so P0 gets 2 by the
///   ordinary path too.
#[test]
fn annie_doubles_zidanes_trigger_when_zidane_itself_is_stolen() {
    for steal_zidane in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let annie = add_annie(&mut scenario, P0);
        let bear = scenario.add_vanilla(P0, 2, 2);
        let steal = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
        let mut runner = scenario.build();
        let stolen = if steal_zidane { zidane } else { bear };

        give_priority(&mut runner, P1);
        runner.cast(steal).target_object(stolen).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, stolen), P1, "REACH: P1 stole it");
        assert_eq!(controller(&runner, annie), P0, "REACH: Annie stayed");
        assert_eq!(
            (treasures(&runner, P0), treasures(&runner, P1)),
            (2, 0),
            "steal_zidane {steal_zidane}: Zidane's trigger and its additional trigger"
        );
    }
}

/// Labelled synthetic: one instruction that takes Annie and Zidane together.
const STEAL_ALL_PERMANENTS: &str = "Gain control of all permanents target opponent controls.";

/// F1, the doubler's side (CR 603.2d + CR 603.10d): Annie is stolen by the same
/// instruction as the permanents whose losses trigger Zidane. Immediately
/// before it, P0 controlled Annie, so each of Zidane's two triggers (Annie's
/// loss, Zidane's loss) triggers an additional time: P0 gets exactly 4. A
/// doubler read after the change (P1's Annie) adds nothing, for 2.
#[test]
fn a_doubler_stolen_with_the_watcher_keeps_its_event_time_eligibility() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let annie = add_annie(&mut scenario, P0);
    let steal = add_free_spell(&mut scenario, P1, "Steal Everything", STEAL_ALL_PERMANENTS);
    let mut runner = scenario.build();
    let p0_permanents = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[*id].controller == P0)
        .count();
    assert_eq!(
        p0_permanents, 2,
        "REACH: P0 controls exactly Annie and Zidane"
    );

    give_priority(&mut runner, P1);
    runner.cast(steal).target_player(P0).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (controller(&runner, annie), controller(&runner, zidane)),
        (P1, P1),
        "REACH: one instruction took both"
    );
    assert_eq!(
        (treasures(&runner, P0), treasures(&runner, P1)),
        (4, 0),
        "two losses, each triggering twice"
    );
}

/// Verbatim Oracle text (Scryfall), Scrambleverse.
const SCRAMBLEVERSE: &str = "For each nonland permanent, choose a player at random. Then each \
    player gains control of each permanent for which they were chosen. Untap those permanents.";

fn controller_changes(events: &[GameEvent]) -> Vec<(ObjectId, ControlChangeEvidence)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                evidence,
                ..
            } => Some((*object_id, *evidence)),
            _ => None,
        })
        .collect()
}

/// F2 (maintainer round 2; CR 608.2c + CR 608.2e): Scrambleverse's parsed
/// population drops "for which they were chosen", so it doesn't partition the
/// permanents by their randomly chosen recipient, and a land (never a
/// Scrambleverse member: "for each nonland permanent") can be handed to a
/// seat. Not an allowlisted shape: every change it publishes is `Unrecorded`,
/// so no land loss is observed and P1's Zidane creates nothing.
///
/// Paired positive on the same board: Brooding Saurian's allowlisted action
/// ("each player gains control of all nontoken permanents they own") hands
/// P0's land back from P1, an opponent gaining a permanent from P1: P1 +1.
#[test]
fn scrambleverse_publishes_no_observed_land_loss() {
    for scramble in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P1);
        let p1_land = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Red);
        let p0_land = scenario.add_basic_land(P0, engine::types::mana::ManaColor::White);
        scenario.add_vanilla(P0, 2, 2);
        let steal = add_free_spell(&mut scenario, P1, "Steal", STEAL_ANY_PERMANENT);
        let spell = if scramble {
            add_free_spell(&mut scenario, P0, "Scrambleverse", SCRAMBLEVERSE)
        } else {
            add_free_spell(
                &mut scenario,
                P0,
                "Mandatory Return",
                MANDATORY_RETURN_INSTANT,
            )
        };
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);
        runner.cast(steal).target_object(p0_land).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(controller(&runner, p0_land), P1, "REACH: P1 took P0's land");
        let before = treasures(&runner, P1);

        give_priority(&mut runner, P0);
        let outcome = runner.cast(spell).resolve();
        let changes = controller_changes(outcome.events());
        runner.advance_until_stack_empty();
        assert!(!changes.is_empty(), "REACH: the spell changed control");
        if scramble {
            assert!(
                changes.iter().all(|(_, evidence)| evidence.is_unrecorded()),
                "Scrambleverse publishes no observed change: {changes:?}"
            );
            assert!(
                changes
                    .iter()
                    .filter(|(id, _)| *id == p1_land || *id == p0_land)
                    .all(|(_, evidence)| evidence.is_unrecorded()),
                "no land loss is observed"
            );
            assert_eq!(treasures(&runner, P1) - before, 0, "P1 creates nothing");
        } else {
            assert_eq!(
                controller(&runner, p0_land),
                P0,
                "REACH: P0's land went home"
            );
            assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane stayed");
            assert_eq!(
                treasures(&runner, P1) - before,
                1,
                "an opponent gained P0's land from P1"
            );
        }
    }
}

/// Verbatim Oracle text (Scryfall), Aminatou, the Fateshifter (loyalty 3).
const AMINATOU: &str = "+1: Draw a card, then put a card from your hand on top of your \
    library.\n\u{2212}1: Exile another target permanent you own, then return it to the \
    battlefield under your control.\n\u{2212}6: Choose left or right. Each player gains control \
    of all nonland permanents other than Aminatou controlled by the next player in the chosen \
    direction.\nAminatou, the Fateshifter can be your commander.";

/// F2 (maintainer round 2; CR 102.3 + CR 608.2c + CR 608.2e): Two-Headed
/// Giant, P0/P1 against P2/P3. Aminatou's parsed −6 population drops "the
/// next player in the chosen direction", so a seat's population isn't the
/// permanents the printed text hands it, and a P1→P0 handoff (teammates, never
/// an opponent's gain) can't be told from an opponent's gain. Not an
/// allowlisted shape: every change it publishes is `Unrecorded`, and P1's
/// Zidane creates nothing, in either direction.
///
/// Paired positive in the same topology: P1 controls a P2-owned creature
/// (an opponent's) and a P0-owned creature (a teammate's); Brooding Saurian's
/// allowlisted action hands both back. Only the opponent's gain counts: P1 +1.
#[test]
fn aminatous_unpartitioned_team_handoff_does_not_fire() {
    for direction in ["Left", "Right"] {
        let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P1);
        let aminatou = scenario
            .add_planeswalker_from_oracle(P2, "Aminatou, the Fateshifter", "Aminatou", 7, AMINATOU)
            .as_legendary()
            .id();
        scenario.add_vanilla(P0, 2, 2);
        let mut runner = scenario.build();
        give_priority(&mut runner, P2);
        let before = treasures(&runner, P1);

        let outcome = runner
            .activate(aminatou, 2)
            .choose_option(direction)
            .resolve();
        let events = outcome.events().to_vec();
        runner.advance_until_stack_empty();
        let changes = controller_changes(&events);
        assert!(
            changes.iter().any(|(id, _)| *id == zidane),
            "REACH ({direction}): the −6 moved P1's Zidane: {changes:?}"
        );
        assert!(
            changes.iter().all(|(_, evidence)| evidence.is_unrecorded()),
            "{direction}: Aminatou's −6 publishes no observed change: {changes:?}"
        );
        assert_eq!(
            treasures(&runner, P1) - before,
            0,
            "{direction}: P1 creates nothing"
        );
    }

    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P1);
    let opponents = scenario.add_vanilla(P2, 2, 2);
    let teammates = scenario.add_vanilla(P0, 2, 2);
    let steal_a = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let steal_b = add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON);
    let returns = add_free_spell(
        &mut scenario,
        P2,
        "Mandatory Return",
        MANDATORY_RETURN_INSTANT,
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    runner.cast(steal_a).target_object(opponents).resolve();
    runner.advance_until_stack_empty();
    runner.cast(steal_b).target_object(teammates).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (
            controller(&runner, opponents),
            controller(&runner, teammates)
        ),
        (P1, P1),
        "REACH: P1 holds both"
    );
    let before = treasures(&runner, P1);
    give_priority(&mut runner, P2);
    runner.cast(returns).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (
            controller(&runner, opponents),
            controller(&runner, teammates)
        ),
        (P2, P0),
        "REACH: both went home"
    );
    assert_eq!(controller(&runner, zidane), P1, "REACH: Zidane stayed");
    assert_eq!(
        treasures(&runner, P1) - before,
        1,
        "only the opponent's gain counts (CR 102.3)"
    );
}

/// Verbatim Oracle text (Scryfall), Risky Move.
const RISKY_MOVE: &str = "At the beginning of each player's upkeep, that player gains control \
    of this enchantment.\nWhen you gain control of this enchantment from another player, choose \
    a creature you control and an opponent. Flip a coin. If you lose the flip, that opponent \
    gains control of that creature.";
/// Verbatim Oracle text (Scryfall), Opalescence.
const OPALESCENCE: &str = "Each other non-Aura enchantment is a creature in addition to its \
    other types and has base power and base toughness each equal to its mana value.";
/// Verbatim Oracle text (Scryfall), Overwhelming Splendor.
const OVERWHELMING_SPLENDOR: &str = "Enchant player\nCreatures enchanted player controls lose all \
    abilities and have base power and toughness 1/1.\nEnchanted player can't activate abilities \
    that aren't mana abilities or loyalty abilities.";
/// Verbatim Oracle text (Scryfall), Word of Seizing.
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't \
    cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and \
    gain control of it until end of turn. It gains haste until end of turn.";

fn has_control_change_trigger(runner: &GameRunner, id: ObjectId) -> bool {
    let state = runner.state();
    engine::game::functioning_abilities::active_trigger_definitions(state, &state.objects[&id]).any(
        |active| {
            matches!(
                active.definition.mode,
                engine::types::triggers::TriggerMode::ChangesController { .. }
            )
        },
    )
}

/// P1 casts Word of Seizing on P0's Risky Move. Opalescence (P1's) makes Risky
/// Move a creature, so Overwhelming Splendor, when it curses `cursed`, strips
/// its abilities while that player controls it. Returns (Risky Move had its
/// gain trigger before, has it after, Risky Move triggers that resulted).
fn risky_move_gain_board(cursed: Option<PlayerId>) -> (bool, bool, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let risky = scenario
        .add_enchantment_from_oracle(P0, "Risky Move", RISKY_MOVE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red, ManaCostShard::Red],
            generic: 3,
        })
        .id();
    scenario.add_enchantment_from_oracle(P1, "Opalescence", OPALESCENCE);
    scenario.add_vanilla(P1, 2, 2);
    scenario.add_vanilla(P0, 2, 2);
    let splendor = cursed.map(|_| {
        let mut builder = scenario.add_creature_from_oracle(
            P1,
            "Overwhelming Splendor",
            0,
            0,
            OVERWHELMING_SPLENDOR,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    });
    let word = scenario
        .add_spell_to_hand_from_oracle(P1, "Word of Seizing", true, "")
        .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    if let (Some(splendor), Some(player)) = (splendor, cursed) {
        engine::game::effects::attach::attach_to_player(runner.state_mut(), splendor, player);
    }
    engine::game::layers::evaluate_layers(runner.state_mut());
    let before = has_control_change_trigger(&runner, risky);

    give_priority(&mut runner, P1);
    runner.cast(word).target_object(risky).commit();
    let mut events = Vec::new();
    for _ in 0..2 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == word)
        {
            if let Ok(result) = runner.act(GameAction::PassPriority) {
                events.extend(result.events);
            }
        }
    }
    assert_eq!(controller(&runner, risky), P1, "REACH: P1 took Risky Move");
    let after = has_control_change_trigger(&runner, risky);
    let triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == risky && entry.controller == P1)
        .count();
    (before, after, triggers)
}

/// R2-1 (CR 603.10): "When you gain control of this enchantment from another
/// player" is an ordinary trigger, checked immediately after the event, unlike
/// the CR 603.10d lose side. Which abilities exist is read from the board after
/// the change:
/// - uncursed: Risky Move has its trigger throughout; one P1 trigger;
/// - Splendor on P0 (the loser): it had no abilities under P0 and regains
///   them under P1; exactly one P1 trigger;
/// - Splendor on P1 (the gainer): it had them under P0 and loses them under
///   P1; no trigger.
#[test]
fn risky_moves_gain_trigger_reads_the_board_after_the_change() {
    assert_eq!(
        risky_move_gain_board(None),
        (true, true, 1),
        "uncursed positive"
    );
    assert_eq!(
        risky_move_gain_board(Some(P0)),
        (false, true, 1),
        "abilities restored by the change"
    );
    assert_eq!(
        risky_move_gain_board(Some(P1)),
        (true, false, 0),
        "abilities removed by the change"
    );
}

/// Verbatim Oracle text (Scryfall), Roaming Throne.
const ROAMING_THRONE: &str = "Ward {2}\nAs this creature enters, choose a creature type.\nThis \
    creature is the chosen type in addition to its other types.\nIf a triggered ability of \
    another creature you control of the chosen type triggers, it triggers an additional time.";
/// Labelled synthetic: steal, then sacrifice it (the thief does).
const STEAL_AND_SACRIFICE: &str = "Gain control of target creature. Sacrifice it.";
/// Labelled synthetic: steal, then blink into a new object under the thief.
const STEAL_AND_BLINK: &str =
    "Gain control of target creature. Exile it, then return that card to the battlefield under your control.";

/// What happens to the stolen Roaming Throne in the same resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ThroneAftermath {
    /// Act of Treason: it stays, now P1's.
    Kept,
    /// It's sacrificed: it ends the resolution in the graveyard.
    Sacrificed,
    /// It's exiled and returned under P1, a new object; P1 makes its
    /// as-enters choice (Elf), a choice boundary inside the resolution.
    Blinked,
}

/// R2-2 (CR 603.2d + CR 603.10d): the doubler is judged as it was immediately
/// before the change, including its source-relative facts. P0 controls Zidane
/// (a Human) and a Roaming Throne that chose `chosen`; P1 steals the Throne,
/// paying its ward. Zidane triggers for the loss, and the Throne, as it was
/// then, was P0's doubler of `chosen`. Returns P0's Treasures.
fn roaming_throne_board(chosen: &str, aftermath: ThroneAftermath) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let throne = scenario
        .add_creature_to_hand_from_oracle(P0, "Roaming Throne", 4, 4, "")
        .from_oracle_text_with_keywords(&["Ward"], ROAMING_THRONE)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let splendor = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Overwhelming Splendor",
            0,
            0,
            OVERWHELMING_SPLENDOR,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };
    let steal = match aftermath {
        ThroneAftermath::Kept => {
            add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON)
        }
        ThroneAftermath::Sacrificed => add_free_spell(
            &mut scenario,
            P1,
            "Steal and Sacrifice",
            STEAL_AND_SACRIFICE,
        ),
        ThroneAftermath::Blinked => {
            add_free_spell(&mut scenario, P1, "Steal and Blink", STEAL_AND_BLINK)
        }
    };
    // The Throne's ward {2}: P1 pays it, so the steal resolves.
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to_player(runner.state_mut(), splendor, P1);

    // The Throne is cast and its as-enters choice made.
    give_priority(&mut runner, P0);
    runner.cast(throne).commit();
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: chosen.to_string(),
                    })
                    .expect("the creature type is chosen");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => break,
        }
    }
    let throne = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Roaming Throne")
        .expect("REACH: the Throne entered");
    assert!(
        runner.state().objects[&throne]
            .chosen_attributes
            .iter()
            .any(|attribute| matches!(
                attribute,
                engine::types::ability::ChosenAttribute::CreatureType(t) if t == chosen
            )),
        "REACH: {chosen} was really chosen"
    );
    assert_eq!(controller(&runner, zidane), P0);
    let before = treasures(&runner, P0);

    give_priority(&mut runner, P1);
    runner.cast(steal).target_object(throne).commit();
    let mut events = Vec::new();
    for _ in 0..8 {
        if !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == steal)
            && !matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. })
        {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::UnlessPayment { .. } => GameAction::PayUnlessCost { pay: true },
            // The returned Throne's own as-enters choice, made by P1: a type
            // that doesn't match Zidane.
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: "Elf".to_string(),
            },
            _ => GameAction::PassPriority,
        };
        if let Ok(result) = runner.act(action) {
            events.extend(result.events);
        }
    }
    let evidence = events
        .iter()
        .find_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                evidence,
                ..
            } if *object_id == throne => Some(*evidence),
            _ => None,
        })
        .expect("REACH: P1 gained control of the Throne");
    let observation = runner
        .state()
        .control_change_observation(evidence)
        .expect("REACH: Zidane's pending trigger keeps the observation");
    assert!(
        observation
            .doublers
            .iter()
            .any(|doubler| doubler.source_id() == throne && doubler.controller() == P0),
        "REACH: the observation latched P0's Throne as a doubler"
    );
    if aftermath == ThroneAftermath::Sacrificed {
        assert_eq!(
            runner.state().objects[&throne].zone,
            Zone::Graveyard,
            "REACH: the Throne was sacrificed in the same resolution"
        );
    }
    if aftermath == ThroneAftermath::Blinked {
        let returned = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == "Roaming Throne")
            .expect("REACH: the Throne returned");
        let object = &runner.state().objects[&returned];
        assert_eq!(object.controller, P1, "REACH: it returned under P1");
        assert!(
            object.chosen_attributes.iter().any(|attribute| matches!(
                attribute,
                engine::types::ability::ChosenAttribute::CreatureType(t) if t == "Elf"
            )),
            "REACH: the returned Throne is a new object that chose Elf"
        );
    }
    runner.advance_until_stack_empty();
    treasures(&runner, P0) - before
}

/// R2-2: the Throne's chosen type is latched with it, and judged as it was
/// when Zidane's ability triggered. A Human Throne: 2 whether it stays, is
/// sacrificed in the same resolution or is blinked. An Elf Throne: 1.
///
/// Production documentation, not a discriminator: the blinked Throne's
/// as-enters choice is a boundary at which the pending triggers are collected
/// before it chooses again, and the sacrificed Throne's departed object still
/// answers its chosen type. The doubling seam is discriminated by
/// `the_doubling_seam_reads_the_latched_doublers_chosen_type`.
#[test]
fn a_latched_doubler_keeps_its_chosen_type() {
    use ThroneAftermath::*;
    for aftermath in [Kept, Sacrificed, Blinked] {
        assert_eq!(
            roaming_throne_board("Human", aftermath),
            2,
            "Human Throne, {aftermath:?}"
        );
    }
    assert_eq!(
        roaming_throne_board("Elf", Sacrificed),
        1,
        "non-matching type"
    );
}

/// R2-2 at the doubling seam (CR 603.2d + CR 603.10d), discriminating: the
/// event carries evidence captured from the board, latching Zidane under P0
/// and P0's Roaming Throne that chose Human. The live Throne then reads Elf (as
/// a new object at the same id would), and the change is collected. Zidane's
/// trigger is counted against the Throne as latched: P0's Human doubler, so it
/// triggers twice. A doubler read from the live object (Elf) wouldn't double.
#[test]
fn the_doubling_seam_reads_the_latched_doublers_chosen_type() {
    assert_eq!(
        latched_throne_seam_board(&["Human"]),
        2,
        "Zidane's trigger and its additional trigger from the latched Human Throne"
    );
}

/// Regression: the latched reader uses the same last-stored-choice convention
/// as the live accessor (`GameObject::chosen_creature_type`). A latched Throne
/// that chose Human and then Elf doesn't double Zidane's (Human) trigger; one
/// that chose Elf and then Human does.
#[test]
fn a_latched_doubler_reads_its_last_chosen_type() {
    assert_eq!(
        latched_throne_seam_board(&["Human", "Elf"]),
        1,
        "Human, then Elf"
    );
    assert_eq!(
        latched_throne_seam_board(&["Elf", "Human"]),
        2,
        "Elf, then Human"
    );
}

/// Zidane's triggers on the stack after the seam board: the capture latches
/// P0's Throne with `chosen` (in choice order), then the live Throne reads Elf
/// under P1 and the recorded change is collected.
fn latched_throne_seam_board(chosen: &[&str]) -> usize {
    use engine::types::ability::{ChosenAttribute, ContinuousModification, Duration, TargetFilter};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zidane = add_zidane(&mut scenario, P0);
    let throne = scenario
        .add_creature_from_oracle(P0, "Roaming Throne", 4, 4, "")
        .from_oracle_text_with_keywords(&["Ward"], ROAMING_THRONE)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.objects.get_mut(&throne).unwrap().chosen_attributes = chosen
        .iter()
        .map(|chosen| ChosenAttribute::CreatureType(chosen.to_string()))
        .collect();
    engine::game::layers::flush_layers(state);

    let evidence = state.control_change_evidence_for_test(throne);
    let observation = state
        .control_change_observation(evidence)
        .expect("REACH: the evidence is recorded");
    assert!(
        observation
            .doublers
            .iter()
            .any(|doubler| doubler.source_id() == throne && doubler.controller() == P0),
        "REACH: the capture latched P0's Throne"
    );
    assert!(
        observation
            .watchers
            .iter()
            .any(|watcher| watcher.identity.reference.object_id == zidane),
        "REACH: the capture latched Zidane"
    );

    state
        .add_transient_continuous_effect(
            throne,
            P1,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: throne },
            vec![ContinuousModification::ChangeController],
            None,
        )
        .expect("the control effect installs");
    state.objects.get_mut(&throne).unwrap().chosen_attributes =
        vec![ChosenAttribute::CreatureType("Elf".to_string())];
    engine::game::layers::flush_layers(state);
    assert_eq!(
        state.objects[&throne].controller, P1,
        "REACH: P1 controls it"
    );

    engine::game::triggers::process_triggers(
        state,
        &[GameEvent::ControllerChanged {
            object_id: throne,
            old_controller: P0,
            new_controller: P1,
            evidence,
        }],
    );
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == zidane && entry.controller == P0)
        .count()
}

/// A labelled synthetic "triggers an additional time" static with scope
/// `affected` (CR 603.2d).
fn double_triggers(affected: TargetFilter) -> engine::types::ability::StaticDefinition {
    engine::types::ability::StaticDefinition::new(
        engine::types::statics::StaticMode::DoubleTriggers {
            cause: engine::types::statics::TriggerCause::Any,
        },
    )
    .affected(affected)
}

/// Labelled synthetic: a steal that pumps what it takes.
const STEAL_AND_PUMP: &str = "Gain control of target creature. It gets +4/+4 until end of turn.";

/// Casts `spell` (P1) on `target` and resolves it alone, returning its events.
fn resolve_targeted(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    give_priority(runner, P1);
    runner.cast(spell).target_object(target).commit();
    let mut events = Vec::new();
    for _ in 0..2 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == spell)
        {
            if let Ok(result) = runner.act(GameAction::PassPriority) {
                events.extend(result.events);
            }
        }
    }
    events
}

/// R2-2 residual (CR 603.2d + CR 603.10d): a latched doubler's scope is
/// judged with its characteristics as they were. P0 controls Zidane (3/3) and
/// a labelled synthetic 2/2 doubler whose scope is "creatures you control with
/// power greater than this's". P1 steals the doubler, pumping it to 6/6 with
/// `pump`. Zidane's trigger for that loss is P0's and, as things stood then,
/// Zidane (3) had greater power than the doubler (2): P0 gets 2 either way.
#[test]
fn a_latched_doubler_is_judged_with_its_power_then() {
    use engine::types::ability::{ControllerRef, FilterProp, TypedFilter};
    for pump in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let doubler = scenario
            .add_creature(P0, "Relative Doubler", 2, 2)
            .with_static_definition(double_triggers(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::PowerGTSource]),
            )))
            .id();
        let spell = if pump {
            add_free_spell(&mut scenario, P1, "Steal and Pump", STEAL_AND_PUMP)
        } else {
            add_free_spell(&mut scenario, P1, "Act of Treason", ACT_OF_TREASON)
        };
        let mut runner = scenario.build();
        let before = treasures(&runner, P0);

        let events = resolve_targeted(&mut runner, spell, doubler);
        let observed: Vec<_> = controller_changes(&events)
            .into_iter()
            .filter(|(_, evidence)| !evidence.is_unrecorded())
            .collect();
        assert_eq!(observed.len(), 1, "REACH: one observed control change");
        let observation = runner
            .state()
            .control_change_observation(observed[0].1)
            .expect("REACH: Zidane's pending trigger keeps the observation");
        let latched = observation
            .doublers
            .iter()
            .find(|latched| latched.source_id() == doubler)
            .expect("REACH: the doubler is latched");
        assert_eq!(
            (latched.controller(), latched.source.lki.power),
            (P0, Some(2)),
            "REACH: latched as P0's 2-power doubler"
        );
        assert!(
            observation.watchers.iter().any(|watcher| {
                watcher.identity.reference.object_id == zidane && watcher.lki.power == Some(3)
            }),
            "REACH: Zidane latched with power 3"
        );
        assert_eq!(controller(&runner, doubler), P1, "REACH: P1 took it");
        if pump {
            assert_eq!(
                runner.state().objects[&doubler].power,
                Some(6),
                "REACH: the live doubler is a 6/6"
            );
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            treasures(&runner, P0) - before,
            2,
            "pump {pump}: doubled, judged as things stood"
        );
    }
}

/// R2-3 (CR 603.2d): a doubler whose scope is itself ("~", `SelfRef`) is
/// judged by exact identity against the latched watcher. A labelled synthetic
/// "If a triggered ability of ~ triggers, it triggers an additional time" on:
/// - Zidane, with Zidane stolen (Word of Seizing): 2;
/// - Zidane, with P0's bear stolen instead: 2;
/// - the bear, with a second P0 bear stolen: 1 (the scope names the bear, not
///   Zidane).
///
/// A doubler naming itself by id (`SpecificObject`) isn't printed (the parser
/// emits `SelfRef` for "~"); the shared self-exclusion
/// (`filter_references_self`) leaves it undoubled on every path.
#[test]
fn a_self_scoped_doubler_is_judged_by_identity() {
    for (doubler_on_zidane, steal_zidane, expected) in
        [(true, true, 2), (true, false, 2), (false, false, 1)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let zidane = add_zidane(&mut scenario, P0);
        let bear = scenario.add_vanilla(P0, 2, 2);
        let other_bear = scenario.add_vanilla(P0, 2, 2);
        let word = scenario
            .add_spell_to_hand_from_oracle(P1, "Word of Seizing", true, "")
            .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let carrier = if doubler_on_zidane { zidane } else { bear };
        {
            let object = runner.state_mut().objects.get_mut(&carrier).unwrap();
            object
                .static_definitions
                .push(double_triggers(TargetFilter::SelfRef));
            std::sync::Arc::make_mut(&mut object.base_static_definitions)
                .push(double_triggers(TargetFilter::SelfRef));
        }
        engine::game::layers::flush_layers(runner.state_mut());
        let before = treasures(&runner, P0);
        let stolen = match (steal_zidane, doubler_on_zidane) {
            (true, _) => zidane,
            (false, true) => bear,
            (false, false) => other_bear,
        };

        resolve_targeted(&mut runner, word, stolen);
        assert_eq!(controller(&runner, stolen), P1, "REACH: P1 took it");
        runner.advance_until_stack_empty();
        assert_eq!(
            treasures(&runner, P0) - before,
            expected,
            "doubler on Zidane {doubler_on_zidane}, Zidane stolen {steal_zidane}"
        );
    }
}
