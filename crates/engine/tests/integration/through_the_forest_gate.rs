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
//!
//! The fix records the silent remainder as `DigRestRemainder::StayInLibrary`
//! rather than routing it to a destination. That distinction matters beyond this
//! card: `rest_destination: Some(Zone::Library)` would put the remainder on the
//! BOTTOM (CR 401.4), which moves and reorders the library — the card below
//! covers that difference, because this card's own printed "then shuffle" hides
//! the resulting order.
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{DigRestOrder, DigRestRemainder};
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

/// ENGINE-LEVEL counterpart: the printed card shuffles immediately after the put,
/// so "left in place" and "moved to the bottom" are indistinguishable there.
/// This case drives the same `WaitingFor::DigChoice` submission the card reaches,
/// with the remainder the parser marks for a silent instruction and with no
/// shuffle behind it, and asserts the unkept cards are still in their original
/// positions. Routing them to `rest_destination: Some(Zone::Library)` would leave
/// the same contents in a different order — the difference the fix exists for.
#[test]
fn a_silent_remainder_is_never_moved_to_the_library_bottom() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let above = add_library_card(&mut runner, "Above", false);
    let land = add_library_card(&mut runner, "Land", true);
    let below = add_library_card(&mut runner, "Below", false);
    let deepest = add_library_card(&mut runner, "Deep Sentinel", false);
    assert_eq!(library_ids(&runner), vec![above, land, below, deepest]);

    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![above, land, below],
        selectable_cards: vec![land],
        keep_count: 1,
        up_to: true,
        kept_destination: Some(Zone::Battlefield),
        rest_destination: None,
        rest_remainder: DigRestRemainder::StayInLibrary,
        rest_split_top_count: None,
        rest_order: DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("putting the one land card onto the battlefield must be accepted");

    let st = runner.state();
    assert_eq!(
        st.objects[&land].zone,
        Zone::Battlefield,
        "the chosen land is put onto the battlefield"
    );
    assert!(
        st.players[0].graveyard.is_empty(),
        "CR 701.20e: nothing routes a card that was only looked at, got {:?}",
        st.players[0].graveyard
    );
    assert_eq!(
        library_ids(&runner),
        vec![above, below, deepest],
        "the unkept looked-at cards keep their positions; a bottom placement \
         would leave the same cards in a different order"
    );
}

/// The REORDER form of the same rule: "put one of those cards back on top of
/// your library" and say nothing about the others. The choice handler lifts the
/// looked-at cards out of the library so the kept one can take the top slot; on
/// a silent remainder only the KEPT card is lifted, so the unchosen ones are
/// never moved and never re-inserted (the `Route` default would both move them
/// and, in this arm, drop them out of the library entirely).
#[test]
fn a_silent_remainder_in_the_reorder_form_keeps_the_unchosen_cards_in_place() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let above = add_library_card(&mut runner, "Above", false);
    let kept = add_library_card(&mut runner, "Kept", false);
    let below = add_library_card(&mut runner, "Below", false);
    let deepest = add_library_card(&mut runner, "Deep Sentinel", false);
    assert_eq!(library_ids(&runner), vec![above, kept, below, deepest]);

    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![above, kept, below],
        selectable_cards: vec![above, kept, below],
        keep_count: 1,
        up_to: true,
        kept_destination: Some(Zone::Library),
        rest_destination: None,
        rest_remainder: DigRestRemainder::StayInLibrary,
        rest_split_top_count: None,
        rest_order: DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("putting one looked-at card back on top must be accepted");

    let st = runner.state();
    assert!(
        st.players[0].graveyard.is_empty(),
        "CR 701.20e: nothing routes a card that was only looked at, got {:?}",
        st.players[0].graveyard
    );
    assert_eq!(
        library_ids(&runner),
        vec![kept, above, below, deepest],
        "the kept card takes the top; the unchosen looked-at cards are untouched, \
         including their order above the deeper card"
    );
}

/// The same reorder form with an EMPTY selection. "Any number" includes zero
/// (CR 107.1c), so declining every card must leave the library exactly as it
/// was — contents AND order. This is a different code path from the one-card
/// case: nothing is lifted out of the library at all, so a remainder routed to
/// the graveyard would strand or drop the whole looked-at pile instead of only
/// the unchosen tail.
#[test]
fn a_zero_card_selection_in_the_reorder_form_leaves_the_library_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let above = add_library_card(&mut runner, "Above", false);
    let kept = add_library_card(&mut runner, "Kept", false);
    let below = add_library_card(&mut runner, "Below", false);
    let deepest = add_library_card(&mut runner, "Deep Sentinel", false);
    let before = library_ids(&runner);
    assert_eq!(before, vec![above, kept, below, deepest]);

    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![above, kept, below],
        selectable_cards: vec![above, kept, below],
        keep_count: 1,
        up_to: true,
        kept_destination: Some(Zone::Library),
        rest_destination: None,
        rest_remainder: DigRestRemainder::StayInLibrary,
        rest_split_top_count: None,
        rest_order: DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect("an empty selection is a legal \"any number\" choice");

    let st = runner.state();
    assert!(
        st.players[0].graveyard.is_empty(),
        "CR 701.20e: nothing routes a card that was only looked at, got {:?}",
        st.players[0].graveyard
    );
    assert_eq!(
        library_ids(&runner),
        before,
        "declining every card leaves the library untouched, order included"
    );
    for card in [above, kept, below, deepest] {
        assert_eq!(
            st.objects[&card].zone,
            Zone::Library,
            "{} must stay in the library",
            st.objects[&card].name
        );
    }
}

/// CR 701.20a + CR 701.20e: a reveal-dig with a silent remainder ends the reveal
/// at the choice's information boundary. The unchosen cards stay in the library,
/// so a surviving `revealed_cards` marker would pair each name with the position
/// it holds after any later shuffle in the same resolution (CR 401.2).
#[test]
fn a_silent_remainder_ends_the_reveal_of_the_cards_it_leaves_in_the_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let above = add_library_card(&mut runner, "Above", false);
    let land = add_library_card(&mut runner, "Land", true);
    let below = add_library_card(&mut runner, "Below", false);
    for card in [above, land, below] {
        runner.state_mut().revealed_cards.insert(card);
    }

    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![above, land, below],
        selectable_cards: vec![land],
        keep_count: 1,
        up_to: true,
        kept_destination: Some(Zone::Battlefield),
        rest_destination: None,
        rest_remainder: DigRestRemainder::StayInLibrary,
        rest_split_top_count: None,
        rest_order: DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: true,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("putting the one land card onto the battlefield must be accepted");

    let st = runner.state();
    // Positive reach guard: the kept card really moved, so the cleanup ran on a
    // path that had something to deliver.
    assert_eq!(st.objects[&land].zone, Zone::Battlefield);
    assert_eq!(st.objects[&above].zone, Zone::Library);
    assert_eq!(st.objects[&below].zone, Zone::Library);
    assert!(
        !st.revealed_cards.contains(&above) && !st.revealed_cards.contains(&below),
        "the reveal ends with the instruction that caused it, got {:?}",
        st.revealed_cards
    );
}

/// CR 608.2c + CR 701.20e — the WHOLE-SET boundary of the remainder rule.
///
/// Petals of Insight (verbatim Scryfall text): "Look at the top three cards of
/// your library. You may put those cards on the bottom of your library in any
/// order. If you do, return Petals of Insight to its owner's hand. Otherwise,
/// draw three cards."
///
/// "Those cards" names every looked-at card, not a remainder of a keep. When the
/// parser folded it into the look's rest destination, the zero-keep look never
/// applied it, so accepting left all three cards on top. It now lowers to its own
/// placement over the looked-at set, which must move all three to the bottom.
#[test]
fn put_those_cards_on_the_bottom_moves_every_looked_at_card() {
    const PETALS_OF_INSIGHT: &str = "Look at the top three cards of your library. You may put \
those cards on the bottom of your library in any order. If you do, return Petals of Insight to \
its owner's hand. Otherwise, draw three cards.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Petals of Insight", false, PETALS_OF_INSIGHT);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 4,
    });
    let spell_id = builder.id();
    let mut runner = scenario.build();

    let looked_at: Vec<ObjectId> = (0..3)
        .map(|index| add_library_card(&mut runner, &format!("Looked {index}"), false))
        .collect();
    let deeper: Vec<ObjectId> = (0..2)
        .map(|index| add_library_card(&mut runner, &format!("Deeper {index}"), false))
        .collect();
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 4);
    let hand_before = runner.state().players[0].hand.len();

    let outcome = runner.cast(spell_id).accept_optional().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves fully, got {:?}",
        outcome.final_waiting_for()
    );

    let st = runner.state();
    assert!(
        st.stack.is_empty(),
        "Petals of Insight finished resolving, stack: {:?}",
        st.stack
    );
    // Positive reach guard: the "if you do" branch ran — Petals returned to hand
    // and the "otherwise" draw did not.
    assert_eq!(
        st.objects[&spell_id].zone,
        Zone::Hand,
        "accepting the placement returns Petals of Insight to its owner's hand"
    );
    assert_eq!(
        st.players[0].hand.len(),
        hand_before,
        "the cast card left the hand and came back; the \"otherwise\" draw never ran"
    );

    let library = library_ids(&runner);
    assert_eq!(library.len(), 5, "no card left the library");
    assert_eq!(
        &library[..2],
        deeper.as_slice(),
        "the two cards never looked at are now on top"
    );
    let mut bottom = library[2..].to_vec();
    bottom.sort();
    let mut expected = looked_at.clone();
    expected.sort();
    assert_eq!(
        bottom, expected,
        "all three looked-at cards went to the bottom, not one of them"
    );
    assert!(st.players[0].graveyard.is_empty());
}

/// CR 701.20e + CR 608.2c — the graveyard destination of the same whole-set
/// boundary. Looking at a card leaves it where it is (CR 701.20e follows
/// CR 701.20b), so "put those cards into your graveyard" after a bare look must
/// read the looked-at cards out of the LIBRARY.
///
/// No printed card pairs a bare look with this clause (Covenant of Minds reveals
/// and gives the choice to an opponent), so the text below is the minimal shape
/// of that class. Before the fix the move defaulted to the impulse exile origin,
/// scanned exile, and moved nothing.
#[test]
fn put_those_cards_into_the_graveyard_after_a_look_moves_every_looked_at_card() {
    const LOOK_THEN_GRAVEYARD: &str =
        "Look at the top three cards of your library. Put those cards into your graveyard.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Grim Tally", false, LOOK_THEN_GRAVEYARD);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    });
    let spell_id = builder.id();
    let mut runner = scenario.build();

    let looked_at: Vec<ObjectId> = (0..3)
        .map(|index| add_library_card(&mut runner, &format!("Looked {index}"), false))
        .collect();
    let deeper = add_library_card(&mut runner, "Deeper", false);
    add_mana(&mut runner, ManaType::Blue, 1);

    let outcome = runner.cast(spell_id).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves fully, got {:?}",
        outcome.final_waiting_for()
    );

    let st = runner.state();
    assert!(st.stack.is_empty(), "stack: {:?}", st.stack);
    assert!(
        st.players[0].mana_pool.is_empty(),
        "the {{U}} cost was paid"
    );
    for card in &looked_at {
        assert_eq!(
            st.objects[card].zone,
            Zone::Graveyard,
            "{} was looked at, so \"those cards\" moves it",
            st.objects[card].name
        );
    }
    assert_eq!(
        st.objects[&deeper].zone,
        Zone::Library,
        "the card below the looked-at window is never touched"
    );
    assert_eq!(library_ids(&runner), vec![deeper]);
}
