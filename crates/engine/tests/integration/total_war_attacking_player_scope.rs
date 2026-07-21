//! Total War (Ice Age) — "Whenever a player attacks with one or more creatures,
//! destroy all untapped non-Wall creatures that player controls that didn't
//! attack, except for creatures the player hasn't controlled continuously since
//! the beginning of the turn."
//!
//! Misparse-backlog category #9 ("Wrong player/controller scope"): the "that
//! player controls" clause in the effect body must bind to the ATTACKING player
//! (the triggering-event player), not to Total War's own controller. Before the
//! fix, `relative_player_scope_for_condition` had no arm for the
//! "a player attacks with" head-noun trigger, so the DestroyAll filter defaulted
//! to `You` — Total War would destroy its OWN controller's creatures.
//!
//! The fix adds `condition_introduces_attacking_player` →
//! `ControllerRef::TriggeringPlayer`; at runtime `extract_player_from_event`'s
//! `AttackersDeclared` arm resolves that to the attacking player
//! (CR 508.1 + CR 603.2c).
//!
//! Fixture is designed so the intentionally-out-of-scope continuity exemption
//! ("except for creatures the player hasn't controlled continuously ...", a
//! separately-tracked dropped-clause gap) never changes the expected result:
//! every creature is placed pre-existing (entered a prior turn, via
//! `add_creature`), so it would be exempt under NO reading of the clause.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (data/card-data.json).
const TOTAL_WAR: &str =
    "Whenever a player attacks with one or more creatures, destroy all untapped \
non-Wall creatures that player controls that didn't attack, except for creatures the player hasn't \
controlled continuously since the beginning of the turn.";

/// Move the turn to `attacker` and advance to the declare-attackers step,
/// mirroring `attack_qualifier_stack_conditions::hand_turn_to`.
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };
    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

fn order_triggers_if_needed(runner: &mut GameRunner) {
    while let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering the Total War trigger should succeed");
    }
}

/// True iff a battlefield object with this exact name exists (id-independent —
/// destruction moves the card to the graveyard, so a destroyed creature's name
/// no longer appears on the battlefield).
fn on_battlefield_named(runner: &GameRunner, name: &str) -> bool {
    runner
        .state()
        .objects
        .values()
        .any(|o| o.zone == Zone::Battlefield && o.name == name)
}

/// Discriminating regression: Total War's DestroyAll must scope to the ATTACKING
/// player (P1), destroying only P1's untapped non-Wall non-attacking creatures.
///
/// Revert-failing assertions: reverting the `condition_introduces_attacking_player`
/// arm makes the filter default to `You` (P0), so Total War would destroy P0's
/// creature and spare P1's — flipping BOTH the "P1 Idler destroyed" and the
/// "P0 Bystander survives" assertions.
#[test]
fn total_war_destroys_attacking_players_idle_creatures_only() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Total War (enchantment) and a bystander creature.
    {
        let mut b = scenario.add_creature(P0, "Total War", 0, 1);
        b.as_enchantment().from_oracle_text(TOTAL_WAR);
    }
    scenario.add_creature(P0, "P0 Bystander", 2, 2);

    // P1 (the attacker) controls four creatures, all pre-existing since a prior
    // turn (sidesteps the continuity exemption gap):
    //   - the attacker itself (attacks → excluded by "didn't attack" + becomes tapped),
    //   - an idle untapped non-Wall creature (THE destroyed one),
    //   - an idle Wall creature (excluded by "non-Wall"),
    //   - an idle tapped creature (excluded by "untapped").
    let p1_attacker = scenario.add_creature(P1, "P1 Attacker", 2, 2).id();
    scenario.add_creature(P1, "P1 Idler", 2, 2);
    {
        let mut w = scenario.add_creature(P1, "P1 Wall", 0, 4);
        w.with_subtypes(vec!["Wall"]);
    }
    let p1_tapped = scenario.add_creature(P1, "P1 Tapped", 2, 2).id();

    // P2 controls an idle creature — a non-attacking, non-controller third party.
    scenario.add_creature(P2, "P2 Bystander", 2, 2);

    let mut runner = scenario.build();
    // Tap P1's "tapped" creature so the untapped filter excludes it.
    runner
        .state_mut()
        .objects
        .get_mut(&p1_tapped)
        .unwrap()
        .tapped = true;

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[(p1_attacker, AttackTarget::Player(P0))])
        .expect("P1's attacker should be a legal attack declaration");
    order_triggers_if_needed(&mut runner);
    runner.advance_until_stack_empty();

    // The attacking player's idle untapped non-Wall creature is destroyed.
    assert!(
        !on_battlefield_named(&runner, "P1 Idler"),
        "P1's untapped non-Wall non-attacking creature must be destroyed by Total War"
    );

    // Everything scoped OUT survives:
    assert!(
        on_battlefield_named(&runner, "P1 Attacker"),
        "the attacker itself attacked (and is tapped) — excluded from destruction"
    );
    assert!(
        on_battlefield_named(&runner, "P1 Wall"),
        "P1's Wall is excluded by the non-Wall filter"
    );
    assert!(
        on_battlefield_named(&runner, "P1 Tapped"),
        "P1's tapped creature is excluded by the untapped filter"
    );
    // The controller-scope discriminator: Total War's OWN controller (P0) and an
    // unrelated third party (P2) keep their idle creatures — proving the filter
    // binds to the attacking player, not to `You`.
    assert!(
        on_battlefield_named(&runner, "P0 Bystander"),
        "Total War's controller (P0) must NOT lose creatures — the filter scopes to \
         the attacking player (P1), not the ability controller"
    );
    assert!(
        on_battlefield_named(&runner, "P2 Bystander"),
        "a non-attacking third party (P2) must keep its idle creature"
    );
}

/// Discriminating regression for the continuity exemption (#5641 follow-up):
/// "..., except for creatures the player hasn't controlled continuously since
/// the beginning of the turn." Total War must SPARE an idle creature the
/// attacking player gained this turn (summoning-sick → not controlled
/// continuously, CR 508.1a) while still destroying one it has controlled since
/// the turn began.
///
/// Revert-failing: without the `parse_except_continuity_exemption_suffix` arm
/// the DestroyAll filter carries no `ControlledContinuouslySinceTurnBegan`
/// restriction, so the freshly-gained creature is destroyed too — flipping the
/// "Fresh Idler survives" assertion.
#[test]
fn total_war_spares_creatures_not_controlled_since_turn_began() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    {
        let mut b = scenario.add_creature(P0, "Total War", 0, 1);
        b.as_enchantment().from_oracle_text(TOTAL_WAR);
    }

    // P1 (attacker): an attacker, an idle creature controlled since the turn
    // began (destroyed), and an idle creature gained THIS turn (spared).
    let p1_attacker = scenario.add_creature(P1, "P1 Attacker", 2, 2).id();
    scenario.add_creature(P1, "P1 Established Idler", 2, 2);
    let p1_fresh = scenario.add_creature(P1, "P1 Fresh Idler", 2, 2).id();

    let mut runner = scenario.build();

    hand_turn_to(&mut runner, P1);
    // Mark the fresh creature summoning-sick AFTER the turn advance (so the untap
    // step can't clear it): at Total War's resolution it reads as not controlled
    // continuously since the turn began.
    runner
        .state_mut()
        .objects
        .get_mut(&p1_fresh)
        .unwrap()
        .summoning_sick = true;

    runner
        .declare_attackers(&[(p1_attacker, AttackTarget::Player(P0))])
        .expect("P1's attacker should be a legal attack declaration");
    order_triggers_if_needed(&mut runner);
    runner.advance_until_stack_empty();

    // Controlled since the turn began → destroyed.
    assert!(
        !on_battlefield_named(&runner, "P1 Established Idler"),
        "an idle creature the attacking player controlled since the turn began must be destroyed"
    );
    // THE CONTINUITY EXEMPTION: gained this turn → spared.
    assert!(
        on_battlefield_named(&runner, "P1 Fresh Idler"),
        "a creature the attacking player hasn't controlled continuously since the beginning of \
         the turn must be exempt from Total War's destruction"
    );
    // The attacker itself attacked (tapped) — excluded regardless.
    assert!(
        on_battlefield_named(&runner, "P1 Attacker"),
        "the attacker attacked and is excluded by the didn't-attack filter"
    );
}
