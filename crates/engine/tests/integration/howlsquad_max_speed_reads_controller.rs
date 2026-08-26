//! Howlsquad Heavy — "Max speed — {T}: Add {R} for each Goblin you control."
//!
//! Reported from a real game: the Goblin was taken from its owner, and its max
//! speed mana ability could be activated by its new controller even though that
//! controller was at speed 2. The owner was at speed 4.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 702.178a: "Max speed — [Ability]" means "As long as YOUR speed is 4,
//!   this object has '[Ability]'." The glossary entry spells out whose speed:
//!   "that permanent's controller (or that card's owner, if it isn't on the
//!   battlefield)". On the battlefield it is the CONTROLLER, always.
//! - CR 702.179e: a player has max speed if their speed is 4.
//!
//! The three rows below vary exactly one thing at a time, so the failing row
//! names the wrong player rather than merely reporting that something is off.

use engine::game::scenario::GameRunner;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::ability::PlayerFilter;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

const HOWLSQUAD: &str = "Start your engines!\nOther Goblins you control have haste.\nAt the beginning of combat on your turn, create a 1/1 red Goblin creature token. That token attacks this combat if able.\nMax speed — {T}: Add {R} for each Goblin you control.";

fn set_speed(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, speed: Option<u8>) {
    for p in runner.state_mut().players.iter_mut() {
        if p.id == player {
            p.speed = speed;
        }
    }
}

/// The index of Howlsquad's mana ability among the permanent's abilities.
fn mana_ability_index(runner: &engine::game::scenario::GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::Mana { .. }))
        .expect("Howlsquad Heavy must carry a Mana activated ability")
}

/// Can the controller actually activate the max speed mana ability?
///
/// Measured by ATTEMPTING the activation, which is what the player did. An
/// earlier revision of this file read `ai_support::legal_actions` instead and
/// measured nothing at all: that list does not surface mana abilities at
/// priority — a plain Mountain is absent from it too, which the harness row at
/// the bottom of this file pins.
fn mana_produced_by_activating(
    runner: &mut engine::game::scenario::GameRunner,
    source: ObjectId,
) -> usize {
    let index = mana_ability_index(runner, source);
    let before = runner.state().players[0].mana_pool.mana.len();
    if runner
        .act(engine::types::actions::GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .is_err()
    {
        return 0;
    }
    // Mana abilities do not use the stack (CR 605.3a), so the pool is the
    // outcome. Counting pips rather than trusting the activation's `Ok` keeps
    // "offered but inert" and "actually produced" apart — the report says the
    // player saw real red mana appear.
    runner.state().players[0]
        .mana_pool
        .mana
        .len()
        .saturating_sub(before)
}

/// P0 controls Howlsquad Heavy; P1 owns it. `p0` / `p1` are the two speeds.
fn howlsquad_under_p0_control(
    p0: Option<u8>,
    p1: Option<u8>,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    // Owned by P1, controlled by P0 — the board the report describes.
    let howlsquad = scenario
        .add_creature_from_oracle(P1, "Howlsquad Heavy", 4, 4, HOWLSQUAD)
        // "Add {R} for each Goblin you control" counts Howlsquad itself, so the
        // subtype is load-bearing: without it the ability produces nothing even
        // when it is legitimately active, and every row reads zero.
        .with_subtypes(vec!["Goblin"])
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();
    set_speed(&mut runner, P0, p0);
    set_speed(&mut runner, P1, p1);
    (runner, howlsquad)
}

/// Control row: the CONTROLLER is at max speed, so the ability is available.
///
/// Without this row a failing row below would only show that the ability is
/// never offered, which would prove nothing about whose speed is read.
#[test]
fn the_controller_at_max_speed_may_activate_it() {
    let (mut runner, howlsquad) = howlsquad_under_p0_control(Some(4), Some(0));
    assert!(
        mana_produced_by_activating(&mut runner, howlsquad) > 0,
        "CR 702.178a: the controller has speed 4, so the ability is active"
    );
}

/// Baseline: nobody is at max speed, so nothing is available.
#[test]
fn nobody_at_max_speed_means_no_ability() {
    let (mut runner, howlsquad) = howlsquad_under_p0_control(Some(2), Some(2));
    assert_eq!(
        mana_produced_by_activating(&mut runner, howlsquad),
        0,
        "no player has speed 4, so the max speed ability grants nothing"
    );
}

/// The report: only the OWNER is at max speed. The controller is not.
///
/// This row differs from the baseline above in exactly one value — P1's speed,
/// which CR 702.178a does not consult for a permanent on the battlefield. If
/// this row can activate while the baseline cannot, the engine is reading the
/// wrong player's speed.
#[test]
fn only_the_owner_at_max_speed_must_not_unlock_the_ability() {
    let (mut runner, howlsquad) = howlsquad_under_p0_control(Some(2), Some(4));
    assert_eq!(
        mana_produced_by_activating(&mut runner, howlsquad),
        0,
        "CR 702.178a reads the CONTROLLER's speed (2), never the owner's (4)"
    );
}

/// Why this file does not read `ai_support::legal_actions`.
///
/// That list does not surface mana abilities while a player holds priority —
/// measured here on a plain Mountain, which is as ordinary a mana source as
/// exists. Recorded rather than deleted: it is the reason the rows above
/// attempt the activation instead, and without it a future reader would
/// reasonably reach for the same wrong instrument.
#[test]
fn the_legal_action_list_does_not_surface_mana_abilities_at_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let mountain = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    let runner = scenario.build();
    assert!(
        !engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(|action| action.source_object() == Some(mountain)),
        "if a Mountain DOES appear here, this note is stale and the rows above \
         could use the cheaper instrument"
    );
}

/// CR 702.178a is SOURCE-relative, so the activating player is the wrong person
/// to ask. Raised in review of the fix for this file: the restriction evaluator
/// is handed whoever is activating, and CR 602.2 lets that be someone other
/// than the controller — 42 cards in the pool print "Any player may activate
/// this ability" (`PlayerFilter::All`), measured against
/// `client/public/card-data.json`.
///
/// No printed card combines that permission with a max speed ability today, so
/// these rows stamp the permission onto Howlsquad's ability directly. That is
/// the honest way to test the class: the leaf must read the source's player
/// whether or not a card currently exercises the difference.
fn open_activation_to_every_player(runner: &mut GameRunner, source: ObjectId) {
    let index = mana_ability_index(runner, source);
    let object = runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("the source must still be on the battlefield");
    // `abilities` is shared behind an `Arc`; `make_mut` clones it for this object
    // alone rather than reaching through the shared handle.
    std::sync::Arc::make_mut(&mut object.abilities)[index].activator_filter =
        Some(PlayerFilter::All);
}

/// Is `player` — who need not hold priority right now — permitted to activate?
///
/// Measured on `can_activate_ability_now`, the gate the activation path itself
/// consults, because it takes the activating player as an argument. The rows
/// above drive the whole pipeline instead; this one cannot, since driving P1 to
/// priority inside P0's main phase would measure the priority system rather than
/// the condition. Stated plainly: these two rows prove the GATE reads the right
/// player, not that the pipeline behind it produces mana.
fn may_activate_as(runner: &GameRunner, source: ObjectId, player: PlayerId) -> bool {
    let index = mana_ability_index(runner, source);
    engine::game::casting::can_activate_ability_now(runner.state(), player, source, index)
}

/// The controller has max speed, so the ability EXISTS (CR 702.178a) and any
/// player permitted to activate it may — even one at speed 0.
#[test]
fn a_non_controller_may_activate_it_while_the_controller_is_at_max_speed() {
    let (mut runner, howlsquad) = howlsquad_under_p0_control(Some(4), Some(0));
    open_activation_to_every_player(&mut runner, howlsquad);
    assert!(
        may_activate_as(&runner, howlsquad, P1),
        "CR 702.178a reads the CONTROLLER's speed (4); the activator's own speed \
         is not part of the condition"
    );
}

/// The mirror row, and the one that fails if the evaluator reads the activator:
/// the activator is at max speed and the controller is not, so the ability does
/// not exist at all.
#[test]
fn a_non_controller_at_max_speed_cannot_activate_it_while_the_controller_is_not() {
    let (mut runner, howlsquad) = howlsquad_under_p0_control(Some(2), Some(4));
    open_activation_to_every_player(&mut runner, howlsquad);
    assert!(
        !may_activate_as(&runner, howlsquad, P1),
        "CR 702.178a reads the CONTROLLER's speed (2), never the activator's (4)"
    );
}
