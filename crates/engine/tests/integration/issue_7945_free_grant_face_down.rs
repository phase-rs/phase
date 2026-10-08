//! Issue #7945 — a free-cast exile grant must not auto-route into the
//! disguise face-down cast.
//!
//! CR 118.9a ("Only one alternative cost can be applied to any one spell as
//! it's being cast") + CR 601.2b ("A player can't apply two alternative
//! methods of casting or two alternative costs to a single spell"): the
//! grant's "without paying its mana cost" IS the alternative cost being
//! applied, and disguise's face-down-for-{3} (CR 702.168a) would be a second
//! one. Under an `ExileWithAltCost` grant the only legal cast is face up,
//! free. Observed live: a Dauthi Voidwalker grant on a disguise card cast
//! face down with {3} charged and no prompt, because the face-down offer
//! priced the normal path at the printed cost (the #7778 auto-route).
//!
//! REVERT DISCRIMINATOR: without the variant-aware zone-authority gate in
//! `has_exile_cast_permission`, the face-down block auto-routes (only the
//! {3} looks affordable) — the free-grant test's permanent enters face down
//! with the pool drained.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VOIDWALKER: &str = "If a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
const MURDER: &str = "Destroy target creature.";
const DISGUISE_LINE: &str = "Disguise {5}{U}";

fn three_generic() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 3]
}

fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&object)
        .expect("object exists")
        .zone
}

fn is_face_down(runner: &GameRunner, object: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&object)
        .expect("object exists")
        .face_down
}

fn pool_size(runner: &GameRunner, player: usize) -> usize {
    runner.state().players[player].mana_pool.mana.len()
}

/// The dangerous state from the live report: the doomed creature carries
/// disguise, its printed cost {5} is NOT affordable, but the {3} face-down
/// cost IS — exactly what made the greedy offer auto-route face down.
fn smuggler_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, VOIDWALKER)
        .id();
    let smuggler = scenario
        .add_creature_from_oracle(P1, "Doomed Smuggler", 2, 2, DISGUISE_LINE)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let murder = scenario
        .add_spell_to_hand(P0, "Test Murder", false)
        .from_oracle_text(MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, three_generic());
    let mut runner = scenario.build();
    runner.cast(murder).target_object(smuggler).resolve();
    assert_eq!(zone_of(&runner, smuggler), Zone::Exile);
    (runner, walker, smuggler)
}

/// Drive the activation to a settled empty stack (pay the sacrifice cost,
/// pick the exiled card, pass priority).
fn drive_activation(runner: &mut GameRunner, walker: ObjectId, pick: ObjectId) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![walker],
                    })
                    .expect("paying the sacrifice cost must succeed");
            }
            WaitingFor::ChooseFromZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("picking the exiled card must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority must be accepted mid-drive");
            }
            other => panic!("unexpected prompt during the activation: {other:?}"),
        }
    }
    panic!("activation never settled within 64 steps");
}

#[test]
fn a_free_granted_disguise_card_casts_face_up_and_free() {
    // CR 118.9a + CR 601.2b: with only the `ExileWithAltCost` grant as zone
    // authority, the face-down cast is not a legal method — the cast must be
    // the granted one: face up, nothing paid.
    let (mut runner, walker, smuggler) = smuggler_board();
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
        })
        .expect("activating the {T}+sacrifice ability must succeed");
    drive_activation(&mut runner, walker, smuggler);

    runner.cast(smuggler).resolve();
    assert_eq!(
        zone_of(&runner, smuggler),
        Zone::Battlefield,
        "the granted cast must succeed"
    );
    assert!(
        !is_face_down(&runner, smuggler),
        "a card cast without paying its mana cost cannot be cast face down \
         (CR 601.2b) — face down here means the disguise cast hijacked the grant"
    );
    assert_eq!(
        pool_size(&runner, 0),
        3,
        "the granted cast pays nothing — a drained pool means the {{3}} \
         face-down cost was charged"
    );
}

#[test]
fn from_hand_the_face_down_auto_route_is_preserved() {
    // Positive counter-direction (#7778 stays intact): from HAND the normal
    // cast is a legal method; with only the {3} affordable the auto-route
    // into the face-down cast must survive the new zone-authority gate.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smuggler = scenario
        .add_creature_to_hand(P0, "Hand Smuggler", 2, 2)
        .from_oracle_text(DISGUISE_LINE)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(P0, three_generic());
    let mut runner = scenario.build();

    runner.cast(smuggler).resolve();
    assert_eq!(zone_of(&runner, smuggler), Zone::Battlefield);
    assert!(
        is_face_down(&runner, smuggler),
        "from hand, only the {{3}} affordable must still auto-route face down (#7778)"
    );
    assert_eq!(
        pool_size(&runner, 0),
        0,
        "the face-down cast charges its {{3}}"
    );
}
