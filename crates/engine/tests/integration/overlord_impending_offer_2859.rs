//! Engine-contract guard for #2859 — Overlord of the Balemurk "cannot be cast
//! for its Impending cost".
//!
//! Oracle text:
//!   Impending 5—{1}{B} (If you cast this spell for its impending cost, it
//!   enters with five time counters and isn't a creature until the last is
//!   removed. At the beginning of your end step, remove a time counter from it.)
//!   Whenever this permanent enters or attacks, mill four cards, then you may
//!   return a non-Avatar creature card or a planeswalker card from your
//!   graveyard to your hand.
//!
//! ROOT CAUSE (display layer): the engine DOES surface the Impending choice for
//! an Overlord-cycle creature (creature + enter/attack trigger) — the bug was in
//! the frontend, whose `AlternativeCastChoice.keyword` TypeScript union and the
//! `AlternativeCostModal` switch both omitted `Impending` (and `Emerge`,
//! `Prototype`, `Spectacle`), so when the engine sent `keyword: Impending` the
//! modal produced no copy and the option never rendered. That is fixed in
//! `client/`; this test locks the engine half of the contract so a future
//! refactor can't silently stop firing the offer the frontend now consumes.
//!
//! CR 702.176a: a card with Impending may be cast either for its mana cost
//! (normal creature) or for its impending cost (enters with time counters and
//! isn't a creature until the last is removed). When both costs are affordable
//! the engine must present the choice (`AlternativeCastChoice`); when only the
//! impending cost is payable it must route straight into the impending path.
//!
//! THE GATE: with enough mana for BOTH the printed `{3}{B}{B}` and the impending
//! `{1}{B}`, casting must yield
//! `WaitingFor::AlternativeCastChoice { keyword: Impending, .. }` — even for an
//! Overlord-cycle creature that also carries an enter/attack trigger.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::{AlternativeCastKeyword, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Overlord of the Balemurk's full Oracle text, including the Impending keyword
/// line and the "enters or attacks" trigger that defines the Overlord cycle.
const OVERLORD_ORACLE: &str = "Impending 5—{1}{B} (If you cast this spell for its impending cost, it enters with five time counters and isn't a creature until the last is removed. At the beginning of your end step, remove a time counter from it.)\nWhenever this permanent enters or attacks, mill four cards, then you may return a non-Avatar creature card or a planeswalker card from your graveyard to your hand.";

/// Fund P0's mana pool with `n` black mana (deterministic — no land modelling).
fn add_black_mana(state: &mut engine::types::game_state::GameState, n: u32) {
    for _ in 0..n {
        let unit = ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]);
        state.players[0].mana_pool.add(unit);
    }
}

#[test]
fn overlord_cycle_creature_surfaces_impending_alternative_cast() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Build Overlord of the Balemurk in P0's hand: printed cost {3}{B}{B},
    // Impending 5—{1}{B}, plus the enter/attack trigger. The "Impending"
    // keyword hint makes the keyword line parse to `Keyword::Impending` exactly
    // as MTGJSON's keywords array does in production.
    let overlord = scenario
        .add_creature_to_hand_from_oracle(P0, "Overlord of the Balemurk", 6, 5, OVERLORD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 3,
        })
        .from_oracle_text_with_keywords(&["Impending"], OVERLORD_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Sanity: the object must actually carry the Impending keyword (otherwise the
    // test would pass vacuously by never reaching the offer block).
    assert!(
        runner.state().objects[&overlord]
            .keywords
            .iter()
            .any(|k| matches!(k, engine::types::keywords::Keyword::Impending { .. })),
        "Overlord must carry Keyword::Impending after Oracle build"
    );

    // Five black mana covers BOTH the printed {3}{B}{B} (five mana, ≥2 black)
    // and the impending {1}{B} (two mana, ≥1 black) — both costs affordable, so
    // CR 702.176a requires the engine to present the alternative-cost choice.
    add_black_mana(runner.state_mut(), 5);

    let card_id = runner.state().objects[&overlord].card_id;
    runner
        .act(engine::types::actions::GameAction::CastSpell {
            object_id: overlord,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("CastSpell must be accepted by the engine");

    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice { keyword, .. } => {
            assert_eq!(
                *keyword,
                AlternativeCastKeyword::Impending,
                "expected the Impending alternative-cost choice, got {keyword:?}"
            );
        }
        other => panic!(
            "expected WaitingFor::AlternativeCastChoice(Impending) for an Overlord-cycle \
             creature with both costs affordable, got {other:?}"
        ),
    }
}
