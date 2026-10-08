//! Dandân shared pile: every effect, cost, static and view that reads a named
//! seat's library or graveyard resolves it through the storage authority.
//! Assertions are pile-side; which hand holds a drawn or dug card belongs to
//! the hand-entry rebind.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::events::{ClashResult, GameEvent};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn ids(v: &im::Vector<ObjectId>) -> Vec<ObjectId> {
    v.iter().copied().collect()
}

fn pile(state: &GameState) -> Vec<ObjectId> {
    ids(state.library_of(P1))
}

fn grave(state: &GameState) -> Vec<ObjectId> {
    ids(state.graveyard_of(P1))
}

/// All five colors plus colorless, enough for any card in this file.
fn plenty_of_mana() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ]
    .into_iter()
    .flat_map(|color| (0..6).map(move |_| ManaUnit::new(color, ObjectId(0), false, vec![])))
    .collect()
}

/// A two-seat game in `format` at P1's precombat main, `actor` holding priority.
fn scenario(format: FormatConfig) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(format, 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn stage(
    scenario: &mut GameScenario,
    db: &CardDatabase,
    zone: Zone,
    cards: &[(PlayerId, &str)],
) -> Vec<ObjectId> {
    cards
        .iter()
        .map(|&(owner, name)| scenario.add_real_card(owner, name, zone, db))
        .collect()
}

fn start(mut scenario: GameScenario, actor: PlayerId) -> GameRunner {
    scenario.with_mana_pool(actor, plenty_of_mana());
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = actor;
    state.priority_player = actor;
    state.waiting_for = WaitingFor::Priority { player: actor };
    runner
}

/// Dandân (shared pile) when `shared`, otherwise Standard (per-seat zones).
fn format_of(shared: bool) -> FormatConfig {
    if shared {
        FormatConfig::dandan()
    } else {
        FormatConfig::standard()
    }
}

/// Dandân read from either seat, and the Standard control: the shared-zone
/// axis is the only difference, so every row's assertion is the same.
const CASES: [(bool, PlayerId); 3] = [(true, P1), (true, P0), (false, P1)];

/// P1-owned filler, top first, in the order the rows assert against.
const FILLER: [(PlayerId, &str); 6] = [
    (P1, "Island"),
    (P1, "Memory Lapse"),
    (P1, "Brainstorm"),
    (P1, "Predict"),
    (P1, "Opt"),
    (P1, "Island"),
];

/// An opponent-targeting card is cast by `P0` against the pile, by `P1` (the canonical seat: the control) and in Standard.
const OPPONENT_CASES: [(bool, PlayerId); 3] = [(true, P0), (true, P1), (false, P0)];

fn owned_by(owner: PlayerId) -> Vec<(PlayerId, &'static str)> {
    FILLER.iter().map(|&(_, name)| (owner, name)).collect()
}

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

/// Cast `spell` with `intent` declared, then pass priority until the stack
/// resolves or a prompt parks.
fn cast_to_prompt(
    runner: &mut GameRunner,
    spell: ObjectId,
    intent: impl FnOnce(engine::game::scenario::SpellCast<'_>) -> engine::game::scenario::SpellCast<'_>,
) {
    intent(runner.cast(spell)).commit();
    runner.resolve_top();
}

fn waiting(outcome: &Outcome) -> WaitingFor {
    outcome.final_waiting_for().clone()
}

/// Answer the common resolution prompts with their first offer until none parks.
fn drive_first_offers(runner: &mut GameRunner) {
    for _ in 0..8 {
        let answer = match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { options, .. } => GameAction::ChooseOption {
                choice: options[0].clone(),
            },
            WaitingFor::EffectZoneChoice { cards, .. }
            | WaitingFor::ChooseFromZoneChoice { cards, .. } => GameAction::SelectCards {
                cards: vec![cards[0]],
            },
            _ => return,
        };
        runner.act(answer).expect("prompt answered");
    }
}

fn select(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    runner
        .act(GameAction::SelectCards { cards })
        .expect("selection accepted");
}

// ---------------------------------------------------------------------------
// V1 dig
// ---------------------------------------------------------------------------

#[test]
fn v1_telling_time_looks_at_the_pile_top_three_from_either_seat() {
    let Some(db) = shared_card_db() else { return };
    for actor in [P1, P0] {
        let mut sc = scenario(FormatConfig::dandan());
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (P1, "Island"),
                (P0, "Brainstorm"),
                (P1, "Predict"),
                (P0, "Island"),
                (P1, "Opt"),
            ],
        );
        let spell = sc.add_real_card(actor, "Telling Time", Zone::Hand, db);
        let mut runner = start(sc, actor);
        assert_eq!(pile(runner.state()), cards, "reach: pile staged");
        assert!(runner.state().players[1].library.is_empty());

        let outcome = runner.cast(spell).resolve();
        let WaitingFor::DigChoice { cards: looked, .. } = waiting(&outcome) else {
            panic!("expected DigChoice, got {:?}", outcome.final_waiting_for());
        };
        assert_eq!(
            looked,
            cards[..3].to_vec(),
            "{actor:?}: the pile's front three"
        );

        select(&mut runner, vec![looked[0]]);
        let WaitingFor::DigRestSplitChoice { cards: rest, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected DigRestSplitChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(rest.len(), 2);
        select(&mut runner, vec![rest[1], rest[0]]);

        let after = pile(runner.state());
        assert_eq!(after[0], rest[1], "{actor:?}: one on top of the pile");
        assert_eq!(
            *after.last().unwrap(),
            rest[0],
            "{actor:?}: one on the bottom"
        );
        assert!(
            !after.contains(&looked[0]),
            "{actor:?}: the kept card left the pile"
        );
        assert_eq!(after.len(), cards.len() - 1);
    }
}

// ---------------------------------------------------------------------------
// V2 mill
// ---------------------------------------------------------------------------

#[test]
fn v2_mental_note_mills_the_top_two_of_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &FILLER);
        let spell = sc.add_real_card(actor, "Mental Note", Zone::Hand, db);
        let mut runner = start(sc, actor);
        assert_eq!(pile(runner.state()), cards, "reach: pile staged");

        runner.cast(spell).resolve();

        let state = runner.state();
        let mut milled = cards[..2].to_vec();
        milled.push(spell);
        assert_eq!(grave(state), milled, "{shared} {actor:?}: milled in order");
        assert_eq!(
            pile(state),
            cards[3..].to_vec(),
            "{shared} {actor:?}: mill two, draw one"
        );
    }
}

#[test]
fn v2_predict_mills_the_pile_for_either_target_and_counts_the_name() {
    let Some(db) = shared_card_db() else { return };
    // (named card, cards drawn): the pile top is Memory Lapse.
    for (shared, actor) in CASES {
        for target in [P0, P1] {
            if !shared && target == P0 {
                continue;
            }
            for (name, drawn) in [("Memory Lapse", 2), ("Control Magic", 1)] {
                let mut sc = scenario(format_of(shared));
                let cards = stage(&mut sc, db, Zone::Library, &FILLER[1..]);
                let spell = sc.add_real_card(actor, "Predict", Zone::Hand, db);
                let mut runner = start(sc, actor);
                runner.state_mut().all_card_names = std::sync::Arc::from([name.to_string()]);

                runner
                    .cast(spell)
                    .target_player(target)
                    .choose_option(name)
                    .resolve();

                let state = runner.state();
                let tag = format!("{shared} {actor:?} targets {target:?} naming {name}");
                let mut milled = grave(state);
                milled.sort();
                assert_eq!(milled, vec![cards[0], spell], "{tag}: top milled");
                assert_eq!(pile(state), cards[1 + drawn..].to_vec(), "{tag}: draws");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// V3 pile-side positive controls
// ---------------------------------------------------------------------------

#[test]
fn v3_brainstorm_draws_from_and_puts_back_onto_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let spell = sc.add_real_card(actor, "Brainstorm", Zone::Hand, db);
        let mut runner = start(sc, actor);
        let back = vec![cards[2], cards[0]];

        runner.cast(spell).effect_zone(&back).resolve();

        let state = runner.state();
        let after = ids(state.library_of(actor));
        assert_eq!(after.len(), cards.len() - 3 + 2, "{shared} {actor:?}");
        assert_eq!(
            after[..2].to_vec(),
            back,
            "{shared} {actor:?}: put back in order"
        );
    }
}

#[test]
fn v3_surgical_bay_draws_from_the_pile_and_is_sacrificed_into_it() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let bay = sc.add_real_card(actor, "The Surgical Bay", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        runner.state_mut().objects.get_mut(&bay).unwrap().tapped = false;
        let index = runner.state().objects[&bay]
            .abilities
            .iter()
            .position(|ability| format!("{:?}", ability.cost).contains("Sacrifice"))
            .expect("the Bay has a sacrifice ability");

        runner.activate(bay, index).resolve();

        let state = runner.state();
        assert_eq!(
            pile(state).len(),
            cards.len() - 1,
            "{shared} {actor:?}: one drawn"
        );
        assert_eq!(state.objects[&bay].zone, Zone::Graveyard);
        assert!(ids(state.graveyard_of(actor)).contains(&bay));
    }
}

#[test]
fn v3_metamorphose_puts_the_target_on_top_of_the_owners_library() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let victim = sc.add_real_card(other(actor), "Island", Zone::Battlefield, db);
        let spell = sc.add_real_card(actor, "Metamorphose", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner
            .cast(spell)
            .target_object(victim)
            .decline_optional()
            .resolve();

        let state = runner.state();
        assert_eq!(
            state.objects[&victim].zone,
            Zone::Library,
            "{shared} {actor:?}"
        );
        assert_eq!(state.library_of(other(actor)).front(), Some(&victim));
        assert_eq!(state.players[1].library.is_empty(), shared);
    }
}

#[test]
fn v3_accumulated_knowledge_counts_the_shared_graveyard_once() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &[
                (actor, "Accumulated Knowledge"),
                (actor, "Accumulated Knowledge"),
            ],
        );
        let spell = sc.add_real_card(actor, "Accumulated Knowledge", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        assert_eq!(
            ids(runner.state().library_of(actor)).len(),
            cards.len() - 3,
            "{shared} {actor:?}: 1 + 2 copies, counted once by id"
        );
    }
}

// ---------------------------------------------------------------------------
// V4 library-consuming handlers
// ---------------------------------------------------------------------------

#[test]
fn v4_opt_scries_the_pile_front_and_bottoms_it() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &FILLER);
        let spell = sc.add_real_card(actor, "Opt", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let WaitingFor::ScryChoice { cards: looked, .. } = runner.state().waiting_for.clone()
        else {
            panic!("expected ScryChoice, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(looked, vec![cards[0]], "{shared} {actor:?}: the pile front");
        select(&mut runner, vec![]);
        let after = pile(runner.state());
        assert_eq!(
            after.last(),
            Some(&cards[0]),
            "{shared} {actor:?}: bottomed"
        );
        assert_eq!(after.len(), cards.len() - 1, "the follow-up draw took one");
    }
}

#[test]
fn v4_consider_surveils_the_pile_front_into_the_shared_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &FILLER);
        let spell = sc.add_real_card(actor, "Consider", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let WaitingFor::SurveilChoice { cards: looked, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected SurveilChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(looked, vec![cards[0]], "{shared} {actor:?}: the pile front");
        select(&mut runner, vec![]);
        assert!(
            grave(runner.state()).contains(&cards[0]),
            "{shared} {actor:?}"
        );
        assert!(!pile(runner.state()).contains(&cards[0]));
    }
}

#[test]
fn v4_reckless_impulse_exiles_the_pile_top_two() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &FILLER);
        let spell = sc.add_real_card(actor, "Reckless Impulse", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        let state = runner.state();
        for id in &cards[..2] {
            assert_eq!(state.objects[id].zone, Zone::Exile, "{shared} {actor:?}");
        }
        assert_eq!(pile(state), cards[2..].to_vec());
    }
}

fn zone_of(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

fn revealed(events: &[GameEvent]) -> Vec<Vec<ObjectId>> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn v4_merfolk_branchwalker_explores_the_pile_top() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let spell = sc.add_real_card(actor, "Merfolk Branchwalker", Zone::Hand, db);
        let mut runner = start(sc, actor);

        let outcome = runner.cast(spell).resolve();

        assert!(
            revealed(outcome.events()).contains(&vec![cards[0]]),
            "{shared} {actor:?}: the pile front is revealed"
        );
        assert_eq!(
            pile(runner.state()),
            cards[1..].to_vec(),
            "the land left the pile"
        );
    }
}

#[test]
fn v4_explore_over_an_empty_pile_adds_a_counter() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let spell = sc.add_real_card(actor, "Merfolk Branchwalker", Zone::Hand, db);
        let mut runner = start(sc, actor);

        let outcome = runner.cast(spell).resolve();

        outcome.assert_counters(spell, engine::types::counter::CounterType::Plus1Plus1, 1);
    }
}

#[test]
fn v4_treasure_hunt_reveals_the_pile_down_to_the_first_nonland() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (actor, "Island"),
                (actor, "Island"),
                (actor, "Brainstorm"),
                (actor, "Opt"),
            ],
        );
        let spell = sc.add_real_card(actor, "Treasure Hunt", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        assert_eq!(
            pile(runner.state()),
            cards[3..].to_vec(),
            "{shared} {actor:?}"
        );
    }
}

#[test]
fn v4_fact_or_fiction_reveals_the_pile_front_five() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let spell = sc.add_real_card(actor, "Fact or Fiction", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let state = runner.state();
        for id in &cards[..5] {
            assert!(
                state.revealed_cards.contains(id),
                "{shared} {actor:?}: {id:?}"
            );
        }
        assert!(!state.revealed_cards.contains(&cards[5]));
    }
}

#[test]
fn v4_nesting_instinct_seeks_the_land_in_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (actor, "Brainstorm"),
                (actor, "Memory Lapse"),
                (actor, "Island"),
                (actor, "Opt"),
            ],
        );
        let spell = sc.add_real_card(actor, "Nesting Instinct", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        let state = runner.state();
        assert_ne!(
            zone_of(state, cards[2]),
            Zone::Library,
            "{shared} {actor:?}: the land was sought"
        );
        assert_eq!(pile(state).len(), cards.len() - 1);
    }
}

#[test]
fn v4_ethrimik_manifests_dread_from_the_pile_top_two() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let spell = sc.add_real_card(actor, "Ethrimik, Imagined Fiend", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let WaitingFor::ManifestDreadChoice { cards: offered, .. } =
            runner.state().waiting_for.clone()
        else {
            panic!(
                "expected ManifestDreadChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            offered,
            cards[..2].to_vec(),
            "{shared} {actor:?}: the pile's front two"
        );
        select(&mut runner, vec![cards[1]]);

        let state = runner.state();
        assert_eq!(zone_of(state, cards[1]), Zone::Battlefield);
        assert!(state.objects[&cards[1]].face_down, "manifested face down");
        assert_eq!(
            zone_of(state, cards[0]),
            Zone::Graveyard,
            "the other is milled"
        );
        assert_eq!(pile(state), cards[2..].to_vec());
    }
}

#[test]
fn v4_sifter_wurm_reveals_the_pile_top_for_life() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (actor, "Dandân"),
                (actor, "Island"),
                (actor, "Island"),
                (actor, "Opt"),
            ],
        );
        let spell = sc.add_real_card(actor, "Sifter Wurm", Zone::Hand, db);
        let mut runner = start(sc, actor);

        let outcome = runner.cast(spell).resolve();

        outcome.assert_life_delta(actor, 2);
        assert_eq!(
            pile(runner.state()),
            cards,
            "{shared} {actor:?}: scry kept all, reveal moves nothing"
        );
    }
}

#[test]
fn v4_tashas_hideous_laughter_exiles_the_opponents_pile_to_twenty() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in OPPONENT_CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(other(actor), "Capture of Jingzhou"); 6],
        );
        let spell = sc.add_real_card(actor, "Tasha's Hideous Laughter", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        let state = runner.state();
        for id in &cards[..4] {
            assert_eq!(
                zone_of(state, *id),
                Zone::Exile,
                "{shared} {actor:?}: {id:?}"
            );
        }
        assert_eq!(
            pile(state),
            cards[4..].to_vec(),
            "stops at total mana value 20"
        );
    }
}

#[test]
fn v4_grave_expectations_heists_the_opponents_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in OPPONENT_CASES {
        let mut sc = scenario(format_of(shared));
        let o = other(actor);
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (o, "Brainstorm"),
                (o, "Island"),
                (o, "Memory Lapse"),
                (o, "Island"),
                (o, "Predict"),
            ],
        );
        let spell = sc.add_real_card(actor, "Grave Expectations", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast.modes(&[0]).target_player(o));

        let WaitingFor::ChooseFromZoneChoice { cards: offered, .. } =
            runner.state().waiting_for.clone()
        else {
            panic!(
                "expected ChooseFromZoneChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        let mut offered_sorted = offered;
        offered_sorted.sort();
        let mut nonland = vec![cards[0], cards[2], cards[4]];
        nonland.sort();
        assert_eq!(
            offered_sorted, nonland,
            "{shared} {actor:?}: the three nonland cards"
        );
    }
}

#[test]
fn v4_trumpeting_carnosaur_discovers_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(actor, "Island"), (actor, "Brainstorm"), (actor, "Opt")],
        );
        let spell = sc.add_real_card(actor, "Trumpeting Carnosaur", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let state = runner.state();
        assert_eq!(
            zone_of(state, cards[0]),
            Zone::Exile,
            "{shared} {actor:?}: the miss"
        );
        assert_eq!(
            zone_of(state, cards[1]),
            Zone::Exile,
            "{shared} {actor:?}: the hit"
        );
        assert_eq!(pile(state), vec![cards[2]]);
    }
}

#[test]
fn v4_sweet_gum_recluse_cascades_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(actor, "Island"), (actor, "Brainstorm"), (actor, "Opt")],
        );
        let spell = sc.add_real_card(actor, "Sweet-Gum Recluse", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let state = runner.state();
        assert_eq!(
            zone_of(state, cards[0]),
            Zone::Exile,
            "{shared} {actor:?}: the miss"
        );
        assert_eq!(
            zone_of(state, cards[1]),
            Zone::Exile,
            "{shared} {actor:?}: the hit"
        );
        assert_eq!(pile(state), vec![cards[2]]);
    }
}

#[test]
fn v4_thrumming_stone_ripples_the_pile_front_four() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        sc.add_real_card(actor, "Thrumming Stone", Zone::Battlefield, db);
        let spell = sc.add_real_card(actor, "Mental Note", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::RippleRevealChoice { .. }
            ),
            "reach: ripple offered, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::RippleChoice {
                choice: CastChoice::Cast,
            })
            .expect("ripple reveal accepted");

        let WaitingFor::RippleBottomOrder {
            cards: revealed_cards,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected RippleBottomOrder, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(revealed_cards, cards[..4].to_vec(), "{shared} {actor:?}");
    }
}

// ---------------------------------------------------------------------------
// V5 search
// ---------------------------------------------------------------------------

#[test]
fn v5_doomsday_searches_the_pile_library_and_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let buried = stage(&mut sc, db, Zone::Graveyard, &[(actor, "Opt")])[0];
        let spell = sc.add_real_card(actor, "Doomsday", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);

        let WaitingFor::SearchChoice {
            cards: mut found, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected SearchChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        found.sort();
        let mut expected = cards.clone();
        expected.push(buried);
        assert_eq!(
            found, expected,
            "{shared} {actor:?}: the pile library and graveyard"
        );
    }
}

// ---------------------------------------------------------------------------
// V6 graveyard enumerators
// ---------------------------------------------------------------------------

fn graveyard_of_creatures(
    actor: PlayerId,
    names: &[&'static str],
) -> Vec<(PlayerId, &'static str)> {
    names.iter().map(|&name| (actor, name)).collect()
}

#[test]
fn v6_organ_grinder_exiles_three_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let dead = stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &graveyard_of_creatures(actor, &["Island", "Opt", "Predict"]),
        );
        let grinder = sc.add_real_card(actor, "Organ Grinder", Zone::Battlefield, db);
        let mut runner = start(sc, actor);

        runner
            .activate(grinder, 0)
            .target_player(other(actor))
            .pay_with(&dead)
            .resolve();

        let state = runner.state();
        for id in &dead {
            assert_eq!(zone_of(state, *id), Zone::Exile, "{shared} {actor:?}");
        }
        assert_eq!(state.players[other(actor).0 as usize].life, 17);
    }
}

#[test]
fn v6_gruesome_menagerie_chooses_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let dead = stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &graveyard_of_creatures(actor, &["Llanowar Elves", "Dandân"]),
        );
        let spell = sc.add_real_card(actor, "Gruesome Menagerie", Zone::Hand, db);
        let mut runner = start(sc, actor);

        cast_to_prompt(&mut runner, spell, |cast| cast);
        drive_first_offers(&mut runner);

        let state = runner.state();
        assert_eq!(
            zone_of(state, dead[0]),
            Zone::Battlefield,
            "{shared} {actor:?}: the mana value 1 creature"
        );
        assert_eq!(
            zone_of(state, dead[1]),
            Zone::Graveyard,
            "the mana value 2 creature is not chosen"
        );
    }
}

// ---------------------------------------------------------------------------
// V7 graveyard permissions
// ---------------------------------------------------------------------------

#[test]
fn v7_think_twice_flashback_is_offered_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let dead = stage(&mut sc, db, Zone::Graveyard, &[(actor, "Think Twice")])[0];
        let runner = start(sc, actor);

        let offered = engine::game::casting::spell_objects_available_to_cast(runner.state(), actor);

        assert!(offered.contains(&dead), "{shared} {actor:?}: {offered:?}");
    }
}

#[test]
fn v7_crucible_of_worlds_plays_a_land_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let land = stage(&mut sc, db, Zone::Graveyard, &[(actor, "Island")])[0];
        sc.add_real_card(actor, "Crucible of Worlds", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        let card_id = runner.state().objects[&land].card_id;

        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("the land play from the graveyard is accepted");

        assert_eq!(
            zone_of(runner.state(), land),
            Zone::Battlefield,
            "{shared} {actor:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// V8 top-of-library statics
// ---------------------------------------------------------------------------

fn hidden_from(state: &GameState, viewer: PlayerId, id: ObjectId) -> bool {
    engine::game::visibility::filter_state_for_viewer(state, viewer).objects[&id].card_id
        == engine::types::identifiers::CardId(0)
}

#[test]
fn v8_future_sight_reveals_and_casts_from_the_pile_top() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (actor, "Mental Note"),
                (actor, "Island"),
                (actor, "Island"),
                (actor, "Island"),
            ],
        );
        sc.add_real_card(actor, "Future Sight", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        engine::game::derived::sync_continuous_reveals(runner.state_mut());

        assert!(
            runner.state().revealed_cards.contains(&cards[0]),
            "{shared} {actor:?}: the pile top is revealed"
        );
        runner.cast(cards[0]).resolve();

        assert_ne!(
            zone_of(runner.state(), cards[0]),
            Zone::Library,
            "{shared} {actor:?}: cast from the top"
        );
    }
}

#[test]
fn v8_sphinx_of_jwar_isle_lets_the_controller_see_the_pile_top() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        sc.add_real_card(actor, "Sphinx of Jwar Isle", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        engine::game::derived::derive_display_state(runner.state_mut());
        let state = runner.state();

        assert!(
            !hidden_from(state, actor, cards[0]),
            "{shared} {actor:?}: the looker sees the top"
        );
        assert!(
            hidden_from(state, actor, cards[1]),
            "reach: the second card stays hidden"
        );
        if shared {
            assert!(
                hidden_from(state, other(actor), cards[0]),
                "the seat that may not look still cannot see the shared top"
            );
        }
    }
}

#[test]
fn v8_mul_daya_channelers_reads_the_pile_top_type() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (top, bonus) in [("Dandân", true), ("Island", false)] {
            let mut sc = scenario(format_of(shared));
            stage(
                &mut sc,
                db,
                Zone::Library,
                &[(actor, top), (actor, "Island")],
            );
            let channelers = sc.add_real_card(actor, "Mul Daya Channelers", Zone::Battlefield, db);
            let mut runner = start(sc, actor);
            engine::game::layers::evaluate_layers(runner.state_mut());

            let object = &runner.state().objects[&channelers];
            let expected = if bonus { (5, 5) } else { (2, 2) };
            assert_eq!(
                (object.power, object.toughness),
                (Some(expected.0), Some(expected.1)),
                "{shared} {actor:?}: top {top}"
            );
        }
    }
}

#[test]
fn v8_soul_summons_manifests_the_pile_top_whoever_owns_it() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let top_owner = if shared { P0 } else { actor };
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (top_owner, "Island"),
                (top_owner, "Island"),
                (top_owner, "Opt"),
            ],
        );
        let spell = sc.add_real_card(actor, "Soul Summons", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(spell).resolve();

        let state = runner.state();
        assert_eq!(
            zone_of(state, cards[0]),
            Zone::Battlefield,
            "{shared} {actor:?}"
        );
        assert!(state.objects[&cards[0]].face_down);
        assert_eq!(pile(state), cards[1..].to_vec());
    }
}

#[test]
fn v8_fblthp_offers_plot_for_a_nonland_pile_top_only() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (top, offered) in [("Brainstorm", true), ("Island", false)] {
            let mut sc = scenario(format_of(shared));
            let cards = stage(
                &mut sc,
                db,
                Zone::Library,
                &[(actor, top), (actor, "Island")],
            );
            sc.add_real_card(actor, "Fblthp, Lost on the Range", Zone::Battlefield, db);
            let mut runner = start(sc, actor);
            engine::game::derived::derive_display_state(runner.state_mut());

            let plot = engine::ai_support::legal_actions(runner.state()).into_iter().find(|action| {
                matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == cards[0])
            });

            assert_eq!(plot.is_some(), offered, "{shared} {actor:?}: top {top}");
        }
    }
}

#[test]
fn v8_crown_of_convergence_offers_the_pile_to_bottom() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let crown = sc.add_real_card(actor, "Crown of Convergence", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        runner
            .act(GameAction::ActivateAbility {
                source_id: crown,
                ability_index: 0,
            })
            .expect("activation accepted");
        runner.resolve_top();

        let WaitingFor::EffectZoneChoice { cards: offered, .. } =
            runner.state().waiting_for.clone()
        else {
            panic!(
                "expected EffectZoneChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        let mut offered_sorted = offered.clone();
        offered_sorted.sort();
        assert_eq!(
            offered_sorted, cards,
            "{shared} {actor:?}: the pile is offered"
        );
        select(&mut runner, vec![offered[0]]);

        let after = pile(runner.state());
        assert_eq!(
            after.last(),
            Some(&offered[0]),
            "{shared} {actor:?}: the choice is the bottom"
        );
        assert_eq!(after.len(), cards.len());
    }
}

// ---------------------------------------------------------------------------
// V9 size and count gates
// ---------------------------------------------------------------------------

#[test]
fn v9_manakin_and_millikin_mills_the_pile_to_pay() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (size, payable) in [(1, true), (0, false)] {
            let mut sc = scenario(format_of(shared));
            stage(&mut sc, db, Zone::Library, &vec![(actor, "Island"); size]);
            let manakin = sc.add_real_card(actor, "Manakin and Millikin", Zone::Battlefield, db);
            let mut runner = start(sc, actor);
            runner
                .state_mut()
                .objects
                .get_mut(&manakin)
                .unwrap()
                .entered_battlefield_turn = Some(0);
            runner.state_mut().turn_number = 5;

            let result = runner.act(GameAction::ActivateAbility {
                source_id: manakin,
                ability_index: 0,
            });

            assert_eq!(
                result.is_ok(),
                payable,
                "{shared} {actor:?} pile of {size}: {:?}",
                result.err()
            );
            if payable {
                assert!(
                    pile(runner.state()).is_empty(),
                    "the milled card left the pile"
                );
                assert_eq!(grave(runner.state()).len(), 1);
            }
        }
    }
}

#[test]
fn v9_eldritch_pact_counts_the_targets_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in OPPONENT_CASES {
        let target = other(actor);
        let mut sc = scenario(format_of(shared));
        stage(&mut sc, db, Zone::Library, &owned_by(target));
        stage(&mut sc, db, Zone::Graveyard, &[(target, "Island"); 3]);
        let spell = sc.add_real_card(actor, "Eldritch Pact", Zone::Hand, db);
        let mut runner = start(sc, actor);

        let outcome = runner.cast(spell).target_player(target).resolve();

        outcome.assert_life_delta(target, -3);
    }
}

#[test]
fn v9_golgari_thug_dredge_reads_the_pile_size() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (size, offered) in [(4, true), (3, false)] {
            let mut sc = scenario(format_of(shared));
            stage(&mut sc, db, Zone::Library, &vec![(actor, "Island"); size]);
            stage(&mut sc, db, Zone::Graveyard, &[(actor, "Golgari Thug")]);
            let spell = sc.add_real_card(actor, "Opt", Zone::Hand, db);
            let mut runner = start(sc, actor);

            cast_to_prompt(&mut runner, spell, |cast| cast);
            if matches!(runner.state().waiting_for, WaitingFor::ScryChoice { .. }) {
                select(&mut runner, vec![]);
            }

            let waiting = runner.state().waiting_for.clone();
            let is_dredge_prompt = matches!(waiting, WaitingFor::ReplacementChoice { .. });
            assert_eq!(
                is_dredge_prompt, offered,
                "{shared} {actor:?} pile of {size}: {waiting:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// V10 views and debug
// ---------------------------------------------------------------------------

#[test]
fn v10_debug_views_and_mill_act_on_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let mut runner = start(sc, actor);
        let state = runner.state_mut();
        state.debug_mode = true;
        state.debug_permitted.insert(actor);

        let mut listed: Vec<ObjectId> =
            engine::game::derived_views::derive_views(runner.state(), Some(actor))
                .debug_library_cards
                .iter()
                .map(|card| card.object_id)
                .collect();
        listed.sort();
        let mut expected = cards.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "{shared} {actor:?}: the debug view lists the pile"
        );

        runner
            .act(GameAction::Debug(
                engine::types::actions::DebugAction::Mill {
                    player_id: actor,
                    count: 2,
                },
            ))
            .expect("debug mill accepted");
        assert_eq!(
            grave(runner.state()),
            cards[..2].to_vec(),
            "{shared} {actor:?}"
        );
        assert_eq!(pile(runner.state()), cards[2..].to_vec());
    }
}

// ---------------------------------------------------------------------------
// V15 clash reveals the top card of each library
// ---------------------------------------------------------------------------

/// Casts Oaken Brawler and passes priority until its enters trigger's clash
/// parks the first placement prompt. Returns the card each seat revealed and
/// the events emitted on the way.
fn clash_reveals(
    runner: &mut GameRunner,
    brawler: ObjectId,
) -> (Vec<(PlayerId, ObjectId)>, Vec<GameEvent>) {
    runner.cast(brawler).commit();
    let mut events = Vec::new();
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ClashCardPlacement { .. }
        ) {
            break;
        }
        let result = engine::game::engine::apply_as_current_for_simulation(
            runner.state_mut(),
            GameAction::PassPriority,
        )
        .expect("priority passes");
        events.extend(result.events);
    }
    let WaitingFor::ClashCardPlacement {
        player,
        card,
        remaining,
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ClashCardPlacement, got {:?}",
            runner.state().waiting_for
        );
    };
    let mut revealed = vec![(player, card)];
    revealed.extend(remaining);
    (revealed, events)
}

fn clash_event(events: &[GameEvent]) -> (Option<u32>, Option<u32>, ClashResult) {
    events
        .iter()
        .find_map(|event| match event {
            GameEvent::Clash {
                controller_mana_value,
                opponent_mana_value,
                result,
                ..
            } => Some((*controller_mana_value, *opponent_mana_value, *result)),
            _ => None,
        })
        .expect("the Brawler's trigger clashed")
}

#[test]
fn v15_clash_reveals_the_top_card_of_a_standard_library() {
    let Some(db) = shared_card_db() else { return };
    // Each library is (top, bottom); the pair swapped end-for-end flips the result.
    for ((mine_top, theirs_top), expected) in [
        (
            ("Control Magic", "Island"),
            (Some(4), Some(0), ClashResult::Won),
        ),
        (
            ("Island", "Control Magic"),
            (Some(0), Some(4), ClashResult::Lost),
        ),
    ] {
        let mine_bottom = if mine_top == "Island" {
            "Control Magic"
        } else {
            "Island"
        };
        let theirs_bottom = if theirs_top == "Island" {
            "Control Magic"
        } else {
            "Island"
        };
        let mut sc = scenario(FormatConfig::standard());
        let mine = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(P0, mine_top), (P0, "Island"), (P0, mine_bottom)],
        );
        let theirs = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(P1, theirs_top), (P1, "Island"), (P1, theirs_bottom)],
        );
        let brawler = sc.add_real_card(P0, "Oaken Brawler", Zone::Hand, db);
        let mut runner = start(sc, P0);

        let (revealed, events) = clash_reveals(&mut runner, brawler);

        assert!(
            revealed.contains(&(P0, mine[0])),
            "P0 reveals its top: {revealed:?}"
        );
        assert!(
            revealed.contains(&(P1, theirs[0])),
            "P1 reveals its top: {revealed:?}"
        );
        assert_eq!(clash_event(&events), expected, "CR 701.30a + CR 701.30d");
    }
}

#[test]
fn v15_clash_reveals_the_pile_top_for_either_seat() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P1, P0] {
        let mut sc = scenario(FormatConfig::dandan());
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[(P0, "Control Magic"), (P0, "Island"), (P0, "Island")],
        );
        let brawler = sc.add_real_card(caster, "Oaken Brawler", Zone::Hand, db);
        let mut runner = start(sc, caster);

        let (revealed, events) = clash_reveals(&mut runner, brawler);

        assert!(
            revealed.contains(&(caster, cards[0])),
            "{caster:?} reveals the pile top: {revealed:?}"
        );
        assert_eq!(
            clash_event(&events).0,
            Some(4),
            "{caster:?}: the pile top is Control Magic"
        );
    }
}

// ---------------------------------------------------------------------------
// V6 costs that exile from the graveyard
// ---------------------------------------------------------------------------

#[test]
fn v6_corpseberry_cultivator_forages_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let dead = stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &graveyard_of_creatures(actor, &["Island", "Opt", "Predict"]),
        );
        sc.add_real_card(actor, "Corpseberry Cultivator", Zone::Battlefield, db);
        let mut runner = start(sc, actor);

        runner.advance_to_phase(Phase::BeginCombat);
        for _ in 0..12 {
            match runner.state().waiting_for.clone() {
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("forage accepted");
                }
                WaitingFor::EffectZoneChoice { cards, count, .. } => {
                    select(&mut runner, cards.into_iter().take(count).collect());
                }
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.resolve_top();
                }
                _ => break,
            }
        }

        let state = runner.state();
        for id in &dead {
            assert_eq!(
                zone_of(state, *id),
                Zone::Exile,
                "{shared} {actor:?}: {:?}",
                state.waiting_for
            );
        }
    }
}

#[test]
fn v6_kylox_voltstrider_collects_evidence_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let evidence = stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &graveyard_of_creatures(actor, &["Capture of Jingzhou"; 3]),
        );
        let vehicle = sc.add_real_card(actor, "Kylox's Voltstrider", Zone::Battlefield, db);
        let mut runner = start(sc, actor);
        let index = runner.state().objects[&vehicle]
            .abilities
            .iter()
            .position(|ability| format!("{:?}", ability.cost).contains("CollectEvidence"))
            .expect("the Voltstrider has a collect-evidence ability");

        runner
            .act(GameAction::ActivateAbility {
                source_id: vehicle,
                ability_index: index,
            })
            .expect("collect evidence 6 is payable from the pile graveyard");

        let WaitingFor::CollectEvidenceChoice { cards, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected CollectEvidenceChoice, got {:?}",
                runner.state().waiting_for
            );
        };
        let mut offered = cards;
        offered.sort();
        assert_eq!(
            offered, evidence,
            "{shared} {actor:?}: the pile graveyard is offered"
        );
        select(&mut runner, evidence[..2].to_vec());
        for id in &evidence[..2] {
            assert_eq!(
                zone_of(runner.state(), *id),
                Zone::Exile,
                "{shared} {actor:?}"
            );
        }
    }
}

#[test]
fn v6_altar_of_the_wretched_crafts_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (names, offered) in [
            (&["Dandân", "Llanowar Elves"][..], true),
            (&["Island"][..], false),
        ] {
            let mut sc = scenario(format_of(shared));
            let dead = stage(
                &mut sc,
                db,
                Zone::Graveyard,
                &graveyard_of_creatures(actor, names),
            );
            let altar = sc.add_real_card(actor, "Altar of the Wretched", Zone::Battlefield, db);
            sc.with_mana_pool(actor, plenty_of_mana());
            let mut runner = start(sc, actor);
            let index = runner.state().objects[&altar]
                .abilities
                .iter()
                .position(|ability| format!("{:?}", ability.cost).contains("ExileMaterials"))
                .expect("the Altar has a craft ability");

            let result = runner.act(GameAction::ActivateAbility {
                source_id: altar,
                ability_index: index,
            });

            assert_eq!(
                result.is_ok(),
                offered,
                "{shared} {actor:?} {names:?}: {:?}",
                result.err()
            );
            if offered {
                let shown = format!("{:?}", runner.state().waiting_for);
                for id in &dead {
                    assert!(
                        shown.contains(&format!("{id:?}")),
                        "{shared} {actor:?}: {shown}"
                    );
                }
            }
        }
    }
}

#[test]
fn v6_baron_helmut_zemo_boasts_by_exiling_black_cards_from_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for (merchants, offered) in [(2, true), (1, false)] {
            let mut sc = scenario(format_of(shared));
            let mut names = vec!["Phyrexian Obliterator"; 3];
            names.extend(vec!["Gray Merchant of Asphodel"; merchants]);
            stage(
                &mut sc,
                db,
                Zone::Graveyard,
                &graveyard_of_creatures(actor, &names),
            );
            let baron = sc.add_real_card(actor, "Baron Helmut Zemo", Zone::Battlefield, db);
            let mut runner = start(sc, actor);
            runner
                .state_mut()
                .creatures_attacked_this_turn
                .insert(baron);
            let index = runner.state().objects[&baron]
                .abilities
                .iter()
                .position(|ability| format!("{:?}", ability.cost).contains("ExileWithAggregate"))
                .expect("Baron has a boast ability");

            let result = runner.act(GameAction::ActivateAbility {
                source_id: baron,
                ability_index: index,
            });

            assert_eq!(
                result.is_ok(),
                offered,
                "{shared} {actor:?} with {merchants} Merchants: {:?}",
                result.err()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// V9 turn-based triggers that read the pile
// ---------------------------------------------------------------------------

#[test]
fn v9_deep_spawn_pays_its_upkeep_mill_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        sc.at_phase(Phase::End);
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        let spawn = sc.add_real_card(actor, "Deep Spawn", Zone::Battlefield, db);
        let mut runner = start(sc, other(actor));

        runner.advance_to_upkeep();
        runner.advance_until_stack_empty();
        let waiting = runner.state().waiting_for.clone();
        assert!(
            matches!(waiting, WaitingFor::UnlessPayment { .. }),
            "{shared} {actor:?}: expected the unless-mill payment, got {waiting:?}"
        );
        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("the mill is paid");

        let state = runner.state();
        assert_eq!(
            zone_of(state, spawn),
            Zone::Battlefield,
            "{shared} {actor:?}: not sacrificed"
        );
        assert_eq!(
            grave(state),
            cards[..2].to_vec(),
            "{shared} {actor:?}: the top two milled"
        );
    }
}

#[test]
fn v4_mirelurk_queen_rad_counters_mill_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(
            &mut sc,
            db,
            Zone::Library,
            &[
                (actor, "Island"),
                (actor, "Opt"),
                (actor, "Opt"),
                (actor, "Island"),
            ],
        );
        let queen = sc.add_real_card(actor, "Mirelurk Queen", Zone::Hand, db);
        let mut runner = start(sc, actor);

        runner.cast(queen).target_player(actor).resolve();
        assert_eq!(
            runner.state().players[actor.0 as usize]
                .player_counter(&engine::types::player::PlayerCounterKind::Rad),
            2,
            "reach: the enters trigger gave two rad counters"
        );
        let state = runner.state_mut();
        state.phase = Phase::Upkeep;
        state.priority_player = actor;
        state.waiting_for = WaitingFor::Priority { player: actor };
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_until_stack_empty();

        let state = runner.state();
        assert_eq!(
            grave(state),
            cards[1..3].to_vec(),
            "{shared} {actor:?}: the next two cards milled (CR 728.1)"
        );
        let player = &state.players[actor.0 as usize];
        assert_eq!(
            player.life, 18,
            "{shared} {actor:?}: one life lost per nonland milled"
        );
        assert_eq!(
            player.player_counter(&engine::types::player::PlayerCounterKind::Rad),
            0,
            "{shared} {actor:?}: one rad counter removed per nonland milled"
        );
    }
}

#[test]
fn v5_parker_luck_reveals_the_pile_top_for_each_targeted_seat() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(format_of(shared));
        let cards = stage(&mut sc, db, Zone::Library, &owned_by(actor));
        sc.add_real_card(actor, "Parker Luck", Zone::Battlefield, db);
        let mut runner = start(sc, actor);

        runner.advance_to_end_step();
        let before = runner.state().waiting_for.clone();
        if matches!(before, WaitingFor::TriggerTargetSelection { .. }) {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![
                        engine::types::ability::TargetRef::Player(P0),
                        engine::types::ability::TargetRef::Player(P1),
                    ],
                })
                .expect("both players targeted");
        }
        let mut events = Vec::new();
        for _ in 0..10 {
            if runner.state().stack.is_empty() {
                break;
            }
            let result = engine::game::engine::apply_as_current_for_simulation(
                runner.state_mut(),
                GameAction::PassPriority,
            )
            .expect("priority passes");
            events.extend(result.events);
        }

        for seat in [P0, P1].into_iter().filter(|seat| shared || *seat == actor) {
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    GameEvent::CardsRevealed { player, card_ids, .. }
                        if *player == seat && card_ids.contains(&cards[0])
                )),
                "{shared} {actor:?}: {seat:?} reveals the pile top: {before:?}"
            );
        }
    }
}
