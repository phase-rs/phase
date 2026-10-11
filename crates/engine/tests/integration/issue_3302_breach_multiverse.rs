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
//!   * Tracked-set EXTEND-vs-FRESH (CR 608.2c): clause 1 mills cards
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
use engine::types::game_state::{CastPaymentMode, WaitingFor, ZoneOpponentChooserPurpose};
use engine::types::identifiers::ObjectId;
use engine::types::phase::{Phase, TurnDirection};
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

    // CR 101.4c: the caster makes every pick, so the caster chooses their
    // order. Both graveyards hold a candidate, so the first prompt is the
    // caster's order prompt — and the caster's own graveyard is a candidate
    // ("for each player"). Choose P1's graveyard first (not APNAP).
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            ..
        } => {
            assert_eq!(*player, P0, "the caster orders their own choices");
            assert_eq!(candidates, &vec![P0, P1], "both graveyards are offered");
        }
        other => panic!("expected the caster's order prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P1 })
        .expect("ordering P1 first is legal");
    answer_pick(&mut runner, P0, p1_chosen);
    advance_to_choice_or_empty(&mut runner);
    answer_pick(&mut runner, P0, p0_chosen);

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

    // CR 608.2c: the milled-vs-chosen discriminator. The milled
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

/// CR 101.4c + CR 707.10: a COPY of Breach the Multiverse resolves its own
/// per-player iteration under its own resolution carrier (the copy's stack
/// entry), then the original resolves under its own. Each iteration is
/// admitted while its own carrier resolves, and ordered by the caster — whose
/// own graveyard is a candidate. A validation control for the carrier
/// admission rule, not evidence of a known mismatch.
#[test]
fn copied_breach_resolves_each_iteration_under_its_own_carrier() {
    const TWINCAST: &str =
        "Copy target instant or sorcery spell. You may choose new targets for the copy.";
    let mut scenario = GameScenario::new_n_player(2, 3303);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        let names: Vec<String> = (0..20).map(|i| format!("Filler {i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(pid, &names);
    }
    let p0_creatures: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P0, &format!("P0 Creature {i}"), 2, 2)
                .id()
        })
        .collect();
    let p1_creatures: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P1, &format!("P1 Creature {i}"), 2, 2)
                .id()
        })
        .collect();
    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Breach the Multiverse", false, BREACH_ORACLE)
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .id();
    let mut runner = scenario.build();

    let breach_card = runner.state().objects[&breach].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: breach,
            card_id: breach_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Breach must be accepted");
    let twincast_card = runner.state().objects[&twincast].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: twincast,
            card_id: twincast_card,
            targets: vec![breach],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Twincast on Breach must be accepted");

    let mut carriers = Vec::new();
    for _ in 0..400 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneOpponentChooser {
                player,
                candidates,
                purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
                ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(
                    candidates,
                    vec![P0, P1],
                    "the caster's own graveyard is offered"
                );
                let entry = runner
                    .state()
                    .resolving_stack_entry
                    .clone()
                    .expect("an installed carrier resolves the iteration");
                assert!(
                    runner.state().active_per_player_zone_choice().is_some(),
                    "the order prompt belongs to the parked iteration"
                );
                carriers.push(entry.id);
                runner
                    .act(GameAction::ChooseZoneOpponentChooser { opponent: P1 })
                    .expect("order pick");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("pick");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("keep the copy's targets");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }

    assert_eq!(
        carriers.len(),
        2,
        "the copy and the original each iterate: {carriers:?}"
    );
    assert_ne!(carriers[0], carriers[1], "each under its own carrier");
    assert_eq!(
        carriers[1], breach,
        "the original resolves under its own stack entry"
    );
    let reanimated = p0_creatures
        .iter()
        .chain(p1_creatures.iter())
        .filter(|id| runner.state().battlefield.contains(id))
        .count();
    assert_eq!(reanimated, 4, "one creature per graveyard per resolution");
}

/// CR 101.4c: "for each player" includes the caster, so the AI's order picks
/// include the caster's own graveyard.
#[test]
fn ai_order_picks_include_the_casters_own_graveyard() {
    let mut scenario = GameScenario::new_n_player(2, 3304);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        let names: Vec<String> = (0..10).map(|i| format!("Filler {i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(pid, &names);
    }
    let _ = scenario
        .add_creature_to_graveyard(P0, "P0 Creature", 2, 2)
        .id();
    let _ = scenario
        .add_creature_to_graveyard(P1, "P1 Creature", 2, 2)
        .id();
    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Breach the Multiverse", false, BREACH_ORACLE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&breach].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: breach,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Breach must be accepted");
    advance_to_choice_or_empty(&mut runner);

    let mut offered: Vec<PlayerId> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseZoneOpponentChooser { opponent } => Some(opponent),
            _ => None,
        })
        .collect();
    offered.sort();
    assert_eq!(offered, vec![P0, P1]);
}

/// CR 101.4c: Ghouls' Night Out ("For each player, choose a creature card in
/// that player's graveyard. Put those cards onto the battlefield under your
/// control. ...") is the same controller-chosen per-player iteration. In a
/// three-player game the caster orders all three graveyards, in a non-APNAP
/// order, and every chosen creature enters under the caster's control.
#[test]
fn ghouls_night_out_caster_orders_every_graveyard() {
    const GHOULS_NIGHT_OUT: &str = "For each player, choose a creature card in that player's \
         graveyard. Put those cards onto the battlefield under your control. They're black \
         Zombies in addition to their other colors and types and they gain decayed. (A creature \
         with decayed can't block. When it attacks, sacrifice it at end of combat.)";
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 3305);
    scenario.at_phase(Phase::PreCombatMain);
    let p0_dead = scenario
        .add_creature_to_graveyard(P0, "P0 Creature", 2, 2)
        .id();
    let p1_dead = scenario
        .add_creature_to_graveyard(P1, "P1 Creature", 2, 2)
        .id();
    let p2_dead = scenario
        .add_creature_to_graveyard(P2, "P2 Creature", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghouls' Night Out", false, GHOULS_NIGHT_OUT)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Ghouls' Night Out must be accepted");
    advance_to_choice_or_empty(&mut runner);

    let order_prompt = |runner: &GameRunner| match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            ..
        } => {
            assert_eq!(*player, P0, "the caster orders their own choices");
            candidates.clone()
        }
        other => panic!("expected the caster's order prompt, got {other:?}"),
    };
    assert_eq!(order_prompt(&runner), vec![P0, P1, P2]);
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("ordering P2 first is legal");
    answer_pick(&mut runner, P0, p2_dead);
    assert_eq!(
        order_prompt(&runner),
        vec![P0, P1],
        "two graveyards remain to order"
    );
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P0 })
        .expect("ordering the caster's own graveyard next is legal");
    answer_pick(&mut runner, P0, p0_dead);
    answer_pick(&mut runner, P0, p1_dead);
    runner.advance_until_stack_empty();

    for creature in [p0_dead, p1_dead, p2_dead] {
        assert_eq!(zone_of(&runner, creature), Zone::Battlefield);
        assert_eq!(controller_of(&runner, creature), P0);
    }
}

/// The engine's own save/restore pipeline.
fn restore(
    runner: &GameRunner,
) -> Result<engine::types::game_state::GameState, engine::types::game_state::PersistedRestoreError>
{
    let persisted = serde_json::to_string(&engine::types::game_state::PersistedGameState::capture(
        runner.state().clone(),
    ))
    .expect("persisted state serializes");
    serde_json::from_str::<engine::types::game_state::PersistedGameState>(&persisted)
        .expect("persisted state decodes")
        .prepare_for_restore(
            engine::types::game_state::PersistedRestoreFinalization::DeferUntilRehydrated,
        )?
        .finalize_after_rehydration(|_| Ok(()))
}

/// Reshape the live parked frame into its legacy (v97) form.
fn make_frame_legacy(runner: &mut GameRunner) {
    let mut frame = runner
        .state_mut()
        .take_active_per_player_zone_choice()
        .expect("the per-player frame is the top")
        .expect("reach: a parked frame");
    frame.current = None;
    frame.nominee = None;
    runner.state_mut().push_per_player_zone_choice(frame);
}

/// Cast Breach in a 3-player game under `direction` (`Reversed` casts a real
/// "Reverse the game's turn order." spell first), order `first` then
/// pick from its graveyard, then order `second`, leaving `second`'s pool
/// pending. Returns the runner and every graveyard creature.
fn breach_parked_on_second_pool(
    seed: u64,
    direction: TurnDirection,
    first: PlayerId,
    second: PlayerId,
) -> (GameRunner, Vec<ObjectId>) {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, seed);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1, P2] {
        let names: Vec<String> = (0..10).map(|i| format!("Filler {i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(pid, &names);
    }
    let creatures: Vec<ObjectId> = [P0, P1, P2]
        .iter()
        .map(|&pid| {
            scenario
                .add_creature_to_graveyard(pid, &format!("P{} Creature", pid.0), 2, 2)
                .id()
        })
        .collect();
    let reverser = scenario
        .add_spell_to_hand_from_oracle(P0, "Aeon Probe", false, "Reverse the game's turn order.")
        .id();
    let breach = scenario
        .add_spell_to_hand_from_oracle(P0, "Breach the Multiverse", false, BREACH_ORACLE)
        .id();
    let mut runner = scenario.build();
    let cast = |runner: &mut GameRunner, id: ObjectId| {
        let card_id = runner.state().objects[&id].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: id,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast");
    };
    match direction {
        TurnDirection::Normal => {}
        TurnDirection::Reversed => {
            cast(&mut runner, reverser);
            runner.advance_until_stack_empty();
        }
    }
    cast(&mut runner, breach);
    advance_to_choice_or_empty(&mut runner);
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: first })
        .expect("order the first graveyard");
    let first_pick = creatures[first.0 as usize];
    answer_pick(&mut runner, P0, first_pick);
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: second })
        .expect("order the second graveyard");
    (runner, creatures)
}

fn finish_breach(runner: &mut GameRunner, picks: &[ObjectId]) {
    for &pick in picks {
        advance_to_choice_or_empty(runner);
        answer_pick(runner, P0, pick);
    }
    runner.advance_until_stack_empty();
    for &pick in picks {
        assert_eq!(
            zone_of(runner, pick),
            Zone::Battlefield,
            "{pick:?} reanimated"
        );
    }
}

/// R7-B: a genuine v97 Breach save under REVERSED turn order (APNAP
/// [P0, P2, P1]) parked on P2's pool with `remaining = [P1]` restores with P2
/// as owner — the migration's population follows the turn-order direction —
/// and the resolution finishes.
///
/// REVERT PROBE: order the static population clockwise and this save is
/// rejected.
#[test]
fn legacy_breach_save_under_reversed_turn_order_restores() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, creatures) =
        breach_parked_on_second_pool(3306, TurnDirection::Reversed, P0, P2);
    assert_eq!(
        engine::game::players::apnap_order(runner.state()),
        vec![P0, P2, P1],
        "reach: reversed APNAP order"
    );
    assert!(
        runner.state().players.iter().all(|p| !p.is_eliminated),
        "reach: no departures"
    );
    let frame = runner
        .state()
        .active_per_player_zone_choice()
        .expect("reach: a parked frame")
        .clone();
    assert_eq!(frame.current, Some(P2), "reach: P2's pool is pending");
    assert_eq!(frame.remaining_players, vec![P1], "reach: the v97 queue");
    make_frame_legacy(&mut runner);

    let restored = restore(&runner).expect("a reversed-order v97 save restores");
    assert_eq!(
        restored
            .active_per_player_zone_choice()
            .and_then(|frame| frame.current),
        Some(P2),
        "the owner is re-established"
    );
    let mut runner = GameRunner::from_state(restored);
    finish_breach(&mut runner, &[creatures[2], creatures[1]]);
}

/// R7-B control: the same reversed position saved in the current schema
/// (explicit `current`) restores and finishes without any inference.
#[test]
fn current_schema_breach_save_under_reversed_turn_order_restores() {
    const P2: PlayerId = PlayerId(2);
    let (runner, creatures) = breach_parked_on_second_pool(3307, TurnDirection::Reversed, P0, P2);
    let restored = restore(&runner).expect("a current-schema save restores");
    let mut runner = GameRunner::from_state(restored);
    finish_breach(&mut runner, &[creatures[2], creatures[1]]);
}

/// R7-B control: a v97 Breach save under the NORMAL turn order (APNAP
/// [P0, P1, P2]) parked on P1's pool with `remaining = [P2]` restores with P1
/// as owner.
#[test]
fn legacy_breach_save_under_normal_turn_order_restores() {
    const P2: PlayerId = PlayerId(2);
    let (mut runner, creatures) = breach_parked_on_second_pool(3308, TurnDirection::Normal, P0, P1);
    assert_eq!(
        engine::game::players::apnap_order(runner.state()),
        vec![P0, P1, P2]
    );
    make_frame_legacy(&mut runner);
    let restored = restore(&runner).expect("a normal-order v97 save restores");
    assert_eq!(
        restored
            .active_per_player_zone_choice()
            .and_then(|frame| frame.current),
        Some(P1)
    );
    let mut runner = GameRunner::from_state(restored);
    finish_breach(&mut runner, &[creatures[1], creatures[2]]);
}
