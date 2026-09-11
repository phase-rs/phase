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
//! Both wordings are tested in both directions. Helmut Zemo carries the first;
//! the second is carried by a labelled stand-in, because Ogre Battlecaster — the
//! only printed card that reaches this recognizer with that wording — cannot
//! discriminate: its "+X/+0 … where X is that spell's mana value" binds X against
//! a spell not yet cast and resolves to 0 either way (a separate open defect).
//! Said here rather than left to be discovered: one direction alone would also
//! pass if the consequent were dropped entirely.
//!
//! Corpus reach: two cards change parse — Helmut Zemo and Ogre Battlecaster.
//!
//! Discord, Lord of Disharmony USED to be a third. Review of PR #8749 found that
//! wrapping its consequent turned a wrong consequent into no consequent at all:
//! its permission ("you may cast a COPY of a spell with that name") declares no
//! object referent, so `valid_card: ParentTarget` binds to nothing and the
//! over-fire guard in `delayed_trigger::resolve` refuses to install the trigger.
//! The recognizer now declines a chain that declared no object referent, and
//! Discord keeps its `main` lowering — still mis-lowered to `CopySpell`, which is
//! pre-existing and untouched here. Pinned by
//! `oracle_effect::tests::a_targetless_cast_permission_keeps_its_consequent_unwrapped`.

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

/// CR 603.7 (issue #8721): the positive direction, and the DISCRIMINATING half of
/// this pair.
///
/// CORRECTED after review — the first version of this comment said the opposite
/// on all three counts, and every correction is a measurement:
///
/// - It does NOT stay green without the fix. Both probes turn it red: disabling
///   the engine's rider-tail handling, and disabling `strip_cast_this_way_gate`.
///   Both fail on the POST-cast assertion (`left: 0, right: 1`).
/// - On `main` the counter was not placed "too early" — it was never placed at
///   all. MEASURED in the baseline corpus bake: Zemo's `PutCounter` hangs as a
///   `SequentialSibling` UNDER the rider, which is exactly the position the
///   pre-#8721 branch discarded.
/// - The negative test above is NOT the discriminating one. Zero counters is the
///   outcome on `main`, with the gate disabled, and as shipped, so it is green in
///   every configuration PROBED HERE — not in every configuration, since it is
///   exactly what goes red if the family allowlist ever grows a counter-placing
///   family while this gate is absent. That early-fire guard is its job, and the
///   pre-cast assertion inside THIS test does the same work.
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
    // BOTH SIDES IN ONE RUN. The permission now exists and has NOT been used.
    // Asserting zero here is what makes the post-cast assertion below mean
    // "because of the cast" rather than "at some point during this test" — a
    // consequent that fired at grant time would already have placed the counter.
    assert_eq!(
        p1p1(&runner, zemo),
        0,
        "the permission is granted but unused, so the gated counter must not be placed yet"
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

/// CR 603.7, review of PR #8749: the "when you cast that spell" wording, proven
/// with an OBSERVABLE consequent in both directions.
///
/// A STAND-IN, and said plainly: this creature is a synthetic composite, not a
/// printed card. It exists because the corpus cannot supply the proof —
/// MEASURED, seven cards print "when you cast that spell," and only ONE of them
/// reaches this recognizer. The rest carry it inside parenthetical reminder text
/// (Crackling Spellslinger, Dark Apostle, "Elda, Conjurer of Spectacle",
/// "TARDIS Bay", The Twelfth Doctor) or lower through `parse_static_ability`,
/// which never reaches `parse_effect_chain_ir` ("Yume, Chronicler of Valor",
/// whose printing is NOT reminder text). The one that does reach it is Ogre
/// Battlecaster, whose
/// consequent is "+X/+0 … where X is that spell's mana value" and resolves X to 0
/// either way (a separate, pre-existing defect). So no printed card can
/// distinguish this wording at runtime today.
///
/// Its permission and rider are Helmut Zemo's shape verbatim; only the gate
/// wording is swapped to Ogre's, and the consequent is a +1/+1 counter, which is
/// observable. That isolates the wording as the single difference from
/// `zemo_pays_out_the_counter_once_the_granted_spell_is_actually_cast`.
///
/// Both directions in one run: zero after the grant, one after the cast. A
/// dropped consequent fails the second assertion; a consequent that fires at
/// grant time fails the first.
///
/// Counter-probe, MEASURED rather than predicted — and the obvious guess is
/// wrong: disabling `strip_cast_this_way_gate` turns the SECOND assertion red
/// (`left: 0, right: 1`), not the first. Without the gate the consequent lowers
/// to a bare `PutCounter` tail, and the rider-tail allowlist
/// (`tail_family_has_runtime_evidence`) does not admit that family — so the
/// counter is never placed at all rather than placed too early. The first
/// assertion still earns its place: it is the guard that catches an EARLY fire,
/// which is exactly what would happen if that allowlist ever grew a
/// counter-placing family while this gate was absent.
#[test]
fn the_when_you_cast_that_spell_wording_pays_out_only_after_the_cast() {
    // Synthetic: Zemo's permission and rider with Ogre's gate wording.
    const WHEN_YOU_CAST_STAND_IN: &str =
        "Whenever Gate Wording Stand-In attacks, you may cast target instant or sorcery card \
from your graveyard. If that spell would be put into your graveyard, exile it instead. When \
you cast that spell, put a +1/+1 counter on Gate Wording Stand-In.";

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let standin = scenario
        .add_creature_from_oracle(P0, "Gate Wording Stand-In", 2, 2, WHEN_YOU_CAST_STAND_IN)
        .id();
    // The grant does not waive the cost, so the cast needs real mana.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(standin, AttackTarget::Player(P1))])
        .expect("the stand-in must be a legal attacker");
    settle_attack_trigger(&mut runner, true);

    assert_eq!(
        runner.state().objects[&bolt].casting_permissions.len(),
        1,
        "reach guard: the permission must exist before the cast can use it"
    );
    assert_eq!(
        p1p1(&runner, standin),
        0,
        "\"when you cast that spell\" is gated on the CAST, so granting the permission alone \
         must not place the counter"
    );

    runner
        .cast(bolt)
        .target_players(&[P1])
        .try_resolve()
        .expect("the granted graveyard cast must succeed");

    assert_eq!(
        p1p1(&runner, standin),
        1,
        "casting the granted spell must place exactly the one printed counter — a dropped \
         consequent leaves this at zero"
    );
}

/// CR 603.7 (issue #8721): Ogre Battlecaster itself — the one PRINTED card whose
/// parse this recognizer changes with the "when you cast that spell" wording.
///
/// It asserts the LOWERING, not a runtime payoff, and that is a deliberate
/// change from an earlier revision of this test. Ogre's consequent is "+X/+0 …
/// where X is that spell's mana value", and MEASURED, X binds against a spell not
/// yet cast and resolves to 0 whether the pump is gated or not — so any
/// power-based assertion here stays green with the fix removed and answers "yes"
/// to the only question a reviewer asks. The parsed shape does not: without the
/// recognizer the consequent is a sequential instruction with no delayed trigger
/// at all.
///
/// The runtime half of this wording is proved by
/// `the_when_you_cast_that_spell_wording_pays_out_only_after_the_cast`, whose
/// stand-in swaps Ogre's consequent for an observable one.
///
/// Counter-probe: disabling `strip_cast_this_way_gate` turns the lowering
/// assertion red.
#[test]
fn ogre_battlecaster_lowers_its_gate_to_a_delayed_trigger() {
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
        "reach guard: the trigger must have granted its cast permission, so the printed \
         ability really resolved through the production pipeline"
    );

    // The discriminating assertion: Ogre's own printed consequent must have been
    // lowered to a delayed trigger keyed to the later cast, rather than placed as
    // a sequential instruction of the granting resolution.
    // Exact rather than shape-matching: the delayed trigger must be keyed to a
    // SpellCast AND carry Ogre's own printed payload, a `Pump` on `SelfRef`.
    // MEASURED, that `SelfRef` is a SECOND parse change on this card — the old
    // lowering produced `target: Any` for "this creature" — so asserting it here
    // pins that change instead of leaving it silent.
    fn walk(def: &engine::types::ability::AbilityDefinition) -> bool {
        use engine::types::ability::{DelayedTriggerCondition, Effect, TargetFilter};
        if let Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WhenNextEvent { trigger, .. },
            effect,
            ..
        } = &*def.effect
        {
            if matches!(
                trigger.mode,
                engine::types::triggers::TriggerMode::SpellCast
            ) && matches!(
                &*effect.effect,
                Effect::Pump {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ) {
                return true;
            }
        }
        def.sub_ability.as_deref().is_some_and(walk)
            || def.else_ability.as_deref().is_some_and(walk)
    }
    // `Definitions` deliberately exposes no `iter()` (its module doc explains
    // why: iterating must go through the CR-gated `functioning_abilities`), so
    // read the one printed trigger positionally, the way the neighbouring
    // integration tests do.
    let attack_trigger = runner.state().objects[&ogre]
        .trigger_definitions
        .first()
        .expect("Ogre must carry its printed attack trigger")
        .clone();
    assert!(
        attack_trigger
            .definition
            .execute
            .as_ref()
            .is_some_and(|body| walk(body)),
        "\"when you cast that spell\" must lower to a delayed trigger keyed to the later cast \
         (CR 603.7) carrying Ogre's own `Pump` on `SelfRef` — not to a sequential instruction \
         of the granting resolution, and not to a pump aimed at anything else"
    );

    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&ogre].power,
        Some(3),
        "and no spell was cast under the permission, so nothing pumped — kept as the \
         companion check, NOT as evidence. MEASURED END TO END, with the granted Bolt \
         actually cast and layers re-evaluated, Ogre is `Some(3)` both with this change and \
         with the parser gate textually removed: `ObjectManaValue` with an `EventSource` scope \
         resolves to 0 in the delayed trigger, so the printed card cannot discriminate the \
         wording and the stand-in below carries that evidence. This change fixes the TIMING \
         of Ogre's consequent, not the value of its X (issue #8775)"
    );
}
