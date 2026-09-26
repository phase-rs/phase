//! The Necrobloom — "Land cards in your graveyard have dredge 2." grants Dredge
//! to a card that never had it printed. Two independent engine defects compound:
//!
//! 1. Every candidate-discovery path for a Draw replacement sources candidates
//!    exclusively from `obj.replacement_definitions`, which is populated only by
//!    `synthesize_dredge` reading a PRINTED `Keyword::Dredge`. A land whose only
//!    Dredge is granted at runtime has an empty `replacement_definitions`, so it
//!    is never even offered.
//! 2. Every touch point that reads a candidate's mode / choice authority / execute
//!    effect / display label is borrow-based against that same stored field, so
//!    even a registered virtual candidate would fall through to generic,
//!    unlabeled defaults without its own touch-point branches.
//!
//! `crates/engine/src/game/replacement.rs` gained a new virtual-candidate family
//! (`GRANTED_DREDGE_INDEX` / `is_granted_dredge_replacement` /
//! `granted_dredge_value`) mirroring the existing `GrantedEtbKeyword` (printed-
//! vs-granted synthesis split) and `is_commander_hand_or_library_return_replacement`
//! (Optional, multi-touch-point virtual family) precedents, plus a shared
//! `database::synthesis::dredge_replacement_definition` builder extracted from
//! `synthesize_dredge` so printed and granted Dredge apply through the identical
//! definition shape.
//!
//! Candidate MULTIPLICITY — two or more candidates produced by one grant for a
//! single draw — is a first-class coverage axis of this module, not an edge
//! case: the grant's subject ("Land cards in your graveyard") is plural. It is
//! covered both ACROSS objects (several graveyard cards claiming one draw) and
//! ON A SINGLE OBJECT (one land carrying a printed Dredge alongside the granted
//! one — the board `granted_dredge_value`'s redundancy comparison exists for).
//!
//! These tests drive the real engine pipeline (`GameScenario` + `GameRunner`,
//! `DebugAction::DrawCards` → the real `start_draw_sequence` replacement pipeline,
//! `GameAction::ChooseReplacement`) with The Necrobloom's verbatim Oracle text —
//! no shape-only assertions.
//!
//! Oracle text verbatim from Scryfall (`mh3`, collector number 194):
//! "Landfall — Whenever a land you control enters, create a 0/1 green Plant
//! creature token. If you control seven or more lands with different names,
//! create a 2/2 black Zombie creature token instead.\nLand cards in your
//! graveyard have dredge 2. (You may return a land card from your graveyard to
//! your hand and mill two cards instead of drawing a card.)"
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 702.52a: Dredge — instead of drawing, mill N and return this card from
//!   graveyard to hand.
//! - CR 702.52b: fewer than N library cards ⇒ dredge is not offered.
//! - CR 613.1f: Layer 6 ability-adding (keyword-granting) continuous effect.
//! - CR 611.3b: a static's continuous effect applies while its source is in the
//!   appropriate zone, even though its recipients (graveyard cards) are not.
//! - CR 614.6: a replaced event never happens — accepting dredge must not also
//!   draw.
//! - CR 616.1: two or more applicable replacement effects ⇒ the affected player
//!   chooses the order; each candidate must be distinguishably labeled.
//! - CR 109.4 + CR 108.4a: a graveyard object has no controller; use its owner.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const NECROBLOOM_ORACLE: &str = "Landfall — Whenever a land you control enters, create a 0/1 green Plant creature token. If you control seven or more lands with different names, create a 2/2 black Zombie creature token instead.\nLand cards in your graveyard have dredge 2. (You may return a land card from your graveyard to your hand and mill two cards instead of drawing a card.)";

/// A fresh two-player scenario with a small deterministic library for each
/// player (>= Dredge 2's CR 702.52b threshold) so a draw or a dredge accept
/// never trips an empty-library loss.
fn base_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Lib A", "P0 Lib B", "P0 Lib C"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C"]);
    scenario
}

fn draw_one(runner: &mut GameRunner, player: PlayerId) {
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: player,
            count: 1,
        }))
        .expect("debug draw must succeed");
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.zone)
        .expect("object must exist")
}

/// Drive any outstanding `ReplacementChoice` prompts to completion, preferring
/// `preferred_source`'s non-Decline option whenever it's offered and declining
/// anything else — so accepting the preferred candidate can never accidentally
/// also accept an unrelated competing candidate (the "accepting one does not
/// consume/duplicate the other" property under test).
fn resolve_preferring(runner: &mut GameRunner, preferred_source: ObjectId) {
    for _ in 0..6 {
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            return;
        };
        let idx = candidates
            .iter()
            .position(|c| c.source_id == preferred_source && c.description != "Decline")
            .or_else(|| candidates.iter().position(|c| c.description == "Decline"))
            .unwrap_or(0);
        runner
            .act(GameAction::ChooseReplacement { index: idx })
            .expect("replacement choice must be accepted");
    }
}

/// The current `ReplacementChoice` prompt as `(source_id, description)` pairs,
/// or `None` when no replacement prompt is parked. The candidate-multiplicity
/// rows below drive the pipeline by explicit index picks against this instead of
/// `resolve_preferring`, which answers *every* outstanding prompt in a loop and
/// would silently consume the very prompt those rows must inspect; it is also
/// what makes a "no prompt remains" failure print the offending candidate list.
fn replacement_prompt(runner: &GameRunner) -> Option<Vec<(ObjectId, String)>> {
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => Some(
            candidates
                .iter()
                .map(|c| (c.source_id, c.description.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// Reach-guard for the printed-dredge fixtures, which are built from Oracle
/// `"Dredge N"` so the scenario harness's production `synthesize_all` (and so
/// `synthesize_dredge`) creates their object-carried Draw replacement: the
/// object must really carry the printed keyword AND the synthesized candidate,
/// so a "one candidate" row cannot pass on a board that has no printed dredge.
fn assert_printed_dredge(runner: &GameRunner, id: ObjectId, n: u32) {
    let obj = runner.state().objects.get(&id).expect("object must exist");
    assert!(
        obj.keywords.contains(&Keyword::Dredge(n)),
        "fixture must carry printed Dredge {n}, got {:?}",
        obj.keywords
    );
    assert!(
        obj.replacement_definitions
            .as_slice()
            .iter()
            .any(|r| matches!(r.event, ReplacementEvent::Draw)),
        "synthesize_dredge must have produced the printed Draw replacement"
    );
}

/// Row 1 (positive) + Row 5: a land with no printed Dredge, sitting in the
/// graveyard of Necrobloom's controller, is offered as a real Draw replacement
/// labeled exactly `("Accept", "Decline")` — the `optional_replacement_choice_labels`
/// verified-no-op branch (Finding 2).
#[test]
fn necrobloom_grants_dredge_offers_replacement_labeled_accept_decline() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();

    draw_one(&mut runner, P0);

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ReplacementChoice offering granted dredge, got {:?}",
            runner.state().waiting_for
        );
    };
    let descriptions: Vec<&str> = candidates.iter().map(|c| c.description.as_str()).collect();
    assert_eq!(
        descriptions,
        vec!["Accept", "Decline"],
        "a solo granted-dredge candidate must present exactly Accept/Decline"
    );
    assert!(
        candidates.iter().all(|c| c.source_id == land),
        "both options must be attributed to the graveyard land granting the offer, got {candidates:?}"
    );
}

/// Row 1 reach-guard: the identical fixture MINUS Necrobloom on the battlefield
/// must not offer any replacement — proving the offer above is caused by the
/// grant, not a pre-existing artifact of a land sitting in the graveyard.
#[test]
fn necrobloom_dredge_requires_necrobloom_on_battlefield() {
    let mut scenario = base_scenario();
    scenario.add_land_to_graveyard(P0, "Forest");
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    draw_one(&mut runner, P0);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "without Necrobloom, a graveyard land must not offer dredge, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "the draw must proceed normally with no grant present"
    );
}

/// Row 1 hostile fixture: Necrobloom's grant is scoped to "your graveyard"
/// (Necrobloom's controller, P0's, per CR 611.3b — live, not latched). A land
/// with no printed Dredge sitting in P1's OWN graveyard must not receive the
/// grant even though P1 is the one drawing — proving the static's own
/// controller-relative filter resolution is correct, not merely that
/// registration happens to scope by the drawing player.
#[test]
fn necrobloom_dredge_does_not_cross_into_opponents_graveyard() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    scenario.add_land_to_graveyard(P1, "Island");
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P1);

    draw_one(&mut runner, P1);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "P0's Necrobloom must not grant dredge into P1's own graveyard, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_len(&runner, P1),
        hand_before + 1,
        "P1's draw must proceed normally"
    );
}

/// Row 2 (positive): accepting mills exactly 2 and returns the land to hand;
/// hand increases by exactly 1 (not 2) — the discriminating CR 614.6 signal
/// that the draw was REPLACED, not supplemented.
#[test]
fn necrobloom_dredge_accept_mills_two_and_returns_land_not_double_draw() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "fixture precondition: dredge must be offered before accepting it"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the granted dredge offer");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, land),
        Zone::Hand,
        "the dredged land must return to hand"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 (the dredged land) — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled (CR 702.52a: dredge 2)"
    );
}

/// Row 2 sibling: declining leaves the land in the graveyard and the draw
/// proceeds normally (the natural top-of-library card, not the land).
#[test]
fn necrobloom_dredge_decline_draws_normally_land_stays_in_graveyard() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    draw_one(&mut runner, P0);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let decline_idx = candidates
        .iter()
        .position(|c| c.description == "Decline")
        .expect("a Decline option must be offered");
    runner
        .act(GameAction::ChooseReplacement { index: decline_idx })
        .expect("decline the granted dredge offer");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, land),
        Zone::Graveyard,
        "a declined land must remain in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "decline must still draw exactly 1 card normally"
    );
}

/// Row 2 hostile fixture (CR 702.52b): with fewer than 2 library cards, dredge
/// must not be offered at all.
#[test]
fn necrobloom_dredge_not_offered_when_library_smaller_than_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Only Card"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C"]);
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    scenario.add_land_to_graveyard(P0, "Forest");
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    draw_one(&mut runner, P0);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 702.52b: a library smaller than N must not offer dredge, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "the draw must proceed normally when dredge is unavailable"
    );
}

/// Rows 3 + 4: printed AND granted dredge on separate graveyard objects both
/// surface together as a real CR 616.1 ordering prompt, distinguishably
/// labeled (Finding 1 — the granted candidate must not fall through to the
/// generic `"Replacement effect"` placeholder), and accepting one does not
/// consume or duplicate the other.
#[test]
fn necrobloom_printed_and_granted_dredge_both_surface_with_distinct_labels() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let printed = scenario
        .add_creature_to_graveyard(P0, "Test Dredger", 1, 1)
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 3")
        .id();
    let mut runner = scenario.build();

    draw_one(&mut runner, P0);

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a CR 616.1 ordering prompt with both dredge candidates, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "both the printed-dredge card and the granted-dredge land must surface together, got {candidates:?}"
    );

    let granted = candidates
        .iter()
        .find(|c| c.source_id == land)
        .expect("the granted-dredge candidate must be present");
    assert_ne!(
        granted.description, "Replacement effect",
        "Finding 1: the granted candidate must not fall through to the generic label"
    );
    assert!(
        granted.description.contains("Dredge 2"),
        "the granted candidate must interpolate its own resolved N, got {:?}",
        granted.description
    );

    let printed_candidate = candidates.iter().find(|c| c.source_id == printed).expect(
        "the printed-dredge candidate must also be present (not excluded by the granted branch)",
    );
    assert_ne!(
        printed_candidate.description, granted.description,
        "the two co-occurring dredge candidates must be distinguishably labeled"
    );

    resolve_preferring(&mut runner, land);
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, land),
        Zone::Hand,
        "the granted candidate must have resolved, returning the land to hand"
    );
    assert_eq!(
        zone_of(&runner, printed),
        Zone::Graveyard,
        "accepting the granted candidate must not consume or duplicate the printed candidate"
    );
}

/// Matrix row 4 — the SAME-OBJECT completion of the row above, on the shape
/// `granted_dredge_value`'s redundancy comparison actually exists for: one
/// LAND that both prints Dredge 2 (Dakmor Salvage) and receives Necrobloom's
/// granted dredge 2. Necrobloom's filter is "Land cards in your graveyard", so
/// a creature — which is what every landed printed+granted row uses — can never
/// reach this board.
///
/// CR 702.52a: the two values coincide, so the granted candidate is redundant
/// with the object-carried one and must NOT register a second time; exactly ONE
/// candidate is offered, as a solo optional Accept/Decline prompt rather than a
/// CR 616.1 ordering prompt carrying two per-candidate labels. Reverting
/// `granted_dredge_value`'s redundancy branch turns this row red with two
/// candidates — that branch is the code under test here.
#[test]
fn necrobloom_land_with_identical_printed_dredge_offers_one_candidate() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let dakmor = scenario
        .add_land_to_graveyard(P0, "Dakmor Salvage")
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 2")
        .id();
    let mut runner = scenario.build();
    assert_printed_dredge(&runner, dakmor, 2);
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    // Reach-guard: the board really offers something, and it is really
    // attributed to the land — "exactly one candidate" cannot pass vacuously on
    // a board that offered nothing.
    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the land's dredge offer, got {:?}",
            runner.state().waiting_for
        )
    });
    assert!(
        prompt.iter().all(|(source, _)| *source == dakmor),
        "every option must be attributed to the one graveyard land, got {prompt:?}"
    );
    let descriptions: Vec<&str> = prompt.iter().map(|(_, d)| d.as_str()).collect();
    assert_eq!(
        descriptions,
        vec!["Accept", "Decline"],
        "CR 702.52a: a granted dredge 2 identical to the printed dredge 2 must \
         collapse to the single object-carried candidate — a solo optional \
         prompt, not a two-label CR 616.1 ordering prompt, got {prompt:?}"
    );

    let accept = prompt
        .iter()
        .position(|(_, description)| description == "Accept")
        .unwrap_or_else(|| panic!("an Accept option must be offered, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("accept the land's dredge offer");
    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: nothing may be re-offered against the replaced draw, \
         got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, dakmor),
        Zone::Hand,
        "CR 702.52a: the accepted dredge must return THIS card to hand"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled (dredge 2)"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "CR 614.6: the draw was replaced, so hand grows by exactly 1"
    );
}

/// Matrix row 5 — the same land with a DIFFERING printed value (dredge 3 printed,
/// dredge 2 granted). The two are distinct instances of the ability, so both
/// must surface from ONE object: a real CR 616.1 ordering prompt with two
/// distinguishable labels, both attributed to the same `source_id`. Accepting
/// the granted one returns the land exactly once and mills the GRANTED count.
///
/// `base_scenario`'s 3-card library is exactly dredge 3's CR 702.52b threshold,
/// so both candidates are legal on this board.
#[test]
fn necrobloom_land_with_differing_printed_dredge_offers_both_candidates() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let dakmor = scenario
        .add_land_to_graveyard(P0, "Dakmor Salvage")
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 3")
        .id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a CR 616.1 ordering prompt with both same-object candidates, got {:?}",
            runner.state().waiting_for
        )
    });
    // Reach-guard: both candidates are present, both on ONE object, before
    // either accept — so neither accept direction can pass by picking the other.
    assert_eq!(
        prompt.len(),
        2,
        "CR 616.1: a printed dredge 3 and a granted dredge 2 on one object are two \
         distinct instances and must both surface, got {prompt:?}"
    );
    assert!(
        prompt.iter().all(|(source, _)| *source == dakmor),
        "both candidates must be attributed to the same land, got {prompt:?}"
    );
    let granted_idx = prompt
        .iter()
        .position(|(_, description)| description.contains("Dredge 2"))
        .unwrap_or_else(|| {
            panic!("the granted candidate must interpolate its OWN value, got {prompt:?}")
        });
    assert!(
        prompt
            .iter()
            .any(|(_, description)| description.starts_with("CR 702.52a")),
        "the object-carried printed candidate must keep its synthesized label, got {prompt:?}"
    );
    assert_ne!(
        prompt[0].1, prompt[1].1,
        "the two same-object candidates must read distinctly in the ordering prompt"
    );
    assert!(
        prompt
            .iter()
            .all(|(_, description)| description != "Replacement effect"),
        "neither candidate may fall through to the generic placeholder label, got {prompt:?}"
    );

    runner
        .act(GameAction::ChooseReplacement { index: granted_idx })
        .expect("order-pick the granted candidate");
    let accept_prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the Accept/Decline prompt for the chosen granted candidate, got {:?}",
            runner.state().waiting_for
        )
    });
    let accept = accept_prompt
        .iter()
        .position(|(_, description)| description != "Decline")
        .unwrap_or_else(|| panic!("an accept option must be offered, got {accept_prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("accept the granted candidate");
    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: the printed sibling on the same object may not be \
         re-offered against the replaced draw, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, dakmor),
        Zone::Hand,
        "CR 702.52a: the land must return to hand"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "the land must reach hand exactly ONCE — never +2 from both instances"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "the GRANTED candidate's own count (2) must be milled, not the printed 3"
    );
}

/// Matrix row 5, the other accept direction: picking the PRINTED candidate on
/// the same object mills its own count (3) and still returns the land exactly
/// once. Pairing both directions is what proves the choice is real rather than
/// one candidate silently standing in for the other.
#[test]
fn necrobloom_land_with_differing_printed_dredge_accepting_the_printed_one_returns_it_once() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let dakmor = scenario
        .add_land_to_graveyard(P0, "Dakmor Salvage")
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 3")
        .id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a CR 616.1 ordering prompt with both same-object candidates, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "reach-guard: both same-object candidates must be present before the \
         printed one is picked, got {prompt:?}"
    );
    let printed_idx = prompt
        .iter()
        .position(|(_, description)| description.starts_with("CR 702.52a"))
        .unwrap_or_else(|| {
            panic!("the object-carried printed candidate must be present, got {prompt:?}")
        });

    runner
        .act(GameAction::ChooseReplacement { index: printed_idx })
        .expect("order-pick the printed candidate");
    let accept_prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the Accept/Decline prompt for the chosen printed candidate, got {:?}",
            runner.state().waiting_for
        )
    });
    let accept = accept_prompt
        .iter()
        .position(|(_, description)| description != "Decline")
        .unwrap_or_else(|| panic!("an accept option must be offered, got {accept_prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("accept the printed candidate");
    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: the granted sibling on the same object may not be \
         re-offered against the replaced draw, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, dakmor),
        Zone::Hand,
        "CR 702.52a: the land must return to hand"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "the land must reach hand exactly ONCE — never +2 from both instances"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 3,
        "CR 702.52b: the PRINTED candidate's own count (3) must be milled, not the granted 2"
    );
}

/// CR 121.2a + CR 121.6b: a 2-card draw instruction (two INDIVIDUAL draw
/// units) with a granted-dredge land in the graveyard — the granted-mechanism
/// sibling of `multi_draw_dredges_one_of_two_units_other_draws_normally`
/// (`crates/engine/src/game/replacement.rs`), which covers the identical
/// per-unit mechanics for PRINTED dredge. Accepting the offer on unit 1
/// physically returns the land to hand, removing it from the graveyard, so
/// unit 2 must not re-offer the SAME land (no double-offer of the same land
/// within one instruction) and its individual draw must proceed normally.
///
/// Scope note (updated): the stronger fixture this note once deferred — TWO
/// independently dredgeable GRANTED lands live for the same draw — is now
/// FIXED and covered. The defect was that the granted-dredge registration
/// block in `find_applicable_replacements` pushed candidates without
/// consulting the registry's `ReplacementEvent::Draw` matcher, so the
/// CR 616.1f re-scan re-offered every sibling against a draw the accepted
/// dredge had already substituted away (CR 614.6). It is closed by that
/// block's registration gate, and the candidate-multiplicity tests at the end
/// of this module are its coverage: candidate multiplicity (N >= 2 candidates
/// from one grant, in both accept directions across printed and granted
/// candidate kinds) is a first-class coverage axis of this module.
///
/// Still open and unmeasured (deferred, not claimed safe): on a DECLINE,
/// `continue_replacement_impl`'s `abandon_active_post_replacement_drains` arm
/// drops a resident continuation installed by an earlier accepted candidate,
/// and its `ResidentDrainPolicy::Replace` arm overwrites one. Only the dredge
/// multi-candidate defect on Draw is closed here — not the Draw event in
/// general, because `draw_is_substituted_away` classifies a rescaled or
/// same-player `Effect::Draw` accept as a SURVIVING draw, which can leave
/// `count > 0` with a drain resident and a CR 616.1f-legitimate sibling. The
/// same question stands for any accept whose applier modifies rather than
/// annihilates its event. It is recorded here, at the `Replace`-policy comment
/// in `crates/engine/src/game/replacement.rs`, and in the PR body as a
/// follow-up.
#[test]
fn necrobloom_multi_draw_dredges_granted_land_other_draw_proceeds_normally() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 2,
        }))
        .expect("debug draw of 2 must succeed");

    // Unit 1: the granted-dredge land must be offered.
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected unit 1's dredge offer to pause on ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let accept_idx = candidates
        .iter()
        .position(|c| c.source_id == land && c.description != "Decline")
        .expect("the land's accept option must be present for unit 1");
    runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("accept unit 1's dredge offer");

    assert_eq!(
        zone_of(&runner, land),
        Zone::Hand,
        "the land must have been dredged back to hand for unit 1"
    );

    // Unit 2: the land already left the graveyard, so it must not be
    // re-offered — no dredge-eligible card remains, so the second individual
    // draw proceeds as an ordinary, unreplaced draw with no separate pause at
    // all (it completes automatically within the same action, since nothing
    // needs a player decision) — the discriminating "no double-offer" signal.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "with no dredge-eligible card left in the graveyard, unit 2 must not \
         pause on a ReplacementChoice at all, got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 2,
        "hand must increase by exactly 2: the dredged land plus unit 2's normal draw"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 3,
        "library must be reduced by exactly 3: 2 milled by unit 1's dredge plus 1 drawn by unit 2"
    );
}

/// Hostile fixture: Necrobloom leaves the battlefield (destroyed) AFTER the
/// granted-dredge `ReplacementChoice` has already been parked but BEFORE the
/// player submits `GameAction::ChooseReplacement`. This exercises the `None`
/// degradation paths `apply_single_replacement` / `continue_replacement_impl`
/// added for a grant that vanishes between registration and the player's
/// answer: submitting the stale "Accept" index must not panic, must not
/// fabricate a "Dredge 0" mill-and-return, and must not leave the draw
/// zeroed with no compensating effect — the event must fall back to
/// proceeding UNAFFECTED, exactly like a graceful decline.
#[test]
fn necrobloom_removed_mid_choice_stale_accept_degrades_to_normal_draw() {
    let mut scenario = base_scenario();
    let necrobloom = scenario
        .add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE)
        .id();
    let land = scenario.add_land_to_graveyard(P0, "Forest").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the granted-dredge offer to pause before Necrobloom is removed, got {:?}",
            runner.state().waiting_for
        );
    };
    let accept_idx = candidates
        .iter()
        .position(|c| c.source_id == land && c.description != "Decline")
        .expect("land's accept option must be present before Necrobloom is removed");

    // Put Necrobloom into its owner's graveyard now, with the choice still
    // parked, through the production replacement-aware zone pipeline (the
    // CR 704.5f zero-toughness SBA route: `ZoneMoveRequest::state_based_action`
    // → `ProposedEvent::ZoneChange`) so the grant is gone by the time the stale
    // "Accept" index is submitted.
    let mut events = Vec::new();
    let needs_choice = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::state_based_action(necrobloom, Zone::Graveyard),
        &mut events,
    );
    assert!(
        !needs_choice,
        "no replacement applies to Necrobloom's death; the move must complete immediately"
    );
    assert_eq!(
        zone_of(&runner, necrobloom),
        Zone::Graveyard,
        "Necrobloom must have left the battlefield before the stale choice is submitted"
    );

    runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("submitting the stale accept index must not error or panic");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the stale choice must resolve cleanly, not re-park or wedge, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        zone_of(&runner, land),
        Zone::Graveyard,
        "with the grant gone, the land must NOT be returned to hand for free"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "the draw must proceed normally (unaffected), not be zeroed and not doubled"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 1,
        "exactly 1 card must be drawn from the library — no mill occurred"
    );
}

// --- Candidate multiplicity: two or more granted-dredge candidates per draw ---

/// Matrix row 1 — the primary multi-candidate regression. TWO granted-dredge
/// lands are live for one draw: the CR 616.1 ordering prompt must carry both,
/// and accepting the chosen one must return THAT land to hand (CR 702.52a),
/// leave the sibling in the graveyard, mill exactly 2, and leave no further
/// prompt — the accepted dredge substituted the draw away (CR 614.6), so under
/// CR 616.1f no other dredge "would now be applicable".
///
/// Before the registration gate, the sibling was re-offered against the dead
/// draw and answering that stray prompt either abandoned the accepted dredge's
/// continuation (decline: no draw, no mill, no return) or overwrote it with the
/// sibling's (accept: the chosen land lost) — both CR 614.5 violations. The
/// `Zone::Hand` assertion below is what flips back to `Graveyard` on a revert.
#[test]
fn necrobloom_two_granted_dredge_lands_accepting_one_leaves_the_other_in_graveyard() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let accepted = scenario.add_land_to_graveyard(P0, "Forest").id();
    let sibling = scenario.add_land_to_graveyard(P0, "Island").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    // Positive reach-guard: the CR 616.1 ordering path really was reached with
    // two live granted candidates.
    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a CR 616.1 ordering prompt with both granted-dredge lands, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "both graveyard lands must surface as granted-dredge candidates, got {prompt:?}"
    );

    resolve_preferring(&mut runner, accepted);
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, accepted),
        Zone::Hand,
        "CR 702.52a: the accepted land must return to hand"
    );
    assert_eq!(
        zone_of(&runner, sibling),
        Zone::Graveyard,
        "the sibling granted-dredge land must stay untouched in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 (the dredged land) — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled (CR 702.52a: dredge 2)"
    );
    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 616.1f: no replacement prompt may remain once the draw was replaced, got {leftover:?}"
    );
}

/// Matrix row 2 — a NEGATIVE CONTROL that passes at HEAD too, and must not be
/// deleted as dead weight: it pins the gate's precision. Order-picking A and
/// then DECLINING it leaves `count` at 1, so the registration gate does not
/// fire at all and B stays applicable exactly as CR 616.1e ("any of the
/// applicable effects may be chosen") and CR 616.1f require.
///
/// The deciding conjunct here is `already_applied` (CR 614.5), not the new
/// gate: A is kept off the follow-up prompt purely by the applied set, which is
/// what the `source_id == b` assertion measures — the
/// `/add-replacement-effect` checklist's "the applied set must prevent
/// reapplication of exactly the selected replacement".
#[test]
fn necrobloom_two_granted_declining_one_still_offers_the_other() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "reach-guard: both granted lands must be live before the decline, got {prompt:?}"
    );
    let order_idx = prompt
        .iter()
        .position(|(source, _)| *source == a)
        .unwrap_or_else(|| panic!("A must be orderable, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: order_idx })
        .expect("order-picking A must be accepted");

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected A's accept/decline prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    let decline_idx = prompt
        .iter()
        .position(|(source, description)| *source == a && description == "Decline")
        .unwrap_or_else(|| panic!("A must offer a Decline option, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: decline_idx })
        .expect("declining A must be accepted");

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "CR 616.1e + CR 616.1f: a declined dredge leaves the draw live, so B must \
             still be offered, got {:?}",
            runner.state().waiting_for
        )
    });
    assert!(
        prompt.iter().all(|(source, _)| *source == b),
        "CR 614.5: only B may remain after A was declined, got {prompt:?}"
    );
    let accept_idx = prompt
        .iter()
        .position(|(source, description)| *source == b && description != "Decline")
        .unwrap_or_else(|| panic!("B must offer an accept option, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("accepting B must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, b),
        Zone::Hand,
        "CR 702.52a: the accepted sibling must return to hand"
    );
    assert_eq!(
        zone_of(&runner, a),
        Zone::Graveyard,
        "the declined land must stay in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled (CR 702.52a: dredge 2)"
    );
}

/// Matrix row 3 — the other NEGATIVE CONTROL that passes at HEAD: declining
/// BOTH granted candidates must leave the draw untouched, so the ordinary draw
/// happens (library −1, both lands still in the graveyard). This is the
/// empty-path control for the whole family; like row 2 it is deliberately
/// retained, not dead weight.
#[test]
fn necrobloom_two_granted_declining_both_draws_normally() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "reach-guard: both granted lands must be live before either decline, got {prompt:?}"
    );
    let order_idx = prompt
        .iter()
        .position(|(source, _)| *source == a)
        .unwrap_or_else(|| panic!("A must be orderable, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: order_idx })
        .expect("order-picking A must be accepted");

    for expected_source in [a, b] {
        let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
            panic!(
                "CR 616.1f: {expected_source:?} must still be offered before it is declined, \
                 got {:?}",
                runner.state().waiting_for
            )
        });
        let decline_idx = prompt
            .iter()
            .position(|(source, description)| {
                *source == expected_source && description == "Decline"
            })
            .unwrap_or_else(|| {
                panic!("{expected_source:?} must offer a Decline option, got {prompt:?}")
            });
        runner
            .act(GameAction::ChooseReplacement { index: decline_idx })
            .expect("declining must be accepted");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, a),
        Zone::Graveyard,
        "a declined land must stay in the graveyard"
    );
    assert_eq!(
        zone_of(&runner, b),
        Zone::Graveyard,
        "a declined land must stay in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "declining every dredge must still draw exactly 1 card"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 1,
        "CR 614.6 does not apply: nothing replaced the draw, so exactly 1 card leaves the library"
    );
}

/// Matrix row 4 — the fix is in N, not in 2. THREE granted-dredge lands: the
/// ordering prompt carries all three, and accepting one leaves the other two in
/// the graveyard with no stray prompt. Before the gate, the CR 616.1f re-scan
/// parked a two-candidate ordering prompt against the dead draw.
#[test]
fn necrobloom_three_granted_dredge_lands_only_the_accepted_one_moves() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let accepted = scenario.add_land_to_graveyard(P0, "Island").id();
    let c = scenario.add_land_to_graveyard(P0, "Mountain").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a 3-candidate CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        3,
        "reach-guard: all three granted lands must surface, got {prompt:?}"
    );

    for stage in ["order-pick", "accept"] {
        let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
            panic!(
                "expected the {stage} prompt for the chosen land, got {:?}",
                runner.state().waiting_for
            )
        });
        let idx = prompt
            .iter()
            .position(|(source, description)| *source == accepted && description != "Decline")
            .unwrap_or_else(|| {
                panic!("the chosen land must offer a non-Decline option at {stage}, got {prompt:?}")
            });
        runner
            .act(GameAction::ChooseReplacement { index: idx })
            .expect("the choice must be accepted");
    }

    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: no sibling may be re-offered once the draw was \
         substituted away, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, accepted),
        Zone::Hand,
        "CR 702.52a: the accepted land must return to hand"
    );
    assert_eq!(
        zone_of(&runner, a),
        Zone::Graveyard,
        "the first sibling must stay in the graveyard"
    );
    assert_eq!(
        zone_of(&runner, c),
        Zone::Graveyard,
        "the second sibling must stay in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled (CR 702.52a: dredge 2)"
    );
}

/// Matrix row 5 — a multi-authority fixture: TWO candidate *sources* claim the
/// same draw (two virtual granted candidates plus one object-carried printed
/// one). Accepting a granted candidate must re-offer NEITHER the granted
/// sibling (the new registration gate) NOR the printed candidate (the
/// pre-existing matcher gate in `object_replacement_candidate_applies`). At
/// HEAD only the granted sibling came back, which is what isolated the granted
/// registration block as the sole defect site.
#[test]
fn necrobloom_two_granted_plus_printed_accepting_a_granted_one_reoffers_nothing() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let accepted = scenario.add_land_to_graveyard(P0, "Forest").id();
    let sibling = scenario.add_land_to_graveyard(P0, "Island").id();
    let printed = scenario
        .add_creature_to_graveyard(P0, "Test Dredger", 1, 1)
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 3")
        .id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a 3-candidate CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        3,
        "reach-guard: two granted candidates and one printed candidate must all \
         surface, got {prompt:?}"
    );
    // Reach-guard on the fixture's KIND mix: the printed candidate carries the
    // synthesized CR 702.52a description, the granted ones their own label.
    assert!(
        prompt.iter().any(
            |(source, description)| *source == printed && description.starts_with("CR 702.52a")
        ),
        "the fixture must really contain an object-carried printed candidate, got {prompt:?}"
    );
    assert!(
        prompt
            .iter()
            .filter(
                |(source, description)| (*source == accepted || *source == sibling)
                    && description.contains("Dredge 2")
            )
            .count()
            == 2,
        "the fixture must really contain two granted candidates, got {prompt:?}"
    );

    for stage in ["order-pick", "accept"] {
        let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
            panic!(
                "expected the {stage} prompt for the chosen granted land, got {:?}",
                runner.state().waiting_for
            )
        });
        let idx = prompt
            .iter()
            .position(|(source, description)| *source == accepted && description != "Decline")
            .unwrap_or_else(|| {
                panic!("the chosen land must offer a non-Decline option at {stage}, got {prompt:?}")
            });
        runner
            .act(GameAction::ChooseReplacement { index: idx })
            .expect("the choice must be accepted");
    }

    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: neither the granted sibling nor the printed candidate \
         may be re-offered against the replaced draw, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, accepted),
        Zone::Hand,
        "CR 702.52a: the accepted land must return to hand"
    );
    assert_eq!(
        zone_of(&runner, sibling),
        Zone::Graveyard,
        "the granted sibling must stay in the graveyard"
    );
    assert_eq!(
        zone_of(&runner, printed),
        Zone::Graveyard,
        "the printed-dredge card must stay in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 2,
        "exactly 2 cards must be milled by the accepted granted dredge 2"
    );
}

/// Matrix row 5b — the mirror of row 5, and the row that proves the gate keys
/// on the EVENT's payload rather than on which family produced the accept:
/// here the count is zeroed by the PRINTED applier (`draw_is_substituted_away`)
/// on a real board, and both granted lands must still be refused by the granted
/// registration block on the CR 616.1f re-scan. `base_scenario`'s 3-card
/// library is exactly Dredge 3's CR 702.52b threshold, so the printed accept is
/// legal and mills the library to 0.
#[test]
fn necrobloom_two_granted_plus_printed_accepting_the_printed_one_reoffers_neither_granted_land() {
    let mut scenario = base_scenario();
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let printed = scenario
        .add_creature_to_graveyard(P0, "Test Dredger", 1, 1)
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 3")
        .id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected a 3-candidate CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        3,
        "reach-guard: two granted candidates and one printed candidate must all \
         surface, got {prompt:?}"
    );

    // The row is meaningless unless the PRINTED candidate is the one accepted.
    // The ordering pick is made by the candidate's own printed CR 702.52a
    // label, and the chosen index's `source_id` is then asserted to be the
    // printed object — so the row cannot pass by accidentally accepting a
    // granted land.
    let order_idx = prompt
        .iter()
        .position(|(_, description)| description.starts_with("CR 702.52a"))
        .unwrap_or_else(|| {
            panic!("the printed candidate must be orderable by its own label, got {prompt:?}")
        });
    assert_eq!(
        prompt[order_idx].0, printed,
        "the candidate carrying the printed dredge label must be the printed object, got {prompt:?}"
    );
    runner
        .act(GameAction::ChooseReplacement { index: order_idx })
        .expect("order-picking the printed candidate must be accepted");

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the printed candidate's accept/decline prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    let accept_idx = prompt
        .iter()
        .position(|(source, description)| *source == printed && description != "Decline")
        .unwrap_or_else(|| {
            panic!("the printed candidate must offer an accept option, got {prompt:?}")
        });
    runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("accepting the printed candidate must be accepted");

    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 614.6 + CR 616.1f: the printed applier zeroed the draw, so neither granted \
         land may be re-offered against it, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, printed),
        Zone::Hand,
        "CR 702.52a: the accepted printed-dredge card must return to hand"
    );
    assert_eq!(
        zone_of(&runner, a),
        Zone::Graveyard,
        "the first granted land must stay in the graveyard"
    );
    assert_eq!(
        zone_of(&runner, b),
        Zone::Graveyard,
        "the second granted land must stay in the graveyard"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "hand must increase by exactly 1 — CR 614.6, the draw was replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 3,
        "exactly 3 cards must be milled (CR 702.52a: the printed dredge 3)"
    );
}

/// Matrix row 6 — CR 121.2 + CR 121.2a: a 2-card draw instruction is two
/// INDIVIDUAL draw units, and the gate must be scoped to the unit whose count
/// was consumed. Unit 1 offers both granted lands and accepts A; unit 2 is a
/// FRESH event with `count == 1`, so B must be offered there and accepted
/// normally. A gate that leaked across units would show an empty unit-2 prompt.
/// This is the N >= 2 sibling of
/// `necrobloom_multi_draw_dredges_granted_land_other_draw_proceeds_normally`.
#[test]
fn necrobloom_two_granted_two_card_draw_one_per_unit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C"]);
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = runner.state().players[0].library.len();

    // `draw_one` hardcodes `count: 1`, so the instruction is issued inline.
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 2,
        }))
        .expect("debug draw of 2 must succeed");

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected unit 1's CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "reach-guard: unit 1 must offer both granted lands, got {prompt:?}"
    );

    for stage in ["order-pick", "accept"] {
        let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
            panic!(
                "expected unit 1's {stage} prompt for A, got {:?}",
                runner.state().waiting_for
            )
        });
        let idx = prompt
            .iter()
            .position(|(source, description)| *source == a && description != "Decline")
            .unwrap_or_else(|| {
                panic!("A must offer a non-Decline option at {stage}, got {prompt:?}")
            });
        runner
            .act(GameAction::ChooseReplacement { index: idx })
            .expect("the choice must be accepted");
    }

    assert_eq!(
        zone_of(&runner, a),
        Zone::Hand,
        "CR 702.52a: unit 1's accepted land must return to hand"
    );

    // Unit 2 is a fresh individual draw (CR 121.2), so B must be offered.
    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "CR 121.2 + CR 121.2a: unit 2 is a fresh draw and must still offer B, got {:?}",
            runner.state().waiting_for
        )
    });
    assert!(
        prompt.iter().all(|(source, _)| *source == b),
        "unit 2 must offer ONLY B (A is already in hand), got {prompt:?}"
    );
    let accept_idx = prompt
        .iter()
        .position(|(source, description)| *source == b && description != "Decline")
        .unwrap_or_else(|| panic!("B must offer an accept option in unit 2, got {prompt:?}"));
    runner
        .act(GameAction::ChooseReplacement { index: accept_idx })
        .expect("accepting B in unit 2 must be accepted");

    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 616.1f: nothing may remain once both units' draws were replaced, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, b),
        Zone::Hand,
        "CR 702.52a: unit 2's accepted land must return to hand"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 2,
        "hand must increase by exactly 2 (both dredged lands) — CR 614.6, both draws were replaced"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 4,
        "exactly 4 cards must be milled (dredge 2 twice)"
    );
}

/// Matrix row 7 — a hostile boundary fixture stacking CR 702.52b on CR 614.6.
/// With a library of exactly 2, both granted lands are legally dredgeable, but
/// accepting one mills the library to 0: the sibling must not be offered
/// against the replaced draw, and nothing may be drawn from the emptied
/// library. At HEAD the sibling's accept/decline prompt came back anyway.
#[test]
fn necrobloom_two_granted_library_exactly_two_sibling_not_offered_after_accept() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Only A", "Only B"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C"]);
    scenario.add_creature_from_oracle(P0, "The Necrobloom", 2, 7, NECROBLOOM_ORACLE);
    let accepted = scenario.add_land_to_graveyard(P0, "Forest").id();
    let sibling = scenario.add_land_to_graveyard(P0, "Island").id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    draw_one(&mut runner, P0);

    let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
        panic!(
            "expected the CR 616.1 ordering prompt, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(
        prompt.len(),
        2,
        "reach-guard: with library 2 both granted lands are legally dredgeable \
         (CR 702.52b), got {prompt:?}"
    );

    for stage in ["order-pick", "accept"] {
        let prompt = replacement_prompt(&runner).unwrap_or_else(|| {
            panic!(
                "expected the {stage} prompt for the chosen land, got {:?}",
                runner.state().waiting_for
            )
        });
        let idx = prompt
            .iter()
            .position(|(source, description)| *source == accepted && description != "Decline")
            .unwrap_or_else(|| {
                panic!("the chosen land must offer a non-Decline option at {stage}, got {prompt:?}")
            });
        runner
            .act(GameAction::ChooseReplacement { index: idx })
            .expect("the choice must be accepted");
    }

    let leftover = replacement_prompt(&runner);
    assert!(
        leftover.is_none(),
        "CR 702.52b + CR 614.6: with the library emptied by the accepted mill, the \
         sibling must not be offered against the dead draw, got {leftover:?}"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, accepted),
        Zone::Hand,
        "CR 702.52a: the accepted land must return to hand"
    );
    assert_eq!(
        zone_of(&runner, sibling),
        Zone::Graveyard,
        "the sibling must stay in the graveyard"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        0,
        "the 2-card library must be entirely milled by the accepted dredge 2"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "CR 614.6: hand must increase by exactly 1 (the dredged land) — nothing was \
         drawn from the emptied library"
    );
}
