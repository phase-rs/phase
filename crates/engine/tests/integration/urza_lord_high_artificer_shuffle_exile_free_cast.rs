//! Regression: Urza, Lord High Artificer's third ability —
//! "{5}: Shuffle your library, then exile the top card. Until end of turn, you
//! may play that card without paying its mana cost."
//!
//! Before the fix the clause misparsed into a whole-library TUTOR (a generic
//! `ChangeZone` over the entire library that opened a `WaitingFor::
//! EffectZoneChoice` selection prompt) plus a `CastFromZone { target:
//! ParentTarget }` that bound to nothing. The correct parse is a deterministic
//! top-of-library exile followed by a tracked-set free-cast grant:
//!
//!   Shuffle{Controller}
//!     -> ExileTop { player: Controller, count: Fixed(1) }
//!     -> CastFromZone { target: TrackedSet(0), without_paying_mana_cost: true,
//!                       mode: Play, duration: UntilEndOfTurn }
//!
//! CR 701.24a: after the shuffle the library order is RANDOM, so the exiled top
//! card is fixed by post-shuffle order — the player makes NO selection. The
//! ABSENCE of a tutor prompt is the rules-correct behavior. CR 118.9 + CR 601.2a:
//! the exiled card is then castable for free until end of turn.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Find the index (into `obj.abilities`) of Urza's `{5}` ability — the only one
/// whose top-level effect is `Effect::Shuffle`. Robust against ability ordering.
fn shuffle_ability_index(state: &engine::types::game_state::GameState, urza: ObjectId) -> usize {
    state.objects[&urza]
        .abilities
        .iter()
        .position(|def| matches!(&*def.effect, Effect::Shuffle { .. }))
        .expect("Urza must have a `{5}` shuffle-then-exile ability")
}

/// The full end-to-end discriminator: activating Urza's `{5}` exiles the
/// post-shuffle top card (NOT a tutor) and makes exactly that card castable for
/// free until end of turn.
#[test]
fn urza_five_ability_exiles_top_and_offers_free_cast() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urza = scenario.add_real_card(P0, "Urza, Lord High Artificer", Zone::Battlefield, db);
    // A single, known, castable card in P0's library so the post-shuffle top is
    // deterministic (a one-card library shuffles to itself). Grizzly Bears is a
    // vanilla {1}{G} creature — no targets, so the free cast completes cleanly to
    // the stack, and the {1}{G} cost makes "zero mana paid" a real discriminator.
    let spell = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Reduce P0's library to exactly the spell so the shuffled top is known.
    {
        let player = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap();
        player.library.retain(|&id| id == spell);
        assert_eq!(
            player.library.len(),
            1,
            "library must contain exactly the known top card"
        );
    }

    // Fund the {5} activation cost from the pool (source auto-tap is not modeled).
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for _ in 0..5 {
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    let idx = shuffle_ability_index(runner.state(), urza);

    // Activate and resolve the {5}. If the buggy library-wide tutor were still
    // present, resolution would stop at WaitingFor::EffectZoneChoice and the
    // activation driver would panic on the unhandled prompt — so reaching a
    // clean Priority window is itself part of the discriminator.
    let outcome = runner.activate(urza, idx).resolve();

    // NEGATIVE: no tutor selection prompt was raised over the library.
    assert!(
        !matches!(
            outcome.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "the {{5}} ability must NOT open a library-wide tutor prompt; got {:?}",
        outcome.state().waiting_for
    );

    // Exactly one Library->Exile move: the spell is now in exile and gone from
    // the library (a deterministic top-of-library exile, not a mass move).
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Exile,
        "the post-shuffle top card must be exiled"
    );
    assert!(
        !runner.state().players[0].library.contains(&spell),
        "the exiled top card must have left the library"
    );

    // POSITIVE discriminator (fails if the cast binding is broken): the exiled
    // card surfaces as a CastSpell in legal_actions. With the original
    // `target: ParentTarget` bug this grant bound to nothing and no such action
    // existed.
    let legal = legal_actions(runner.state());
    let has_free_cast = legal.iter().any(|a| {
        matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )
    });
    assert!(
        has_free_cast,
        "the exiled top card must be castable from exile; legal_actions={legal:?}"
    );

    // Cast it for FREE: the spell moves Exile->Stack with zero mana paid. The
    // pool was emptied by the {5} activation, so a {1}{G} cost would make the
    // cast illegal here — a vanilla creature has no targets, so the cast lands
    // directly on the stack.
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "precondition: the activation drained the pool, so the cast must be free"
    );
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
        })
        .expect("the exiled top card must be castable from exile without paying mana");
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "casting the exiled card for free must move it Exile->Stack"
    );
}

/// DURATION discriminator: the free-cast offer is scoped `UntilEndOfTurn`. After
/// the turn ends the permission is pruned, so the exiled card is no longer a
/// legal cast (CR 611.2a — the granted `ExileWithAltCost { duration }` expires).
#[test]
fn urza_free_cast_offer_expires_at_end_of_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let urza = scenario.add_real_card(P0, "Urza, Lord High Artificer", Zone::Battlefield, db);
    let spell = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let player = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap();
        player.library.retain(|&id| id == spell);
    }
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for _ in 0..5 {
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    let idx = shuffle_ability_index(runner.state(), urza);
    runner.activate(urza, idx).resolve();

    // Precondition: the free cast is currently legal.
    let legal_now = legal_actions(runner.state());
    assert!(
        legal_now.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )),
        "precondition: the exiled card must be castable this turn"
    );

    // Advance into the next turn's upkeep, crossing the end-of-turn cleanup that
    // prunes the UntilEndOfTurn casting permission (CR 514 / CR 611.2a).
    runner.advance_to_phase(Phase::Upkeep);

    let legal_later = legal_actions(runner.state());
    assert!(
        !legal_later.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )),
        "the free-cast offer must NOT survive past end of turn (CR 611.2a); \
         legal_actions={legal_later:?}"
    );
}
