//! CR 109.5 — "you" inside a replacement effect's rider is the REPLACING
//! object's controller, never the controller of the object the replaced event
//! happened to affect. Issue #7086.
//!
//! Head of the Hunt (verified against `client/public/card-data.json`'s
//! `"head of the hunt"` entry):
//!   "Flash
//!    If a creature an opponent controls would die, exile it instead. When you
//!    do, create a 2/2 green Wolf creature token."
//!
//! The class this pins is every THIRD-PARTY replacement — one whose `valid_card`
//! scope is some other player's permanent rather than `SelfRef` — that carries a
//! rider phrased in the first person. The post-replacement continuation used to
//! be resolved with `ability.controller` bound to the affected object's
//! controller, so the rider ran for the wrong player. Same shape, same defect:
//! Kalitas, Traitor of Ghet (Zombie token), Nemata, Primeval Warden (Saproling
//! token), Valentin, Dean of the Vein (pay {2}, Pest token), The Darkness
//! Crystal ("you gain 2 life"), Twists and Turns ("you scry 1").
//!
//! The companion assertions cover the other side of the split: "that player" /
//! "its controller" riders must STILL name the affected object's controller
//! (The Doctor's Tomb's "that creature's controller loses 2 life"), which is
//! what `scoped_player` now carries.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::PlayerId;

/// Verbatim Oracle text from the card-data export.
const HEAD_OF_THE_HUNT: &str = "Flash\nIf a creature an opponent controls would die, exile it instead. When you do, create a 2/2 green Wolf creature token.";

/// Kalitas prints the same class without the reflexive "When you do" wrapper —
/// the rider is a plain conjoined clause, so it exercises the mandatory-stash
/// path rather than the `WhenYouDo` gate.
const KALITAS: &str = "Lifelink\nIf a nontoken creature an opponent controls would die, instead exile that card and create a 2/2 black Zombie creature token.";

/// The other side of the split: the rider names *that creature's controller*,
/// not "you", so it must keep resolving to the affected object's controller.
const DOCTORS_TOMB: &str =
    "If a creature would die, instead exile it and that creature's controller loses 2 life.";

/// The non-token half of the same class: the rider is a bare first-person
/// "you gain 2 life", with no player reference in the AST at all — it reads
/// `ability.controller` directly.
const DARKNESS_CRYSTAL: &str =
    "If a nontoken creature an opponent controls would die, instead exile it and you gain 2 life.";

/// Controllers of every token on the battlefield whose subtype matches `subtype`.
fn token_controllers(runner: &GameRunner, subtype: &str) -> Vec<PlayerId> {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.is_token
                && obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype))
        })
        .map(|obj| obj.controller)
        .collect()
}

/// CR 109.5 + CR 614.6: Head of the Hunt's controller creates the Wolf.
///
/// RED before the fix: `wolves == [P1]` — the token was created under the
/// control of the opponent whose creature died (issue #7086).
#[test]
fn head_of_the_hunt_wolf_is_created_for_its_own_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Head of the Hunt", 4, 3, HEAD_OF_THE_HUNT);
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(removal).target_object(victim).resolve();

    // Reach-guard: the replacement really applied, so the token assertion below
    // is not vacuously satisfied by a run in which nothing was replaced.
    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Exile,
        "the would-die replacement must have applied (the creature is exiled, not \
         in the graveyard) — otherwise no rider ran and the controller assertion \
         below proves nothing"
    );
    assert_eq!(
        token_controllers(&runner, "Wolf"),
        vec![P0],
        "CR 109.5: \"When you do, create a … Wolf\" is text on Head of the Hunt, so \
         \"you\" is Head of the Hunt's controller (P0) — not the controller of the \
         creature that died (P1)"
    );
}

/// Same class, conjoined-clause rider instead of a `WhenYouDo` reflexive.
#[test]
fn kalitas_zombie_is_created_for_its_own_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS);
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(removal).target_object(victim).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Exile,
        "reach-guard: Kalitas's would-die replacement must have applied"
    );
    assert_eq!(
        token_controllers(&runner, "Zombie"),
        vec![P0],
        "CR 109.5: Kalitas's controller (P0) creates the Zombie, not the opponent \
         whose creature died (P1)"
    );
}

/// The controller of the replacement is symmetric: with the roles swapped the
/// token must follow the shield, not the board seat. Guards against a fix that
/// merely hard-codes the non-active or non-affected player.
#[test]
fn head_of_the_hunt_token_follows_the_shield_when_the_opponent_controls_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Head of the Hunt", 4, 3, HEAD_OF_THE_HUNT);
    // P0 is the active player and the caster; the dying creature is P0's own.
    let victim = scenario.add_creature(P0, "Own Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(removal).target_object(victim).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Exile,
        "reach-guard: the replacement must have applied to P0's dying creature"
    );
    assert_eq!(
        token_controllers(&runner, "Wolf"),
        vec![P1],
        "CR 109.5: the Wolf follows Head of the Hunt's controller (P1) even though \
         P0 is the active player, the caster, and the dying creature's controller"
    );
}

/// The other half of the split: a rider that names "that creature's controller"
/// must still resolve to the AFFECTED object's controller. If the CR 109.5 fix
/// had simply rebound every player reference to the shield's controller, this
/// would drain the wrong player's life.
#[test]
fn doctors_tomb_life_loss_still_hits_the_dying_creatures_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Doctor's Tomb", 2, 2, DOCTORS_TOMB);
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.cast(removal).target_object(victim).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Exile,
        "reach-guard: the would-die replacement must have applied"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 2,
        "CR 109.5: \"that creature's controller loses 2 life\" names the DYING \
         creature's controller (P1)"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before,
        "the replacement's own controller (P0) must not lose life — the CR 109.5 \
         split moves \"you\" to the shield without moving \"that player\" with it"
    );
}

/// The class is not token-shaped: any bare first-person rider on a third-party
/// replacement had the same defect. The Darkness Crystal's "you gain 2 life"
/// carries no player reference at all, so it reads `ability.controller`
/// directly — the shortest possible path to the bug.
///
/// RED before the fix: P1 (the opponent whose creature died) gained the life.
#[test]
fn darkness_crystal_life_gain_goes_to_its_own_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Darkness Crystal", 2, 2, DARKNESS_CRYSTAL);
    let victim = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.cast(removal).target_object(victim).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Exile,
        "reach-guard: the would-die replacement must have applied"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before + 2,
        "CR 109.5: \"you gain 2 life\" is text on The Darkness Crystal, so its \
         controller (P0) gains the life"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before,
        "the opponent whose creature died (P1) must not gain life"
    );
}

/// The commonest way a creature dies is lethal combat damage, which reaches the
/// replacement through the state-based-action check rather than through a
/// resolving removal spell. Same rider, same controller — this pins that the
/// CR 109.5 binding is a property of the replacement, not of the delivery path.
#[test]
fn head_of_the_hunt_wolf_controller_is_the_same_on_the_combat_damage_path() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Head of the Hunt", 4, 3, HEAD_OF_THE_HUNT);
    // P0 attacks; P1's blocker takes lethal damage and would die.
    let attacker = scenario.add_creature(P0, "Attacking Bear", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Blocking Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the attacker must be able to attack");
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("the blocker must be able to block");
    runner.combat_damage();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&blocker].zone,
        engine::types::zones::Zone::Exile,
        "reach-guard: the lethally damaged opposing creature must be exiled by the \
         would-die replacement, not put into the graveyard"
    );
    assert_eq!(
        token_controllers(&runner, "Wolf"),
        vec![P0],
        "CR 109.5: the Wolf follows Head of the Hunt's controller on the \
         state-based-action death path too"
    );
}
