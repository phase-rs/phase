//! Issue #7754 — Through the Forest Gate.
//!
//! ORACLE TEXT (verbatim, Scryfall):
//! "Look at the top twenty cards of your library, put any number of land cards
//! from among them onto the battlefield tapped, then shuffle. You gain 8 life."
//!
//! CR 701.20e: looking at cards in a library does not move them out of it. Only
//! the land cards the player chooses are put onto the battlefield (CR 608.2c:
//! follow the written instruction), so every other looked-at card simply STAYS
//! IN THE LIBRARY; the printed "then shuffle" (CR 701.24a) randomizes the whole
//! library afterwards, and "You gain 8 life" (CR 119.3) follows on the same
//! chain.
//!
//! Before the fix the parser left `Effect::Dig.rest_destination` as `None` for
//! the "put any number … from among them" instruction, and the `DigChoice`
//! handler's `None` → graveyard default milled all seventeen unchosen cards.
//! Both assertions below fail on that build: the looked-at cards land in the
//! graveyard and leave the library.
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Verbatim Oracle text — a paraphrase can take a different parser branch and
/// go green while the real card stays broken.
const THROUGH_THE_FOREST_GATE: &str = "Look at the top twenty cards of your \
library, put any number of land cards from among them onto the battlefield \
tapped, then shuffle. You gain 8 life.";

fn gate_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 6,
    }
}

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Put a card into P0's library. `create_object` appends to the library's back,
/// so cards added FIRST are the topmost — the looked-at window is the first
/// twenty added here.
fn add_library_card(runner: &mut GameRunner, name: &str, land: bool) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(if land {
        CoreType::Land
    } else {
        CoreType::Creature
    });
    id
}

/// Cast the real card and stop at the "put any number of land cards" prompt.
struct GateFixture {
    runner: GameRunner,
    lands: Vec<ObjectId>,
    nonlands: Vec<ObjectId>,
    deep: ObjectId,
    life_before: i32,
    journal_start: usize,
}

fn cast_gate() -> GateFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Through the Forest Gate",
        false,
        THROUGH_THE_FOREST_GATE,
    );
    builder.with_mana_cost(gate_cost());
    let spell_id = builder.id();
    let mut runner = scenario.build();

    // The looked-at window: three lands and seventeen nonlands.
    let lands: Vec<ObjectId> = (0..3)
        .map(|index| add_library_card(&mut runner, &format!("Land {index}"), true))
        .collect();
    let nonlands: Vec<ObjectId> = (0..17)
        .map(|index| add_library_card(&mut runner, &format!("Spell {index}"), false))
        .collect();
    // Deeper than the twenty-card window: never looked at, never touched.
    let deep = add_library_card(&mut runner, "Deep Sentinel", false);

    add_mana(&mut runner, ManaType::Green, 2);
    add_mana(&mut runner, ManaType::Colorless, 6);

    let life_before = runner.state().players[0].life;
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let outcome = runner.cast(spell_id).resolve();

    match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            cards,
            selectable_cards,
            keep_count,
            up_to,
            ..
        } => {
            assert_eq!(cards.len(), 20, "look at the top twenty cards");
            assert_eq!(
                selectable_cards, &lands,
                "only the land cards may be put onto the battlefield"
            );
            assert!(*up_to, "\"any number\" is an up-to choice");
            assert_eq!(
                *keep_count,
                selectable_cards.len(),
                "the choice is capped at the number of land cards"
            );
        }
        other => panic!("expected the land-selection DigChoice, got {other:?}"),
    }

    GateFixture {
        runner,
        lands,
        nonlands,
        deep,
        life_before,
        journal_start,
    }
}

fn library_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

/// Assert the printed "then shuffle" actually ran: the shared resolved-command
/// journal records one library shuffle after `journal_start`.
fn assert_library_shuffled(runner: &GameRunner, journal_start: usize) {
    assert!(
        runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .skip(journal_start)
            .any(|entry| matches!(
                entry.command.as_ref(),
                Some(ResolvedRulesCommand::LibraryShuffle(_))
            )),
        "CR 701.24a: the printed \"then shuffle\" must shuffle the library"
    );
}

/// MAIN TEST. The seventeen land-less looked-at cards stay in the library (they
/// were only looked at), the three chosen lands enter tapped, the library is
/// shuffled, and the 8 life is gained.
#[test]
fn through_the_forest_gate_keeps_the_unchosen_looked_at_cards_in_the_library() {
    let GateFixture {
        mut runner,
        lands,
        nonlands,
        deep,
        life_before,
        journal_start,
    } = cast_gate();

    runner
        .act(GameAction::SelectCards {
            cards: lands.clone(),
        })
        .expect("putting all three land cards onto the battlefield must be accepted");
    runner.advance_until_stack_empty();

    let st = runner.state();
    assert!(
        matches!(st.waiting_for, WaitingFor::Priority { .. }),
        "the spell must finish resolving, got {:?}",
        st.waiting_for
    );

    // CR 608.2c + CR 614.1: the chosen lands are on the battlefield, tapped.
    for land in &lands {
        assert_eq!(
            st.objects[land].zone,
            Zone::Battlefield,
            "{} must be put onto the battlefield",
            st.objects[land].name
        );
        assert!(
            st.objects[land].tapped,
            "{} enters tapped",
            st.objects[land].name
        );
    }

    // THE REGRESSION ASSERTION: the unchosen looked-at cards were never moved
    // out of the library (before the fix they were milled into the graveyard).
    for card in &nonlands {
        assert_eq!(
            st.objects[card].zone,
            Zone::Library,
            "{} was only looked at, so it must stay in the library",
            st.objects[card].name
        );
        assert!(
            !st.players[0].graveyard.contains(card),
            "{} must not be put into the graveyard",
            st.objects[card].name
        );
    }

    // The pre-existing library card below the window is untouched, and the
    // seventeen remaining looked-at cards are still in the library with it.
    assert_eq!(st.objects[&deep].zone, Zone::Library);
    assert_eq!(
        library_ids(&runner).len(),
        18,
        "seventeen looked-at nonlands plus the untouched library card"
    );
    for card in &nonlands {
        assert!(
            library_ids(&runner).contains(card),
            "{} must still be in the library",
            st.objects[card].name
        );
    }

    // CR 701.24a: the printed shuffle runs.
    assert_library_shuffled(&runner, journal_start);

    // CR 119.3: "You gain 8 life."
    assert_eq!(
        runner.state().players[0].life,
        life_before + 8,
        "\"You gain 8 life\" must be applied"
    );
}

/// "Any number" includes zero (CR 107.1c): declining every land still shuffles
/// and still gains the 8 life, and every looked-at card stays in the library.
/// This is the sibling arm of the same defect — the `DigChoice` handler's
/// empty-selection path is a different branch from the kept-cards path above.
#[test]
fn through_the_forest_gate_declining_every_land_still_shuffles_and_gains_life() {
    let GateFixture {
        mut runner,
        nonlands,
        deep,
        life_before,
        journal_start,
        ..
    } = cast_gate();

    runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect("putting zero land cards onto the battlefield is a legal \"any number\" choice");
    runner.advance_until_stack_empty();

    let st = runner.state();
    assert!(
        matches!(st.waiting_for, WaitingFor::Priority { .. }),
        "the spell must finish resolving, got {:?}",
        st.waiting_for
    );
    for card in &nonlands {
        assert_eq!(
            st.objects[card].zone,
            Zone::Library,
            "{} must stay in the library",
            st.objects[card].name
        );
    }
    assert_eq!(st.objects[&deep].zone, Zone::Library);
    assert_eq!(
        library_ids(&runner).len(),
        21,
        "all twenty looked-at cards plus the untouched library card"
    );
    assert_library_shuffled(&runner, journal_start);
    assert_eq!(
        runner.state().players[0].life,
        life_before + 8,
        "\"You gain 8 life\" must be applied"
    );
}
