//! Max speed from a graveyard — the OWNER arm of `ParsedCondition::HasMaxSpeed`.
//!
//! `restrictions.rs` scopes the max-speed gate to the source's controller on the
//! battlefield and to its owner anywhere else. The battlefield arm is covered by
//! `howlsquad_max_speed_reads_controller.rs`; this file drives the owner arm on
//! the production `GameAction::ActivateAbility` path, from the zone that makes
//! it reachable.
//!
//! Card: Loxodon Surveyor (Oracle text verbatim from `client/public/card-data.json`),
//! one of five Aetherdrift Surveyors printing
//! "Max speed — {3}, Exile this card from your graveyard: Draw a card."
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 702.178a: "Max speed — [Ability]" means "As long as your speed is 4,
//!   this object has '[Ability]'."
//! - CR 702.178b: a max speed ability functions from whatever zones the ability
//!   it grants functions from — which is what puts this one in a graveyard.
//! - CR 702.179e: a player has max speed if their speed is 4.
//! - CR 108.4 + CR 108.4a: a card that is not a permanent or spell has no
//!   controller, and anything asking for one uses its owner instead. That is the
//!   rules basis for the `else` arm reading `owner`.
//!
//! MEASURED LIMIT — what these rows cannot prove. CR 108.4a's substitution is
//! also what the engine performs on the zone change: `the_engine_resets_the_
//! controller_when_a_permanent_dies` below pins that a permanent owned by P0 and
//! controlled by P1 arrives in the graveyard with `controller == owner == P0`.
//! So for every reachable graveyard state the owner arm and a controller read
//! return the same player, and no test can separate them there. What the two
//! activation rows do separate is the owner from the OTHER player: the opponent
//! sits at the opposite speed in both, so a gate reading the wrong player flips
//! both rows. The `owner` spelling is kept because CR 108.4a says owner, not
//! because a controller read is currently observable.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LOXODON_SURVEYOR: &str = "Start your engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\nMax speed — {3}, Exile this card from your graveyard: Draw a card.";

fn floating_generic(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn set_speed(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, speed: Option<u8>) {
    for p in runner.state_mut().players.iter_mut() {
        if p.id == player {
            p.speed = speed;
        }
    }
}

/// P0 owns a Loxodon Surveyor in their graveyard and holds exactly the {3} the
/// ability costs. `p0` / `p1` are the two speeds.
fn surveyor_in_p0_graveyard(
    p0: Option<u8>,
    p1: Option<u8>,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let surveyor = scenario
        .add_creature_to_graveyard(P0, "Loxodon Surveyor", 3, 3)
        .from_oracle_text(LOXODON_SURVEYOR)
        .id();
    scenario.with_mana_pool(P0, floating_generic(3));
    let mut runner = scenario.build();
    set_speed(&mut runner, P0, p0);
    set_speed(&mut runner, P1, p1);
    (runner, surveyor)
}

/// Attempt the activation the player would make, and report whether the cost was
/// actually paid.
///
/// The outcome is read from the card's ZONE, not from the `Ok`/`Err` of the
/// submission: "Exile this card from your graveyard" is part of the activation
/// cost (CR 602.1a), so a genuinely activated ability has already moved the card
/// to exile. That keeps "accepted" and "accepted but inert" apart.
fn activated_from_graveyard(
    runner: &mut engine::game::scenario::GameRunner,
    surveyor: ObjectId,
) -> bool {
    let index = runner.state().objects[&surveyor]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Loxodon Surveyor must carry its max speed activated ability");
    let _ = runner.act(GameAction::ActivateAbility {
        source_id: surveyor,
        ability_index: index,
    });
    runner.state().objects[&surveyor].zone == Zone::Exile
}

#[test]
fn the_owner_at_max_speed_may_activate_it_from_their_graveyard() {
    // Owner at 4, opponent at 0 — if the gate read the opponent, this row fails.
    let (mut runner, surveyor) = surveyor_in_p0_graveyard(Some(4), Some(0));
    assert!(
        activated_from_graveyard(&mut runner, surveyor),
        "CR 702.178a + CR 108.4a: the card's owner has speed 4, so the granted \
         graveyard ability exists and its cost can be paid"
    );
}

#[test]
fn the_owner_below_max_speed_may_not_activate_it_from_their_graveyard() {
    // The mirror: owner at 3, opponent at 4. Both rows move together only if the
    // gate reads the owner.
    let (mut runner, surveyor) = surveyor_in_p0_graveyard(Some(3), Some(4));
    assert!(
        !activated_from_graveyard(&mut runner, surveyor),
        "CR 702.179e: the owner is at speed 3, so the ability is not granted — \
         the opponent's speed 4 must not stand in for it"
    );
    assert_eq!(
        runner.state().objects[&surveyor].zone,
        Zone::Graveyard,
        "a rejected activation pays no cost, so the card stays put"
    );
}

/// Nail-down, not evidence: this row is green with and without the max speed
/// fix. It pins the measurement the module doc rests on — that a graveyard card
/// never carries a controller different from its owner, which is why the two
/// rows above cannot discriminate the owner arm from a controller read.
#[test]
fn the_engine_resets_the_controller_when_a_permanent_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let surveyor = scenario
        .add_creature_from_oracle(P0, "Loxodon Surveyor", 3, 3, LOXODON_SURVEYOR)
        .controlled_by(P1)
        .with_damage_marked(3)
        .id();
    let mut runner = scenario.build();
    runner.pass_both_players();
    let obj = &runner.state().objects[&surveyor];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "lethal damage is a CR 704.5g SBA"
    );
    assert_eq!(
        obj.owner, P0,
        "CR 404.1: a card goes to its owner's graveyard"
    );
    assert_eq!(
        obj.controller, P0,
        "CR 108.4: a card in a graveyard is neither permanent nor spell, so it \
         has no controller — the engine substitutes the owner (CR 108.4a)"
    );
}
