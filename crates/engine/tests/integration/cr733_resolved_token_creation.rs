//! CR733 P2 coverage for ordinary token creation.
//!
//! This is the first family whose replay MATERIALIZES its subject rather than
//! verifying and installing into an object that already exists, so the applier's
//! precondition is inverted: the recorded id must be ABSENT.
//!
//! Two allocator draws are recorded because both would otherwise be re-drawn —
//! the `ObjectId` (from `next_object_id`) and the CR 613.7d entry timestamp. A
//! replay that re-drew either would hand out a colliding id or reorder the token
//! against continuous effects in the layer system.
//!
//! SCOPE: ordinary `TokenSpec` births. Copy births (CR 707.2) share this command
//! through `ResolvedTokenBody::Copy` and are covered in
//! `cr733_resolved_copy_token_creation`. Meld is not a birth at all — it reuses
//! the existing component object's id — so it is not in this family.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

const SOLDIER_TOKEN_ORACLE: &str = "Create a 1/1 white Soldier creature token.";

#[test]
fn token_creation_journals_an_exact_resolved_birth() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Built from Oracle text so the real parser produces the TokenSpec the
    // production resolver consumes — a hand-written `Effect::Token` literal would
    // also break on every new field.
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, SOLDIER_TOKEN_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let committed = runner.cast(spell_id).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 111.1 reach guard: a token actually came into existence on the
    // battlefield. Without it the journal assertion could pass vacuously.
    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 111.1: the resolved effect must create a token");
    let token = &state.objects[&token_id];
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);

    // The discriminating assertion: the birth is journaled as an exact resolved
    // command. A raw `create_object` + in-place mutation records nothing here.
    let births: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        births.len(),
        1,
        "the token authority must journal exactly one resolved creation"
    );

    let birth = &births[0];
    assert_eq!(
        birth.entry_timestamp, token.timestamp,
        "CR 613.7d: the journaled timestamp is the one the token received"
    );
    assert!(
        birth.resulting_next_object_id > token_id.0,
        "the recorded high-water is above the id it allocated"
    );

    // Replay-exactness: applying the recorded command to the pre-resolution state
    // materializes the SAME object at the SAME id with the SAME timestamp, with
    // no re-draw from `next_object_id` or `next_timestamp`.
    let mut replay = pre_state;
    assert!(
        !replay.objects.contains_key(&token_id),
        "the token does not exist before the command is applied"
    );
    engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth)
        .expect("the recorded birth must replay against its captured predecessor");

    let replayed = &replay.objects[&token_id];
    assert!(replayed.is_token, "replay materializes a token");
    assert_eq!(
        replayed.timestamp, birth.entry_timestamp,
        "CR 613.7d: replay installs the recorded timestamp instead of re-drawing one"
    );
    assert_eq!(
        replayed.power, token.power,
        "replay installs the same body the resolve path built"
    );
    assert_eq!(replayed.toughness, token.toughness);
    assert!(
        replay.battlefield.contains(&token_id),
        "replay adds the token to the battlefield zone list, not just the object map"
    );

    // CR 302.6: the applier installs the RECORDED entry turn, never the live
    // one. Advancing `turn_number` on the replay state before applying is what
    // makes this non-vacuous — it fails if the applier reads `state.turn_number`.
    let mut shifted = state.clone();
    shifted.objects.remove(&token_id);
    shifted.battlefield.retain(|id| *id != token_id);
    shifted.turn_number = birth.entry_turn + 5;
    engine::game::effects::token::apply_resolved_token_creation(&mut shifted, birth)
        .expect("the recorded birth must replay regardless of the live turn");
    assert_eq!(
        shifted.objects[&token_id].entered_battlefield_turn,
        Some(birth.entry_turn),
        "CR 302.6: replay stamps the recorded entry turn, not the live one"
    );

    // A birth draws from BOTH allocators, so replay must carry both past the
    // values it installed. The object-id side is asserted by the applier's own
    // high-water guard; this is the timestamp side, which is asserted by DRAWING
    // so it pins the consequence rather than the counter field. Without it a
    // later draw reissues this token's timestamp, and CR 613.7 orders effects
    // within a layer by timestamp alone, leaving the two objects unordered.
    let next_drawn = replay.next_timestamp();
    assert!(
        next_drawn > birth.entry_timestamp,
        "CR 613.7d: replay installed entry timestamp {} but the next draw handed out {}",
        birth.entry_timestamp,
        next_drawn
    );
    assert!(
        replay.next_object_id > token_id.0,
        "CR 111.1: replay carries the object-id allocator past the id it installed"
    );

    // The inverted precondition: this applier CREATES its subject, so a second
    // application is a typed invariant failure rather than a silent duplicate.
    assert!(
        engine::game::effects::token::apply_resolved_token_creation(&mut replay, birth).is_err(),
        "a token birth is not idempotent: re-applying it must fail closed"
    );
}
