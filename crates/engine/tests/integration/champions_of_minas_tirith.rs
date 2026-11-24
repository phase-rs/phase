//! Phase 4 (unit 3b, Champions of Minas Tirith): third-party may-pay with a
//! decline attack restriction ("that opponent may pay {X}, where X is the
//! number of cards in their hand. If they don't, they can't attack you this
//! combat").
//!
//! V-row map: V1 standing fire behavior (all 3 seats); V2 prompt identity (P1
//! only, 3P); V3 X = payer's hand read at payment-offer time; V3b X=0
//! short-circuit pin (DEFERRED rules question); V4 intervening-if at both CR
//! 603.4 checkpoints; V5 restriction presence/expiry/second combat; V6 pay
//! path + attack resolves; V7 multiplayer scope (decliner only, 3P); V8 prune
//! authority (both teardown callers prune; untap/cleanup spare).

use engine::game::combat::{build_declare_attackers_waiting_for, declare_attackers, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::turns::{end_combat_phase_to_postcombat, execute_cleanup, execute_untap};
use engine::types::ability::{
    AbilityCost, GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ExtraPhase, GameState, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const CHAMPIONS: &str = "When this creature enters, you become the monarch.\nAt the beginning of combat on each opponent's turn, if you're the monarch, that opponent may pay {X}, where X is the number of cards in their hand. If they don't, they can't attack you this combat.";
const THRONE: &str = "{T}: Add {C}.\n{4}, {T}, Sacrifice this land: You become the monarch.";

/// Add `count` colorless mana to `player`'s pool (mirrors
/// call_for_aid_phase2::add_mana). Called AFTER driving to the spending phase:
/// CR 500.5 empties pools between phases, so pre-seeded mana never survives
/// to BeginCombat.
fn add_mana(runner: &mut GameRunner, player: PlayerId, count: usize) {
    let target = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for _ in 0..count {
        target.mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Seed `count` generic cards into a hand (pre-build).
fn hand(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    let names: Vec<String> = (0..count).map(|i| format!("Card{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_cards_in_hand(player, &refs);
}

struct ChampionsStage {
    runner: GameRunner,
    p1_bear: ObjectId,
    p1_bear_b: ObjectId,
}

/// Stage: P0 holds Champions (attack-ready 3/4); P1 (+P2 in 3P) hold
/// attack-ready 2/2 bears; hands seeded; P0 monarch; `active` to move at
/// PreCombatMain. Mana is funded post-drive via `add_mana` (CR 500.5).
fn stage(active: PlayerId, hands: &[usize], three_player: bool) -> ChampionsStage {
    let mut scenario = if three_player {
        GameScenario::new_n_player(3, 42)
    } else {
        GameScenario::new()
    };
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Champions of Minas Tirith", 3, 4, CHAMPIONS)
        .id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let p1_bear_b = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    if three_player {
        scenario.add_creature(P2, "P2 Bear", 2, 2);
    }
    for (i, count) in hands.iter().enumerate() {
        hand(&mut scenario, PlayerId(i as u8), *count);
    }
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.monarch = Some(P0);
        state.active_player = active;
        state.priority_player = active;
        state.waiting_for = WaitingFor::Priority { player: active };
    }
    ChampionsStage {
        runner,
        p1_bear,
        p1_bear_b,
    }
}

/// Pass priority until `phase` is current (bounded; fails on any prompt or
/// on crossing a turn boundary — every drive here stays within one turn).
fn drive_to_phase(runner: &mut GameRunner, phase: Phase) {
    let turn = runner.state().turn_number;
    for _ in 0..24 {
        assert_eq!(
            runner.state().turn_number,
            turn,
            "drive must not cross a turn boundary"
        );
        if runner.state().phase == phase {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority while driving to {phase:?}, got {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass");
    }
    panic!("never reached {phase:?}");
}

/// Pass priority until the stack empties or a non-Priority prompt opens;
/// return the terminal waiting state.
fn resolve_until_settled(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..40 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    runner.state().waiting_for.clone()
}

/// Expect the UnlessPayment prompt; return (player, cost, remaining poll).
fn expect_unless_payment(state: &GameState) -> (PlayerId, AbilityCost, Vec<PlayerId>) {
    match &state.waiting_for {
        WaitingFor::UnlessPayment {
            player,
            cost,
            remaining,
            ..
        } => (*player, cost.clone(), remaining.clone()),
        other => panic!("expected UnlessPayment, got {other:?}"),
    }
}

/// Assert the single installed restriction is the Champions decline shape:
/// P1 restricted, EndOfCombat, P0 protected.
fn expect_champions_restriction(state: &GameState) {
    assert_eq!(
        state.restrictions.len(),
        1,
        "exactly one restriction installed, got {:?}",
        state.restrictions
    );
    assert!(
        matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::SpecificPlayer(P1),
                expiry: RestrictionExpiry::EndOfCombat,
                activity: ProhibitedActivity::Attack {
                    protected_player: Some(P0),
                    ..
                },
                ..
            }
        ),
        "expected the Champions decline restriction, got {:?}",
        state.restrictions[0]
    );
}

/// CR 508.1c legality probe on a state clone (mirrors call_for_aid_phase2):
/// can `attacker` (controlled by `controller`) attack `target`?
fn attack_legal(
    state: &GameState,
    controller: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = state.clone();
    state.active_player = controller;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

/// Park the runner at a DeclareAttackers prompt for P1 over P0+P2 (mirrors
/// call_for_aid_phase2's park_declare): the submission validator recomputes
/// legality, so the hardcoded payload never masks a prohibition.
fn park_declare_p1(runner: &mut GameRunner, attackers: &[ObjectId]) {
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: attackers.to_vec(),
        valid_attack_targets: vec![AttackTarget::Player(P0), AttackTarget::Player(P2)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

/// Per-attacker legal targets from the REAL payload builder.
fn payload_targets_for(state: &GameState, attacker: ObjectId) -> Vec<AttackTarget> {
    match build_declare_attackers_waiting_for(state) {
        WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker: Some(map),
            ..
        } => map.get(&attacker).cloned().unwrap_or_default(),
        other => panic!("expected a DeclareAttackers payload, got {other:?}"),
    }
}

const TIME_STOP: &str = "End the turn.";

/// Fund `count` mana of `ty` (`add_mana` is colorless-only; Time Stop needs
/// blue). Called after driving to the spending phase (CR 500.5).
fn add_typed_mana(runner: &mut GameRunner, player: PlayerId, ty: ManaType, count: usize) {
    let target = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for _ in 0..count {
        target
            .mana_pool
            .add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
    }
}

/// Drive across whole turns until the next UnlessPayment prompt opens (here:
/// the next Champions offer), answering everything else: priority passes,
/// empty attack/block declarations. Bounded; panics on any other prompt.
fn drive_to_next_unless_payment(runner: &mut GameRunner) {
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment { .. } => return,
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("decline to attack");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("decline to block");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt while driving turns, got {other:?}"),
        }
    }
    panic!("never reached the next offer");
}

/// V1 (row 1) — CR 603.2 + CR 500.1: standing fire behavior in all three
/// seats. Controller's own BeginCombat is silent (reach-guard: the phase-3
/// constraint holds end to end); each opponent's BeginCombat fires (prompt).
#[test]
fn champions_fires_only_on_opponent_turns_all_seats() {
    // Seat P0 (controller): silent.
    let mut stage0 = stage(P0, &[2, 5, 7], true);
    drive_to_phase(&mut stage0.runner, Phase::BeginCombat);
    assert!(
        stage0.runner.state().stack.is_empty(),
        "controller's BeginCombat must not fire"
    );
    // Seats P1, P2 (opponents): fire.
    for seat in [P1, P2] {
        let mut stage = stage(seat, &[2, 5, 7], true);
        drive_to_phase(&mut stage.runner, Phase::BeginCombat);
        assert!(
            !stage.runner.state().stack.is_empty(),
            "opponent {seat:?}'s BeginCombat must fire"
        );
        let waiting = resolve_until_settled(&mut stage.runner);
        assert!(
            matches!(waiting, WaitingFor::UnlessPayment { .. }),
            "opponent {seat:?}'s trigger must prompt, got {waiting:?}"
        );
        let (player, _, _) = expect_unless_payment(stage.runner.state());
        assert_eq!(player, seat, "prompt goes to the firing opponent");
    }
    // Negative leg: no monarch — the `if you're the monarch` intervening-if
    // gate (CR 603.4) keeps the trigger silent on the opponent's turn even
    // though the phase matches. The firing legs above are the reach-guards.
    let mut stage_nomarch = stage(P1, &[2, 5, 7], true);
    stage_nomarch.runner.state_mut().monarch = None;
    drive_to_phase(&mut stage_nomarch.runner, Phase::BeginCombat);
    assert!(
        stage_nomarch.runner.state().stack.is_empty(),
        "without the monarch the opponent-turn trigger must not fire"
    );
}

/// V2 (row 2) — CR 118.12 + CR 603.5: the payment prompt goes to the active
/// opponent only (single-payer poll, nobody else queued); answering it as P1
/// installs the P1-scoped restriction (paired reach-guard).
#[test]
fn unless_prompt_goes_to_active_opponent_only() {
    let mut stage = stage(P1, &[2, 5, 7], true);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    let (player, _, remaining) = expect_unless_payment(stage.runner.state());
    assert_eq!(player, P1, "prompt goes to P1, never P0/P2");
    assert!(
        remaining.is_empty(),
        "single-payer poll: nobody queued behind P1, got {remaining:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut stage.runner);
    expect_champions_restriction(stage.runner.state());
}

/// V3 (row 3) — CR 107.3c + CR 608.2g: X is the payer's hand read at
/// payment-offer time (resolution), not snapshotted at trigger creation.
/// P1 holds 3 when the trigger fires, draws to 5 before it resolves:
/// offered {5} — not {3} (creation), {2} (controller), or {7} (P2/max).
#[test]
fn x_is_payers_hand_at_payment_offer() {
    let mut stage = stage(P1, &[2, 3, 7], true);
    // Library top-up for the mid-stack draw-up.
    {
        let state = stage.runner.state_mut();
        for name in ["DrawA", "DrawB"] {
            let card_id = engine::types::identifiers::CardId(state.next_object_id);
            let id = engine::game::zones::create_object(
                state,
                card_id,
                P1,
                name.to_string(),
                Zone::Library,
            );
            let ps = state
                .players
                .iter_mut()
                .find(|p| p.id == P1)
                .expect("P1 exists");
            ps.library.retain(|&oid| oid != id);
            ps.library.insert(0, id);
        }
    }
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    assert!(
        !stage.runner.state().stack.is_empty(),
        "trigger fired while P1 held 3 (creation-time reach-guard)"
    );
    // Mid-stack draw-up to 5 (stand-in for a real draw; no draw triggers in
    // play, so the missing CardDrawn event is unobservable).
    {
        let state = stage.runner.state_mut();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id = state
                .players
                .iter_mut()
                .find(|p| p.id == P1)
                .expect("P1 exists")
                .library
                .pop_front()
                .expect("library top-up present");
            ids.push(id);
        }
        for id in ids {
            state
                .players
                .iter_mut()
                .find(|p| p.id == P1)
                .expect("P1 exists")
                .hand
                .push_back(id);
            state.objects.get_mut(&id).expect("card").zone = Zone::Hand;
        }
    }
    assert_eq!(
        stage
            .runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .expect("P1 exists")
            .hand
            .len(),
        5,
        "P1 holds 5 at payment-offer time"
    );
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    let (player, cost, _) = expect_unless_payment(stage.runner.state());
    assert_eq!(player, P1);
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(5),
        },
        "X read the payer's hand (5) at offer time, got {cost:?}"
    );
}

/// V3b (row 3) — CR 118.5 + CR 118.12a: with a hellbent payer X=0 still
/// OPENS the prompt (a zero payment is offered, never assumed refused).
/// Accept leg: paying {0} avoids the restriction and the attack stays legal.
/// The offered cost is `Mana { {0} }`, kept distinct from `NoCost` (which
/// means "no payment exists to offer").
#[test]
fn x_zero_prompts_and_accepting_avoids_restriction() {
    let mut stage = stage(P1, &[2, 0], false);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "X=0 must still prompt, got {waiting:?}"
    );
    let (player, cost, _) = expect_unless_payment(stage.runner.state());
    assert_eq!(player, P1);
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(0),
        },
        "X read the hellbent hand (0) as payable {{0}}, not NoCost, got {cost:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay {0}");
    resolve_until_settled(&mut stage.runner);
    assert!(
        stage.runner.state().restrictions.is_empty(),
        "paying {{0}} avoids the restriction, got {:?}",
        stage.runner.state().restrictions
    );
    assert!(
        attack_legal(
            stage.runner.state(),
            P1,
            stage.p1_bear,
            AttackTarget::Player(P0)
        ),
        "after paying {{0}} the attack at P0 must be legal"
    );
}

/// V3b decline leg: refusing the {0} payment installs the restriction and
/// bars the attack, exactly like a nonzero decline.
#[test]
fn x_zero_decline_installs_restriction() {
    let mut stage = stage(P1, &[2, 0], false);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "X=0 must still prompt, got {waiting:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut stage.runner);
    expect_champions_restriction(stage.runner.state());
    assert!(
        !attack_legal(
            stage.runner.state(),
            P1,
            stage.p1_bear,
            AttackTarget::Player(P0)
        ),
        "after declining {{0}} the attack at P0 must be barred"
    );
}

/// V4 (row 4) — CR 603.4: the monarch intervening-if holds at BOTH
/// checkpoints. Positive: monarch throughout → prompt. Negative: monarch at
/// fire, lost before resolution → removed with no prompt and no restriction.
/// The loss runs through a REAL instant-speed response (Throne of the High
/// City activation), standing in for the CR 725.2 inherent combat-damage
/// transfer, which cannot interleave between a BeginCombat trigger's firing
/// and its resolution.
#[test]
fn monarch_required_at_fire_and_resolution() {
    // Positive leg: monarch held throughout → prompt.
    let mut stage = stage(P1, &[2, 3], false);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "monarch held: must prompt, got {waiting:?}"
    );

    // Negative leg: lose the monarch in response to the trigger.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Champions of Minas Tirith", 3, 4, CHAMPIONS)
        .id();
    scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    hand(&mut scenario, P0, 2);
    hand(&mut scenario, P1, 3);
    let throne = scenario
        .add_land_from_oracle(P1, "Throne of the High City", THRONE)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.monarch = Some(P0);
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    drive_to_phase(&mut runner, Phase::BeginCombat);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Champions fired with P0 monarch (fire checkpoint passed)"
    );
    add_mana(&mut runner, P1, 4);
    // Find the monarch ability (line order: mana ability first, monarch second).
    let throne_abilities = runner
        .state()
        .objects
        .get(&throne)
        .expect("throne exists")
        .abilities
        .len();
    assert_eq!(throne_abilities, 2, "throne parses both abilities");
    runner
        .act(GameAction::ActivateAbility {
            source_id: throne,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate throne in response");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "throne ability stacked above Champions"
    );
    // Resolve the throne response only; the monarch must flip first.
    for _ in 0..20 {
        if runner.state().stack.len() == 1 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        runner.state().monarch,
        Some(P1),
        "response resolved: P1 is now monarch (reach-guard)"
    );
    // Now resolve Champions: the recheck must remove it unprompted.
    for _ in 0..40 {
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
            "removed trigger must never prompt"
        );
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "only priority while resolving, got {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().stack.is_empty(),
        "trigger left the stack via the failed recheck"
    );
    assert!(
        runner.state().restrictions.is_empty(),
        "no restriction without resolution, got {:?}",
        runner.state().restrictions
    );
}

/// V5 (row 5) — CR 511.2 + CR 506.2: the decline restriction is present
/// mid-combat, absent after, and the turn's SECOND combat starts clean (the
/// re-fired trigger is a fresh prompt, answered here by paying, after which
/// P1 attacks P0 freely). The extra combat is scheduled through the engine's
/// own `ExtraPhase` struct (the same scheduling spells use), pushed directly
/// as scaffolding — the prune, not the scheduling, is the claim.
#[test]
fn decline_restriction_expires_at_end_of_combat_with_second_combat() {
    let mut stage = stage(P1, &[2, 3, 0], true);
    // Scaffolding: an extra combat after this one (mirrors turns.rs's own
    // ExtraPhase tests, which push the struct directly).
    stage.runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        segment: TurnSegment::Phase(PhaseGroup::Combat),
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    });
    // Combat 1: decline → restriction present mid-combat.
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "combat 1 must prompt, got {waiting:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut stage.runner);
    expect_champions_restriction(stage.runner.state());
    // Combat 1 plays out: P1 attacks P2 (restriction bars P0 only).
    drive_to_phase(&mut stage.runner, Phase::DeclareAttackers);
    stage
        .runner
        .declare_attackers(&[(stage.p1_bear, AttackTarget::Player(P2))])
        .expect("P1 attacks P2 through combat 1");
    drive_to_phase(&mut stage.runner, Phase::DeclareBlockers);
    stage
        .runner
        .declare_blockers(&[])
        .expect("P2 declines to block in combat 1");
    stage.runner.combat_damage();
    // Leaving EndCombat#1 prunes the combat-1 restriction (the extra combat
    // inserts directly; no PostCombatMain intervenes). Combat 2 starts clean
    // with a fresh fire, answered here by paying.
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    assert!(
        stage.runner.state().restrictions.is_empty(),
        "pruned at end of combat 1, got {:?}",
        stage.runner.state().restrictions
    );
    assert!(
        !stage.runner.state().stack.is_empty(),
        "combat 2 re-fires its own trigger"
    );
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "combat 2 re-fires its own prompt, got {waiting:?}"
    );
    let (player, cost, _) = expect_unless_payment(stage.runner.state());
    assert_eq!(player, P1);
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(3),
        },
        "combat-2 X still reads P1's hand (3), got {cost:?}"
    );
    add_mana(&mut stage.runner, P1, 3);
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay in combat 2");
    resolve_until_settled(&mut stage.runner);
    assert!(
        stage.runner.state().restrictions.is_empty(),
        "paid: no restriction in combat 2"
    );
    // P1 attacks P0 freely in combat 2 with the fresh bear (the combat-1
    // attacker is tapped): reach-guard that the combat works.
    drive_to_phase(&mut stage.runner, Phase::DeclareAttackers);
    stage
        .runner
        .declare_attackers(&[(stage.p1_bear_b, AttackTarget::Player(P0))])
        .expect("P1 attacks P0 in combat 2");
    drive_to_phase(&mut stage.runner, Phase::DeclareBlockers);
    stage
        .runner
        .declare_blockers(&[])
        .expect("P0 declines to block in combat 2");
    let outcome = stage.runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "unblocked 2/2 connects in combat 2"
    );
}

/// V6 (row 6) — CR 118.12 + CR 510.2: paying {X} prevents the restriction
/// (pool actually drained) and the attack at P0 resolves to damage.
#[test]
fn paying_prevents_restriction_and_attack_resolves() {
    let mut stage = stage(P1, &[2, 3], false);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    add_mana(&mut stage.runner, P1, 3);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    let (player, cost, _) = expect_unless_payment(stage.runner.state());
    assert_eq!(player, P1);
    assert_eq!(
        cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(3),
        },
        "offered {{3}} for a 3-card hand, got {cost:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay");
    resolve_until_settled(&mut stage.runner);
    assert!(
        stage.runner.state().restrictions.is_empty(),
        "paid: no restriction installed, got {:?}",
        stage.runner.state().restrictions
    );
    let pool_len = stage
        .runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .mana_pool
        .total();
    assert_eq!(pool_len, 0, "the {{3}} payment actually drained the pool");
    drive_to_phase(&mut stage.runner, Phase::DeclareAttackers);
    stage
        .runner
        .declare_attackers(&[(stage.p1_bear, AttackTarget::Player(P0))])
        .expect("P1 attacks P0 after paying");
    drive_to_phase(&mut stage.runner, Phase::DeclareBlockers);
    stage
        .runner
        .declare_blockers(&[])
        .expect("P0 declines to block");
    let outcome = stage.runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "unblocked 2/2 connects after paying"
    );
}

/// V7 (row 7) — CR 508.1c + CR 109.5 + CR 608.2c: in 3P the DECLINER is
/// restricted from attacking the controller, while the other opponent is
/// untouched — legality probe, real payload, submission reject/accept, and a
/// resolving attack (mirrors call_for_aid_phase2's V2).
#[test]
fn decline_restricts_only_the_declining_opponent() {
    let mut stage = stage(P1, &[2, 3, 0], true);
    // Reach-guard: pre-decline, P1's bear can attack P0.
    assert!(
        attack_legal(
            stage.runner.state(),
            P1,
            stage.p1_bear,
            AttackTarget::Player(P0)
        ),
        "pre-decline the attack at P0 must be legal"
    );
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut stage.runner);
    expect_champions_restriction(stage.runner.state());

    // Legality: P0 barred, P2 open.
    assert!(
        !attack_legal(
            stage.runner.state(),
            P1,
            stage.p1_bear,
            AttackTarget::Player(P0)
        ),
        "P1 must not attack the protected controller P0"
    );
    assert!(
        attack_legal(
            stage.runner.state(),
            P1,
            stage.p1_bear,
            AttackTarget::Player(P2)
        ),
        "P1 must still attack the unprotected opponent P2"
    );

    // Payload: P0 absent from P1's per-attacker targets while P2 present.
    park_declare_p1(&mut stage.runner, &[stage.p1_bear]);
    let targets = payload_targets_for(stage.runner.state(), stage.p1_bear);
    assert!(
        !targets.contains(&AttackTarget::Player(P0)),
        "payload must exclude P0, got {targets:?}"
    );
    assert!(
        targets.contains(&AttackTarget::Player(P2)),
        "payload must keep P2, got {targets:?}"
    );

    // Submission: attack at P0 rejected, at P2 accepted and resolving.
    park_declare_p1(&mut stage.runner, &[stage.p1_bear]);
    assert!(
        stage
            .runner
            .declare_attackers(&[(stage.p1_bear, AttackTarget::Player(P0))])
            .is_err(),
        "submitting the attack at P0 must be rejected"
    );
    park_declare_p1(&mut stage.runner, &[stage.p1_bear]);
    stage
        .runner
        .declare_attackers(&[(stage.p1_bear, AttackTarget::Player(P2))])
        .expect("submitting the attack at P2 must be accepted");
    drive_to_phase(&mut stage.runner, Phase::DeclareBlockers);
    stage
        .runner
        .declare_blockers(&[])
        .expect("P2 declines to block");
    let outcome = stage.runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P2),
        -2,
        "unblocked 2/2 connects at P2 (reach-guard)"
    );
}

/// V8 (row 8) — CR 511.2: the end-of-combat teardown is the SOLE prune
/// authority for EndOfCombat game restrictions. Both teardown callers prune
/// (the natural EndCombat step, proven by V5's absence legs, and the CR
/// 724.2d skip path, driven here through the real
/// `end_combat_phase_to_postcombat`); the other retains provably spare the
/// combat window (untap handles only UntilPlayerNextTurn, cleanup only
/// EndOfTurn — read-verified at their match arms and pinned at runtime
/// below), and an EndOfTurn control restriction survives the teardown
/// (expiry-selectivity, not a blanket clear).
#[test]
fn end_of_combat_prune_covers_both_callers_selectively() {
    let mut stage = stage(P1, &[2, 3], false);
    drive_to_phase(&mut stage.runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut stage.runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut stage.runner);
    expect_champions_restriction(stage.runner.state());

    // The sibling retains spare the combat window (read-verified match arms,
    // pinned at runtime against the real functions).
    {
        let mut events = Vec::new();
        let state = stage.runner.state_mut();
        execute_untap(state, &mut events);
        assert_eq!(
            state.restrictions.len(),
            1,
            "untap must spare the EndOfCombat restriction"
        );
        let _ = execute_cleanup(state, &mut events);
        assert_eq!(
            state.restrictions.len(),
            1,
            "cleanup must spare the EndOfCombat restriction"
        );
    }

    // Control: same activity, EndOfTurn expiry — must survive the teardown.
    stage
        .runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: ObjectId(0),
            affected_players: RestrictionPlayerScope::SpecificPlayer(P1),
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::Attack {
                defended: engine::types::triggers::AttackTargetFilter::Player,
                protected_player: Some(P0),
                protected_scope: None,
            },
        });
    // Second teardown caller (CR 724.2d skip path) through the real function.
    {
        let mut events = Vec::new();
        let state = stage.runner.state_mut();
        end_combat_phase_to_postcombat(state, &mut events);
        assert_eq!(
            state.phase,
            Phase::PostCombatMain,
            "the skip path ran (reach-guard)"
        );
        assert_eq!(
            state.restrictions.len(),
            1,
            "teardown prunes exactly the combat window, got {:?}",
            state.restrictions
        );
        assert!(
            matches!(
                &state.restrictions[0],
                GameRestriction::ProhibitActivity {
                    expiry: RestrictionExpiry::EndOfTurn,
                    ..
                }
            ),
            "the EndOfTurn control survives, got {:?}",
            state.restrictions[0]
        );
    }
}

/// MED 3 — CR 724.1d + CR 511.2: declining Champions then ending the turn
/// mid-combat expires the EndOfCombat ban through the end-of-combat teardown
/// (cleanup provably spares it — V8). A later combat with a paid fresh offer
/// attacks freely: the old ban is gone.
#[test]
fn champions_decline_then_end_turn_expires_ban_before_later_paid_combat() {
    // Custom stage: like stage(P1, &[2, 3], false) plus Time Stop in P1's
    // hand and stocked libraries (two full turns of draws + monarch).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Champions of Minas Tirith", 3, 4, CHAMPIONS)
        .id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    scenario.add_creature(P1, "P1 Bear B", 2, 2);
    hand(&mut scenario, P0, 2);
    hand(&mut scenario, P1, 3);
    let time_stop = scenario
        .add_spell_to_hand_from_oracle(P1, "Time Stop", true, TIME_STOP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                engine::types::mana::ManaCostShard::Blue,
                engine::types::mana::ManaCostShard::Blue,
            ],
            generic: 4,
        })
        .id();
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.monarch = Some(P0);
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // Turn N (P1): decline the offer at BeginCombat; the ban installs.
    drive_to_phase(&mut runner, Phase::BeginCombat);
    let waiting = resolve_until_settled(&mut runner);
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { .. }),
        "must prompt, got {waiting:?}"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    resolve_until_settled(&mut runner);
    expect_champions_restriction(runner.state());
    assert!(
        !attack_legal(runner.state(), P1, p1_bear, AttackTarget::Player(P0)),
        "reach-guard: the decline bars the attack"
    );

    // Still mid-combat: P1 casts Time Stop; the turn ends through cleanup.
    add_typed_mana(&mut runner, P1, ManaType::Colorless, 4);
    add_typed_mana(&mut runner, P1, ManaType::Blue, 2);
    let turn_before = runner.state().turn_number;
    runner.cast(time_stop).resolve();
    assert_eq!(
        runner.state().phase,
        Phase::Cleanup,
        "Time Stop must jump out of combat to cleanup (reach-guard)"
    );
    assert!(
        runner.state().restrictions.is_empty(),
        "the EndOfCombat ban must expire via the combat teardown, got {:?}",
        runner.state().restrictions
    );
    // Complete cleanup into P0's turn (no triggers, no discards expected).
    for _ in 0..24 {
        if runner.state().turn_number != turn_before {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority while finishing cleanup, got {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        runner.state().turn_number,
        turn_before + 1,
        "cleanup must complete into the next turn"
    );

    // P0's turn plus P1's next turn, up to the fresh offer.
    drive_to_next_unless_payment(&mut runner);
    let (player, cost, _) = expect_unless_payment(runner.state());
    assert_eq!(player, P1, "the fresh offer goes to P1");
    // Fund generously: the exact X is hand-counting (V3 owns that claim);
    // paying is what matters here.
    let _ = cost;
    add_typed_mana(&mut runner, P1, ManaType::Colorless, 10);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay the fresh offer");
    resolve_until_settled(&mut runner);
    assert!(
        runner.state().restrictions.is_empty(),
        "paid: no restriction installed, got {:?}",
        runner.state().restrictions
    );

    // The later paid combat attacks freely: the old ban is gone.
    drive_to_phase(&mut runner, Phase::DeclareAttackers);
    runner
        .declare_attackers(&[(p1_bear, AttackTarget::Player(P0))])
        .expect("P1 attacks P0 in the later paid combat");
    drive_to_phase(&mut runner, Phase::DeclareBlockers);
    runner.declare_blockers(&[]).expect("P0 declines to block");
    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "unblocked 2/2 connects in the later paid combat"
    );
}
