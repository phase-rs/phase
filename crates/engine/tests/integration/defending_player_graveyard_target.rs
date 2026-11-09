//! CR 508.5 + CR 508.5a: "target card from defending player's graveyard" names
//! the graveyard of the ONE player the triggering creature is attacking, not
//! any player's graveyard.
//!
//! Every row is a real card's verbatim Oracle text, attacked through the real
//! pipeline in a three-player game so a non-defending opponent's graveyard and
//! the attacker's own graveyard are both live wrong answers. Each graveyard
//! holds two cards so the trigger always prompts (a single legal card would be
//! auto-chosen), and the assertion reads the exact legal-target set the engine
//! offers at that prompt (CR 603.3d). A parser that drops the owner binding
//! offers every graveyard's cards and fails.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const GRAVEN_ABOMINATION: &str =
    "Whenever this creature attacks, exile target card from defending player's graveyard.";
const RYSORIAN_BADGER: &str = "Whenever this creature attacks and isn't blocked, you may exile up to two target creature cards from defending player's graveyard. If you do, you gain 1 life for each card exiled this way and this creature assigns no combat damage this turn.";

/// Two creature cards in each player's graveyard, as `(owner, card)`.
fn scenario(attacker_oracle: &str) -> (GameScenario, ObjectId, Vec<(PlayerId, ObjectId)>) {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for _ in 0..10 {
            sc.add_card_to_library_top(player, "Plains");
        }
    }
    let attacker = sc
        .add_creature_from_oracle(P0, "Attacker", 2, 2, attacker_oracle)
        .id();
    // Idle defenders so each defending player gets a declare-blockers prompt.
    sc.add_creature(P1, "P1 Bystander", 1, 1);
    sc.add_creature(P2, "P2 Bystander", 1, 1);
    let mut graveyards = Vec::new();
    for player in [P0, P1, P2] {
        for n in 0..2 {
            let id = sc
                .add_creature_to_graveyard(player, &format!("Corpse {n} of {player:?}"), 1, 1)
                .id();
            graveyards.push((player, id));
        }
    }
    (sc, attacker, graveyards)
}

/// Declare `attacker` at `defender`, decline all blocks, and return the legal
/// targets of the first trigger target prompt.
fn legal_targets_at_trigger_prompt(
    r: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
) -> Vec<TargetRef> {
    r.advance_to_combat();
    r.declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attack is legal");
    for _ in 0..32 {
        match r.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => return target_slots[selection.current_slot].legal_targets.clone(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("ordering is legal");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declaring no blocks is legal");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected prompt before the trigger target: {other:?}"),
        }
    }
    panic!(
        "the trigger never asked for a target: {:?}",
        r.state().waiting_for
    );
}

/// The legal set is exactly the defending player's two graveyard cards.
fn assert_only_defenders_cards(
    legal: &[TargetRef],
    graveyards: &[(PlayerId, ObjectId)],
    defender: PlayerId,
) {
    let mut offered: Vec<ObjectId> = legal
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    offered.sort();
    let mut expected: Vec<ObjectId> = graveyards
        .iter()
        .filter(|(owner, _)| *owner == defender)
        .map(|(_, card)| *card)
        .collect();
    expected.sort();
    assert_eq!(
        offered, expected,
        "only {defender:?}'s graveyard cards may be targeted: {legal:?}"
    );
}

/// CR 508.5a: the defending player is fixed per attack, so attacking P1 and
/// attacking P2 each offer only that player's graveyard.
#[test]
fn graven_abomination_targets_only_the_attacked_players_graveyard() {
    for defender in [P1, P2] {
        let (sc, attacker, graveyards) = scenario(GRAVEN_ABOMINATION);
        let mut r = sc.build();
        let legal = legal_targets_at_trigger_prompt(&mut r, attacker, defender);
        assert_only_defenders_cards(&legal, &graveyards, defender);
    }
}

/// An attacks-and-isn't-blocked trigger with an "up to two" graveyard target
/// binds the same single defending player.
#[test]
fn rysorian_badger_targets_only_the_attacked_players_graveyard() {
    for defender in [P1, P2] {
        let (sc, attacker, graveyards) = scenario(RYSORIAN_BADGER);
        let mut r = sc.build();
        let legal = legal_targets_at_trigger_prompt(&mut r, attacker, defender);
        assert_only_defenders_cards(&legal, &graveyards, defender);
    }
}
