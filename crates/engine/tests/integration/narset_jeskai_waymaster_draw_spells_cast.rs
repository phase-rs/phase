//! Runtime pipeline regression — Narset, Jeskai Waymaster.
//!
//! Oracle: "At the beginning of your end step, you may discard your hand. If you
//! do, draw cards equal to the number of spells you've cast this turn."
//!
//! `SpellsCastThisTurn { Controller }` was already a recognized quantity ref,
//! but the draw effect-construction path (`try_parse_equal_to_quantity_effect`)
//! only consulted `parse_event_context_quantity`, so this non-event-context
//! count dropped the whole draw clause to `Unimplemented`. The B2 fix adds a
//! `parse_cda_quantity` fallback after the event-context attempt, so the count
//! now resolves. This test seeds the per-turn spell tally (the runtime input the
//! count consumes), drives the parsed trigger's discard+draw chain through the
//! real `resolve_ability_chain` pipeline, accepts the optional discard, and
//! asserts cards drawn == spells cast this turn.
//!
//! DISCRIMINATING: with exactly two spells recorded this turn the controller
//! draws exactly two. If the fix is reverted the draw clause is an
//! `Unimplemented` no-op and zero cards are drawn. The count reads only
//! `spells_cast_this_turn_by_player` (a per-turn collection cleared at the turn
//! boundary, CR 117.1), so a spell cast on a prior turn is not in the collection
//! and cannot inflate the count.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{SpellCastRecord, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Minimal blue instant cast-record for the per-turn tally.
fn spell_record(name: &str) -> SpellCastRecord {
    SpellCastRecord {
        name: name.to_string(),
        core_types: vec![engine::types::card_type::CoreType::Instant],
        supertypes: Vec::new(),
        subtypes: Vec::new(),
        keywords: Vec::new(),
        colors: vec![ManaColor::Blue],
        mana_value: 1,
        has_x_in_cost: false,
        from_zone: Zone::Hand,
        cast_variant: engine::types::game_state::CastingVariant::Normal,
        was_kicked: false,
        spell_object_id: None,
    }
}

#[test]
fn narset_draws_equal_to_spells_cast_this_turn() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database unavailable");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let narset = scenario.add_real_card(P0, "Narset, Jeskai Waymaster", Zone::Battlefield, db);

    // Hand to discard (the "you may discard your hand" head) and a library to
    // draw the replacement cards from.
    for name in ["Plains", "Island"] {
        scenario.add_real_card(P0, name, Zone::Hand, db);
    }
    for name in ["Swamp", "Mountain", "Forest", "Plains", "Island"] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Two spells cast THIS turn — the per-turn tally the count reads. A spell
    // cast on a prior turn would not be in this collection (CR 117.1 reset).
    runner.state_mut().spells_cast_this_turn_by_player.insert(
        P0,
        engine::im::Vector::from(vec![spell_record("Bolt One"), spell_record("Bolt Two")]),
    );

    // Pull Narset's parsed end-step trigger and build the resolvable
    // discard+draw chain from its real (DB-parsed) execute ability.
    let face = db
        .get_face_by_name("Narset, Jeskai Waymaster")
        .expect("Narset must be in the card database");
    let execute = face.triggers[0]
        .execute
        .as_ref()
        .expect("Narset's end-step trigger must carry an execute chain")
        .clone();
    let ability = build_resolved_from_def(&execute, narset, P0);

    let library_before = runner.state().players[0].library.len();

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Narset discard+draw chain must resolve");

    // Drain the optional-discard prompt (accept) and any follow-up card
    // selection so the gated Draw sub-ability runs.
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional discard");
            }
            WaitingFor::PayCost { .. } => {
                let cards = runner.state().players[0].hand.clone().into_iter().collect();
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("discard the whole hand");
            }
            _ => break,
        }
    }

    let library_after = runner.state().players[0].library.len();
    let drawn = library_before as i64 - library_after as i64;
    assert_eq!(
        drawn, 2,
        "Narset must draw cards equal to the two spells cast this turn; \
         library_before={library_before}, library_after={library_after}"
    );
}
