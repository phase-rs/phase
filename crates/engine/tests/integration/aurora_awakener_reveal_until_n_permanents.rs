//! Runtime regression for the "reveal cards from the top of your library until
//! you reveal X [type] cards" + "put any number of those onto the battlefield,
//! rest on the bottom in a random order" pattern (Aurora Awakener).
//!
//! Aurora Awakener (ETB): "reveal cards from the top of your library until you
//! reveal X permanent cards, where X is the number of colors among permanents
//! you control. Put any number of those permanent cards onto the battlefield,
//! then put the rest of the revealed cards on the bottom of your library in a
//! random order."
//!
//! The single true engine gap was VARIABLE-LENGTH termination — revealing until
//! N (dynamic) matches, not until the first match. This drives the real
//! `Effect::RevealUntil` resolver (with the new `count` parameterization and the
//! `ChooseAnyNumber` matched-set disposition) and submits the keep-selection
//! through `apply()` (the production `DigChoice` handler), then asserts:
//!   * the until-loop terminates at exactly X matches (not 1, not all),
//!   * the chosen permanent reaches the battlefield,
//!   * the rest of the revealed cards (non-chosen matches + misses) go to the
//!     BOTTOM of the library,
//!   * X = 0 reveals nothing.
//!
//! REVERT-PROOF: if `count` were reverted to `Fixed(1)` the loop would stop at
//! the FIRST permanent and the second permanent (`perm_b`) would never be
//! revealed — the `assert!(revealed.contains(&perm_b))` and the "perm_b on the
//! bottom" assertions both flip. If the `ChooseAnyNumber` disposition were
//! reverted to `KeepEach`, no `DigChoice` would be surfaced and the
//! `WaitingFor::DigChoice` match below would panic.

use engine::game::effects::reveal_until;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    ControllerRef, Effect, QuantityExpr, QuantityRef, ResolvedAbility, RevealUntilDisposition,
    TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};

/// "the number of colors among permanents you control" — the Aurora/Sanar count.
fn distinct_colors_count() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctColorsAmongPermanents {
            filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
        },
    }
}

/// "reveal until you reveal X permanent cards. Put any number of those permanent
/// cards onto the battlefield, then put the rest on the bottom in a random
/// order." built manually (no card-data.json).
fn aurora_reveal_until(count: QuantityExpr, source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::permanent()),
            count,
            matched_disposition: RevealUntilDisposition::ChooseAnyNumber,
            kept_destination: Zone::Battlefield,
            rest_destination: Zone::Library,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        },
        vec![],
        source,
        P0,
    )
}

#[test]
fn aurora_reveals_until_two_permanents_keeps_one_rest_to_bottom() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Two distinctly-colored permanents you control → X = 2.
    let white_perm = scenario.add_creature(P0, "White Bear", 2, 2).id();
    let blue_perm = scenario.add_creature(P0, "Blue Bear", 2, 2).id();

    // Library top → bottom: land (miss), permA (match #1), land (miss),
    // permB (match #2), permC (a third permanent that must NOT be revealed —
    // the loop stops at the 2nd match).
    let miss1 = scenario.add_card_to_library_top(P0, "Library Bottom Marker");
    let perm_c = scenario.add_card_to_library_top(P0, "Perm C");
    let perm_b = scenario.add_card_to_library_top(P0, "Perm B");
    let miss2 = scenario.add_card_to_library_top(P0, "Miss 2");
    let perm_a = scenario.add_card_to_library_top(P0, "Perm A");
    let miss_top = scenario.add_card_to_library_top(P0, "Miss Top");

    let source = scenario.add_creature(P0, "Aurora Source", 1, 1).id();

    let mut runner = scenario.build();

    // Color the battlefield permanents (white + blue → 2 distinct colors).
    runner
        .state_mut()
        .objects
        .get_mut(&white_perm)
        .unwrap()
        .color = vec![ManaColor::White];
    runner
        .state_mut()
        .objects
        .get_mut(&blue_perm)
        .unwrap()
        .color = vec![ManaColor::Blue];

    // Type the library cards. Permanents are creatures; misses are sorceries.
    for &id in &[perm_a, perm_b, perm_c] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }
    for &id in &[miss_top, miss2, miss1] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
    }

    // Drive the production resolver (the effect's entry point).
    let ability = aurora_reveal_until(distinct_colors_count(), source);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).expect("resolve ok");

    // The disposition must surface a DigChoice over the matched set.
    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        keep_count,
        up_to,
        kept_destination,
        rest_destination,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected DigChoice for the ChooseAnyNumber disposition, got {:?}",
            runner.state().waiting_for
        );
    };

    // The until-loop terminated at exactly X=2 matches: perm_a and perm_b were
    // revealed; perm_c (the 3rd permanent) was NOT.
    assert!(
        cards.contains(&perm_a) && cards.contains(&perm_b),
        "both matched permanents must be revealed; revealed = {cards:?}"
    );
    assert!(
        !cards.contains(&perm_c),
        "the loop must stop at the 2nd match — perm_c must NOT be revealed; revealed = {cards:?}"
    );
    assert!(
        cards.contains(&miss_top) && cards.contains(&miss2),
        "interleaved non-permanent cards revealed before the 2nd match are part of the pile"
    );
    assert!(
        !cards.contains(&miss1),
        "the post-2nd-match library remainder must not be revealed"
    );
    assert_eq!(
        selectable_cards,
        vec![perm_a, perm_b],
        "only the matched permanents are selectable for the battlefield"
    );
    assert_eq!(
        keep_count, 2,
        "keep_count is the matched-set size (X reached)"
    );
    assert!(up_to, "the controller may keep any number (0..=X)");
    assert_eq!(kept_destination, Some(Zone::Battlefield));
    assert_eq!(rest_destination, Some(Zone::Library));

    // Submit the keep-selection through apply(): keep perm_a only.
    runner
        .act(GameAction::SelectCards {
            cards: vec![perm_a],
        })
        .expect("DigChoice SelectCards must succeed");

    let state = runner.state();

    // The chosen permanent reaches the battlefield.
    assert_eq!(
        state.objects[&perm_a].zone,
        Zone::Battlefield,
        "the chosen permanent must enter the battlefield"
    );

    // The rest of the revealed cards (non-chosen match perm_b + the two misses)
    // go to the BOTTOM of the library; perm_c and miss1 (never revealed) keep
    // their relative position above them.
    let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();
    for &id in &[perm_b, miss_top, miss2] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Library,
            "rest card {id:?} returns to the library"
        );
        assert!(
            library.contains(&id),
            "rest card {id:?} must be in the library; library = {library:?}"
        );
    }
    // perm_b (a revealed-but-not-chosen permanent) must be BELOW perm_c (never
    // revealed, stayed in place): the rest pile is bottomed.
    let pos = |needle: ObjectId| library.iter().position(|&c| c == needle);
    assert!(
        pos(perm_b) > pos(perm_c),
        "the revealed-not-chosen permanent must be bottomed below the un-revealed remainder; \
         library = {library:?}"
    );
    assert!(
        !state.players[0].hand.contains(&perm_a) && !state.players[0].hand.contains(&perm_b),
        "no revealed card is stolen into hand"
    );
}

/// CR 701.20a: X = 0 (zero colors among permanents you control) reveals nothing
/// and resolves with no interaction. Discriminates the dynamic `count`: a
/// reverted `Fixed(1)` would reveal one card and surface a DigChoice.
#[test]
fn aurora_with_zero_colors_reveals_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // A single COLORLESS permanent → 0 distinct colors → X = 0.
    let colorless = scenario.add_creature(P0, "Colorless Golem", 3, 3).id();

    let lib_card = scenario.add_card_to_library_top(P0, "Top Library Card");
    let source = scenario.add_creature(P0, "Aurora Source", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&colorless)
        .unwrap()
        .color = vec![];
    runner
        .state_mut()
        .objects
        .get_mut(&lib_card)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let ability = aurora_reveal_until(distinct_colors_count(), source);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).expect("resolve ok");

    // No DigChoice — nothing was revealed.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "X=0 must not surface a DigChoice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&lib_card].zone,
        Zone::Library,
        "the top card must not be revealed or moved when X=0"
    );
    assert!(
        !runner.state().revealed_cards.contains(&lib_card),
        "no card is revealed when X=0"
    );
}

/// The `Fixed(1)` default preserves the historical single-hit behavior end to
/// end: a default-count `KeepEach` RevealUntil reveals until the FIRST match and
/// routes it to `kept_destination`, surfacing no DigChoice. This is the negative
/// control for the parameterization — if the resolver always ran the
/// multi-match `ChooseAnyNumber` path, this would surface a DigChoice instead.
#[test]
fn default_count_keep_each_preserves_single_hit_behavior() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let land = scenario.add_card_to_library_top(P0, "Library Land");
    let creature = scenario.add_card_to_library_top(P0, "Top Creature");
    let source = scenario.add_creature(P0, "Source", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    runner
        .state_mut()
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let ability = ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            matched_disposition: RevealUntilDisposition::KeepEach,
            kept_destination: Zone::Hand,
            rest_destination: Zone::Library,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        },
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).expect("resolve ok");

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "default KeepEach single-hit RevealUntil must not surface a DigChoice"
    );
    assert!(
        runner.state().players[0].hand.contains(&creature),
        "the single matched creature must go to hand (kept_destination)"
    );
}
