//! Runtime resolution regression for **issue #5631** (The Dragon-Kami Reborn).
//!
//! Chapters I–II read: *"You gain 2 life. Look at the top three cards of your
//! library. **Exile one of them face down with a hatching counter on it**, then
//! put the rest on the bottom of your library in any order."*
//!
//! Root cause (pre-fix): `parse_exile_one_of_them_face_down` required
//! end-of-clause immediately after `"face down"`, so the CR 122.1 counter rider
//! (`"with a hatching counter on it"`) made the recognizer decline. With the
//! recognizer declining, the Hideaway-style fusion never fired: the `Dig`
//! short-circuited as a `keep_count: 0` pure-peek and a sibling
//! `ChangeZone { ParentTarget }` exiled the **trigger source itself** (the Saga,
//! with a hatching counter) instead of one of the three looked-at cards.
//!
//! Fix (parser): the recognizer accepts the optional counter rider and threads
//! it into the fused exile as `Dig(keep_count:1, Exile) ->
//! HideawayConceal(ParentTarget) -> PutCounter(hatching, ParentTarget)`. The
//! `DigChoice` resolution binds the CHOSEN dug card onto the continuation
//! chain's `ParentTarget`, so the hatching counter lands on the one card the
//! player selected.
//!
//! The existing PR coverage only inspects the parsed AST shape. This test drives
//! the real resolution path: it resolves the chapter effect, answers the
//! `DigChoice`, and asserts (a) the player-selected card is exiled face down
//! with exactly one hatching counter, and (b) the Saga stays on the battlefield
//! with NO hatching counter. Both assertions flip when the fusion or the
//! counter-target binding is reverted (the counter would ride the source Saga).
//!
//! CR 122.1: counters are placed on the object the instruction specifies.
//! CR 406.3 / CR 708.2: a card exiled face down has no characteristics.
//! CR 608.2c: a `ParentTarget` sub-ability inherits the parent link's target.
//! CR 702.75a: Hideaway is the structural analog this lowering mirrors.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::zones::Zone;

/// Chapters I–II of The Dragon-Kami Reborn, verbatim minus the "I, II —"
/// chapter prefix (the parser receives the effect text, not the chapter label).
const CHAPTER_I_II: &str = "You gain 2 life. Look at the top three cards of your library. \
Exile one of them face down with a hatching counter on it, then put the rest on the bottom of your library in any order.";

/// The fused-exile clause in isolation (no leading "You gain 2 life."), so the
/// Dig sits at the chain root for the AST shape guard.
const FUSED_CLAUSE: &str = "Look at the top three cards of your library. \
Exile one of them face down with a hatching counter on it, then put the rest on the bottom of your library in any order.";

#[test]
fn hatching_counter_lands_on_chosen_exiled_card_not_the_saga() {
    let hatching = parse_counter_type("hatching");
    assert_eq!(
        hatching,
        CounterType::Generic("hatching".to_string()),
        "sanity: the rider counter is a generic hatching counter"
    );

    // POSITIVE REACH-GUARD (foot-gun #6): prove the clause parses to the fused
    // choose-one exile with the hatching counter chained onto the chosen card.
    // Without this the "Saga has no counter" assertion below could pass
    // vacuously (e.g. if the effect stopped parsing to a Dig at all). Walk the
    // parsed chain's sub_ability spine, capturing the fused shape.
    let fused = parse_effect_chain(FUSED_CLAUSE, AbilityKind::Spell);
    let mut dig_keep_count: Option<u32> = None;
    let mut dig_destination: Option<Zone> = None;
    let mut saw_conceal = false;
    let mut counter_rider: Option<(CounterType, QuantityExpr, TargetFilter)> = None;
    let mut cursor: Option<&AbilityDefinition> = Some(&fused);
    while let Some(node) = cursor {
        match &*node.effect {
            Effect::Dig {
                keep_count,
                destination,
                ..
            } => {
                dig_keep_count = *keep_count;
                dig_destination = *destination;
            }
            Effect::HideawayConceal { .. } => saw_conceal = true,
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                counter_rider = Some((counter_type.clone(), count.clone(), target.clone()));
            }
            _ => {}
        }
        cursor = node.sub_ability.as_deref();
    }

    assert_eq!(
        dig_keep_count,
        Some(1),
        "the fusion must patch the peek Dig to keep exactly one card"
    );
    assert_eq!(
        dig_destination,
        Some(Zone::Exile),
        "the chosen card is routed to exile by the Dig itself"
    );
    assert!(
        saw_conceal,
        "a HideawayConceal must be chained onto the Dig (CR 702.75a)"
    );
    let (rider_type, rider_count, rider_target) =
        counter_rider.expect("the counter rider must append a PutCounter");
    assert_eq!(rider_type, hatching, "the rider counter is hatching");
    assert_eq!(rider_count, QuantityExpr::Fixed { value: 1 });
    assert_eq!(
        rider_target,
        TargetFilter::ParentTarget,
        "the counter must land on the chosen dug card (ParentTarget), not the source"
    );

    // Build the game: stack the library so the top three are the looked-at cards
    // and a fourth, deeper card must NOT be seen (proves the dig is bounded to
    // three). `add_card_to_library_top` inserts at the top, so add the deepest
    // card first.
    let mut scenario = GameScenario::new();
    let lib_deep = scenario.add_card_to_library_top(P0, "Lib Deep Card");
    let lib3 = scenario.add_card_to_library_top(P0, "Lib Card 3");
    let lib2 = scenario.add_card_to_library_top(P0, "Lib Card 2");
    let lib1 = scenario.add_card_to_library_top(P0, "Lib Card 1");

    let mut runner = scenario.build();

    // The Dragon-Kami Reborn as an Enchantment Saga on the battlefield. Its
    // chapter ability's source is this permanent — it must stay put and must not
    // catch the hatching counter.
    let saga = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = zones::create_object(
            state,
            card_id,
            P0,
            "The Dragon-Kami Reborn".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.summoning_sick = false;
        id
    };

    let life_before = runner.state().players[0].life;

    // Resolve the FULL chapter effect (source = the Saga). GainLife runs, then
    // the Dig pauses at a DigChoice; its conceal + counter continuation is
    // stashed. Using the whole chapter text keeps this faithful to the card.
    let def = parse_effect_chain(CHAPTER_I_II, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&def, saga, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter I/II resolution must reach the DigChoice");

    // POSITIVE REACH-GUARD: the "gain 2 life" clause ran before the Dig paused,
    // proving the chain actually executed rather than short-circuiting.
    assert_eq!(
        runner.state().players[0].life,
        life_before + 2,
        "the chapter must gain 2 life before looking at the library"
    );

    // The Dig surfaced a choice over exactly the top three cards (CR 701.20e) —
    // the deeper card was not looked at.
    let looked_at = match runner.state().waiting_for.clone() {
        WaitingFor::DigChoice { cards, .. } => cards,
        other => panic!("expected a DigChoice from the fused exile, got {other:?}"),
    };
    assert_eq!(looked_at.len(), 3, "the chapter looks at three cards");
    assert_eq!(looked_at, vec![lib1, lib2, lib3]);
    assert!(
        !looked_at.contains(&lib_deep),
        "the deeper (4th) card must not be looked at"
    );

    // Player picks the first looked-at card to exile.
    let chosen = looked_at[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("SelectCards (dig keep) accepted");

    let state = runner.state();
    let chosen_obj = &state.objects[&chosen];
    let saga_obj = &state.objects[&saga];

    // DISCRIMINATOR, part 1: the player-selected card is exiled face down with
    // exactly one hatching counter (CR 122.1 + CR 406.3).
    assert_eq!(
        chosen_obj.zone,
        Zone::Exile,
        "the chosen dug card must be exiled"
    );
    assert!(
        chosen_obj.face_down,
        "the exiled dug card must be face down (CR 406.3)"
    );
    assert_eq!(
        chosen_obj.counters.get(&hatching).copied().unwrap_or(0),
        1,
        "the chosen exiled card must carry exactly one hatching counter (CR 122.1)"
    );

    // DISCRIMINATOR, part 2 — the regression direction: the Saga stays on the
    // battlefield and does NOT catch the hatching counter. Pre-fix the discarded
    // sibling `ChangeZone { ParentTarget }` / `PutCounter { ParentTarget }` rode
    // the trigger source, so the counter (and the exile) landed on the Saga.
    assert_eq!(
        saga_obj.zone,
        Zone::Battlefield,
        "the Saga must remain on the battlefield — the dug card is exiled, not the Saga"
    );
    assert!(
        !saga_obj.face_down,
        "the Saga must not be turned face down (CR 406.3)"
    );
    assert_eq!(
        saga_obj.counters.get(&hatching).copied().unwrap_or(0),
        0,
        "the hatching counter must NOT ride the Saga (the #5631 regression)"
    );

    // The other two looked-at cards are not exiled (they go to the bottom of the
    // library) and carry no hatching counter.
    for &id in &[lib2, lib3] {
        let obj = &state.objects[&id];
        assert_ne!(obj.zone, Zone::Exile, "only the chosen card is exiled");
        assert_eq!(
            obj.counters.get(&hatching).copied().unwrap_or(0),
            0,
            "an unchosen looked-at card must not receive the counter"
        );
    }
}
