//! Myriad Landscape — "that share a land type" is a restriction on the chosen
//! cards AS A GROUP, not a per-card filter.
//!
//! Oracle text:
//!
//! > This land enters tapped.
//! > {T}: Add {C}.
//! > {2}, {T}, Sacrifice this land: Search your library for up to two basic land
//! > cards that share a land type, put them onto the battlefield tapped, then
//! > shuffle.
//!
//! Ruling 2018-03-16: "You can choose to find one basic land card with Myriad
//! Landscape's last ability."
//!
//! Before the fix the marker stayed in the per-card filter, where it is
//! vacuously true, so Forest + Island resolved. Now the parser lifts it into
//! `SearchSelectionConstraint::SharesQualities { relation: Shares }` and the
//! `SearchChoice` submission guard rejects a pair with no land type in common
//! (CR 608.2d + CR 205.3i). Wastes has no land type, so two Wastes share none.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{SearchSelectionConstraint, SharedQuality, SharedQualityRelation};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MYRIAD_LANDSCAPE: &str = "This land enters tapped.\n{T}: Add {C}.\n{2}, {T}, Sacrifice this land: Search your library for up to two basic land cards that share a land type, put them onto the battlefield tapped, then shuffle.";

const BASIC: &[Supertype] = &[Supertype::Basic];
const BASIC_SNOW: &[Supertype] = &[Supertype::Basic, Supertype::Snow];

/// One staged library land: name, land subtypes, supertypes.
type LibraryLand = (&'static str, &'static [&'static str], &'static [Supertype]);

const FOREST_A: LibraryLand = ("Forest", &["Forest"], BASIC);
const FOREST_B: LibraryLand = ("Forest", &["Forest"], BASIC);
const ISLAND: LibraryLand = ("Island", &["Island"], BASIC);
const WASTES_A: LibraryLand = ("Wastes", &[], BASIC);
const WASTES_B: LibraryLand = ("Wastes", &[], BASIC);
const SNOW_COVERED_FOREST: LibraryLand = ("Snow-Covered Forest", &["Forest"], BASIC_SNOW);

/// Stage Myriad Landscape on the battlefield with `{2}` in P0's pool and the
/// given lands in P0's library (ids returned in `library` order), then activate
/// the search ability (index 1; index 0 is `{T}: Add {C}`). The default search
/// policy parks the resolution at the `SearchChoice`.
fn stage_and_activate(library: &[LibraryLand]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let myriad = scenario
        .add_land_from_oracle(P0, "Myriad Landscape", MYRIAD_LANDSCAPE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let ids: Vec<ObjectId> = library
        .iter()
        .map(|(name, subtypes, _)| {
            scenario
                .add_land_to_library_top(P0, name)
                .with_subtypes(subtypes.to_vec())
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for (id, (_, _, supertypes)) in ids.iter().zip(library) {
        let obj = runner.state_mut().objects.get_mut(id).unwrap();
        obj.card_types.supertypes = supertypes.to_vec();
        obj.base_card_types = obj.card_types.clone();
    }

    runner.activate(myriad, 1).resolve();
    (runner, myriad, ids)
}

/// Positive reach guard shared by every row: the search ability was activated
/// (Myriad was sacrificed as a cost, CR 113.7a) and resolution parked at the
/// `SearchChoice` carrying the lifted group constraint, offering every staged
/// basic. Returns the offered cards.
fn assert_parked_at_shares_search(
    runner: &GameRunner,
    myriad: ObjectId,
    staged: &[ObjectId],
) -> Vec<ObjectId> {
    let WaitingFor::SearchChoice {
        player,
        count,
        up_to,
        constraint,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach guard: Myriad Landscape's search must park a SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 2, "\"up to two basic land cards\"");
    assert!(*up_to, "\"up to two\" permits finding fewer");
    assert_eq!(
        constraint,
        &SearchSelectionConstraint::SharesQualities {
            qualities: vec![SharedQuality::LandType],
            relation: SharedQualityRelation::Shares,
        },
        "\"that share a land type\" must be a set constraint"
    );
    for id in staged {
        assert!(
            cards.contains(id),
            "every staged basic is a legal candidate"
        );
    }
    assert_eq!(
        runner.state().objects[&myriad].zone,
        Zone::Graveyard,
        "Myriad Landscape was sacrificed as a cost; the ability still resolves (CR 113.7a)"
    );
    cards.clone()
}

fn select(runner: &mut GameRunner, cards: &[ObjectId]) -> bool {
    runner
        .act(GameAction::SelectCards {
            cards: cards.to_vec(),
        })
        .is_ok()
}

/// After a completed search the stack is empty and P0 holds priority.
fn assert_search_completed(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the search must have completed, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_entered_tapped(runner: &GameRunner, ids: &[ObjectId]) {
    for id in ids {
        let obj = &runner.state().objects[id];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "{} must be on the battlefield",
            obj.name
        );
        assert!(obj.tapped, "{} must enter tapped", obj.name);
        assert_eq!(obj.controller, P0);
    }
}

fn library_len(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .len()
}

/// V1. CR 608.2d + CR 205.3i: Forest + Island share no land type, so the pair
/// cannot be chosen; a Forest + Forest pair then resolves from the same prompt.
#[test]
fn mixed_land_types_are_rejected() {
    let (mut runner, myriad, ids) = stage_and_activate(&[FOREST_A, FOREST_B, ISLAND]);
    let (forest_a, forest_b, island) = (ids[0], ids[1], ids[2]);
    let offered = assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(
        !select(&mut runner, &[forest_a, island]),
        "Forest + Island share no land type and must be rejected"
    );
    assert_eq!(
        assert_parked_at_shares_search(&runner, myriad, &ids),
        offered,
        "a rejected submission leaves the same prompt pending"
    );
    assert_eq!(runner.state().objects[&forest_a].zone, Zone::Library);
    assert_eq!(runner.state().objects[&island].zone, Zone::Library);

    assert!(
        select(&mut runner, &[forest_a, forest_b]),
        "the legal Forest pair is accepted from the same prompt"
    );
    assert_entered_tapped(&runner, &[forest_a, forest_b]);
}

/// V2. CR 701.23a + CR 113.7a: a same-type pair is put onto the battlefield
/// tapped under P0's control, the unchosen card stays in the library, and the
/// ability completes although its source left the battlefield as a cost.
#[test]
fn same_land_type_pair_enters_tapped() {
    let (mut runner, myriad, ids) = stage_and_activate(&[FOREST_A, FOREST_B, ISLAND]);
    let (forest_a, forest_b, island) = (ids[0], ids[1], ids[2]);
    assert_parked_at_shares_search(&runner, myriad, &ids);
    assert_eq!(library_len(&runner), 3);

    assert!(select(&mut runner, &[forest_a, forest_b]));

    assert_entered_tapped(&runner, &[forest_a, forest_b]);
    assert_eq!(runner.state().objects[&island].zone, Zone::Library);
    assert_eq!(library_len(&runner), 1);
    assert_search_completed(&runner);
}

/// V3. Ruling 2018-03-16: one basic land card may be found. A group of one
/// cannot fail to share (vacuous below two), whichever land type it has.
#[test]
fn single_basic_is_legal() {
    let (mut runner, myriad, ids) = stage_and_activate(&[FOREST_A, FOREST_B, ISLAND]);
    let island = ids[2];
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(select(&mut runner, &[island]));

    assert_entered_tapped(&runner, &[island]);
    assert_eq!(runner.state().objects[&ids[0]].zone, Zone::Library);
    assert_eq!(runner.state().objects[&ids[1]].zone, Zone::Library);
    assert_search_completed(&runner);
}

/// V4. CR 701.23b + "up to": finding no cards is legal.
#[test]
fn empty_find_is_legal() {
    let (mut runner, myriad, ids) = stage_and_activate(&[FOREST_A, FOREST_B, ISLAND]);
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(select(&mut runner, &[]));

    for id in &ids {
        assert_eq!(runner.state().objects[id].zone, Zone::Library);
    }
    assert_eq!(library_len(&runner), 3);
    assert_search_completed(&runner);
}

/// V5. CR 205.3i: Wastes has no land type, so two Wastes share none and cannot
/// be found together; the legal Forest alone still can.
#[test]
fn two_wastes_are_rejected() {
    let (mut runner, myriad, ids) = stage_and_activate(&[WASTES_A, WASTES_B, FOREST_A]);
    let (wastes_a, wastes_b, forest) = (ids[0], ids[1], ids[2]);
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(
        !select(&mut runner, &[wastes_a, wastes_b]),
        "two Wastes have no land type in common and must be rejected"
    );
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(select(&mut runner, &[forest]));
    assert_entered_tapped(&runner, &[forest]);
}

/// V5b. Ruling 2018-03-16: a lone Wastes is a legal find (vacuous below two),
/// the boundary partner of V5, where the same card is rejected in a pair.
#[test]
fn single_wastes_is_legal() {
    let (mut runner, myriad, ids) = stage_and_activate(&[WASTES_A, WASTES_B, FOREST_A]);
    let (wastes_a, wastes_b, forest) = (ids[0], ids[1], ids[2]);
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(select(&mut runner, &[wastes_a]));

    assert_entered_tapped(&runner, &[wastes_a]);
    assert_eq!(runner.state().objects[&wastes_b].zone, Zone::Library);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Library);
    assert_search_completed(&runner);
}

/// V6. CR 205.3i: only land types matter, not supertypes or names — a
/// Snow-Covered Forest pairs with a Forest.
#[test]
fn snow_covered_forest_pairs_with_forest() {
    let (mut runner, myriad, ids) = stage_and_activate(&[SNOW_COVERED_FOREST, FOREST_A, ISLAND]);
    let (snow_forest, forest) = (ids[0], ids[1]);
    assert_parked_at_shares_search(&runner, myriad, &ids);

    assert!(select(&mut runner, &[snow_forest, forest]));

    assert_entered_tapped(&runner, &[snow_forest, forest]);
    assert_search_completed(&runner);
}
