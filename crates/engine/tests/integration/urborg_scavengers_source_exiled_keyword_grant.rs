//! CR 611.3a + CR 607.2a: Urborg Scavengers — "This creature has flying as long as a card
//! exiled with it has flying. The same is true for first strike, double strike,
//! deathtouch, haste, hexproof, indestructible, lifelink, menace, reach, trample,
//! and vigilance."
//!
//! Runtime regression proving the source-linked exiled-object keyword grant is
//! (a) CONDITIONAL — Urborg has none of the granted keywords with no matching
//! card exiled with it — and (b) PER-ITEM INDEPENDENT — a card exiled with it that
//! has vigilance grants ONLY vigilance, never flying. On `main` the "the same is
//! true for" tail collapses (the continuation is swallowed into an `Unrecognized`
//! condition), so the continuation keywords are never grantable and assertion (b)
//! flips. This drives the real parse → `evaluate_layers` path, not a hand-built
//! AST.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const URBORG_SCAVENGERS: &str = "This creature has flying as long as a card exiled with it has flying. The same is true for first strike, double strike, deathtouch, haste, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance.";

#[test]
fn urborg_scavengers_source_exiled_grant_is_conditional_and_per_item() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urborg = scenario
        .add_creature_from_oracle(P0, "Urborg Scavengers", 2, 2, URBORG_SCAVENGERS)
        .id();
    let mut runner = scenario.build();
    // Force continuous-effect recomputation (SBAs, layers).
    runner.act(GameAction::PassPriority).ok();

    // (a) Conditional: nothing is exiled with Urborg, so it has NONE of the
    // granted keywords. (On `main`, if the collapsed static's `Unrecognized`
    // condition resolves true, Urborg would already have flying here.)
    assert!(
        !runner.state().objects[&urborg].has_keyword(&Keyword::Flying),
        "no card exiled with Urborg ⇒ no flying"
    );
    assert!(
        !runner.state().objects[&urborg].has_keyword(&Keyword::Vigilance),
        "no card exiled with Urborg ⇒ no vigilance"
    );

    // Seed a card exiled WITH Urborg that has vigilance but NOT flying. (A raw
    // state edit bypasses normal change tracking, so re-run the layer engine.)
    {
        let state = runner.state_mut();
        let exiled = create_object(
            state,
            CardId(900),
            P0,
            "Exiled Sentinel".to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&exiled).expect("exiled card exists");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Vigilance);
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: urborg,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    evaluate_layers(runner.state_mut());

    // (b) Per-item independence: the card exiled with Urborg has vigilance, so
    // Urborg gains VIGILANCE — a *continuation* keyword that `main` drops entirely
    // — and does NOT gain flying, because no card exiled with it has flying.
    assert!(
        runner.state().objects[&urborg].has_keyword(&Keyword::Vigilance),
        "a card exiled with Urborg that has vigilance ⇒ Urborg gains vigilance (continuation keyword, independently gated)"
    );
    assert!(
        !runner.state().objects[&urborg].has_keyword(&Keyword::Flying),
        "the exiled card has no flying ⇒ Urborg must NOT gain flying (per-item independence, not a shared condition)"
    );
}
