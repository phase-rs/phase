//! Production-path coverage for Athreos, God of Passage — the declared-target
//! unless-payer class ("[effect] unless target opponent/target player pays
//! [cost]").
//!
//! Oracle (trigger line under test):
//!   "Whenever another creature you control dies, return it to its owner's hand
//!    unless target opponent pays 3 life."
//!
//! Unlike the anaphoric punisher class ("... unless they/that opponent pays"),
//! the payer here is DECLARED as a target INSIDE the unless clause and chosen at
//! stack placement (CR 603.3d). The trigger must therefore surface a player
//! target slot bound to the controller's OPPONENTS, and the resulting
//! `UnlessPayment` prompt must go to the CHOSEN opponent — never the controller.
//!
//! CR ANCHORS (verified against docs/MagicCompRules.txt):
//!   * CR 115.1   — targets are declared as the spell/ability goes on the stack.
//!   * CR 118.12a — "[Do something] unless [a player does something else]."
//!   * CR 119.4   — paying life loses that much life.
//!   * CR 603.3d  — a triggered ability with no legal target is removed from the stack.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const ATHREOS_TRIGGER: &str = "Whenever another creature you control dies, \
     return it to its owner's hand unless target opponent pays 3 life.";

const DECLARED_PAYER_DESTROY: &str = "Destroy target creature unless target opponent pays 3 life.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// True when `player`'s `zone` holds an object named `name`. Name-based (not
/// ObjectId-based) so it survives the CR 400.7 new-object-per-zone-change churn
/// a dying / returning creature goes through.
fn name_in_zone(
    runner: &engine::game::scenario::GameRunner,
    player: PlayerId,
    zone: Zone,
    name: &str,
) -> bool {
    let state = runner.state();
    let p = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");
    let ids = match zone {
        Zone::Hand => &p.hand,
        Zone::Graveyard => &p.graveyard,
        _ => panic!("name_in_zone only supports Hand/Graveyard"),
    };
    ids.iter()
        .any(|id| state.objects.get(id).is_some_and(|o| o.name == name))
}

/// Mark `victim` with lethal damage, run state-based actions so it dies, then
/// process the triggers it produced. Leaves the runner with the Athreos trigger
/// pending (awaiting target selection).
fn kill_and_trigger(runner: &mut engine::game::scenario::GameRunner, victim: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
}

/// Drive priority/resolution until the Athreos trigger surfaces its declared
/// player-target slot; choose `chosen` (must be one of the controller's
/// opponents). Panics if the trigger never asks for a target.
fn select_trigger_target(runner: &mut engine::game::scenario::GameRunner, chosen: PlayerId) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(chosen)),
                    "chosen opponent must be a legal target, slots = {target_slots:?}"
                );
                assert!(
                    !target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P0)),
                    "the controller (P0) must NOT be a legal opponent target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(chosen)],
                    })
                    .expect("targeting the opponent must succeed");
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state before target selection: {other:?}"),
        }
    }
    panic!("Athreos trigger never surfaced a target-player selection");
}

/// Advance to the `UnlessPayment` prompt (resolving the trigger off the stack),
/// asserting the payer is `expected`. Returns once the prompt is reached.
fn advance_to_unless_payment(runner: &mut engine::game::scenario::GameRunner, expected: PlayerId) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment { player, .. } => {
                assert_eq!(
                    player, expected,
                    "the unless-payer must be the chosen opponent, not the controller"
                );
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state before UnlessPayment: {other:?}"),
        }
    }
    panic!("the Athreos trigger never produced an UnlessPayment prompt");
}

/// Build a 3-player game with Athreos under P0 and a vanilla creature P0
/// controls. Returns `(runner, victim_id)`.
fn setup_three_player() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Athreos, God of Passage", 0, 0, ATHREOS_TRIGGER);
    let victim = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let runner = scenario.build();
    (runner, victim)
}

fn setup_destroy_spell_at_unless_prompt() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 43);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Declared Payer Destroy", true, DECLARED_PAYER_DESTROY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let creature = scenario.add_creature(P1, "P1 Bear", 2, 2).id();

    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Colorless, ManaType::Black]);

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast declared-payer destroy spell");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert_eq!(
                    target_slots.len(),
                    2,
                    "spell must surface the creature target and declared payer target"
                );
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P2)),
                    "first target slot must accept the chosen opponent payer, slots = {target_slots:?}"
                );
                assert!(
                    !target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P0)),
                    "the caster must not be legal for a target-opponent payer"
                );
                assert!(
                    target_slots[1]
                        .legal_targets
                        .contains(&TargetRef::Object(creature)),
                    "second target slot must accept the target creature, slots = {target_slots:?}"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P2), TargetRef::Object(creature)],
                    })
                    .expect("select creature target and opponent payer");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected pre-resolution prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P2, .. }
        ),
        "declared payer target must resolve to the chosen opponent, got {:?}",
        runner.state().waiting_for
    );

    (runner, creature)
}

/// CR 115.1 + CR 118.12a + CR 119.4 (V6 decline): the chosen opponent (P2)
/// receives the prompt; declining lets the effect happen — the dead creature
/// returns to its owner (P0)'s hand and P2's life is unchanged.
#[test]
fn athreos_targets_chosen_opponent_decline_returns_creature() {
    let (mut runner, victim) = setup_three_player();
    let p2_life_before = runner.life(P2);

    kill_and_trigger(&mut runner, victim);
    select_trigger_target(&mut runner, P2);
    advance_to_unless_payment(&mut runner, P2);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // CR 118.12a: declining means the effect happens — the creature returns to
    // its owner's hand.
    assert!(
        name_in_zone(&runner, P0, Zone::Hand, "Grizzly Bears"),
        "declining must return the dead creature to its owner (P0)'s hand"
    );
    assert!(
        !name_in_zone(&runner, P0, Zone::Graveyard, "Grizzly Bears"),
        "the creature must have left the graveyard for hand"
    );
    // CR 119.4: declining costs no life.
    assert_eq!(
        runner.life(P2),
        p2_life_before,
        "declining must not change the chosen opponent's life"
    );
}

/// CR 118.12a + CR 119.4 (V7 pay): when the chosen opponent (P2) pays 3 life,
/// the effect is suppressed — the creature stays in the graveyard and P2 loses
/// exactly 3 life.
#[test]
fn athreos_chosen_opponent_pays_keeps_creature_in_graveyard() {
    let (mut runner, victim) = setup_three_player();
    let p2_life_before = runner.life(P2);

    kill_and_trigger(&mut runner, victim);
    select_trigger_target(&mut runner, P2);
    advance_to_unless_payment(&mut runner, P2);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the 3-life unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // CR 118.12a: paying suppresses the effect — the creature stays dead.
    assert!(
        name_in_zone(&runner, P0, Zone::Graveyard, "Grizzly Bears"),
        "paying the unless-cost must keep the creature in the graveyard"
    );
    assert!(
        !name_in_zone(&runner, P0, Zone::Hand, "Grizzly Bears"),
        "paying must NOT return the creature to hand"
    );
    // CR 119.4: paying 3 life loses exactly 3 life.
    assert_eq!(
        runner.life(P2),
        p2_life_before - 3,
        "paying the unless-cost must deduct exactly 3 life from the chosen opponent"
    );
}

/// CR 115.1 + CR 118.12a: resolution-side declared-target payers compose with
/// the primary effect's ordinary target slot. Declining the chosen opponent
/// payer's cost lets the destroy effect happen.
#[test]
fn resolution_declared_payer_decline_destroys_target_creature() {
    let (mut runner, creature) = setup_destroy_spell_at_unless_prompt();
    let p2_life_before = runner.life(P2);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline declared payer cost");
    runner.advance_until_stack_empty();

    assert!(
        name_in_zone(&runner, P1, Zone::Graveyard, "P1 Bear"),
        "declining the declared payer cost must destroy the target creature"
    );
    assert_eq!(
        runner.life(P2),
        p2_life_before,
        "declining must not charge the chosen payer"
    );
    assert_ne!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the original target object must leave the battlefield"
    );
}

/// CR 118.12a + CR 119.4: when the chosen opponent pays the declared-target
/// unless cost, the primary resolution-side effect is suppressed and that
/// opponent loses exactly 3 life.
#[test]
fn resolution_declared_payer_pay_prevents_destroy() {
    let (mut runner, creature) = setup_destroy_spell_at_unless_prompt();
    let p2_life_before = runner.life(P2);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay declared payer cost");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "paying the declared payer cost must prevent the destroy effect"
    );
    assert_eq!(
        runner.life(P2),
        p2_life_before - 3,
        "paying the declared payer cost must charge the chosen opponent"
    );
}

/// CR 603.3d (R1 — required-target removal): Athreos is a MANDATORY trigger.
/// When NO opponent can be legally targeted (here: the sole reachable opponents
/// all have protection from everything, so the declared opponent target has no
/// legal choice), the trigger is REMOVED from the stack and the creature is NOT
/// returned. This proves the opponent is a real required target (CR 115.1), not
/// a silent no-op — reverting the target-slot wiring makes this test fail (the
/// trigger would resolve with no payer and wrongly return the creature, or
/// never prompt at all).
#[test]
fn athreos_trigger_removed_when_no_legal_opponent_target() {
    let (mut runner, victim) = setup_three_player();

    // CR 702.16j: grant every opponent protection from everything, so the
    // declared "target opponent" slot has no legal target. Driven through the
    // single TCE authority — a real continuous-effect grant, not a test hook.
    for opponent in [P1, P2] {
        runner.state_mut().add_transient_continuous_effect(
            ObjectId(0),
            opponent,
            Duration::Permanent,
            TargetFilter::SpecificPlayer { id: opponent },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );
    }

    kill_and_trigger(&mut runner, victim);
    runner.advance_until_stack_empty();

    // CR 603.3d: with no legal opponent target, the trigger is removed — no
    // UnlessPayment prompt ever appears.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "a mandatory trigger with no legal target must NOT reach an UnlessPayment prompt"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "a trigger with no legal target must not hang on target selection"
    );
    // The creature stays dead — the removed trigger never returned it.
    assert!(
        name_in_zone(&runner, P0, Zone::Graveyard, "Grizzly Bears"),
        "the removed trigger must not return the creature to hand (CR 603.3d)"
    );
    assert!(
        !name_in_zone(&runner, P0, Zone::Hand, "Grizzly Bears"),
        "the creature must remain in the graveyard, not return to hand"
    );
}

// ---------------------------------------------------------------------------
// Resolution-side coverage — a SPELL whose PRIMARY effect already declares an
// object target AND carries a declared opponent payer ("Destroy target creature
// unless target opponent pays 3 life"). Unlike Athreos (a triggered ability with
// only the payer target), this surfaces TWO target slots at cast time: the
// effect's own creature object slot and the companion declared-payer player
// slot. The resolution-side path combines these independently of the trigger-
// side flow, so it needs its own production-path coverage (CR 115.1: targets
// declared as the spell goes on the stack).
//
// Slot ORDER is load-bearing: `ability_utils::collect_target_slots` pushes the
// companion payer slot BEFORE the effect's primary creature slot, so the cast
// surfaces `[player payer slot, creature slot]`. The driver below selects per
// slot from each slot's own legal set, so it is order-agnostic — but it asserts
// that BOTH a player slot (offering the opponent) and an object slot (offering
// the creature) were surfaced, which is the dual-slot property under test.

const DESTROY_UNLESS: &str = "Destroy target creature unless target opponent pays 3 life.";

/// True when an object named `name` is on the battlefield. Name-based to survive
/// the CR 400.7 new-object-per-zone-change churn, mirroring `name_in_zone`.
fn name_on_battlefield(runner: &engine::game::scenario::GameRunner, name: &str) -> bool {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|obj| obj.name == name)
}

/// Build a 3-player game in P0's main phase with the resolution-side sorcery in
/// P0's hand and a vanilla creature P1 controls (the natural "Destroy target
/// creature" victim). Returns `(runner, spell_id, victim_id)`.
///
/// THREE players (P0 caster, P1 + P2 opponents) on purpose: with a single
/// opponent the payer slot has exactly one legal target and, combined with the
/// single creature, yields exactly ONE legal target combination — which the
/// caster auto-selects (CR 601.2c), skipping the interactive `TargetSelection`
/// window. A second opponent makes the declared-payer slot ambiguous so the
/// dual-slot interactive selection actually surfaces and can be asserted on
/// (the reviewer's core ask: prove BOTH slots are surfaced at cast time). It
/// also doubles as opponent-restriction coverage — the caster (P0) must never
/// be offered as the declared opponent payer.
fn setup_three_player_spell() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // CR 601.2c: the sorcery (default {0} mana cost — auto-pays from an empty
    // pool, like every `add_spell_to_hand_from_oracle` cast test).
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Declared Payer Destroy", false, DESTROY_UNLESS)
        .id();
    // The victim P0 will target: a creature P1 controls.
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let runner = scenario.build();
    (runner, spell, victim)
}

/// Cast `spell` through the production pipeline, selecting `victim` for the
/// effect's creature slot and `payer` for the declared-payer player slot. Drives
/// the per-slot `TargetSelection` (CR 601.2c) and asserts BOTH the creature
/// object slot and the opponent player slot were surfaced. Stops at the post-
/// cast `Priority` window (spell on the stack, both targets declared).
fn cast_with_creature_and_payer(
    runner: &mut engine::game::scenario::GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    payer: PlayerId,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the sorcery must be accepted");

    let mut saw_creature_slot = false;
    let mut saw_payer_slot = false;

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            // CR 601.2c: declare one target per slot, in slot order. Each slot
            // carries its own legal set; pick the creature for the object slot
            // and the chosen opponent for the player slot.
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = if slot.legal_targets.contains(&TargetRef::Object(victim)) {
                    saw_creature_slot = true;
                    TargetRef::Object(victim)
                } else if slot.legal_targets.contains(&TargetRef::Player(payer)) {
                    saw_payer_slot = true;
                    // CR 115.1 + CR 118.12a: the declared opponent payer is a
                    // real required target scoped to the controller's OPPONENTS.
                    // Both opponents (P1, P2) are offered; the controller (P0)
                    // must NOT be — proving the slot honours the Opponent filter,
                    // not a free all-players choice.
                    assert!(
                        slot.legal_targets.contains(&TargetRef::Player(P2)),
                        "both opponents must be legal declared payers, slot = {slot:?}"
                    );
                    assert!(
                        !slot.legal_targets.contains(&TargetRef::Player(P0)),
                        "the controller (P0) must NOT be a legal opponent payer, slot = {slot:?}"
                    );
                    TargetRef::Player(payer)
                } else {
                    panic!(
                        "cast surfaced a target slot matching neither the creature nor the \
                         declared payer: {slot:?}"
                    );
                };
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(choice),
                    })
                    .expect("declaring the slot target must succeed");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during cast targeting: {other:?}"),
        }
    }

    assert!(
        saw_creature_slot,
        "the cast must surface the effect's creature object slot (Destroy target creature)"
    );
    assert!(
        saw_payer_slot,
        "the cast must surface the companion declared-payer player slot (unless target opponent)"
    );
}

/// Advance resolution (passing priority) until the `UnlessPayment` prompt
/// surfaces, asserting the payer is `expected`. The scenario resolution driver
/// does not auto-answer `UnlessPayment`, so this drives priority manually.
fn advance_spell_to_unless_payment(
    runner: &mut engine::game::scenario::GameRunner,
    expected: PlayerId,
) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment { player, .. } => {
                // CR 118.12a: the unless-payer is the CHOSEN declared opponent
                // (read from the first `TargetRef::Player` in `ability.targets`),
                // never the spell's controller — even though a creature object
                // target also sits in `ability.targets`.
                assert_eq!(
                    player, expected,
                    "the unless-payer must be the chosen declared opponent, not the controller"
                );
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state before UnlessPayment: {other:?}"),
        }
    }
    panic!("the resolving spell never produced an UnlessPayment prompt");
}

/// CR 115.1 + CR 118.12a + CR 701.8a (resolution-side decline): casting "Destroy
/// target creature unless target opponent pays 3 life", choosing P1's creature
/// and P1 as the payer, then DECLINING → the effect happens: the creature is
/// destroyed (moves to its owner P1's graveyard) and P1's life is unchanged.
///
/// This is the production-path proof the trigger-side Athreos coverage cannot
/// give: it exercises the DUAL-slot cast (object + declared payer) and the
/// resolution-side `resolve_unless_payer` → `resolve_effect_player_ref` selection
/// of the Player over the Object in `ability.targets`. Reverting the
/// declared-target slot wiring (`ability_needs_companion_target_player_slot`) or
/// the payer resolver arm makes this fail — no payer slot would surface, or the
/// prompt would go to the wrong player / never appear.
#[test]
fn resolution_destroy_unless_declared_opponent_decline_destroys_creature() {
    let (mut runner, spell, victim) = setup_three_player_spell();
    let p1_life_before = runner.life(P1);

    cast_with_creature_and_payer(&mut runner, spell, victim, P1);
    advance_spell_to_unless_payment(&mut runner, P1);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // CR 118.12a + CR 701.8a: declining means the effect happens — the targeted
    // creature is destroyed and moves to its owner (P1)'s graveyard.
    assert!(
        name_in_zone(&runner, P1, Zone::Graveyard, "Grizzly Bears"),
        "declining must destroy the targeted creature into its owner (P1)'s graveyard"
    );
    assert!(
        !name_on_battlefield(&runner, "Grizzly Bears"),
        "the destroyed creature must leave the battlefield"
    );
    // CR 119.4: declining costs no life.
    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "declining must not change the declared opponent's life"
    );
}

/// CR 118.12a + CR 119.4 + CR 701.8a (resolution-side pay): when the chosen
/// declared opponent (P1) pays 3 life, the effect is suppressed — the targeted
/// creature SURVIVES on the battlefield and P1 loses exactly 3 life.
#[test]
fn resolution_destroy_unless_declared_opponent_pays_keeps_creature() {
    let (mut runner, spell, victim) = setup_three_player_spell();
    let p1_life_before = runner.life(P1);
    assert!(
        p1_life_before >= 3,
        "the declared opponent must start with >= 3 life to pay the cost (got {p1_life_before})"
    );

    cast_with_creature_and_payer(&mut runner, spell, victim, P1);
    advance_spell_to_unless_payment(&mut runner, P1);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the 3-life unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // CR 118.12a: paying suppresses the effect — the creature is NOT destroyed.
    assert!(
        name_on_battlefield(&runner, "Grizzly Bears"),
        "paying the unless-cost must keep the targeted creature on the battlefield"
    );
    assert!(
        !name_in_zone(&runner, P1, Zone::Graveyard, "Grizzly Bears"),
        "paying must NOT send the creature to the graveyard"
    );
    // CR 119.4: paying 3 life loses exactly 3 life.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "paying the unless-cost must deduct exactly 3 life from the declared opponent"
    );
}
