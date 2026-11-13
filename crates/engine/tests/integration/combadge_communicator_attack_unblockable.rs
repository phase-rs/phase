//! Combadge — a granted attack trigger gated on another Communicator-equipped
//! creature, plus the cast-trigger self-copy.
//!
//! Oracle text:
//! "When you cast this spell, copy it. (The copy becomes a token.)
//! Equipped creature has "Whenever this creature attacks while you control
//! another creature with a Communicator attached to it, this creature can't be
//! blocked this turn."
//! Equip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)"
//!
//! CR 508.1m + CR 603.2: the "while" gate is read when attackers are declared.
//! CR 301.5a: the trigger is granted to the equipped creature, so "another
//! creature" excludes that creature. CR 707.10 + CR 111.1: the cast-trigger
//! copy of a permanent spell resolves into a token.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COMBADGE_ORACLE: &str = "When you cast this spell, copy it. (The copy becomes a token.)\nEquipped creature has \"Whenever this creature attacks while you control another creature with a Communicator attached to it, this creature can't be blocked this turn.\"\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";

fn add_combadge(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Combadge", COMBADGE_ORACLE)
        .with_subtypes(vec!["Communicator", "Equipment"])
        .id()
}

fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
    state.layers_dirty.mark_full();
}

struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    blocker: ObjectId,
}

/// What (if anything) P0's second creature wears.
enum AllyGear {
    Combadge,
    PlainEquipment,
    /// A second Combadge on the attacker itself (the board a cast-trigger copy
    /// makes likely); the attacker is not "another creature".
    SecondBadgeOnAttacker,
    Nothing,
}

/// P0: an attacker wearing a Combadge and an ally wearing `ally_gear`;
/// P1: one untapped blocker.
fn board(ally_gear: AllyGear) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Craw Wurm", 6, 4).id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let badge = add_combadge(&mut scenario);
    let ally_equipment = match ally_gear {
        AllyGear::Combadge => Some(add_combadge(&mut scenario)),
        AllyGear::PlainEquipment => Some(
            scenario
                .add_artifact_from_oracle(P0, "Bone Saw", "Equipped creature gets +1/+0.")
                .with_subtypes(vec!["Equipment"])
                .id(),
        ),
        AllyGear::SecondBadgeOnAttacker | AllyGear::Nothing => None,
    };
    let second_badge =
        matches!(ally_gear, AllyGear::SecondBadgeOnAttacker).then(|| add_combadge(&mut scenario));
    scenario.with_library_top(P0, &["Plains"; 5]);
    scenario.with_library_top(P1, &["Plains"; 5]);
    let mut runner = scenario.build();
    attach(&mut runner, badge, attacker);
    if let Some(equipment) = ally_equipment {
        attach(&mut runner, equipment, ally);
    }
    if let Some(equipment) = second_badge {
        attach(&mut runner, equipment, attacker);
    }
    Board {
        runner,
        attacker,
        blocker,
    }
}

/// Declare `attacker` and resolve any attack trigger, stopping at P1's blocker
/// declaration or, when P1 has no legal block, at the declare-blockers step's
/// priority window. Returns whether an attack trigger went on the stack.
fn attack_to_blocks(board: &mut Board) -> bool {
    let runner = &mut board.runner;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(board.attacker, AttackTarget::Player(P1))])
        .expect("attack with the Combadge wearer");
    let triggered = !runner.state().stack.is_empty();
    for _ in 0..16 {
        let state = runner.state();
        if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
            || (state.phase == Phase::DeclareBlockers && state.stack.is_empty())
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    triggered
}

#[test]
fn another_communicator_equipped_creature_makes_the_attacker_unblockable() {
    let mut board = board(AllyGear::Combadge);
    assert!(
        attack_to_blocks(&mut board),
        "CR 508.1m: the granted trigger fires with another Communicator-equipped creature"
    );
    // CR 509.1b: the attacker can't be blocked this turn — P1's untapped Hill
    // Giant has no legal block, so either no block prompt is offered or the
    // block is rejected.
    if matches!(
        board.runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        assert!(
            board
                .runner
                .declare_blockers(&[(board.blocker, board.attacker)])
                .is_err(),
            "the attacker must be unblockable, but the block was accepted"
        );
    } else {
        assert_eq!(board.runner.state().phase, Phase::DeclareBlockers);
    }
    let outcome = board.runner.combat_damage();
    outcome.assert_life_delta(P1, -6);
}

#[test]
fn no_other_communicator_leaves_the_attacker_blockable() {
    let mut board = board(AllyGear::Nothing);
    assert!(
        !attack_to_blocks(&mut board),
        "a lone Combadge wearer must not trigger (it is not \"another creature\")"
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    board
        .runner
        .declare_blockers(&[(board.blocker, board.attacker)])
        .expect("the attacker stays blockable");
    assert!(board.runner.state().stack.is_empty());
}

/// CR 301.5a + CR 603.2: both Combadges grant the trigger to the same creature,
/// but "another creature with a Communicator attached to it" excludes the
/// attacker itself, so two badges on one creature never satisfy the gate.
#[test]
fn two_combadges_on_the_attacker_do_not_make_it_unblockable() {
    let mut board = board(AllyGear::SecondBadgeOnAttacker);
    let state = board.runner.state();
    let badges = state.objects[&board.attacker]
        .attachments
        .iter()
        .filter(|id| state.objects[id].name == "Combadge")
        .count();
    assert_eq!(badges, 2, "precondition: the attacker wears both Combadges");
    assert!(
        !attack_to_blocks(&mut board),
        "the attacker wearing both Combadges is not \"another creature\""
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    board
        .runner
        .declare_blockers(&[(board.blocker, board.attacker)])
        .expect("the attacker stays blockable");
}

#[test]
fn a_non_communicator_equipment_on_the_ally_does_not_count() {
    let mut board = board(AllyGear::PlainEquipment);
    assert!(
        !attack_to_blocks(&mut board),
        "an Equipment that is not a Communicator must not satisfy the gate"
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    board
        .runner
        .declare_blockers(&[(board.blocker, board.attacker)])
        .expect("the attacker stays blockable");
}

#[test]
fn casting_combadge_copies_it_into_a_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let badge = scenario
        .add_artifact_to_hand_from_oracle(P0, "Combadge", COMBADGE_ORACLE)
        .with_subtypes(vec!["Communicator", "Equipment"])
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Plains"; 5]);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&badge].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: badge,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Combadge");
    runner.advance_until_stack_empty();

    let state = runner.state();
    let combadges: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == "Combadge")
        .collect();
    // CR 707.10 + CR 111.1: the original and its token copy both resolve.
    assert_eq!(
        combadges.len(),
        2,
        "original + token copy on the battlefield"
    );
    assert_eq!(state.objects[&badge].zone, Zone::Battlefield);
    assert_eq!(
        combadges
            .iter()
            .filter(|id| state.objects[*id].is_token)
            .count(),
        1,
        "exactly one Combadge is a token"
    );
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}
