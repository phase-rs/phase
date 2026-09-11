//! Descendants' Path — Enchantment {2}{G}, verbatim Oracle:
//!   "At the beginning of your upkeep, reveal the top card of your library. If
//!    it's a creature card that shares a creature type with a creature you
//!    control, you may cast it without paying its mana cost. If you don't cast
//!    it, put it on the bottom of your library."
//!
//! TWO-LAYER DEFECT this file guards (both layers must be present to pass):
//!
//!  1. PARSER layer (`parser/oracle_effect/conditions.rs`): the postnominal
//!     "that shares a creature type with a creature you control" clause folds
//!     onto the free-cast leg's gate as `RevealedHasCardType { card_types:
//!     [Creature], additional_filter: Some(SharesQuality{ CreatureType, Shares,
//!     reference: "a creature you control" }) }`. Reverting it makes the gate
//!     `RevealedHasCardType{[Creature]}` — unconditionally true for ANY revealed
//!     creature.
//!  2. RUNTIME layer (`game/filter.rs::object_shares_quality_with_reference_filter`,
//!     via the new `reference_leg_admits`): CR 109.2 — a zone-less "a creature
//!     you control" reference means a permanent ON THE BATTLEFIELD. Before the
//!     fix the fallback scan admitted any non-stack object, so the revealed
//!     LIBRARY card satisfied its own reference and shared a creature type with
//!     itself ⇒ the gate was always true even on an empty battlefield.
//!
//! DISCRIMINATING ROWS:
//!   * Row A (NEGATIVE, primary): battlefield creature subtype disjoint from the
//!     library card's ⇒ never offered; card put on the bottom. Reverting EITHER
//!     layer flips it (parser: gate becomes always-true; runtime: library card
//!     self-matches).
//!   * Row B (POSITIVE reach-guard): shared subtype ⇒ offered, answered, cast
//!     onto the battlefield. Without it Rows A/C could pass vacuously.
//!   * Row C (zone-rule-specific): ZERO battlefield creatures, but a same-type
//!     "creature you control" sits in HAND ⇒ never offered. Flips ONLY on the
//!     runtime zone rule (self-exclusion alone would still match the hand decoy).
//!
//! CR 109.2 verified verbatim in docs/MagicCompRules.txt: "a permanent … on the
//! battlefield". CR 205.3m: creature types are the shared subtype list. CR
//! 503.1 / 504.1: the drive stops inside the upkeep step, before the CR 504.1
//! draw, so a negative row cannot deck P1 and read healthy while measuring
//! nothing.

use engine::game::scenario::{GameRunner, GameScenario, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text. The card NAME is irrelevant to the parse (the trigger
/// is authored entirely from this text), but is kept exact for provenance.
const DESCENDANTS_PATH_ORACLE: &str = "At the beginning of your upkeep, reveal the top card of your library. If it's a creature card that shares a creature type with a creature you control, you may cast it without paying its mana cost. If you don't cast it, put it on the bottom of your library.";

/// Seed a CREATURE card on top of `player`'s library carrying real creature
/// SUBTYPES on both `card_types` and `base_card_types`.
///
/// Distinct from `runo_stromkirk_reveal_transform_gate.rs::seed_library_top`,
/// which seeds only `core_types`/`base_card_types.core_types`/`mana_cost` and NO
/// subtypes. `SharesQuality{CreatureType}` compares SUBTYPES (CR 205.3m), so a
/// subtype-less seed would make the positive row share nothing and pass
/// vacuously. Both `card_types.subtypes` and `base_card_types.subtypes` are set
/// because the gate's type leg and property leg read different fields.
fn seed_library_top_creature(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    subtypes: &[&str],
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types.core_types = vec![CoreType::Creature];
    obj.card_types.subtypes = subtypes.iter().map(|s| (*s).to_string()).collect();
    obj.base_card_types.subtypes = subtypes.iter().map(|s| (*s).to_string()).collect();
    obj.mana_cost = ManaCost::generic(2);
    let ps = state.players.iter_mut().find(|p| p.id == player).unwrap();
    ps.library.retain(|&o| o != id);
    ps.library.insert(0, id);
    id
}

/// Drive P1's upkeep trigger to the end of ITS OWN resolution and stop there.
///
/// Returns whether the "you may cast it" optional was ever offered. Breaks the
/// instant the revealed card reaches the battlefield (the positive path resolves
/// the free cast) or when the stack empties with no pending optional (the
/// negative path put the card on the bottom of the library). Never falls through
/// to the CR 504.1 upkeep draw, so a negative row cannot deck P1 (CR 704.5b).
fn drive_upkeep_free_cast(runner: &mut GameRunner, top: ObjectId) -> bool {
    assert!(
        !runner.state().stack.is_empty(),
        "reach guard: the upkeep trigger must already be on the stack when the drive \
         starts, or this loop returns immediately and the row measures nothing"
    );
    let mut offered = false;
    for _ in 0..40 {
        // Positive path: the free cast has resolved onto the battlefield.
        if runner.state().objects.get(&top).map(|o| o.zone) == Some(Zone::Battlefield) {
            break;
        }
        // Negative path: the trigger has left the stack and nothing is parked
        // in a mid-resolution choice; the next PassPriority would leave the
        // upkeep step (CR 503.1 -> CR 504.1). Stop before that draw.
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                if runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    offered
}

/// Number of creatures `player` controls on the battlefield.
fn battlefield_creature_count(state: &GameState, player: PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

/// Build the shared board: Descendants' Path (an Enchantment, so it never
/// self-satisfies "a creature you control") under P1, two noncreature filler
/// cards beneath the seeded top so no incidental draw can deck P1, and the
/// scenario advanced into P1's own upkeep (the trigger is OnlyDuringYourTurn).
/// Shared tail: build the runner, seed the library top creature, advance into
/// P1's upkeep, and assert the shared active-player reach guard.
fn scenario_into_upkeep(scenario: GameScenario, top_subtypes: &[&str]) -> GameRunner {
    let mut runner = scenario.build();
    // CR 205.3m: `SharedQuality::CreatureType` counts only subtypes present in
    // `all_creature_types`. The scenario builder does not derive it from
    // inline-built cards, so seed the creature types this file uses — without
    // this the SharesQuality comparison is empty-vs-empty and every row passes
    // vacuously (measured: the positive row went RED, proving the guard).
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Wall".to_string()];
    seed_library_top_creature(runner.state_mut(), P1, "Revealed Creature", top_subtypes);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the trigger is OnlyDuringYourTurn, so this must be P1's own upkeep"
    );
    runner
}

/// Fetch the seeded library top by name (its `ObjectId` after `build()`).
fn revealed_creature_id(state: &GameState) -> ObjectId {
    state
        .objects
        .iter()
        .find(|(_, o)| o.name == "Revealed Creature")
        .map(|(id, _)| *id)
        .expect("seeded 'Revealed Creature' must exist")
}

/// Shared SETUP guard for every row (asserted BEFORE driving): the seeded top
/// carries the intended subtypes (so "shares"/"disjoint" is real, not two empty
/// sets trivially sharing nothing). The reveal-ledger guard is asserted
/// separately AFTER the drive (the reveal only happens during trigger
/// resolution), via `assert_revealed_exactly_one`.
fn assert_seeded_subtypes(runner: &GameRunner, top: ObjectId, expected_subtypes: &[&str]) {
    let got: Vec<String> = runner.state().objects[&top].card_types.subtypes.clone();
    let want: Vec<String> = expected_subtypes.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        got, want,
        "reach guard: the revealed creature must carry its seeded subtypes, or the \
         SharesQuality comparison is vacuous"
    );
}

/// Reveal-ledger reach guard, asserted AFTER the drive: exactly one card was
/// revealed, so the gate had a real subject to read. A row where the trigger
/// silently no-op'd (e.g. fired under the wrong player) would leave this empty.
fn assert_revealed_exactly_one(runner: &GameRunner) {
    assert_eq!(
        runner.state().last_revealed_ids.len(),
        1,
        "reach guard: the reveal step must have produced exactly one card for the \
         gate to read; got {:?}",
        runner.state().last_revealed_ids
    );
}

/// Row A — NEGATIVE (primary discriminator). One battlefield creature whose
/// subtype (`Wall`) is DISJOINT from the revealed creature's (`Goblin`). The
/// free cast must NEVER be offered; the revealed creature stays off the
/// battlefield and is put on the bottom of the library. Reverting EITHER the
/// parser arm (gate becomes always-true) OR the runtime zone rule (library card
/// self-matches) flips this row.
#[test]
fn disjoint_battlefield_creature_type_never_offers_free_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Descendants' Path", DESCENDANTS_PATH_ORACLE);
    let wall = scenario
        .add_creature(P1, "Wall Sentinel", 0, 4)
        .with_subtypes(vec!["Wall"])
        .id();
    scenario.with_library_top(P1, &["Filler One", "Filler Two"]);
    let mut runner = scenario_into_upkeep(scenario, &["Goblin"]);
    let top = revealed_creature_id(runner.state());

    assert_seeded_subtypes(&runner, top, &["Goblin"]);
    assert_eq!(
        runner.state().objects[&wall].card_types.subtypes,
        vec!["Wall".to_string()],
        "reach guard: the battlefield creature must carry its disjoint subtype"
    );
    assert_eq!(
        battlefield_creature_count(runner.state(), P1),
        1,
        "reach guard: exactly the disjoint Wall is on P1's battlefield"
    );

    let offered = drive_upkeep_free_cast(&mut runner, top);

    assert_revealed_exactly_one(&runner);
    assert!(
        !offered,
        "a disjoint battlefield creature must NOT satisfy 'shares a creature type', \
         so the free cast must never be offered"
    );
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "reach guard: the drive must stop inside the upkeep step it measures"
    );
    assert_ne!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "the revealed creature must not have been cast onto the battlefield"
    );
    let library = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .library;
    assert_eq!(
        library.last().copied(),
        Some(top),
        "the un-cast creature must be put on the BOTTOM of P1's library"
    );
}

/// Row B — POSITIVE reach-guard. A shared subtype (`Goblin`) on the battlefield
/// makes the gate true; the free cast is offered, answered, and the revealed
/// creature is cast onto P1's battlefield. This proves the harness CAN offer and
/// resolve the free cast, so the negatives in Rows A/C are non-vacuous.
#[test]
fn shared_battlefield_creature_type_offers_and_casts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Descendants' Path", DESCENDANTS_PATH_ORACLE);
    let goblin = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    scenario.with_library_top(P1, &["Filler One", "Filler Two"]);
    let mut runner = scenario_into_upkeep(scenario, &["Goblin"]);
    let top = revealed_creature_id(runner.state());

    assert_seeded_subtypes(&runner, top, &["Goblin"]);
    assert_eq!(
        runner.state().objects[&goblin].card_types.subtypes,
        vec!["Goblin".to_string()],
        "reach guard: the battlefield creature must carry the shared subtype"
    );

    let offered = drive_upkeep_free_cast(&mut runner, top);

    assert_revealed_exactly_one(&runner);
    assert!(
        offered,
        "a shared battlefield creature type must satisfy the gate and offer the \
         free cast"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "the revealed creature must be cast onto the battlefield"
    );
    assert_eq!(
        runner.state().objects[&top].controller,
        P1,
        "the cast creature must be controlled by P1"
    );
}

/// Row C — zone-rule-specific discriminator. ZERO creatures on P1's battlefield,
/// but a same-type ("Goblin") creature you control sits in P1's HAND. The gate
/// must be false: CR 109.2 restricts "a creature you control" to the
/// battlefield, so a hand creature does not satisfy it. Flips ONLY on the
/// runtime zone rule — mere subject self-exclusion would still let the hand
/// decoy match and the free cast would be offered.
#[test]
fn hand_creature_you_control_does_not_satisfy_battlefield_reference() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Descendants' Path", DESCENDANTS_PATH_ORACLE);
    let hand_decoy = scenario
        .add_creature_to_hand(P1, "Goblin In Hand", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    scenario.with_library_top(P1, &["Filler One", "Filler Two"]);
    let mut runner = scenario_into_upkeep(scenario, &["Goblin"]);
    let top = revealed_creature_id(runner.state());

    assert_seeded_subtypes(&runner, top, &["Goblin"]);
    assert_eq!(
        battlefield_creature_count(runner.state(), P1),
        0,
        "reach guard: Row C must have ZERO creatures on P1's battlefield"
    );
    assert_eq!(
        runner.state().objects[&hand_decoy].zone,
        Zone::Hand,
        "reach guard: the same-type decoy must be a creature you control in HAND"
    );
    assert_eq!(
        runner.state().objects[&hand_decoy].card_types.subtypes,
        vec!["Goblin".to_string()],
        "reach guard: the hand decoy must carry the same subtype as the revealed card"
    );

    let offered = drive_upkeep_free_cast(&mut runner, top);

    assert_revealed_exactly_one(&runner);
    assert!(
        !offered,
        "a 'creature you control' in HAND must NOT satisfy a battlefield reference \
         (CR 109.2), so the free cast must never be offered"
    );
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "reach guard: the drive must stop inside the upkeep step it measures"
    );
    assert_ne!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "the revealed creature must not have been cast onto the battlefield"
    );
    let library = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .library;
    assert_eq!(
        library.last().copied(),
        Some(top),
        "the un-cast creature must be put on the BOTTOM of P1's library"
    );
}
