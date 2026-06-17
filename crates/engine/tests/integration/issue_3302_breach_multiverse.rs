//! Integration test for GitHub issue #3302 — Breach the Multiverse's per-player
//! reanimation chain.
//!
//! Printed Oracle text:
//!   "Each player mills ten cards. For each player, choose a creature or
//!    planeswalker card in that player's graveyard. Put those cards onto the
//!    battlefield under your control. Then each creature you control becomes a
//!    Phyrexian in addition to its other types."
//!
//! The spell's controller (the caster) chooses ONE creature/planeswalker card
//! from EACH player's graveyard, those chosen cards enter the battlefield under
//! the caster's control, and every creature the caster controls becomes a
//! Phyrexian.
//!
//! This file drives the REAL `apply` pipeline in a 2-player game: Breach is cast
//! for free from the caster's hand, the mill + per-player choose loop park
//! interactive `ChooseFromZoneChoice` prompts, each pick is answered via a real
//! `GameAction::SelectCards`, and every observable (zone, controller, subtype)
//! is engine-produced.
//!
//! The scenario is built to discriminate two distinct bugs:
//!   * Clause-3 ORIGIN: "those cards" must be scanned from the GRAVEYARD (where
//!     the choose left them), not the impulse-default exile. A wrong origin
//!     leaves the chosen creatures in the graveyard (no reanimation).
//!   * Tracked-set EXTEND-vs-FRESH (CR 608.2c + CR 603.7): clause 1 mills cards
//!     (publishing a "Milled" tracked set). The FIRST per-player pick must START
//!     a FRESH chosen-card set, NOT extend the milled set — otherwise the milled
//!     creatures reanimate alongside the chosen ones ("those cards" = the chosen
//!     cards only). Each graveyard therefore holds an EXTRA creature and a milled
//!     creature that must REMAIN behind.
//!
//! CR 400.7: a card stays in its current zone until an effect moves it.
//! CR 608.2c: "those cards" refers to the cards chosen in the preceding clause.
//! CR 110.2a: "under your control" sets the entering object's controller.
//! CR 205.1b: "becomes a Phyrexian" adds the Phyrexian creature subtype.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BREACH_ORACLE: &str = "Each player mills ten cards. For each player, choose a creature or \
     planeswalker card in that player's graveyard. Put those cards onto the battlefield under \
     your control. Then each creature you control becomes a Phyrexian in addition to its other \
     types.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .zone
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .controller
}

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn has_subtype(runner: &GameRunner, id: ObjectId, subtype: &str) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(subtype))
}

/// Mark a library/graveyard card as a Creature so it is a legal candidate (and,
/// for milled cards, a wrong-reanimation tripwire) for Breach's choose filter.
fn make_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("object present");
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        obj.card_types.core_types.push(CoreType::Creature);
    }
    obj.base_card_types = obj.card_types.clone();
}

/// Drive the runner forward (passing priority / declaring no attackers/blockers)
/// until it pauses on a `ChooseFromZoneChoice`, or the stack empties.
fn advance_to_choice_or_empty(runner: &mut GameRunner) {
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            _ => return,
        }
    }
}

/// Answer the current per-player `ChooseFromZoneChoice` by selecting `pick`,
/// after asserting the prompt is scoped to the caster and offers `pick`.
fn answer_pick(runner: &mut GameRunner, expected_chooser: PlayerId, pick: ObjectId) {
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(
                *player, expected_chooser,
                "the spell's controller makes every per-player pick"
            );
            assert_eq!(*count, 1, "exactly one card per player");
            assert!(
                cards.contains(&pick),
                "the intended pick {pick:?} must be a legal candidate; offered {cards:?}"
            );
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("selecting one legal creature must succeed");
}

/// CR 400.7 + CR 608.2c + CR 110.2a + CR 205.1b: Breach reanimates exactly the
/// chosen creature from each player's graveyard under the caster's control as a
/// Phyrexian; every non-chosen card (extra creatures, instants, milled
/// creatures) stays in its graveyard.
#[test]
fn breach_reanimates_only_chosen_cards_under_caster_as_phyrexian() {
    let mut scenario = GameScenario::new_n_player(2, 3302);
    scenario.at_phase(Phase::PreCombatMain);

    // Each player's library has ten cards so "mills ten cards" fully resolves.
    // One milled card per player is a CREATURE — the wrong-reanimation tripwire
    // for the milled-vs-chosen tracked-set bug.
    for &pid in &[P0, P1] {
        scenario.with_library_top(
            pid,
            &[
                "Mill 1",
                "Mill 2",
                "Mill 3",
                "Mill 4",
                "Mill 5",
                "Mill 6",
                "Mill 7",
                "Mill 8",
                "Mill 9",
                "Milled Creature",
            ],
        );
    }

    // Pre-seed each graveyard: one creature to CHOOSE, one EXTRA creature that
    // must stay, and one instant that must stay.
    let p0_chosen = scenario
        .add_creature_to_graveyard(P0, "P0 Chosen", 2, 2)
        .id();
    let p0_extra = scenario
        .add_creature_to_graveyard(P0, "P0 Extra", 3, 3)
        .id();
    let p0_instant = scenario.add_spell_to_graveyard(P0, "P0 Bolt", true).id();

    let p1_chosen = scenario
        .add_creature_to_graveyard(P1, "P1 Chosen", 4, 4)
        .id();
    let p1_extra = scenario
        .add_creature_to_graveyard(P1, "P1 Extra", 5, 5)
        .id();
    let p1_instant = scenario.add_spell_to_graveyard(P1, "P1 Bolt", true).id();

    // Breach the Multiverse in the caster's (P0) hand, parsed from real Oracle
    // text. No mana cost is set, so it casts for free.
    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Breach the Multiverse", false, BREACH_ORACLE)
        .id();

    let mut runner = scenario.build();

    // The milled creatures land in each graveyard once the mill resolves; mark
    // them as creatures up front so they are filter-legal candidates that the
    // chosen-set bug could wrongly reanimate. They are at library index 9 (the
    // bottom of the seeded top-ten), so capture them by object id now.
    let p0_milled_creature = *runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .library
        .last()
        .expect("P0 library has cards");
    let p1_milled_creature = *runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .library
        .last()
        .expect("P1 library has cards");
    make_creature(&mut runner, p0_milled_creature);
    make_creature(&mut runner, p1_milled_creature);

    // Cast Breach for free.
    let card_id = runner.state().objects[&breach].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: breach,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Breach must be accepted");

    // Resolve: mill ten per player, then the per-player choose loop parks.
    advance_to_choice_or_empty(&mut runner);

    // The milled creatures must now be in the respective graveyards (proof the
    // mill ran and that they are choose-filter-legal candidates).
    assert_eq!(zone_of(&runner, p0_milled_creature), Zone::Graveyard);
    assert_eq!(zone_of(&runner, p1_milled_creature), Zone::Graveyard);

    // Answer each player's pick (caster chooses). APNAP order from P0 means P0's
    // graveyard is prompted first, then P1's.
    answer_pick(&mut runner, P0, p0_chosen);
    advance_to_choice_or_empty(&mut runner);
    answer_pick(&mut runner, P0, p1_chosen);

    runner.advance_until_stack_empty();

    // CR 400.7 + CR 110.2a: exactly the two chosen creatures are on the
    // battlefield under the CASTER's (P0) control.
    for chosen in [p0_chosen, p1_chosen] {
        assert_eq!(
            zone_of(&runner, chosen),
            Zone::Battlefield,
            "the chosen creature {chosen:?} must be reanimated"
        );
        assert_eq!(
            controller_of(&runner, chosen),
            P0,
            "the reanimated creature {chosen:?} must enter under the caster's control"
        );
        // CR 205.1b: every creature the caster controls becomes a Phyrexian.
        assert!(
            has_subtype(&runner, chosen, "Phyrexian"),
            "the reanimated creature {chosen:?} must become a Phyrexian"
        );
    }

    // CR 608.2c + CR 603.7: the milled-vs-chosen discriminator. The milled
    // creatures must NOT have been swept up by "those cards" — they stay in
    // their graveyards.
    assert_eq!(
        zone_of(&runner, p0_milled_creature),
        Zone::Graveyard,
        "P0's milled creature must NOT reanimate (it was milled, not chosen)"
    );
    assert_eq!(
        zone_of(&runner, p1_milled_creature),
        Zone::Graveyard,
        "P1's milled creature must NOT reanimate (it was milled, not chosen)"
    );

    // The non-chosen extra creatures and the instants stay in their graveyards.
    for stay in [p0_extra, p1_extra, p0_instant, p1_instant] {
        assert_eq!(
            zone_of(&runner, stay),
            Zone::Graveyard,
            "non-chosen card {stay:?} must remain in its graveyard"
        );
    }
    // The instants are not creatures, so they never gain Phyrexian even if they
    // somehow re-entered — a belt-and-suspenders guard on the filter.
    assert!(!is_creature(&runner, p0_instant));
    assert!(!is_creature(&runner, p1_instant));

    // The chain completed with no stall.
    assert!(
        runner.state().stack.is_empty(),
        "Breach's chain must fully resolve"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ),
        "no per-player choice should remain pending"
    );
}
