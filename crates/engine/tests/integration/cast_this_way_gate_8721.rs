//! Regression for GitHub issue #8721, parser half — "if you cast a spell this
//! way, …" / "when you cast that spell, …" is a delayed triggered ability, not a
//! sequential instruction of the granting resolution.
//!
//! CR 603.7: a delayed triggered ability fires when its stated event occurs.
//! CR 601.2: casting a spell is a single action with its own cost and payment
//! steps; anything describing HOW the granted spell is cast belongs to the grant
//! and must be live before the cast, not fired after it.
//!
//! Both cards here grant a LINGERING cast permission (`Effect::CastFromZone`
//! with the default `CastFromZoneDriver::LingeringPermission`, which the corpus
//! parse dump shows as an absent `driver` key), so the granted spell is cast
//! later under priority — never inside the granting resolution.
//! Lowering the consequent as a sequential instruction therefore applied it the
//! moment the permission was granted, whether or not the player ever cast
//! anything.
//!
//! Helmut Zemo is tested in both directions. Ogre Battlecaster has only the
//! negative direction — its positive one is blocked by a separate open defect
//! (its "+X/+0 … where X is that spell's mana value" binds X against a spell not
//! yet cast). Discord, Lord of Disharmony is covered only by the parse dump, not
//! by a runtime test. Said here rather than left to be discovered: a negative
//! direction alone would also pass if the consequent were dropped entirely.
//!
//! Corpus reach: three cards change parse (Helmut Zemo, Ogre Battlecaster,
//! Discord, Lord of Disharmony). Discord's consequent is "copy this ability",
//! which the parser lowers to `CopySpell` — a pre-existing mis-lowering this
//! change neither introduces nor repairs. Discord also has no object target, so
//! the over-fire guard in `delayed_trigger::resolve` refuses to install its
//! trigger at all — its consequent is suppressed rather than re-scoped, which is
//! the fail-closed side of the same defect.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

/// Helmut Zemo, Mastermind. Consequent: a counter on the source.
const HELMUT_ZEMO: &str = "Whenever Helmut Zemo attacks, you may cast target instant or sorcery \
card with mana value less than or equal to his power from your graveyard. If that spell would be \
put into your graveyard, exile it instead. If you cast a spell this way, put a +1/+1 counter on \
Helmut Zemo.";

/// Ogre Battlecaster. Same class, the "when you cast that spell" wording.
const OGRE_BATTLECASTER: &str = "First strike\n\
Whenever this creature attacks, you may cast target instant or sorcery card from your graveyard \
by paying {R}{R} in addition to its other costs. If that spell would be put into a graveyard, \
exile it instead. When you cast that spell, this creature gets +X/+0 until end of turn, where X \
is that spell's mana value.";

fn to_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
}

/// Drive the attack trigger to completion, answering its "you may cast" prompt
/// with `accept`. Accepting GRANTS the permission; it does not cast anything —
/// that separation is the whole point of these tests.
fn settle_attack_trigger(runner: &mut GameRunner, accept: bool) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                if runner.choose_first_legal_target().is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    break;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                if runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
}

fn p1p1(runner: &GameRunner, id: engine::types::identifiers::ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 603.7 (issue #8721): granting the permission is not casting. "If you cast
/// a spell this way, put a +1/+1 counter on Helmut Zemo" must not pay out for a
/// permission the player never used.
#[test]
fn zemo_grants_the_permission_without_paying_out_the_counter() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let zemo = scenario
        .add_creature_from_oracle(P0, "Helmut Zemo, Mastermind", 2, 2, HELMUT_ZEMO)
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(zemo, AttackTarget::Player(P1))])
        .expect("Zemo must be a legal attacker");
    settle_attack_trigger(&mut runner, true);

    // REACH GUARD. Without this the test also passes when the trigger never
    // resolved at all — which is exactly how an earlier draft of it measured
    // nothing.
    assert_eq!(
        runner.state().objects[&bolt].casting_permissions.len(),
        1,
        "reach guard: the trigger must have granted its cast permission, or nothing \
         downstream of it was exercised"
    );
    assert_eq!(
        p1p1(&runner, zemo),
        0,
        "no spell was cast under the permission, so the gated counter must not be placed"
    );
}

/// CR 603.7 (issue #8721): the positive direction — the over-suppression guard.
///
/// COUNTER-PROBE: this test stays GREEN without the fix (before it, the counter
/// was placed at grant time, so it was also 1 by the end). It is therefore not a
/// proof of the fix; its job is the opposite one — to fail if the fix ever
/// suppresses the payout instead of deferring it. The discriminating test is the
/// negative one above, which goes red when the gate is removed.
#[test]
fn zemo_pays_out_the_counter_once_the_granted_spell_is_actually_cast() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let zemo = scenario
        .add_creature_from_oracle(P0, "Helmut Zemo, Mastermind", 2, 2, HELMUT_ZEMO)
        .id();
    // Zemo's grant does not waive the cost, so the cast needs real mana.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(zemo, AttackTarget::Player(P1))])
        .expect("Zemo must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    assert_eq!(
        runner.state().objects[&bolt].casting_permissions.len(),
        1,
        "reach guard: the permission must exist before the cast can use it"
    );

    runner
        .cast(bolt)
        .target_players(&[P1])
        .try_resolve()
        .expect("the granted graveyard cast must succeed");

    assert_eq!(
        p1p1(&runner, zemo),
        1,
        "casting the granted spell must place exactly the one printed counter"
    );
}

/// CR 603.7 (issue #8721): the "when you cast that spell" wording, same class.
///
/// COUNTER-PROBE, stated plainly: this test stays GREEN without the fix, so it
/// proves nothing on its own. The reason is a SECOND, pre-existing gap, MEASURED
/// rather than assumed: driving the granted cast for real leaves the creature at
/// its printed 3 power, so "+X/+0 … where X is that spell's mana value" resolves
/// X to 0 whether the pump is gated or not. What this test buys is the wording
/// coverage ("when you cast that spell" takes the same branch as "if you cast a
/// spell this way") plus the reach guard, and it will start discriminating once
/// that X binding is repaired. It is kept for that, not offered as evidence.
#[test]
fn ogre_battlecaster_does_not_pump_on_the_grant_alone() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let ogre = scenario
        .add_creature_from_oracle(P0, "Ogre Battlecaster", 3, 3, OGRE_BATTLECASTER)
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(ogre, AttackTarget::Player(P1))])
        .expect("Ogre must be a legal attacker");
    settle_attack_trigger(&mut runner, true);

    assert_eq!(
        runner.state().objects[&bolt].casting_permissions.len(),
        1,
        "reach guard: the trigger must have granted its cast permission"
    );
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&ogre].power,
        Some(3),
        "no spell was cast under the permission, so the gated pump must not apply"
    );
}
