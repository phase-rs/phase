//! Crafty Cutpurse's resolved enters trigger creates a turn-long token
//! controller redirect that exists independently of its source (CR 113.7a +
//! CR 611.2a): "each token that would be created under an opponent's control
//! this turn is created under your control instead."
//!
//! The redirect affects no object, so the trigger's source is not a recipient
//! the instruction needs: blinking Cutpurse before its original trigger
//! resolves, or removing it after the redirect exists, must not drop the
//! redirect. "Your" is the trigger's controller (CR 109.5 + CR 603.3a).
//!
//! Every card is built from its verbatim Oracle text (MTGJSON) and driven
//! through real cast / `PassPriority` actions to an empty stack with priority.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const CRAFTY_CUTPURSE: &str = "Flash\nWhen this creature enters, each token that would be created under an opponent's control this turn is created under your control instead.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const MURDER: &str = "Destroy target creature.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";

struct Board {
    runner: GameRunner,
    cutpurse: ObjectId,
    cloudshift: ObjectId,
    stifle: ObjectId,
    alarm: ObjectId,
    murder: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cutpurse = scenario
        .add_creature_to_hand_from_oracle(P0, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    Board {
        runner: scenario.build(),
        cutpurse,
        cloudshift,
        stifle,
        alarm,
        murder,
    }
}

fn pass(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass priority");
}

/// The enters triggers of `source` on the stack, bottom to top.
fn enters_triggers(runner: &GameRunner, source: ObjectId) -> Vec<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .collect()
}

fn soldiers(runner: &GameRunner, controller: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.is_token && obj.name == "Soldier" && obj.controller == controller)
        .count()
}

fn settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(state.stack.is_empty(), "the stack is empty");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "priority boundary, got {:?}",
        state.waiting_for
    );
}

/// P0 casts Crafty Cutpurse; it resolves and its enters trigger waits on the
/// stack. Returns that trigger's stack id.
fn cast_cutpurse(b: &mut Board) -> ObjectId {
    b.runner.cast(b.cutpurse).commit();
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.cutpurse].zone,
        Zone::Battlefield
    );
    let triggers = enters_triggers(&b.runner, b.cutpurse);
    assert_eq!(triggers.len(), 1, "reach guard: the enters trigger waits");
    triggers[0]
}

/// P0 passes; P1 casts Raise the Alarm and it resolves.
fn opponent_raises_the_alarm(b: &mut Board) {
    pass(&mut b.runner);
    b.runner.cast(b.alarm).commit();
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.alarm].zone,
        Zone::Graveyard,
        "reach guard: Raise the Alarm resolved"
    );
    settled(&b.runner);
}

/// Control: Cutpurse never moves. Its trigger resolves, then the opponent's
/// two Soldiers are created under P0's control.
#[test]
fn unchanged_cutpurse_redirects_opponent_tokens() {
    let mut b = board();
    let original = cast_cutpurse(&mut b);
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert!(
        !b.runner.state().stack.iter().any(|e| e.id == original),
        "reach guard: the trigger resolved"
    );
    settled(&b.runner);
    opponent_raises_the_alarm(&mut b);
    assert_eq!(soldiers(&b.runner, P0), 2);
    assert_eq!(soldiers(&b.runner, P1), 0);
}

/// CR 113.7a: the original trigger waits; P0 Cloudshifts Cutpurse (a new
/// object, CR 400.7), then Stifles only the returned Cutpurse's new trigger
/// (CR 701.6a). The original trigger still resolves and creates the redirect.
#[test]
fn original_trigger_redirects_after_cutpurse_is_blinked_and_new_trigger_is_stifled() {
    let mut b = board();
    let original = cast_cutpurse(&mut b);
    let before = b.runner.state().objects[&b.cutpurse].incarnation;

    b.runner
        .cast(b.cloudshift)
        .target_object(b.cutpurse)
        .commit();
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.cloudshift].zone,
        Zone::Graveyard
    );
    assert_eq!(
        b.runner.state().objects[&b.cutpurse].zone,
        Zone::Battlefield
    );
    assert_ne!(
        b.runner.state().objects[&b.cutpurse].incarnation,
        before,
        "the returned Cutpurse is a new object"
    );
    let triggers = enters_triggers(&b.runner, b.cutpurse);
    assert_eq!(triggers.len(), 2, "reach guard: both enters triggers wait");
    assert_eq!(triggers[0], original);
    let new_trigger = triggers[1];

    b.runner.cast(b.stifle).target_object(new_trigger).commit();
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert_eq!(b.runner.state().objects[&b.stifle].zone, Zone::Graveyard);
    assert_eq!(
        enters_triggers(&b.runner, b.cutpurse),
        vec![original],
        "reach guard: only the new trigger was countered"
    );

    pass(&mut b.runner);
    pass(&mut b.runner);
    settled(&b.runner);

    opponent_raises_the_alarm(&mut b);
    assert_eq!(
        soldiers(&b.runner, P0),
        2,
        "both Soldiers are created under P0's control"
    );
    assert_eq!(soldiers(&b.runner, P1), 0);
}

/// CR 113.7a + CR 611.2a: the redirect exists, then Cutpurse is destroyed; the
/// redirect lasts for the turn regardless.
#[test]
fn redirect_outlives_cutpurse_leaving_the_battlefield() {
    let mut b = board();
    cast_cutpurse(&mut b);
    pass(&mut b.runner);
    pass(&mut b.runner);
    settled(&b.runner);

    pass(&mut b.runner);
    b.runner.cast(b.murder).target_object(b.cutpurse).commit();
    pass(&mut b.runner);
    pass(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.cutpurse].zone,
        Zone::Graveyard,
        "reach guard: Cutpurse was destroyed"
    );
    settled(&b.runner);

    opponent_raises_the_alarm(&mut b);
    assert_eq!(soldiers(&b.runner, P0), 2);
    assert_eq!(soldiers(&b.runner, P1), 0);
}

/// CR 109.5 + CR 603.3a: "your" is the triggered ability's controller, latched
/// at install; it does not follow Cutpurse's later controller. The redirect is
/// installed on P1's turn — P0 flashes Cutpurse in — so P1 can cast the sorcery
/// Act of Treason on Cutpurse within the same turn. Once P1 controls Cutpurse,
/// P1's Raise the Alarm tokens still go to P0, the trigger's controller.
#[test]
fn redirect_keeps_the_trigger_controller_after_cutpurse_changes_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cutpurse = scenario
        .add_creature_to_hand_from_oracle(P0, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let treason = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Both libraries must be non-empty so the turn's draw is not a loss.
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest"]);
    let mut runner = scenario.build();

    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "reach guard: P1's turn");

    // P1 passes in their main phase; P0 flashes Cutpurse in.
    pass(&mut runner);
    runner.cast(cutpurse).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(runner.state().objects[&cutpurse].zone, Zone::Battlefield);
    let triggers = enters_triggers(&runner, cutpurse);
    assert_eq!(triggers.len(), 1, "reach guard: the enters trigger waits");
    let original = triggers[0];
    pass(&mut runner);
    pass(&mut runner);
    assert!(
        !runner.state().stack.iter().any(|e| e.id == original),
        "reach guard: the trigger resolved under P0"
    );
    settled(&runner);

    runner.cast(treason).target_object(cutpurse).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(runner.state().objects[&treason].zone, Zone::Graveyard);
    settled(&runner);
    assert_eq!(
        runner.state().objects[&cutpurse].controller,
        P1,
        "reach guard: P1 now controls Cutpurse"
    );

    runner.cast(alarm).commit();
    pass(&mut runner);
    pass(&mut runner);
    assert_eq!(
        runner.state().objects[&alarm].zone,
        Zone::Graveyard,
        "reach guard: Raise the Alarm resolved"
    );
    settled(&runner);
    assert_eq!(
        soldiers(&runner, P0),
        2,
        "the trigger's controller, P0, receives both Soldiers"
    );
    assert_eq!(soldiers(&runner, P1), 0);
}

/// Passes priority until `done` holds, failing after a bounded number of passes.
fn pass_until(runner: &mut GameRunner, what: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..12 {
        if done(runner) {
            return;
        }
        pass(runner);
    }
    assert!(done(runner), "reach guard: {what}");
}

fn has_priority(runner: &GameRunner, player: PlayerId) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player)
}

/// Floating redirects installed for P0 (Crafty Cutpurse's resolved trigger).
fn p0_redirects(runner: &GameRunner) -> Vec<bool> {
    runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter(|def| def.token_owner_redirect.is_some() && def.source_controller == Some(P0))
        .map(|def| def.is_consumed)
        .collect()
}

/// Three players: on P1's turn P0 flashes Cutpurse in and its enters trigger
/// installs the redirect for P0; then `leaver` concedes. Returns the runner
/// at P1's priority with Raise the Alarm's id.
fn three_players_after_concession(leaver: PlayerId) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let cutpurse = scenario
        .add_creature_to_hand_from_oracle(P0, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Every library must be non-empty so the turn's draw is not a loss.
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Forest", "Forest"]);
    }
    let mut runner = scenario.build();

    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "reach guard: P1's turn");

    // P0 flashes Cutpurse in; it and its enters trigger resolve.
    pass_until(&mut runner, "P0 receives priority", |r| has_priority(r, P0));
    runner.cast(cutpurse).commit();
    pass_until(&mut runner, "Cutpurse and its trigger resolved", |r| {
        r.state().objects[&cutpurse].zone == Zone::Battlefield && r.state().stack.is_empty()
    });
    assert_eq!(
        p0_redirects(&runner),
        vec![false],
        "reach guard: the live redirect for P0 is installed before the concession"
    );
    let floating_before = runner.state().pending_damage_replacements.len();

    runner
        .act(GameAction::Concede { player_id: leaver })
        .expect("a player concedes");
    for player in [P0, P1, P2] {
        let eliminated = runner
            .state()
            .players
            .iter()
            .any(|p| p.id == player && p.is_eliminated);
        assert_eq!(
            eliminated,
            player == leaver,
            "reach guard: only {leaver:?} left the game (checked {player:?})"
        );
    }
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        floating_before,
        "the concession removes no floating definition, so floating ids stay stable"
    );

    pass_until(&mut runner, "P1 receives priority", |r| has_priority(r, P1));
    (runner, alarm)
}

/// P1 casts Raise the Alarm and it resolves to an empty stack with priority.
fn p1_raises_the_alarm(runner: &mut GameRunner, alarm: ObjectId) {
    runner.cast(alarm).commit();
    pass_until(runner, "Raise the Alarm resolved", |r| {
        r.state().objects[&alarm].zone == Zone::Graveyard
    });
    settled(runner);
}

/// CR 800.4a: "any effects which give that player control of any objects ...
/// end" when a player leaves the game. P0 concedes after its Cutpurse trigger
/// installed the redirect: the redirect giving P0 control of tokens ends, so
/// P1's Raise the Alarm Soldiers are created under P1's own control.
#[test]
fn redirect_ends_when_its_controller_leaves_the_game() {
    let (mut runner, alarm) = three_players_after_concession(P0);

    p1_raises_the_alarm(&mut runner, alarm);
    assert_eq!(
        soldiers(&runner, P1),
        2,
        "the departed P0's redirect ended, so P1 receives both Soldiers"
    );
    assert_eq!(soldiers(&runner, P0), 0);
    assert_eq!(
        p0_redirects(&runner),
        vec![true],
        "the departed P0's redirect was ended in place, keeping its floating slot"
    );
}

/// CR 800.4a ends only effects giving the LEAVING player control: when an
/// unrelated P2 concedes, the surviving P0's redirect still takes the Soldiers
/// P1 would create.
#[test]
fn redirect_survives_an_unrelated_player_leaving_the_game() {
    let (mut runner, alarm) = three_players_after_concession(P2);

    p1_raises_the_alarm(&mut runner, alarm);
    assert_eq!(
        soldiers(&runner, P0),
        2,
        "the surviving P0's redirect takes both Soldiers"
    );
    assert_eq!(soldiers(&runner, P1), 0);
    assert_eq!(
        p0_redirects(&runner),
        vec![false],
        "the surviving P0's redirect stays live"
    );
}
