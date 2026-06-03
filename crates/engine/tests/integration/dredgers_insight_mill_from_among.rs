//! Regression: GitHub issue #424 — Dredger's Insight ("When this enchantment
//! enters, mill four cards. You may put an artifact, creature, or land card
//! from among the milled cards into your hand.").
//!
//! Bug: the "from among the milled cards" continuation was never recognized —
//! the continuation dispatch only fired for a preceding `Effect::Dig`, not
//! `Effect::Mill`. The "you may put ..." clause was therefore parsed as a
//! standalone follow-on `ChangeZone` carrying a raw `Or[Artifact, Creature,
//! Land]` filter with `origin: null`. At resolution that filter fell back to
//! the engine's default battlefield scan zone, so the engine offered a
//! *battlefield permanent* to return to hand instead of one of the milled
//! cards.
//!
//! Fix: the continuation dispatch now accepts a preceding `Effect::Mill` and
//! pushes a `ChangeZone` sub-ability whose target is
//! `TargetFilter::TrackedSetFiltered` — scoping the selection to the tracked
//! set of milled cards (CR 701.17c — "an effect that refers to a milled card
//! can find that card in the zone it moved to").
//!
//! End-to-end assertion: after Dredger's Insight enters and its ETB trigger
//! resolves, the only card the controller may move to hand is an *eligible
//! milled card* — a creature/artifact/land permanent on the battlefield is
//! never offered.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Issue #424: the put-from-among-milled choice must offer only milled cards.
///
/// Deterministic library top (milled by `Mill 4`): one eligible card
/// (Grizzly Bears — a creature) and three ineligible cards (Lightning Bolt —
/// instants). A battlefield Grizzly Bears acts as the trap: pre-fix, the raw
/// `Or[Artifact, Creature, Land]` filter scanned the battlefield and offered
/// *that* permanent. Post-fix, the `TrackedSetFiltered` target restricts the
/// choice to the milled creature only.
#[test]
fn dredgers_insight_offers_only_milled_cards_not_battlefield() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let insight_id = scenario.add_real_card(P0, "Dredger's Insight", Zone::Hand, db);

    // Library top-first: the first card added is the top of the library
    // (Mill takes `library.iter().take(count)`). Top 4 = one creature + three
    // instants, so exactly one milled card matches the artifact/creature/land
    // filter.
    let milled_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }
    // Padding so the library is not emptied by the mill.
    for _ in 0..4 {
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    }

    // The trap: a creature already on the battlefield. It matches the raw
    // Or[Artifact, Creature, Land] filter but is NOT a milled card.
    let battlefield_bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Colorless, ManaType::Green]);

    // Resolve the spell (enchantment enters) and its ETB trigger. The Mill 4
    // resolves, then the put-from-among-milled `ChangeZone` sub-ability runs;
    // the cast driver stops at the resulting `EffectZoneChoice` (an optional
    // selection it does not auto-answer), leaving the live runner parked there.
    let outcome = runner.cast(insight_id).resolve();

    let WaitingFor::EffectZoneChoice {
        cards, destination, ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected EffectZoneChoice for the put-from-milled clause, got {:?}",
            outcome.final_waiting_for()
        );
    };

    // CR 701.17c: the choice is scoped to the milled cards. The milled creature
    // is the sole eligible card; the battlefield Grizzly Bears must NOT appear.
    assert!(
        cards.contains(&milled_creature),
        "the milled creature must be an offered choice; offered = {cards:?}"
    );
    assert!(
        !cards.contains(&battlefield_bear),
        "a battlefield permanent must NEVER be offered — the selection is \
         scoped to the milled cards (issue #424); offered = {cards:?}"
    );
    assert_eq!(
        *destination,
        Some(Zone::Hand),
        "the milled card is moved to the controller's hand"
    );

    // Resolve the choice by taking the milled creature.
    runner
        .act(GameAction::SelectCards {
            cards: vec![milled_creature],
        })
        .expect("selecting the milled creature should succeed");
    runner.advance_until_stack_empty();

    // Observable outcome: the milled creature is in hand, the battlefield
    // permanent stayed on the battlefield.
    assert!(
        runner.state().players[0].hand.contains(&milled_creature),
        "the chosen milled creature should be in hand; hand = {:?}",
        runner.state().players[0].hand,
    );
    assert_eq!(
        runner.state().objects[&battlefield_bear].zone,
        Zone::Battlefield,
        "the battlefield Grizzly Bears must NOT have been moved"
    );
}
