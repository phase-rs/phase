//! Discriminating runtime regression for **Elixir** (std long-tail batch):
//!
//! > {5}, {T}, Exile this artifact: Shuffle all nonland cards from your
//! > graveyard into your library. You gain life equal to the number of cards
//! > shuffled into your library this way.
//!
//! Two residual gaps:
//!
//! 1. "Shuffle all nonland cards from your graveyard into your library" is a
//!    *mandatory mass move* (CR 400.6) — every eligible nonland card moves with
//!    no interactive choice. The pre-fix parse produced a single-target
//!    `Effect::ChangeZone` whose resolution-time cardinality is "choose up to
//!    one", so only one card moved. The fix routes the leading "all" quantifier
//!    to `Effect::ChangeZoneAll` (filtered by `Non(Land)`, origin Graveyard).
//!
//! 2. The dynamic life quantity "the number of cards shuffled into your library
//!    this way" was dropped (`Effect::Unimplemented`), so no life was gained.
//!    The fix recognizes the phrase as `QuantityRef::EventContextAmount`
//!    (mirroring "milled this way"). `ChangeZoneAll` stamps `last_effect_count`
//!    = number of cards moved, which `EventContextAmount` reads at resolution.
//!
//! The test drives the parsed effect chain (ChangeZoneAll -> Shuffle ->
//! GainLife) through the production resolver `resolve_ability_chain`.
//!
//! DISCRIMINATOR: P0 starts at 20 life with three nonland cards and one land in
//! the graveyard. After resolution all three nonland cards are in the library,
//! the land remains in the graveyard, and P0 has gained exactly 3 life
//! (reaching 23). Reverting EITHER fix flips an assertion:
//! - revert the mass-move fix -> only one nonland card moves (the others stay
//!   in the graveyard) and `last_effect_count` is 1, so life rises by only 1.
//! - revert the dynamic quantity -> the GainLife node never resolves and life
//!   stays 20.
//!
//! CR 119.3: life-gain amount. CR 400.6: mass move of all eligible objects.
//! CR 701.24: shuffle. CR 608.2c: instructions resolve in order.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ELIXIR_ACTIVATED: &str = "{5}, {T}, Exile Elixir: Shuffle all nonland cards from your \
graveyard into your library. You gain life equal to the number of cards shuffled into your \
library this way.";

#[test]
fn elixir_shuffles_all_nonland_cards_and_gains_life_equal_to_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let source = scenario.add_creature(P0, "Elixir", 0, 0).as_artifact().id();
    let mut runner = scenario.build();

    // Seed P0's graveyard with three nonland cards and one land card.
    let mut nonland_ids = Vec::new();
    for i in 0..3 {
        let id = create_object(
            runner.state_mut(),
            CardId(1000 + i),
            P0,
            format!("Nonland {i}"),
            Zone::Graveyard,
        );
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        nonland_ids.push(id);
    }
    let land_id = create_object(
        runner.state_mut(),
        CardId(2000),
        P0,
        "Graveyard Land".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&land_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    // Parse the activated ability and resolve its EFFECT chain (the part after
    // the cost) through the production resolver. We exercise the
    // ChangeZoneAll -> Shuffle -> GainLife chain — the seam this change owns.
    let parsed = parse_oracle_text(
        ELIXIR_ACTIVATED,
        "Elixir",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "expected one activated ability");
    let ability = build_resolved_from_def(&parsed.abilities[0], source, P0);

    let life_before = runner.life(P0);

    // "Shuffle all nonland cards" is a mandatory mass move — `ChangeZoneAll`
    // moves every eligible nonland card with no interactive choice, so the whole
    // chain resolves in one pass with no pause.
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Elixir's shuffle-then-gain-life chain must resolve");
    runner.advance_until_stack_empty();

    // DISCRIMINATOR (mass-move fix): every nonland card left the graveyard for
    // the library; the land stayed behind. Reverting the `ChangeZoneAll` routing
    // leaves only one nonland card moved (the others remain in the graveyard).
    for id in &nonland_ids {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Library,
            "every nonland card must be shuffled into the library; {id:?} did not move"
        );
    }
    assert_eq!(
        runner.state().objects[&land_id].zone,
        Zone::Graveyard,
        "the land card must NOT be shuffled — only nonland cards are eligible"
    );

    // DISCRIMINATOR (dynamic-quantity fix): P0 gained life equal to the number
    // of cards shuffled this way (3). Reverting the dynamic quantity leaves the
    // GainLife unresolved and life unchanged; reverting the mass-move fix caps
    // the gain at 1.
    assert_eq!(
        runner.life(P0),
        life_before + 3,
        "P0 must gain life equal to the number of nonland cards shuffled this way (3)"
    );
}
