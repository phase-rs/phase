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
//! The redirect modifies creation events rather than giving anyone control of
//! an object (CR 611.2c + CR 614.1a), so its controller leaving the game does
//! not end it (CR 800.4a). A token it would create under that departed player
//! is not created at all (CR 800.4b) — for ordinary tokens, token copies, and a
//! creation resumed from a replacement choice alike.
//!
//! Every card is built from its verbatim Oracle text (MTGJSON) and driven
//! through real cast / `PassPriority` actions to an empty stack with priority.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
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

const LEYLINE_INVOCATION: &str = "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of lands you control.";
const CACKLING_COUNTERPART: &str = "Create a token that's a copy of target creature you control.\nFlashback {5}{U}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const EYES_IN_THE_SKIES: &str = "Create a 1/1 white Bird creature token with flying, then populate. (Create a token that's a copy of a creature token you control.)";

/// What else is on the three-player board besides P0's Cutpurse and P1's spells.
#[derive(Clone, Copy, Default)]
struct Extras {
    /// P1 answers P0's Cutpurse trigger with a first Raise the Alarm, which
    /// resolves before the redirect exists: P1 controls two Soldiers, and the
    /// created-token referent names them.
    prior_soldiers: bool,
    /// P2 also flashes in a Crafty Cutpurse, so P2's own redirect exists too.
    p2_cutpurse: bool,
}

struct ConcessionBoard {
    runner: GameRunner,
    p2_cutpurse: ObjectId,
    alarm: ObjectId,
    leyline: ObjectId,
    counterpart: ObjectId,
    eyes: ObjectId,
    bear: ObjectId,
    prior_soldiers: Vec<ObjectId>,
}

/// Floating redirects installed for `player`, as their `is_consumed` flags.
fn redirects_for(runner: &GameRunner, player: PlayerId) -> Vec<bool> {
    runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter(|def| def.token_owner_redirect.is_some() && def.source_controller == Some(player))
        .map(|def| def.is_consumed)
        .collect()
}

/// Three players: on P1's turn P0 flashes Cutpurse in and its enters trigger
/// installs the redirect for P0; then `leaver` (if any) concedes. Returns the
/// board at P1's main-phase priority with an empty stack.
fn concession_board(leaver: Option<PlayerId>, extras: Extras) -> ConcessionBoard {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let cutpurse = scenario
        .add_creature_to_hand_from_oracle(P0, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p2_cutpurse = scenario
        .add_creature_to_hand_from_oracle(P2, "Crafty Cutpurse", 2, 2, CRAFTY_CUTPURSE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let prior_alarm = scenario
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    let leyline = scenario
        .add_spell_to_hand_from_oracle(P1, "Leyline Invocation", false, LEYLINE_INVOCATION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let counterpart = scenario
        .add_spell_to_hand_from_oracle(P1, "Cackling Counterpart", true, CACKLING_COUNTERPART)
        .with_mana_cost(ManaCost::zero())
        .id();
    let eyes = scenario
        .add_spell_to_hand_from_oracle(P1, "Eyes in the Skies", true, EYES_IN_THE_SKIES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let bear = scenario.add_vanilla(P1, 2, 2);
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    // Every library must be non-empty so the turn's draw is not a loss.
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Forest", "Forest"]);
    }
    let mut runner = scenario.build();

    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "reach guard: P1's turn");

    // P0 flashes Cutpurse in; it resolves and its enters trigger waits.
    pass_until(&mut runner, "P0 receives priority", |r| has_priority(r, P0));
    runner.cast(cutpurse).commit();
    pass_until(&mut runner, "Cutpurse's enters trigger waits", |r| {
        r.state().objects[&cutpurse].zone == Zone::Battlefield
            && enters_triggers(r, cutpurse).len() == 1
            && has_priority(r, P1)
    });
    let mut prior_soldiers = Vec::new();
    if extras.prior_soldiers {
        runner.cast(prior_alarm).commit();
        pass_until(&mut runner, "the first Raise the Alarm resolved", |r| {
            r.state().objects[&prior_alarm].zone == Zone::Graveyard
        });
        prior_soldiers = runner.state().last_created_token_ids.clone();
        assert_eq!(
            prior_soldiers.len(),
            2,
            "reach guard: the referent names both"
        );
        assert_eq!(soldiers(&runner, P1), 2, "reach guard: P1's own Soldiers");
    }
    pass_until(&mut runner, "Cutpurse's trigger resolved", |r| {
        r.state().stack.is_empty() && has_priority(r, P1)
    });
    assert_eq!(
        redirects_for(&runner, P0),
        vec![false],
        "reach guard: the live redirect for P0 is installed before the concession"
    );
    if extras.p2_cutpurse {
        pass_until(&mut runner, "P2 receives priority", |r| has_priority(r, P2));
        runner.cast(p2_cutpurse).commit();
        pass_until(&mut runner, "P2's Cutpurse and its trigger resolved", |r| {
            r.state().objects[&p2_cutpurse].zone == Zone::Battlefield
                && r.state().stack.is_empty()
                && has_priority(r, P1)
        });
        assert_eq!(
            redirects_for(&runner, P2),
            vec![false],
            "reach guard: P2's redirect"
        );
    }
    let floating_before = runner.state().pending_damage_replacements.len();

    if let Some(leaver) = leaver {
        runner
            .act(GameAction::Concede { player_id: leaver })
            .expect("a player concedes");
    }
    for player in [P0, P1, P2] {
        let eliminated = runner
            .state()
            .players
            .iter()
            .any(|p| p.id == player && p.is_eliminated);
        assert_eq!(
            eliminated,
            Some(player) == leaver,
            "reach guard: only {leaver:?} left the game (checked {player:?})"
        );
    }
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        floating_before,
        "the concession removes no floating definition"
    );
    assert_eq!(
        redirects_for(&runner, P0),
        vec![false],
        "CR 611.2c: the concession does not end the turn-long redirect"
    );

    pass_until(&mut runner, "P1 receives priority", |r| has_priority(r, P1));
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "reach guard: still P1's main phase"
    );
    ConcessionBoard {
        runner,
        p2_cutpurse,
        alarm,
        leyline,
        counterpart,
        eyes,
        bear,
        prior_soldiers,
    }
}

/// What resolving one of P1's spells prompted along the way.
#[derive(Debug, Default)]
struct Prompts {
    replacement_choices: usize,
    populate_choices: usize,
}

/// Drives `spell`'s resolution to an empty stack with priority. Replacement
/// choices take candidate `pick`; a populate choice takes its first token.
fn resolve_to_settle(runner: &mut GameRunner, spell: ObjectId, pick: usize) -> Prompts {
    let mut prompts = Prompts::default();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                assert_eq!(
                    runner.state().objects[&spell].zone,
                    Zone::Graveyard,
                    "reach guard: the spell resolved"
                );
                settled(runner);
                assert!(
                    runner.state().active_ability_continuation().is_none(),
                    "no stranded continuation"
                );
                return prompts;
            }
            WaitingFor::Priority { .. } => pass(runner),
            WaitingFor::ReplacementChoice { candidates, .. } => {
                prompts.replacement_choices += 1;
                runner
                    .act(GameAction::ChooseReplacement {
                        index: pick.min(candidates.len() - 1),
                    })
                    .expect("replacement choice accepted");
            }
            WaitingFor::PopulateChoice { valid_tokens, .. } => {
                prompts.populate_choices += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(valid_tokens[0])),
                    })
                    .expect("populate choice accepted");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the spell did not settle");
}

/// Every token object named `name`, in any zone, under any player.
fn tokens_named(runner: &GameRunner, name: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.name == name)
        .count()
}

/// Every token object controlled by `player`, in any zone.
fn tokens_controlled_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.controller == player)
        .count()
}

/// CR 800.4b: "If a token would be created under the control of a player who
/// has left the game, no token is created." P0's redirect outlives P0's
/// concession (it modifies creation events, CR 611.2c + CR 614.1a), so P1's
/// Soldiers would be created under the departed P0: none is created for anyone.
#[test]
fn departed_redirect_controller_receives_no_tokens_and_nobody_else_does() {
    let mut b = concession_board(Some(P0), Extras::default());

    b.runner.cast(b.alarm).commit();
    let prompts = resolve_to_settle(&mut b.runner, b.alarm, 0);
    assert_eq!(prompts.replacement_choices, 0);
    assert_eq!(soldiers(&b.runner, P0), 0, "no token under the departed P0");
    assert_eq!(
        soldiers(&b.runner, P1),
        0,
        "the redirect still replaced P1's"
    );
    assert_eq!(soldiers(&b.runner, P2), 0);
    assert_eq!(
        tokens_named(&b.runner, "Soldier"),
        0,
        "no Soldier exists anywhere"
    );
    assert!(
        b.runner.state().last_created_token_ids.is_empty(),
        "the created referent is empty"
    );
    assert_eq!(
        redirects_for(&b.runner, P0),
        vec![false],
        "the redirect lasts the turn"
    );
}

/// Positive control: an unrelated P2 concedes, so the surviving P0's redirect
/// takes the Soldiers P1 would create.
#[test]
fn redirect_survives_an_unrelated_player_leaving_the_game() {
    let mut b = concession_board(Some(P2), Extras::default());

    b.runner.cast(b.alarm).commit();
    resolve_to_settle(&mut b.runner, b.alarm, 0);
    assert_eq!(
        soldiers(&b.runner, P0),
        2,
        "the surviving P0 takes both Soldiers"
    );
    assert_eq!(soldiers(&b.runner, P1), 0);
    assert_eq!(
        redirects_for(&b.runner, P0),
        vec![false],
        "the redirect stays live"
    );
}

/// CR 800.4b + CR 707.2: a token copy is created like any other token, so a
/// copy redirected to the departed P0 is not created either. P1's Bear remains
/// the only creature P1 controls besides the board's lands.
#[test]
fn departed_redirect_controller_receives_no_token_copy() {
    let mut b = concession_board(Some(P0), Extras::default());

    b.runner.cast(b.counterpart).target_object(b.bear).commit();
    resolve_to_settle(&mut b.runner, b.counterpart, 0);
    assert_eq!(
        tokens_controlled_by(&b.runner, P0),
        0,
        "no copy under the departed P0"
    );
    assert_eq!(
        tokens_controlled_by(&b.runner, P1),
        0,
        "the redirect still replaced P1's"
    );
    assert_eq!(
        b.runner.state().objects[&b.bear].zone,
        Zone::Battlefield,
        "reach guard: the copied Bear is still a legal source"
    );
    assert!(b.runner.state().last_created_token_ids.is_empty());
}

/// Positive control for the copy path: with an unrelated P2 gone, P1's copy of
/// its Bear is created under the surviving P0's control.
#[test]
fn redirect_takes_a_token_copy_after_an_unrelated_player_leaves() {
    let mut b = concession_board(Some(P2), Extras::default());

    b.runner.cast(b.counterpart).target_object(b.bear).commit();
    resolve_to_settle(&mut b.runner, b.counterpart, 0);
    assert_eq!(
        tokens_controlled_by(&b.runner, P0),
        1,
        "P0 receives the copy"
    );
    assert_eq!(tokens_controlled_by(&b.runner, P1), 0);
}

/// CR 800.4b + CR 608.2c: Leyline Invocation's "Put X +1/+1 counters on it"
/// reads the token its first sentence created. None was created, so "it" names
/// nothing — in particular not P1's Soldiers from the earlier Raise the Alarm,
/// which the created referent named until this spell resolved.
#[test]
fn departed_redirect_leaves_the_created_token_referent_empty() {
    let mut b = concession_board(
        Some(P0),
        Extras {
            prior_soldiers: true,
            ..Extras::default()
        },
    );

    b.runner.cast(b.leyline).commit();
    resolve_to_settle(&mut b.runner, b.leyline, 0);
    assert_eq!(
        tokens_named(&b.runner, "Fractal"),
        0,
        "no Fractal exists anywhere"
    );
    for soldier in &b.prior_soldiers {
        let obj = &b.runner.state().objects[soldier];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "reach guard: P1's Soldier remains"
        );
        assert_eq!(obj.controller, P1);
        assert!(
            obj.counters.is_empty(),
            "no counter lands on an earlier batch's Soldier: {:?}",
            obj.counters
        );
    }
    assert!(b.runner.state().last_created_token_ids.is_empty());
}

/// CR 800.4b + CR 701.36a: Eyes in the Skies creates a Bird, then populates
/// (copies a creature token P1 controls). Both creations are redirected to the
/// departed P0, so neither token is created; P1 still makes the populate choice.
#[test]
fn departed_redirect_suppresses_the_created_and_the_populated_token() {
    let mut b = concession_board(
        Some(P0),
        Extras {
            prior_soldiers: true,
            ..Extras::default()
        },
    );

    b.runner.cast(b.eyes).commit();
    let prompts = resolve_to_settle(&mut b.runner, b.eyes, 0);
    assert_eq!(
        prompts.populate_choices, 1,
        "reach guard: P1 chose a token to populate"
    );
    assert_eq!(
        tokens_named(&b.runner, "Bird"),
        0,
        "no Bird exists anywhere"
    );
    assert_eq!(
        tokens_named(&b.runner, "Soldier"),
        2,
        "only P1's earlier Soldiers"
    );
    assert_eq!(soldiers(&b.runner, P1), 2);
    assert_eq!(tokens_controlled_by(&b.runner, P0), 0);
}

/// CR 616.1b + CR 614.5 + CR 800.4b: P0's and P2's redirects both modify
/// under whose control P1's Soldiers would be created, so P1 chooses which
/// applies first and the creation resumes from that choice. Each applies once:
/// P2's first sends the Soldiers to P2, then P0's sends them on to the departed
/// P0, so none is created. P0's first sends them to the departed P0, who is no
/// longer a player and so no one's opponent (CR 102.1); P2's redirect is then
/// no longer applicable (CR 616.1f), the event settles under P0, and none is
/// created either. Either order: no Soldier for anyone.
#[test]
fn departed_redirect_suppresses_a_creation_resumed_from_a_replacement_choice() {
    for p2_first in [true, false] {
        let mut b = concession_board(
            Some(P0),
            Extras {
                p2_cutpurse: true,
                ..Extras::default()
            },
        );
        b.runner.cast(b.alarm).commit();
        pass_until(&mut b.runner, "P1 orders the two redirects", |r| {
            matches!(r.state().waiting_for, WaitingFor::ReplacementChoice { .. })
        });
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = b.runner.state().waiting_for.clone()
        else {
            unreachable!("guarded above");
        };
        assert_eq!(player, P1, "CR 616.1: the affected player chooses");
        assert_eq!(candidates.len(), 2, "reach guard: both redirects compete");
        let index = candidates
            .iter()
            .position(|c| (c.source_id == b.p2_cutpurse) == p2_first)
            .expect("the chosen redirect is a candidate");
        b.runner
            .act(GameAction::ChooseReplacement { index })
            .expect("replacement choice accepted");
        let prompts = resolve_to_settle(&mut b.runner, b.alarm, 0);
        assert_eq!(
            prompts.replacement_choices, 0,
            "one choice settles the event"
        );

        assert_eq!(soldiers(&b.runner, P2), 0, "P2 first: {p2_first}");
        assert_eq!(
            soldiers(&b.runner, P0),
            0,
            "nothing under the departed P0 (P2 first: {p2_first})"
        );
        assert_eq!(soldiers(&b.runner, P1), 0, "P2 first: {p2_first}");
        assert_eq!(
            tokens_named(&b.runner, "Soldier"),
            0,
            "no Soldier exists anywhere (P2 first: {p2_first})"
        );
        assert!(
            b.runner.state().last_created_token_ids.is_empty(),
            "the created referent is empty (P2 first: {p2_first})"
        );
    }
}

/// CR 616.1b + CR 707.2 + CR 800.4b: the token copy Cackling Counterpart would
/// create for P1 is under both P0's and P2's redirects, so P1 chooses which
/// applies first and the copy creation resumes from that choice. Either order
/// settles the copy under the departed P0 (as for the Soldiers above), so no
/// copy is created and no copy-token resolution is left parked.
#[test]
fn departed_redirect_suppresses_a_token_copy_resumed_from_a_replacement_choice() {
    for p2_first in [true, false] {
        let mut b = concession_board(
            Some(P0),
            Extras {
                p2_cutpurse: true,
                ..Extras::default()
            },
        );
        b.runner.cast(b.counterpart).target_object(b.bear).commit();
        pass_until(&mut b.runner, "P1 orders the two redirects", |r| {
            matches!(r.state().waiting_for, WaitingFor::ReplacementChoice { .. })
        });
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = b.runner.state().waiting_for.clone()
        else {
            unreachable!("guarded above");
        };
        assert_eq!(player, P1, "CR 616.1: the affected player chooses");
        assert_eq!(candidates.len(), 2, "reach guard: both redirects compete");
        let index = candidates
            .iter()
            .position(|c| (c.source_id == b.p2_cutpurse) == p2_first)
            .expect("the chosen redirect is a candidate");
        b.runner
            .act(GameAction::ChooseReplacement { index })
            .expect("replacement choice accepted");
        let prompts = resolve_to_settle(&mut b.runner, b.counterpart, 0);
        assert_eq!(
            prompts.replacement_choices, 0,
            "one choice settles the event (P2 first: {p2_first})"
        );

        for player in [P0, P1, P2] {
            assert_eq!(
                tokens_controlled_by(&b.runner, player),
                0,
                "no copy under {player:?} (P2 first: {p2_first})"
            );
        }
        assert_eq!(
            b.runner
                .state()
                .objects
                .values()
                .filter(|obj| obj.is_token)
                .count(),
            0,
            "no token exists anywhere (P2 first: {p2_first})"
        );
        assert!(
            b.runner.state().last_created_token_ids.is_empty(),
            "the created referent is empty (P2 first: {p2_first})"
        );
        assert!(
            b.runner.state().active_copy_token().is_none(),
            "no copy-token resolution is left active (P2 first: {p2_first})"
        );
        assert!(
            b.runner.state().resolution_stack.is_empty(),
            "no resolution frame is stranded (P2 first: {p2_first})"
        );
        assert_eq!(
            b.runner.state().objects[&b.bear].zone,
            Zone::Battlefield,
            "reach guard: the copied Bear remains (P2 first: {p2_first})"
        );
        assert!(
            has_priority(&b.runner, P1),
            "P1 holds priority at the settled empty stack (P2 first: {p2_first})"
        );
    }
}
