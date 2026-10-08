//! CR 107.1a/b: the free-entry contract a `NamedChoice` prompt publishes.
//!
//! An unbounded number choice ("choose a number 0 or greater") cannot be
//! offered as an option list, so the player types a value. Something has to tell
//! the client what a legal value is. If the client works that out for itself —
//! by inspecting the serialized `ChoiceType` shape and restating the numeric
//! domain — it becomes a second authority, free to reject a value the engine
//! would have accepted, and free to drift when the engine's domain changes.
//!
//! So the engine publishes the contract on the prompt and enforces answers
//! against that same value. These tests pin both halves at the adapter surface a
//! client actually consumes:
//!
//! 1. the projected prompt carries the contract, and it equals
//!    `choice_type.free_entry()` — the one definition;
//! 2. the contract survives JSON serialization with its bounds readable, so a
//!    client never has to decode `ChoiceType` to find them;
//! 3. the published maximum is exactly the boundary the engine enforces — it
//!    accepts that value and rejects the next one up.
//!
//! Point 3 is what makes this more than a shape test: a published bound that
//! didn't match the enforced bound would pass 1 and 2 and still be the defect.
//!
//! Fail-on-revert: recompute the contract anywhere other than
//! `ChoiceType::free_entry`, or let the prompt omit it, and 1 or 3 fails.

use engine::game::scenario::GameScenario;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{ChoiceType, FreeEntry};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Wheel of Misfortune's choice clause, which states no maximum.
const ORACLE: &str = "Each player secretly chooses a number 0 or greater.";

/// Casts a spell carrying `ORACLE` and stops on the first prompt it raises.
fn stop_at_number_prompt() -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, PlayerId(1)] {
        scenario.with_library_top(player, &["Lib 1", "Lib 2"]);
        scenario.with_life(player, 20);
    }

    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Wheel Probe", false, ORACLE);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Red],
    });
    let spell = builder.id();
    scenario.with_mana_pool(P0, vec![ManaUnit::new(ManaType::Red, spell, false, vec![])]);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must start");

    for _ in 0..64 {
        if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
            return runner;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!("the spell never raised its number prompt");
}

fn prompt_parts(state: &GameState) -> (ChoiceType, Option<FreeEntry>) {
    match &state.waiting_for {
        WaitingFor::NamedChoice {
            choice_type,
            free_entry,
            ..
        } => (choice_type.clone(), *free_entry),
        other => panic!("expected a named choice, got {other:?}"),
    }
}

/// (1) and (2): the prompt a client receives carries the contract, and it is the
/// single definition rather than a copy that could disagree with it.
#[test]
fn an_unbounded_number_prompt_publishes_its_entry_contract() {
    let runner = stop_at_number_prompt();

    // Read it from the PROJECTED state — what a client is actually sent — not
    // from authoritative state, so a projection that dropped the field fails.
    let projected = filter_state_for_viewer(runner.state(), P0);
    let (choice_type, published) = prompt_parts(&projected);

    assert_eq!(
        published,
        choice_type.free_entry(),
        "the published contract must BE the engine's definition, not a second \
         copy of it"
    );
    let Some(FreeEntry::Number { min, max }) = published else {
        panic!("an unbounded number choice must publish a number contract, got {published:?}");
    };
    assert_eq!(min, 0, "the card states \"0 or greater\"");
    assert_eq!(
        max,
        i32::MAX as u32,
        "the maximum is the engine's own quantity domain, which is what a client \
         must be told rather than hard-code"
    );

    // The client reads JSON. Both bounds must be present there without decoding
    // the choice type — that decoding is exactly what this contract replaces.
    let json = serde_json::to_value(&projected.waiting_for).expect("prompt must serialize");
    let entry = json
        .pointer("/data/free_entry")
        .expect("the serialized prompt must carry free_entry");
    assert_eq!(
        entry["kind"], "Number",
        "the contract states its kind: {entry}"
    );
    assert_eq!(entry["min"], 0, "{entry}");
    assert_eq!(entry["max"], i32::MAX, "{entry}");
}

/// (3): the published maximum is the enforced maximum. A client that trusts the
/// contract can neither be surprised by a rejection nor allow an acceptance the
/// engine refuses.
#[test]
fn the_published_bounds_are_the_bounds_the_engine_enforces() {
    let runner = stop_at_number_prompt();
    let (choice_type, published) = prompt_parts(runner.state());
    let Some(FreeEntry::Number { min, max }) = published else {
        panic!("expected a number contract, got {published:?}");
    };

    for (answer, expected, why) in [
        (min.to_string(), true, "the published minimum is legal"),
        (max.to_string(), true, "the published maximum is legal"),
        (
            u64::from(max + 1).to_string(),
            false,
            "one past the published maximum is not",
        ),
        (
            "-1".to_string(),
            false,
            "a negative is below the published minimum",
        ),
    ] {
        assert_eq!(
            choice_type.accepts_free_entry_answer(&answer),
            Some(expected),
            "{why} (answer {answer})"
        );
    }

    // And the interactive handler agrees with the contract it published: a value
    // far past the old invented ceiling, well inside the published range, is
    // taken. This is the assertion a re-introduced UI/engine split would fail.
    let mut runner = runner;
    runner
        .act(GameAction::ChooseOption {
            choice: "1000000".to_string(),
        })
        .expect("a value within the published range must be accepted");
}

/// A choice whose answers ARE enumerable publishes no contract — the option list
/// is the domain. Without this the first test could pass on a prompt that
/// published a contract unconditionally.
#[test]
fn an_enumerated_choice_publishes_no_entry_contract() {
    let bounded = ChoiceType::NumberRange {
        min: 0,
        max: Some(20),
        distinctness: engine::types::ability::NumberDistinctness::Repeatable,
    };
    assert_eq!(
        bounded.free_entry(),
        None,
        "a stated maximum means the options enumerate the domain"
    );
    assert_eq!(
        bounded.accepts_free_entry_answer("5"),
        None,
        "and membership, not a range, validates the answer"
    );
    assert_eq!(
        ChoiceType::creature_type().free_entry(),
        None,
        "non-numeric enumerated choices likewise publish nothing"
    );
}
