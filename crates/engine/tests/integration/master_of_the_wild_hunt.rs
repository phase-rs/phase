//! Master of the Wild Hunt — runtime rows for the whole activated ability.
//!
//! CR 602.2b + CR 601.2c: the activation announces clause 2's target.
//! CR 701.26a + CR 608.2c: only permanents the instruction actually taps are
//!   "tapped this way".
//! CR 120.1: each such Wolf is the source of damage equal to its own power.
//! CR 608.2d + CR 608.2h: clause 3 is an untargeted division — the target's
//!   controller divides the target's power (fixed before the choice) among any
//!   number of the Wolves tapped this way as the ability resolves; each chosen
//!   Wolf receives at least 1, and the damage follows the choice with no
//!   priority window. CR 704.5g: lethal damage destroys afterwards (SBA);
//!   CR 117.3b: the active player then receives priority.
//! CR 608.2b: an ability whose only target is illegal does not resolve.

use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::EngineError;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{DistributionScope, DistributionUnit, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::rules::{drive_with_response, PriorityResponse};

/// Verbatim Oracle text (Scryfall / MTGJSON). Creature — Human Shaman, 3/3.
const MASTER: &str = "At the beginning of your upkeep, create a 2/2 green Wolf creature token.\n\
{T}: Tap all untapped Wolf creatures you control. Each Wolf tapped this way deals damage equal to \
its power to target creature. That creature deals damage equal to its power divided as its \
controller chooses among any number of those Wolves.";

fn wolf(s: &mut GameScenario, owner: PlayerId, name: &str, power: i32, toughness: i32) -> ObjectId {
    s.add_creature(owner, name, power, toughness)
        .with_subtypes(vec!["Wolf"])
        .id()
}

fn master(s: &mut GameScenario) -> ObjectId {
    s.add_creature_from_oracle(P0, "Master of the Wild Hunt", 3, 3, MASTER)
        .with_subtypes(vec!["Human", "Shaman"])
        .id()
}

/// Master's only activated ability ({T}: …) is ability index 0; the upkeep
/// line is a trigger, not an activated ability.
fn activate_master(master: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: master,
        ability_index: 0,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    }
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Activate Master targeting `target`, then pass priority until either clause
/// 3's division prompt appears (returned with every event so far) or the stack
/// empties with no prompt (`None`). Any other wait is a harness failure.
fn activate_until_division(
    runner: &mut GameRunner,
    master: ObjectId,
    target: ObjectId,
) -> (Option<WaitingFor>, Vec<GameEvent>) {
    let mut events = Vec::new();
    events.extend(
        runner
            .act(activate_master(master))
            .expect("activate Master of the Wild Hunt")
            .events,
    );
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("choose clause 2's target")
            .events,
    );
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return (None, events);
            }
            WaitingFor::Priority { .. } => events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority")
                    .events,
            ),
            WaitingFor::DistributeAmong { .. } => {
                return (Some(runner.state().waiting_for.clone()), events);
            }
            other => panic!("unexpected wait while resolving Master: {other:?}"),
        }
    }
    panic!("Master's ability never finished resolving");
}

/// Unpack a CR 608.2d resolution-time division prompt.
fn division(wait: Option<WaitingFor>) -> (PlayerId, u32, Vec<TargetRef>) {
    match wait {
        Some(WaitingFor::DistributeAmong {
            player,
            total,
            targets,
            unit,
            scope,
        }) => {
            assert_eq!(unit, DistributionUnit::Damage);
            assert!(
                matches!(scope, DistributionScope::ResolutionCandidates { .. }),
                "clause 3 is a resolution-time division, got {scope:?}"
            );
            (player, total, targets)
        }
        other => panic!("expected clause 3's division prompt, got {other:?}"),
    }
}

fn divide(runner: &mut GameRunner, portions: &[(ObjectId, u32)]) -> Vec<GameEvent> {
    runner
        .act(GameAction::DistributeAmong {
            distribution: portions
                .iter()
                .map(|(id, amount)| (TargetRef::Object(*id), *amount))
                .collect(),
        })
        .expect("a legal division is accepted")
        .events
}

fn as_set(targets: &[TargetRef]) -> std::collections::BTreeSet<ObjectId> {
    targets
        .iter()
        .map(|t| match t {
            TargetRef::Object(id) => *id,
            TargetRef::Player(p) => panic!("a Wolf candidate is an object, got player {p:?}"),
        })
        .collect()
}

fn ids(list: &[ObjectId]) -> std::collections::BTreeSet<ObjectId> {
    list.iter().copied().collect()
}

/// Master, P0 Wolves 1/1 and 4/4, a pre-tapped P0 Wolf 2/2, and P1's 6/6.
struct Board {
    runner: GameRunner,
    master: ObjectId,
    w1: ObjectId,
    w4: ObjectId,
    pre_tapped: ObjectId,
    giant: ObjectId,
}

fn board() -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let w1 = wolf(&mut s, P0, "Wolf One", 1, 1);
    let w4 = wolf(&mut s, P0, "Wolf Four", 4, 4);
    let pre_tapped = wolf(&mut s, P0, "Tired Wolf", 2, 2);
    let giant = s.add_creature(P1, "Target Giant", 6, 6).id();
    let mut runner = s.build();
    runner
        .state_mut()
        .objects
        .get_mut(&pre_tapped)
        .unwrap()
        .tapped = true;
    Board {
        runner,
        master,
        w1,
        w4,
        pre_tapped,
        giant,
    }
}

/// CR 602.2b + CR 601.2c: activating Master announces clause 2's "target
/// creature" even though the root tap declares no target (the reported bug).
#[test]
fn activation_prompts_for_target_creature() {
    let Board {
        mut runner,
        master,
        giant,
        ..
    } = board();
    runner
        .act(activate_master(master))
        .expect("activate Master of the Wild Hunt");

    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1, "exactly clause 2's target slot");
            assert!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(giant)),
                "the opponent's creature is a legal target"
            );
        }
        other => panic!("activation must prompt for a target creature, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(giant)),
        })
        .expect("choose the target creature");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the ability is on the stack after its target is chosen"
    );
}

/// CR 701.26a + CR 120.1 + CR 608.2d: each Wolf the instruction taps deals its
/// own power; the pre-tapped Wolf and Master are not sources. At the division
/// prompt no clause-3 damage has been dealt yet, and the amount is the target's
/// power (6), not any Wolf's (1, 4), the clause-2 total (5), or Master's (3).
#[test]
fn each_wolf_tapped_this_way_deals_its_power_to_target() {
    let Board {
        mut runner,
        master,
        w1,
        w4,
        pre_tapped,
        giant,
    } = board();
    let (wait, _) = activate_until_division(&mut runner, master, giant);

    assert!(tapped(&runner, master), "Master tapped for its cost");
    assert!(tapped(&runner, w1), "untapped Wolf 1/1 becomes tapped");
    assert!(tapped(&runner, w4), "untapped Wolf 4/4 becomes tapped");
    assert!(tapped(&runner, pre_tapped), "pre-tapped Wolf stays tapped");
    assert_eq!(
        damage(&runner, giant),
        5,
        "only the Wolves tapped this way deal their own power (1 + 4); the pre-tapped \
         Wolf (7) and Master (3) are not sources"
    );
    for id in [master, w1, w4, pre_tapped] {
        assert_eq!(
            damage(&runner, id),
            0,
            "no clause-3 damage before the choice"
        );
    }
    let (_, total, targets) = division(wait);
    assert_eq!(total, 6, "the target's power");
    assert_eq!(as_set(&targets), ids(&[w1, w4]));

    divide(&mut runner, &[(w1, 1), (w4, 5)]);

    assert_eq!(zone(&runner, w1), Zone::Graveyard);
    assert_eq!(zone(&runner, w4), Zone::Graveyard);
    for id in [master, pre_tapped] {
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        assert_eq!(damage(&runner, id), 0);
    }
    assert!(runner.state().stack.is_empty());
}

/// CR 701.26a + CR 608.2d (6c): a Wolf that can't become tapped and an
/// opponent's Wolf are not tapped by the instruction, so neither deals clause-2
/// damage nor is a division candidate.
#[test]
fn wolf_that_cannot_tap_and_opponents_wolf_deal_no_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let tiny = wolf(&mut s, P0, "Tiny Wolf", 1, 1);
    let small = wolf(&mut s, P0, "Small Wolf", 2, 2);
    let held = s
        .add_creature(P0, "Held Wolf", 8, 8)
        .with_subtypes(vec!["Wolf"])
        .with_static(StaticMode::CantTap)
        .id();
    let theirs = wolf(&mut s, P1, "Their Wolf", 4, 4);
    let giant = s.add_creature(P1, "Target Giant", 6, 6).id();
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, giant);

    assert!(tapped(&runner, tiny) && tapped(&runner, small));
    assert!(
        !tapped(&runner, held),
        "reach guard: the can't-tap restriction kept the 8/8 Wolf upright"
    );
    assert!(
        !tapped(&runner, theirs),
        "an opponent's Wolf is not a Wolf you control"
    );
    assert_eq!(
        damage(&runner, giant),
        3,
        "only the Wolves tapped this way deal damage (1 + 2; not with 8 or 4)"
    );
    let (_, total, targets) = division(wait);
    assert_eq!(total, 6);
    assert_eq!(as_set(&targets), ids(&[tiny, small]));

    divide(&mut runner, &[(tiny, 1), (small, 5)]);
    assert_eq!(damage(&runner, held), 0);
    assert_eq!(damage(&runner, theirs), 0);
    assert_eq!(zone(&runner, tiny), Zone::Graveyard);
    assert_eq!(zone(&runner, small), Zone::Graveyard);
}

/// No Wolves you control (6f): Master taps, nothing is tapped this way, the
/// target — itself an opponent's Wolf — takes no damage, and no division is
/// offered.
#[test]
fn no_wolves_taps_master_and_deals_no_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let target_wolf = wolf(&mut s, P1, "Target Wolf", 6, 6);
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, target_wolf);
    assert!(
        wait.is_none(),
        "no DistributeAmong without Wolves tapped this way"
    );

    assert!(tapped(&runner, master), "Master tapped for its cost");
    assert!(
        !tapped(&runner, target_wolf),
        "the opponent's Wolf is not tapped"
    );
    assert_eq!(
        damage(&runner, target_wolf),
        0,
        "a declared target is never a member of the mass tap's set"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 608.2c: the "tapped this way" set belongs to one resolution. The first
/// activation's single tapped Wolf is the only candidate, so the whole amount
/// is applied with no prompt (V5b production row). A second activation that
/// taps nothing must not reuse the first activation's set — for clause 2 or
/// for clause 3's candidates.
#[test]
fn second_activation_does_not_reuse_prior_tapped_set() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let w4 = wolf(&mut s, P0, "Wolf Four", 4, 8);
    let giant = s.add_creature(P1, "Target Giant", 6, 6).id();
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, giant);
    assert!(
        wait.is_none(),
        "a single candidate takes the whole amount unprompted"
    );
    assert_eq!(
        damage(&runner, giant),
        4,
        "first activation: the 4/8 deals 4"
    );
    assert_eq!(
        damage(&runner, w4),
        6,
        "the target's power (6) lands on the lone Wolf"
    );
    assert!(tapped(&runner, w4));

    runner.state_mut().objects.get_mut(&master).unwrap().tapped = false;
    let (wait, _) = activate_until_division(&mut runner, master, giant);

    assert!(
        wait.is_none(),
        "no Wolf was tapped this way the second time"
    );
    assert!(
        tapped(&runner, master),
        "reach guard: the second activation paid its cost"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the second activation resolved"
    );
    assert_eq!(
        damage(&runner, giant),
        4,
        "the already-tapped Wolf is not tapped this way by the second activation"
    );
    assert_eq!(
        damage(&runner, w4),
        6,
        "the prior activation's set is not reused as clause 3's candidates"
    );
}

/// CR 608.2b + ruling (2018-03-16): if the target is illegal on resolution,
/// the ability doesn't resolve — no Wolf becomes tapped and no damage is dealt.
#[test]
fn illegal_target_prevents_wolf_tap_and_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let w1 = wolf(&mut s, P0, "Wolf One", 1, 1);
    let w4 = wolf(&mut s, P0, "Wolf Four", 4, 4);
    let giant = s.add_creature(P1, "Target Giant", 6, 6).id();
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let mut runner = s.build();

    drive_with_response(
        &mut runner,
        activate_master(master),
        &[giant],
        Some(PriorityResponse {
            player: P1,
            instant: murder,
            target: giant,
        }),
    );

    assert_eq!(
        runner.state().objects[&giant].zone,
        Zone::Graveyard,
        "reach guard: the response destroyed the target"
    );
    assert!(tapped(&runner, master), "Master's {{T}} cost was paid");
    for w in [w1, w4] {
        assert!(
            !tapped(&runner, w),
            "no Wolf is tapped when the ability fizzles"
        );
        assert_eq!(damage(&runner, w), 0);
    }
    assert!(runner.state().stack.is_empty());
}

/// CR 608.2d + CR 608.2h + CR 704.5g + CR 117.3b (6a): the target's controller
/// divides the target's power (5 — not a Wolf's 2) among exactly the Wolves
/// tapped this way; after the damage and SBA the chosen lethal Wolves and the
/// target die, the stack is empty, and the active player has priority.
#[test]
fn target_controller_divides_target_power_among_wolves_tapped_this_way() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 2, 2);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let c = wolf(&mut s, P0, "Wolf C", 2, 2);
    let pre_tapped = wolf(&mut s, P0, "Tired Wolf", 2, 2);
    let brute = s.add_creature(P1, "Target Brute", 5, 5).id();
    let mut runner = s.build();
    runner
        .state_mut()
        .objects
        .get_mut(&pre_tapped)
        .unwrap()
        .tapped = true;

    let (wait, _) = activate_until_division(&mut runner, master, brute);
    let (player, total, targets) = division(wait);
    assert_eq!(player, P1, "the target's controller divides");
    assert_eq!(total, 5, "the target's power");
    assert_eq!(as_set(&targets), ids(&[a, b, c]));
    assert_eq!(damage(&runner, brute), 6, "clause 2: 2 + 2 + 2");
    for id in [a, b, c, pre_tapped] {
        assert_eq!(
            damage(&runner, id),
            0,
            "no clause-3 damage before the choice"
        );
    }

    divide(&mut runner, &[(a, 2), (b, 2), (c, 1)]);

    assert_eq!(zone(&runner, a), Zone::Graveyard);
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert_eq!(zone(&runner, c), Zone::Battlefield);
    assert_eq!(damage(&runner, c), 1);
    assert_eq!(zone(&runner, brute), Zone::Graveyard);
    for id in [pre_tapped, master] {
        assert_eq!(zone(&runner, id), Zone::Battlefield);
        assert_eq!(damage(&runner, id), 0);
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: the active player receives priority after resolution"
    );
}

/// 6d + V5 subset row: an opponent's Wolf chosen as the target is not a Wolf
/// tapped this way, so it is not a candidate; the chooser may divide among a
/// strict subset of the candidates.
#[test]
fn opponents_wolf_target_is_not_a_candidate() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let one = wolf(&mut s, P0, "Wolf One", 1, 1);
    let three = wolf(&mut s, P0, "Wolf Three", 3, 3);
    let their_wolf = wolf(&mut s, P1, "Their Wolf", 4, 4);
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, their_wolf);
    let (player, total, targets) = division(wait);
    assert_eq!(player, P1);
    assert_eq!(total, 4);
    assert_eq!(as_set(&targets), ids(&[one, three]));

    divide(&mut runner, &[(three, 4)]);
    assert_eq!(zone(&runner, three), Zone::Graveyard);
    assert_eq!(damage(&runner, one), 0);
}

/// 6e: on a self-target the target's controller is the activator.
#[test]
fn self_target_makes_the_activator_divide() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 2, 2);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let own = s.add_creature(P0, "Own Bear", 3, 3).id();
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, own);
    let (player, total, _) = division(wait);
    assert_eq!(player, P0);
    assert_eq!(total, 3);

    divide(&mut runner, &[(a, 1), (b, 2)]);
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert_eq!(damage(&runner, a), 1);
}

/// 6g (D1 hostile): exactly one Wolf, a 0/1, is tapped this way against a 6/6.
/// "its power" is the TARGET's (CR 608.2h), so the lone Wolf is dealt 6 by the
/// single-candidate auto-apply and dies. An effect-context (`Anaphoric`) binding
/// would read the lone tapped Wolf's power (0) instead — provided clause 2 dealt
/// the target nothing, which assertion 2 measures on the production event log.
#[test]
fn its_power_is_the_targets_even_when_a_lone_wolf_deals_no_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let pup = wolf(&mut s, P0, "Pup", 0, 1);
    let giant = s.add_creature(P1, "Target Giant", 6, 6).id();
    let mut runner = s.build();

    let outcome = runner.activate(master, 0).target_object(giant).resolve();
    let events = outcome.events();

    // 1. Reach guard: node 1 ran inside this log.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentTapped { object_id, .. } if *object_id == pup
        )),
        "reach guard: the lone Wolf was tapped this way"
    );
    // 2. Premise (charter V6g): clause 2 dealt the target nothing.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { target: TargetRef::Object(t), .. } if *t == giant
        )),
        "V6g premise: no DamageDealt targets the target creature"
    );
    // 3. Discriminating outcome: the target's power (6) lands on the lone Wolf.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { source_id, target: TargetRef::Object(t), amount: 6, .. }
                if *source_id == giant && *t == pup
        )),
        "the target deals its own power (6) to the lone Wolf"
    );
    assert_eq!(outcome.state().objects[&giant].damage_marked, 0);
    assert_eq!(outcome.zone_of(pup), Zone::Graveyard);
}

/// V5c production row: a 0-power target has nothing to divide — no prompt, no
/// clause-3 damage, and the division node still reports resolution.
#[test]
fn zero_power_target_divides_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 2, 2);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let wall = s.add_creature(P1, "Zero Wall", 0, 4).id();
    let mut runner = s.build();

    let (wait, events) = activate_until_division(&mut runner, master, wall);
    assert!(wait.is_none(), "nothing to divide, no prompt");
    assert_eq!(damage(&runner, a), 0);
    assert_eq!(damage(&runner, b), 0);
    assert!(runner.state().stack.is_empty());
    let clause_two_damage: u32 = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                target: TargetRef::Object(t),
                amount,
                ..
            } if *t == wall => Some(*amount),
            _ => None,
        })
        .sum();
    assert_eq!(
        clause_two_damage, 4,
        "reach guard: the two Wolves dealt the wall 2 + 2, so the chain reached node 3"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::DealDamage,
                ..
            }
        )),
        "the division node resolves"
    );
}

/// V7 multiplayer routing: in a 3-player game the division routes to the
/// target's controller (P2), not the activator. Other seats are rejected, a
/// resolving ability's division cannot be cancelled, and every illegal division
/// is rejected with the prompt left intact.
#[test]
fn division_routes_to_the_targets_controller_in_multiplayer() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 11);
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 1, 1);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let target = s.add_creature(p2, "P2 Target", 4, 4).id();
    let _bystander = s.add_creature(P1, "P1 Bystander", 2, 2).id();
    let mut runner = s.build();

    let (wait, _) = activate_until_division(&mut runner, master, target);
    let prompt = wait.clone().expect("division prompt");
    let (player, total, _) = division(wait);
    assert_eq!(player, p2);
    assert_eq!(total, 4);
    assert_eq!(runner.state().waiting_for.acting_player(), Some(p2));

    let action = |portions: &[(ObjectId, u32)]| GameAction::DistributeAmong {
        distribution: portions
            .iter()
            .map(|(id, amount)| (TargetRef::Object(*id), *amount))
            .collect(),
    };
    let valid = action(&[(a, 1), (b, 3)]);
    for seat in [P0, P1] {
        assert!(
            matches!(
                engine::game::apply(runner.state_mut(), seat, valid.clone()),
                Err(EngineError::WrongPlayer)
            ),
            "{seat:?} may not answer P2's division"
        );
        assert_eq!(runner.state().waiting_for, prompt);
    }
    assert!(
        engine::game::apply(runner.state_mut(), p2, GameAction::CancelCast).is_err(),
        "a resolving ability's division cannot be cancelled"
    );
    assert_eq!(runner.state().waiting_for, prompt);
    for illegal in [
        action(&[(a, 1), (b, 2)]),
        action(&[(a, 0), (b, 4)]),
        action(&[(target, 4)]),
        action(&[(a, 2), (a, 2)]),
        action(&[]),
    ] {
        assert!(
            engine::game::apply(runner.state_mut(), p2, illegal.clone()).is_err(),
            "{illegal:?} must be rejected"
        );
        assert_eq!(
            runner.state().waiting_for,
            prompt,
            "prompt intact after {illegal:?}"
        );
    }

    engine::game::apply(runner.state_mut(), p2, valid).expect("P2's legal division");
    assert_eq!(zone(&runner, a), Zone::Graveyard);
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 601.2d + CR 608.2d at the raw reducer boundary: a submitted budget that
/// overflows `u32` is refused rather than wrapping to the required total.
#[test]
fn raw_division_with_overflowing_budget_is_refused() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 2, 2);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let brute = s.add_creature(P1, "Target Brute", 1, 1).id();
    let mut runner = s.build();
    let (wait, _) = activate_until_division(&mut runner, master, brute);
    let (_, total, _) = division(wait);
    assert_eq!(total, 1);

    let result = runner.act(GameAction::DistributeAmong {
        distribution: vec![(TargetRef::Object(a), u32::MAX), (TargetRef::Object(b), 2)],
    });
    assert!(result.is_err(), "u32::MAX + 2 must not wrap to 1");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DistributeAmong { .. }
    ));
}

/// One offered resolution-time division, as the AI candidate generator emits it.
type Division = Vec<(TargetRef, u32)>;

/// Every `DistributeAmong` answer the production AI candidate generator
/// (`ai_support::candidate_actions`) offers at the current prompt.
fn offered_divisions(runner: &GameRunner) -> Vec<Division> {
    candidate_actions(runner.state())
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::DistributeAmong { distribution } => Some(distribution),
            _ => None,
        })
        .collect()
}

/// Apply each offered division to a fresh copy of the paused game and count how
/// many of `wolves` it destroys once the ability finishes resolving (CR 704.3:
/// SBAs run after resolution, not mid-resolution).
fn destroyed_wolves_by_division(
    runner: &GameRunner,
    wolves: &[ObjectId],
) -> Vec<(Division, usize)> {
    offered_divisions(runner)
        .into_iter()
        .map(|division| {
            let mut copy = GameRunner::from_state(runner.state().clone());
            copy.act(GameAction::DistributeAmong {
                distribution: division.clone(),
            })
            .expect("every offered division is legal");
            let destroyed = wolves
                .iter()
                .filter(|id| zone(&copy, **id) == Zone::Graveyard)
                .count();
            (division, destroyed)
        })
        .collect()
}

/// Master, two plain 3/3 Wolves (P0), and P1's Vampire Nighthawk (2/3 flying,
/// lifelink, and deathtouch when `deathtouch`) as clause 2's target, paused at
/// clause 3's division prompt: P1 divides the Nighthawk's 2 power.
struct NighthawkPrompt {
    runner: GameRunner,
    w1: ObjectId,
    w2: ObjectId,
    nighthawk: ObjectId,
}

fn nighthawk_division_prompt(deathtouch: bool) -> NighthawkPrompt {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let w1 = wolf(&mut s, P0, "Wolf One", 3, 3);
    let w2 = wolf(&mut s, P0, "Wolf Two", 3, 3);
    let nighthawk = {
        let mut builder = s.add_creature(P1, "Vampire Nighthawk", 2, 3);
        builder.flying().lifelink();
        if deathtouch {
            builder.deathtouch();
        }
        builder.id()
    };
    let mut runner = s.build();
    let (wait, _) = activate_until_division(&mut runner, master, nighthawk);
    let (player, total, targets) = division(wait);
    assert_eq!(
        player, P1,
        "reach guard: the Nighthawk's controller divides"
    );
    assert_eq!(total, 2, "reach guard: the Nighthawk's power");
    assert_eq!(as_set(&targets), ids(&[w1, w2]));
    NighthawkPrompt {
        runner,
        w1,
        w2,
        nighthawk,
    }
}

/// CR 608.2d + CR 702.2b + CR 704.5h: when the creature dividing its damage has
/// deathtouch, one point destroys a Wolf, so the production candidate set must
/// offer the 1 + 1 spread that destroys BOTH Wolves. The paused node still
/// holds the target creature (the damage source) in its targets, unbound.
#[test]
fn deathtouch_divider_is_offered_a_division_that_destroys_both_wolves() {
    let NighthawkPrompt {
        runner,
        w1,
        w2,
        nighthawk,
    } = nighthawk_division_prompt(true);

    // U5: the paused node's damage source is the target, read through the
    // unbound (historical lookup) arm.
    let WaitingFor::DistributeAmong {
        scope: DistributionScope::ResolutionCandidates { pending_effect },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected a resolution-time division prompt");
    };
    assert_eq!(
        pending_effect.targets,
        vec![TargetRef::Object(nighthawk)],
        "the paused node keeps the target creature, its damage source"
    );
    assert_eq!(pending_effect.context.target_damage_source, None);

    let outcomes = destroyed_wolves_by_division(&runner, &[w1, w2]);
    assert!(
        outcomes.iter().any(|(_, destroyed)| *destroyed == 2),
        "a deathtouch divider must be offered a division destroying both Wolves, got {outcomes:?}"
    );
}

/// CR 704.5g control: without deathtouch, 2 damage cannot destroy a 3/3, so no
/// offered division destroys a Wolf, while every offered division is applied
/// (2 damage marked in total) and each Wolf has an all-on-one division.
#[test]
fn non_deathtouch_divider_destroys_no_wolf() {
    let NighthawkPrompt { runner, w1, w2, .. } = nighthawk_division_prompt(false);

    let offered = offered_divisions(&runner);
    for wolf in [w1, w2] {
        assert!(
            offered.contains(&vec![(TargetRef::Object(wolf), 2)]),
            "reach guard: all of the 2 on {wolf:?} is offered, got {offered:?}"
        );
    }
    for division in &offered {
        let mut copy = GameRunner::from_state(runner.state().clone());
        copy.act(GameAction::DistributeAmong {
            distribution: division.clone(),
        })
        .expect("every offered division is legal");
        assert_eq!(
            damage(&copy, w1) + damage(&copy, w2),
            2,
            "reach guard: {division:?} marks the whole 2 on the Wolves"
        );
        for wolf in [w1, w2] {
            assert_eq!(
                zone(&copy, wolf),
                Zone::Battlefield,
                "{division:?} must not destroy {wolf:?} without deathtouch"
            );
        }
    }
}

/// CR 702.2b + CR 704.5h: the generator's deathtouch fact matches the
/// resolver's. The destroy-both division is taken from the GENERATED set at the
/// deathtouch prompt and applied: both Wolves die, and the lifelink source's
/// controller gains the whole 2.
#[test]
fn generated_deathtouch_division_destroys_both_wolves_when_applied() {
    let NighthawkPrompt {
        mut runner, w1, w2, ..
    } = nighthawk_division_prompt(true);
    let (destroy_both, _) = destroyed_wolves_by_division(&runner, &[w1, w2])
        .into_iter()
        .find(|(_, destroyed)| *destroyed == 2)
        .expect("the generated set holds a division destroying both Wolves");
    let life_before = runner.life(P1);

    runner
        .act(GameAction::DistributeAmong {
            distribution: destroy_both,
        })
        .expect("the generated division is legal");

    assert_eq!(zone(&runner, w1), Zone::Graveyard);
    assert_eq!(zone(&runner, w2), Zone::Graveyard);
    assert_eq!(
        runner.life(P1),
        life_before + 2,
        "reach guard: the lifelink Nighthawk dealt the whole 2"
    );
}

/// CR 608.2d + CR 704.5g: dividing among your own Wolves, the generator offers a
/// survivable fill — each Wolf receives at most one less than its lethal
/// damage, in descending capacity — so applying it destroys neither Wolf.
#[test]
fn own_target_division_offers_a_survivable_fill() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 5, 5);
    let b = wolf(&mut s, P0, "Wolf B", 5, 5);
    let own = s.add_creature(P0, "Own Giant", 6, 12).id();
    let mut runner = s.build();
    let (wait, _) = activate_until_division(&mut runner, master, own);
    let (player, total, _) = division(wait);
    assert_eq!(player, P0, "reach guard: the activator divides");
    assert_eq!(total, 6);

    let fill = vec![(TargetRef::Object(a), 4), (TargetRef::Object(b), 2)];
    let offered = offered_divisions(&runner);
    assert!(
        offered.iter().any(|division| {
            division.len() == fill.len() && fill.iter().all(|entry| division.contains(entry))
        }),
        "the survivable 4 + 2 fill is offered, got {offered:?}"
    );

    runner
        .act(GameAction::DistributeAmong { distribution: fill })
        .expect("the survivable fill is legal");
    assert_eq!(zone(&runner, a), Zone::Battlefield);
    assert_eq!(zone(&runner, b), Zone::Battlefield);
    assert_eq!(damage(&runner, a) + damage(&runner, b), 6);
}

/// CR 601.2d: an announce-time division must give every announced target at
/// least one; the raw reducer refuses a split that leaves one out, while the
/// resolution-time scope (CR 608.2d) accepts the same subset.
#[test]
fn raw_announced_division_must_cover_every_target_but_resolution_may_not() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let master = master(&mut s);
    let a = wolf(&mut s, P0, "Wolf A", 2, 2);
    let b = wolf(&mut s, P0, "Wolf B", 2, 2);
    let brute = s.add_creature(P1, "Target Brute", 3, 3).id();
    let mut runner = s.build();
    let (wait, _) = activate_until_division(&mut runner, master, brute);
    let (_, total, targets) = division(wait);
    assert_eq!(total, 3);

    // Same prompt re-scoped to announce time.
    let resolution_wait = runner.state().waiting_for.clone();
    let WaitingFor::DistributeAmong { player, unit, .. } = resolution_wait.clone() else {
        panic!("expected a division prompt");
    };
    runner.state_mut().waiting_for = WaitingFor::DistributeAmong {
        player,
        total,
        targets: targets.clone(),
        unit,
        scope: DistributionScope::AnnouncedTargets,
    };
    let partial = GameAction::DistributeAmong {
        distribution: vec![(TargetRef::Object(a), 3)],
    };
    let announced_wait = runner.state().waiting_for.clone();
    match runner.act(partial.clone()) {
        Err(EngineError::InvalidAction(message)) => assert!(
            message.contains("Each announced target must receive at least 1"),
            "announced target {b:?} would receive nothing — rejected for coverage, got {message:?}"
        ),
        other => panic!("announced target {b:?} would receive nothing, got {other:?}"),
    }
    assert_eq!(
        runner.state().waiting_for,
        announced_wait,
        "the rejected division leaves the prompt intact"
    );

    // Restore the resolution-time prompt: the subset is a legal division.
    runner.state_mut().waiting_for = resolution_wait;
    runner
        .act(partial)
        .expect("a resolution-time subset is legal");
}
