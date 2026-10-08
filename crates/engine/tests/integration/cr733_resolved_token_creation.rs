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
use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

const SOLDIER_TOKEN_ORACLE: &str = "Create a 1/1 white Soldier creature token.";

/// Verbatim Scryfall Oracle text (Craft with Pride, {1}{R} sorcery). Chosen
/// because its whole text is ONE predefined-token creation: the CR 111.10a
/// Treasure ability is contributed by `inject_resolved_token_abilities`, never
/// by the `TokenSpec` the command carries, so it is exactly the class a
/// body-only replay drops.
const CRAFT_WITH_PRIDE_ORACLE: &str = "Create a Treasure token. (It's an artifact with \"{T}, \
     Sacrifice this token: Add one mana of any color.\")";

/// Verbatim Scryfall Oracle text (Audience with Trostani, {2}{G} sorcery).
///
/// Chosen because it is the production shape this test needs: ONE resolution that
/// creates a token and THEN makes a journaled zone change (the draw's Library →
/// Hand move). A spell's own Stack → Graveyard move is not journaled as a
/// `ZoneChange` command, so a two-spell fixture would leave an unjournaled ledger
/// push between the two families and could not distinguish this defect.
const AUDIENCE_WITH_TROSTANI_ORACLE: &str = "Create a 0/1 green Plant creature token, then draw cards equal to the number of differently named creature tokens you control.";

fn semantic_commands_after(state: &GameState, window: usize) -> Vec<ResolvedRulesCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(window)
        .filter_map(|entry| entry.command.clone())
        .collect()
}

/// CR 400.7 + CR 403.3: a token birth and every later same-turn zone change share
/// ONE per-turn index allocator — `zone_changes_this_turn.len()`. The live token
/// authority records its entry through `restrictions::record_zone_change`
/// (`push_committed_token_entry_events`), so the draw that follows it records
/// index N+1. Replay must record the birth too, or the draw's recorded index is
/// compared against a ledger that never advanced and `apply_resolved_zone_change`
/// fails closed.
///
/// This is the composition the rest of the `cr733_*` suite cannot see: those
/// fixtures apply ONE command against a captured predecessor, and a birth alone
/// asserts nothing about the ledger it must advance.
///
/// REVERT-PROBE (discriminating, RUN): delete the `record_zone_change` call in
/// `apply_resolved_token_creation` ⇒ the draw's replay returns
/// `TurnRecordIndexMismatch { expected: 2, found: 1 }` and this test fails.
#[test]
fn token_birth_then_same_turn_zone_change_replays_in_index_lockstep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Audience with Trostani",
            false,
            AUDIENCE_WITH_TROSTANI_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["First", "Second", "Third"]);

    let mut runner = scenario.build();
    let committed = runner.cast(spell).commit();
    let pre_state = committed.state().clone();
    let window = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state().clone();
    let commands = semantic_commands_after(&state, window);

    // ── reach guards: the fixture really produced the composition under test ──
    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 111.1: the resolution must create the Plant token");
    let live_token_record = state
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == token_id && record.to_zone == Zone::Battlefield)
        .expect("CR 400.7: the live birth reaches the per-turn zone-change ledger")
        .clone();
    let follower = commands
        .iter()
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.from == Zone::Library && command.to == Zone::Hand =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .expect("the draw must journal a Library → Hand zone change AFTER the birth");
    // Without this the replay below would be trivially green: an index recorded at
    // or below the birth's own slot cannot detect a ledger that failed to advance.
    assert!(
        follower.turn_zone_change_index > live_token_record.turn_zone_change_index,
        "the journaled follower must sit PAST the birth on the ledger, got follower {} vs birth {}",
        follower.turn_zone_change_index,
        live_token_record.turn_zone_change_index
    );

    // ── replay the whole recorded window in order ──
    let mut replay = pre_state;
    for command in &commands {
        match command {
            ResolvedRulesCommand::TokenCreation(command) => {
                engine::game::effects::token::apply_resolved_token_creation(&mut replay, command)
                    .expect("the recorded birth replays");
            }
            ResolvedRulesCommand::ZoneChange(command) => {
                // THE DISCRIMINATOR. A birth that records nothing leaves the ledger
                // one entry short and this is `Err(TurnRecordIndexMismatch)`.
                engine::game::zones::apply_resolved_zone_change(&mut replay, command)
                    .unwrap_or_else(|error| {
                        panic!(
                            "the recorded zone change must replay in index lockstep with the \
                             token birth that preceded it: {error}"
                        )
                    });
            }
            ResolvedRulesCommand::StackRemoval(command) => {
                engine::game::stack::apply_resolved_stack_removal(&mut replay, command.as_ref())
                    .expect("the recorded stack removal replays");
            }
            ResolvedRulesCommand::LedgerEdit(command) => {
                engine::game::ledger::apply_resolved_ledger_edit(&mut replay, command)
                    .expect("the recorded ledger edit replays");
            }
            other => panic!(
                "fixture drift: this resolution journaled an unexpected family {other:?}; the \
                 replay above must cover every command in the window, not a filtered subset"
            ),
        }
    }

    // The birth landed on the replayed ledger at the SAME slot, carrying the same
    // record the live authority pushed — index parity alone would also be
    // satisfied by a placeholder that trigger look-back could not read.
    let replayed_token_record = replay
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == token_id && record.to_zone == Zone::Battlefield)
        .expect("replay records the birth on the per-turn zone-change ledger");
    assert_eq!(
        *replayed_token_record, live_token_record,
        "the reconstructed entry record must equal the one the live birth recorded"
    );
    assert_eq!(
        replay
            .zone_changes_this_turn
            .iter()
            .position(|record| record.object_id == follower.object.object_id
                && record.to_zone == Zone::Hand),
        Some(follower.turn_zone_change_index),
        "the follower occupies its recorded ledger slot after replay"
    );
    // CR 403.3: `record_zone_change` performs the battlefield-entry bookkeeping,
    // so the replayed token is visible to entered-this-turn queries exactly once.
    assert_eq!(
        replay
            .battlefield_entries_this_turn
            .iter()
            .filter(|record| record.object_id == token_id)
            .count(),
        1,
        "the replayed birth records exactly one CR 403.3 battlefield entry"
    );
}

/// CR 111.3 + CR 111.10a: a predefined token's abilities are contributed by the
/// creating effect's injection pass (`inject_resolved_token_abilities`), NOT by
/// the `TokenSpec` the birth command carries — the spec for "Create a Treasure
/// token." holds a name, types and colors and no ability at all. A replay that
/// only materializes the body therefore installs an ABILITYLESS Treasure that
/// can never be sacrificed for mana.
///
/// This class is structurally invisible to the record-equality assertion in
/// `token_birth_then_same_turn_zone_change_replays_in_index_lockstep`:
/// `ZoneChangeRecord`'s ability surface is `trigger_definitions` only, and the
/// Treasure ability is ACTIVATED, so the two records compare equal while the
/// objects differ. The assertion below reads the OBJECT.
///
/// REVERT-PROBE (discriminating, RUN): delete the injection dispatch in
/// `apply_resolved_token_creation` ⇒ replayed `base_abilities` is empty while
/// live holds one, and this test fails on the count assertion.
#[test]
fn predefined_token_birth_replays_its_injected_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Craft with Pride", false, CRAFT_WITH_PRIDE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let committed = runner.cast(spell).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    let token_id = *state
        .last_created_token_ids
        .first()
        .expect("CR 111.1: the resolution must create the Treasure token");
    let live = &state.objects[&token_id];
    // Reach guard: this really is the predefined-token class, and the injection
    // really ran live. Without it the parity assertion below would be satisfied
    // by empty == empty on any vanilla token.
    assert!(
        live.card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Treasure"),
        "CR 111.10a reach guard: the fixture must produce a Treasure token, got {:?}",
        live.card_types.subtypes
    );
    assert_eq!(
        live.base_abilities.len(),
        1,
        "CR 111.10a reach guard: the live Treasure carries its injected sacrifice-for-mana \
         ability, so a replay that drops it is observable"
    );

    let birth = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .find_map(|command| match command {
            ResolvedRulesCommand::TokenCreation(command)
                if command.object.object_id == token_id =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("the Treasure birth is journaled");

    let mut replay = pre_state;
    engine::game::effects::token::apply_resolved_token_creation(&mut replay, &birth)
        .expect("the recorded birth must replay against its captured predecessor");
    let replayed = &replay.objects[&token_id];

    // THE DISCRIMINATOR. Body-only materialization gives 0 here.
    assert_eq!(
        replayed.base_abilities.len(),
        live.base_abilities.len(),
        "CR 111.3: replay must contribute the same injected abilities the live \
         creation did — live {:?} vs replayed {:?}",
        live.base_abilities,
        replayed.base_abilities
    );
    assert_eq!(
        replayed.base_abilities, live.base_abilities,
        "CR 111.10a: the replayed Treasure carries the SAME ability, not merely as many"
    );
    assert_eq!(
        replayed.token_rules_text, live.token_rules_text,
        "CR 111.10: the injected display rules text is part of the same payload"
    );
}

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
