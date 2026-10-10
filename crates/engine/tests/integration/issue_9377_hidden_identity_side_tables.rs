//! Regression for issue #9377 (section 2): id-keyed identity side tables must
//! not name or characterise an object the same viewer snapshot shows as
//! "Hidden Card".
//!
//! CR 400.2 + CR 400.7: a card that moves into a hidden zone (hand, library,
//! face-down exile) keeps its `ObjectId` in this engine, so every table keyed
//! by (or recording) that id would let a single viewer snapshot (reconnect,
//! spectator, replay export) read the hidden card's name or mana value. The
//! viewer projection (`filter_state_for_viewer` /
//! `filter_state_for_unseated_viewer`) drops the id-keyed LKI for hidden ids
//! and blanks the identifying fields of the turn/game ledgers while keeping
//! their counts. Authoritative state is untouched (CR 608.2h, CR 603.10a).
//!
//! Every table is written by its real production writer here (cast pipeline,
//! ETB, sacrifice cost, Hideaway, damage, attack declaration) and then moved
//! into a hidden zone by a real spell.
//!
//! CR 406.3 + CR 406.3a: the `linked_exile_snapshot` vectors nested in a
//! Hideaway source's departure record and its trigger-source context keep a
//! face-down member's slot, id and owner and lose only its mana value for a
//! viewer who may not look at it (Windbrisk Heights boards F1 and F3).
//!
//! https://github.com/phase-rs/phase/issues/9377

use engine::game::casting::display_spell_cost;
use engine::game::combat::AttackTarget;
use engine::game::derived_views::{derive_views, ClientGameStateRef};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CounterAddedRecord, ExileLink, ExileLinkKind, GameState, LinkedExileSnapshot,
    ManaSpentSourceSnapshot, PayCostKind, StackEntryKind, WaitingFor, ZoneChangeRecord,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const HIDDEN: &str = "Hidden Card";
const GOBLIN_BOMBARDMENT: &str =
    "Sacrifice a creature: This enchantment deals 1 damage to any target.";

fn fund(runner: &mut GameRunner, player: PlayerId, mana: &[(ManaType, usize)]) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool;
    for &(color, count) in mana {
        for _ in 0..count {
            pool.add(ManaUnit::new(color, ObjectId(0), false, vec![]));
        }
    }
}

/// Drive `act()` through cast setup (targeting, mana payment) until priority
/// is reached, leaving the spell on the stack. Copied from
/// issue_6877_departed_triggering_spell.rs.
fn commit_cast(runner: &mut GameRunner, spell: ObjectId, target: Option<TargetRef>) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.clone(),
                    })
                    .expect("declared target must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pool-funded remainder must pay");
            }
            // Capsize's Buyback is an optional additional cost; decline it.
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: false })
                    .expect("declining an optional cost must be accepted");
            }
            other => panic!("unexpected waiting_for while committing cast: {other:?}"),
        }
    }
    panic!("cast did not reach Priority or OrderTriggers");
}

/// Pass priority until the stack is empty and assert it settled.
fn settle(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "stack must settle: {:?} / {:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
}

fn library_top(state: &GameState, player: PlayerId) -> Option<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.library.front().copied())
}

fn cast_records_for(
    state: &GameState,
    player: PlayerId,
    id: ObjectId,
    this_game: bool,
) -> Vec<engine::types::game_state::SpellCastRecord> {
    let table = if this_game {
        &state.spells_cast_this_game_by_player
    } else {
        &state.spells_cast_this_turn_by_player
    };
    table
        .get(&player)
        .map(|records| {
            records
                .iter()
                .filter(|r| r.spell_object_id == Some(id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn cast_len(state: &GameState, player: PlayerId, this_game: bool) -> usize {
    let table = if this_game {
        &state.spells_cast_this_game_by_player
    } else {
        &state.spells_cast_this_turn_by_player
    };
    table.get(&player).map_or(0, |records| records.len())
}

/// Positive reach-guards on raw state: every id-keyed LKI table and both cast
/// lists hold a named record for `id`.
fn assert_raw_spell_tables_name(state: &GameState, id: ObjectId, name: &str, mana_value: u32) {
    assert!(
        state.lki_cache.contains_key(&id),
        "reach-guard: raw lki_cache holds {name}"
    );
    assert!(
        state.lki_by_incarnation.contains_key(&id),
        "reach-guard: raw lki_by_incarnation holds {name}"
    );
    assert!(
        state.lki_copiable_values.contains_key(&id),
        "reach-guard: raw lki_copiable_values holds {name}"
    );
    assert!(
        state.departed_stack_spells.contains_key(&id),
        "reach-guard: raw departed_stack_spells holds {name}"
    );
    for this_game in [false, true] {
        let records = cast_records_for(state, P0, id, this_game);
        assert_eq!(
            records.len(),
            1,
            "reach-guard: raw cast list (this_game={this_game}) links {name}"
        );
        assert_eq!(records[0].name, name);
        assert_eq!(records[0].mana_value, mana_value);
    }
}

/// The hidden-id projection: every id-keyed LKI table lacks `id`; both cast
/// lists keep their length and blank exactly name + id link of the record,
/// keeping its filter columns.
fn assert_projection_redacts_spell(
    raw: &GameState,
    view: &GameState,
    id: ObjectId,
    name: &str,
    core_type: CoreType,
    mana_value: u32,
    label: &str,
) {
    assert_eq!(
        view.objects[&id].name, HIDDEN,
        "reach-guard ({label}): the id is in the hidden set"
    );
    assert!(
        !view.lki_cache.contains_key(&id),
        "{label}: lki_cache still holds the hidden id"
    );
    assert!(
        !view.lki_by_incarnation.contains_key(&id),
        "{label}: lki_by_incarnation still holds the hidden id"
    );
    assert!(
        !view.lki_copiable_values.contains_key(&id),
        "{label}: lki_copiable_values still holds the hidden id"
    );
    assert!(
        !view.departed_stack_spells.contains_key(&id),
        "{label}: departed_stack_spells still holds the hidden id"
    );
    for this_game in [false, true] {
        assert_eq!(
            cast_len(view, P0, this_game),
            cast_len(raw, P0, this_game),
            "{label}: cast count (this_game={this_game}) is preserved"
        );
        assert!(
            cast_records_for(view, P0, id, this_game).is_empty(),
            "{label}: a cast record still links the hidden id (this_game={this_game})"
        );
        let table = if this_game {
            &view.spells_cast_this_game_by_player
        } else {
            &view.spells_cast_this_turn_by_player
        };
        let hidden_records: Vec<_> = table[&P0].iter().filter(|r| r.name == HIDDEN).collect();
        assert_eq!(
            hidden_records.len(),
            1,
            "{label}: exactly the hidden spell's record is blanked (this_game={this_game})"
        );
        let record = hidden_records[0];
        assert_eq!(record.spell_object_id, None);
        assert_eq!(
            record.core_types,
            vec![core_type],
            "{label}: cast-history filter columns stay (B1 invariant)"
        );
        assert_eq!(record.mana_value, mana_value);
        assert!(
            table[&P0].iter().all(|r| r.name != name),
            "{label}: a cast record still names {name}"
        );
    }
}

/// The owner/public projection: every table keeps its entry for `id`.
fn assert_projection_keeps_spell(view: &GameState, id: ObjectId, name: &str, label: &str) {
    assert_eq!(view.objects[&id].name, name, "{label}: card is visible");
    assert!(view.lki_cache.contains_key(&id), "{label}: lki_cache kept");
    assert!(
        view.lki_by_incarnation.contains_key(&id),
        "{label}: lki_by_incarnation kept"
    );
    assert!(
        view.lki_copiable_values.contains_key(&id),
        "{label}: lki_copiable_values kept"
    );
    assert!(
        view.departed_stack_spells.contains_key(&id),
        "{label}: departed_stack_spells kept"
    );
    for this_game in [false, true] {
        let records = cast_records_for(view, P0, id, this_game);
        assert_eq!(records.len(), 1, "{label}: cast record keeps its id link");
        assert_eq!(records[0].name, name, "{label}: cast record keeps its name");
    }
}

/// Test A: a spell bounced from the stack to its owner's hand.
#[test]
fn stack_to_hand_bounce_redacts_spell_side_tables_for_opponent_and_spectator() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let unsub = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[(ManaType::Green, 2), (ManaType::Blue, 2)],
    );

    commit_cast(&mut runner, bears, None);
    commit_cast(&mut runner, unsub, Some(TargetRef::Object(bears)));
    settle(&mut runner);

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Hand, "reach-guard: bounced");
    assert_raw_spell_tables_name(&raw, bears, "Grizzly Bears", 2);

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_projection_redacts_spell(
        &raw,
        &p1,
        bears,
        "Grizzly Bears",
        CoreType::Creature,
        2,
        "P1",
    );
    let json = serde_json::to_string(&p1).expect("P1 projection serializes");
    assert!(
        !json.contains("Grizzly Bears"),
        "DIAGNOSTIC: P1 projection still names the bounced card somewhere"
    );

    // Sibling over-redaction guards: the public Unsubstantiate keeps its record.
    let unsub_records = cast_records_for(&p1, P0, unsub, false);
    assert_eq!(unsub_records.len(), 1, "public spell keeps its id link");
    assert_eq!(unsub_records[0].name, "Unsubstantiate");
    assert_eq!(
        derive_views(&p1, Some(P1)).storm_count,
        derive_views(&raw, Some(P1)).storm_count,
        "storm count is unchanged by the projection"
    );

    let p0 = filter_state_for_viewer(&raw, P0);
    assert_projection_keeps_spell(&p0, bears, "Grizzly Bears", "P0 owner");

    let spectator = filter_state_for_unseated_viewer(&raw);
    assert_projection_redacts_spell(
        &raw,
        &spectator,
        bears,
        "Grizzly Bears",
        CoreType::Creature,
        2,
        "spectator",
    );
}

/// Build Test A2's board: Lightning Bolt countered by Memory Lapse onto the
/// top of P0's library. Returns (runner, bolt, lapse, extra hand cards).
fn bolt_tucked_by_memory_lapse(
    extra_hand: &[&str],
) -> Option<(GameRunner, ObjectId, ObjectId, Vec<ObjectId>)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let lapse = scenario.add_real_card(P0, "Memory Lapse", Zone::Hand, db);
    let extras = extra_hand
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Hand, db))
        .collect();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(&mut runner, P0, &[(ManaType::Red, 1), (ManaType::Blue, 2)]);

    commit_cast(&mut runner, bolt, Some(TargetRef::Player(P1)));
    commit_cast(&mut runner, lapse, Some(TargetRef::Object(bolt)));
    settle(&mut runner);

    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Library,
        "reach-guard: Memory Lapse put the Bolt into the library"
    );
    assert_eq!(
        library_top(runner.state(), P0),
        Some(bolt),
        "reach-guard: the Bolt is on top of its owner's library"
    );
    Some((runner, bolt, lapse, extras))
}

/// Test A2: a spell countered onto the top of its owner's library. The library
/// is hidden from its owner too (CR 401.2), so every viewer gets the redaction.
#[test]
fn stack_to_library_tuck_redacts_spell_side_tables_for_every_viewer() {
    let Some((runner, bolt, lapse, _)) = bolt_tucked_by_memory_lapse(&[]) else {
        return;
    };
    let raw = runner.state().clone();
    assert_raw_spell_tables_name(&raw, bolt, "Lightning Bolt", 1);

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("P0 owner", filter_state_for_viewer(&raw, P0)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_projection_redacts_spell(
            &raw,
            &view,
            bolt,
            "Lightning Bolt",
            CoreType::Instant,
            1,
            label,
        );
        // Two-authority sibling: Memory Lapse's own record (public graveyard)
        // shares the list and keeps its name and id link.
        let lapse_records = cast_records_for(&view, P0, lapse, false);
        assert_eq!(lapse_records.len(), 1, "{label}: public record keeps id");
        assert_eq!(lapse_records[0].name, "Memory Lapse");
    }
}

fn entry_named(
    state: &GameState,
    id: ObjectId,
) -> Option<engine::types::game_state::BattlefieldEntryRecord> {
    state
        .battlefield_entries_this_turn
        .iter()
        .find(|r| r.object_id == id)
        .cloned()
}

/// Test B: a resolved creature bounced from the battlefield to hand.
#[test]
fn battlefield_to_hand_bounce_blanks_battlefield_entry_record() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let ogre = scenario.add_real_card(P0, "Gray Ogre", Zone::Hand, db);
    let unsub = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[
            (ManaType::Green, 2),
            (ManaType::Red, 3),
            (ManaType::Blue, 2),
        ],
    );

    runner.cast(bears).resolve();
    runner.cast(ogre).resolve();
    runner.cast(unsub).target_objects(&[bears]).resolve();

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Hand, "reach-guard: bounced");
    let raw_entry = entry_named(&raw, bears).expect("reach-guard: raw entry for the Bears");
    assert_eq!(raw_entry.name, "Grizzly Bears");
    assert_eq!(
        entry_named(&raw, ogre).map(|r| r.name),
        Some("Gray Ogre".to_string()),
        "reach-guard: the sibling's entry was written"
    );

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(
        p1.objects[&bears].name, HIDDEN,
        "reach-guard: hidden for P1"
    );
    assert_eq!(
        p1.battlefield_entries_this_turn.len(),
        raw.battlefield_entries_this_turn.len(),
        "entry count preserved"
    );
    let entry = entry_named(&p1, bears).expect("the hidden entry is kept");
    assert_eq!(entry.name, HIDDEN);
    assert!(entry.core_types.is_empty());
    assert_eq!(
        entry_named(&p1, ogre).map(|r| r.name),
        Some("Gray Ogre".to_string()),
        "sibling on the battlefield keeps its entry name"
    );

    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(
        entry_named(&p0, bears).map(|r| r.name),
        Some("Grizzly Bears".to_string()),
        "the owner sees their own hand card's entry"
    );
}

/// Test B2: a resolved creature tucked into its owner's library (Time Ebb).
#[test]
fn battlefield_to_library_tuck_blanks_battlefield_entry_record_for_owner_too() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let ebb = scenario.add_real_card(P0, "Time Ebb", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[(ManaType::Green, 2), (ManaType::Blue, 3)],
    );

    runner.cast(bears).resolve();
    runner.cast(ebb).target_objects(&[bears]).resolve();

    let raw = runner.state().clone();
    assert_eq!(
        raw.objects[&bears].zone,
        Zone::Library,
        "reach-guard: tucked"
    );
    assert_eq!(library_top(&raw, P0), Some(bears), "reach-guard: on top");
    assert_eq!(
        entry_named(&raw, bears).map(|r| r.name),
        Some("Grizzly Bears".to_string()),
        "reach-guard: raw entry names the Bears"
    );

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("P0 owner", filter_state_for_viewer(&raw, P0)),
    ] {
        assert_eq!(view.objects[&bears].name, HIDDEN, "reach-guard ({label})");
        let entry = entry_named(&view, bears).expect("the hidden entry is kept");
        assert_eq!(entry.name, HIDDEN, "{label}: entry name blanked");
        assert!(entry.core_types.is_empty(), "{label}: entry types blanked");
        assert_eq!(
            view.battlefield_entries_this_turn.len(),
            raw.battlefield_entries_this_turn.len()
        );
    }
}

/// Activate Goblin Bombardment, sacrificing `victim` and pinging P1.
fn sacrifice_to_bombardment(runner: &mut GameRunner, bombardment: ObjectId, victim: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: bombardment,
            ability_index: 0,
        })
        .expect("activating Goblin Bombardment must succeed");
    let mut sacrificed = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&victim),
                    "victim must be a legal sacrifice"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrifice must succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting the opponent must succeed");
            }
            _ => {
                runner.advance_until_stack_empty();
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
            }
        }
    }
    assert!(sacrificed, "reach-guard: the sacrifice cost was paid");
}

fn sacrifice_named(
    state: &GameState,
    id: ObjectId,
) -> Option<engine::types::game_state::ZoneChangeRecord> {
    state
        .sacrificed_permanents_this_turn
        .iter()
        .find(|r| r.object_id == id)
        .cloned()
}

/// Test C: a sacrificed creature returned from the graveyard to hand.
#[test]
fn sacrificed_then_returned_to_hand_blanks_sacrifice_record() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bombardment = scenario
        .add_creature(P0, "Goblin Bombardment", 0, 0)
        .as_enchantment()
        .from_oracle_text(GOBLIN_BOMBARDMENT)
        .id();
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let ogre = scenario.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db);
    let recover = scenario.add_real_card(P0, "Recover", Zone::Hand, db);
    // Recover draws a card; give the library something to draw.
    scenario.add_real_card(P0, "Shock", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    sacrifice_to_bombardment(&mut runner, bombardment, bears);
    sacrifice_to_bombardment(&mut runner, bombardment, ogre);
    fund(&mut runner, P0, &[(ManaType::Black, 3)]);
    runner.cast(recover).target_objects(&[bears]).resolve();

    let raw = runner.state().clone();
    assert_eq!(
        raw.objects[&bears].zone,
        Zone::Hand,
        "reach-guard: returned"
    );
    assert_eq!(
        sacrifice_named(&raw, bears).map(|r| r.name),
        Some("Grizzly Bears".to_string()),
        "reach-guard: raw sacrifice record names the Bears"
    );
    assert_eq!(raw.objects[&ogre].zone, Zone::Graveyard, "sibling stays");

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(
        p1.objects[&bears].name, HIDDEN,
        "reach-guard: hidden for P1"
    );
    assert_eq!(
        p1.sacrificed_permanents_this_turn.len(),
        raw.sacrificed_permanents_this_turn.len(),
        "sacrifice count preserved"
    );
    let record = sacrifice_named(&p1, bears).expect("the hidden record is kept");
    assert_eq!(record.name, HIDDEN);
    assert_eq!(record.mana_value, 0);
    assert_eq!(
        sacrifice_named(&p1, ogre).map(|r| r.name),
        Some("Gray Ogre".to_string()),
        "a sacrificed sibling still in the graveyard keeps its name"
    );
}

/// Test D (negative control): a bounced card recast onto the battlefield is
/// public again, so none of its history is redacted.
#[test]
fn recast_after_bounce_is_public_again_and_unredacted() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let unsub = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[(ManaType::Green, 4), (ManaType::Blue, 2)],
    );

    commit_cast(&mut runner, bears, None);
    commit_cast(&mut runner, unsub, Some(TargetRef::Object(bears)));
    settle(&mut runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Hand);
    runner.cast(bears).resolve();

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Battlefield, "reach-guard");
    assert!(raw.lki_cache.contains_key(&bears), "reach-guard: raw LKI");
    assert!(
        cast_records_for(&raw, P0, bears, false)
            .iter()
            .any(|r| r.name == "Grizzly Bears"),
        "reach-guard: raw cast record links the Bears"
    );
    assert!(entry_named(&raw, bears).is_some(), "reach-guard: raw entry");

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(
        p1.objects[&bears].name, "Grizzly Bears",
        "paired positive: the permanent is public"
    );
    assert!(p1.lki_cache.contains_key(&bears), "LKI is not redacted");
    assert!(
        cast_records_for(&p1, P0, bears, false)
            .iter()
            .any(|r| r.name == "Grizzly Bears"),
        "cast record is not redacted"
    );
    assert_eq!(
        entry_named(&p1, bears).map(|r| r.name),
        Some("Grizzly Bears".to_string()),
        "entry is not redacted"
    );
}

/// Drive Windbrisk Heights' real Hideaway ETB, hiding `hidden`. Copied from
/// issue_3246_windbrisk_heights_hideaway_exiled_by_source.rs.
fn play_windbrisk_and_hide(
    runner: &mut GameRunner,
    windbrisk: ObjectId,
    card_id: CardId,
    hidden: ObjectId,
) -> bool {
    runner
        .act(GameAction::PlayLand {
            object_id: windbrisk,
            card_id,
        })
        .expect("playing Windbrisk Heights must be legal");

    let mut saw_dig_choice = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                saw_dig_choice = true;
                assert!(cards.contains(&hidden));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![hidden],
                    })
                    .expect("SelectCards (Hideaway pick) accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && saw_dig_choice {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt while driving Windbrisk's Hideaway ETB: {other:?}"),
        }
    }
    saw_dig_choice
}

/// Test F1: a Hideaway source destroyed while its face-down exiled card stays
/// hidden. `linked_exile_lki`, the public departure record's
/// `linked_exile_snapshot` and its trigger-source context's copy keep the
/// member slot but lose its mana value for the opponent and a spectator; P0,
/// whom the Hideaway instruction lets look, keeps the real value.
#[test]
fn hideaway_source_leaving_blanks_hidden_linked_exile_member() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let windbrisk = scenario.add_real_card(P0, "Windbrisk Heights", Zone::Hand, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    // Seeded sibling: a face-up exiled card linked to the same source.
    let lapse = scenario.add_real_card(P0, "Memory Lapse", Zone::Exile, db);
    let rain = scenario.add_real_card(P0, "Molten Rain", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let windbrisk_card_id = runner.state().objects[&windbrisk].card_id;

    assert!(
        play_windbrisk_and_hide(&mut runner, windbrisk, windbrisk_card_id, shock),
        "reach-guard: the real Hideaway ETB surfaced a DigChoice"
    );
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: lapse,
        source_id: windbrisk,
        kind: ExileLinkKind::TrackedBySource,
    });
    fund(&mut runner, P0, &[(ManaType::Red, 3)]);
    runner.cast(rain).target_objects(&[windbrisk]).resolve();

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&windbrisk].zone, Zone::Graveyard, "reach-guard");
    assert_eq!(raw.objects[&shock].zone, Zone::Exile, "reach-guard");
    assert!(
        raw.objects[&shock].face_down,
        "reach-guard: hidden face down"
    );
    let raw_members = raw
        .linked_exile_lki
        .get(&windbrisk)
        .expect("reach-guard: Windbrisk's departure wrote linked_exile_lki");
    assert_eq!(raw_members.len(), 2, "reach-guard: both members");
    let mv = |members: &[engine::types::game_state::LinkedExileSnapshot], id: ObjectId| {
        members
            .iter()
            .find(|m| m.exiled_id == id)
            .map(|m| m.mana_value)
    };
    assert_eq!(mv(raw_members, shock), Some(1), "reach-guard: Shock mv 1");
    assert_eq!(mv(raw_members, lapse), Some(2), "reach-guard: Lapse mv 2");

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(
        p1.objects[&shock].name, HIDDEN,
        "reach-guard: member hidden"
    );
    let members = p1
        .linked_exile_lki
        .get(&windbrisk)
        .expect("a public source keeps its linked-exile entry");
    assert_eq!(members.len(), 2, "linked-exile count preserved");
    assert_eq!(mv(members, shock), Some(0), "hidden member loses its mv");
    assert_eq!(mv(members, lapse), Some(2), "public member keeps its mv");

    // CR 603.10a + CR 607.2a: the departure record and its synced trigger-source
    // context latch the same members, in the same slot order.
    let raw_record = heights_departure(&raw, windbrisk);
    let (raw_record_members, raw_context_members) = record_linked_members(raw_record);
    assert_eq!(
        raw_record_members.len(),
        2,
        "reach-guard: the departure record latches both members"
    );
    assert_eq!(mv(raw_record_members, shock), Some(1), "reach-guard: Shock");
    assert_eq!(mv(raw_record_members, lapse), Some(2), "reach-guard: Lapse");
    assert_eq!(
        raw_context_members, raw_record_members,
        "reach-guard: the context vector was synced from the record"
    );

    // CR 406.3 + CR 406.3a: the opponent and a spectator keep both slots, ids,
    // owners and order in the public departure record; only Shock's mana value
    // is zeroed.
    for (label, view) in [
        ("P1", p1),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&shock].name, HIDDEN,
            "reach-guard ({label}): Shock is hidden"
        );
        let (record_members, context_members) =
            record_linked_members(heights_departure(&view, windbrisk));
        for (vector, members) in [("record", record_members), ("context", context_members)] {
            let ids: Vec<ObjectId> = members.iter().map(|m| m.exiled_id).collect();
            let raw_ids: Vec<ObjectId> = raw_record_members.iter().map(|m| m.exiled_id).collect();
            assert_eq!(ids, raw_ids, "{label} {vector}: ids, slots and order kept");
            assert!(
                members
                    .iter()
                    .zip(raw_record_members)
                    .all(|(m, r)| m.owner == r.owner),
                "{label} {vector}: owners kept"
            );
            assert_eq!(mv(members, shock), Some(0), "{label} {vector}: Shock's mv");
            assert_eq!(mv(members, lapse), Some(2), "{label} {vector}: Lapse kept");
        }
    }

    // Authorized looker (CR 406.3: the Hideaway instruction lets P0 look, and the
    // look outlives its source): P0 keeps the real values.
    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(
        p0.objects[&shock].name, "Shock",
        "reach-guard: P0 may still look at the hideaway card"
    );
    let (record_members, context_members) =
        record_linked_members(heights_departure(&p0, windbrisk));
    for (vector, members) in [("record", record_members), ("context", context_members)] {
        assert_eq!(members, raw_record_members, "P0 {vector}: unchanged");
        assert_eq!(mv(members, shock), Some(1), "P0 {vector}: Shock's mv");
        assert_eq!(mv(members, lapse), Some(2), "P0 {vector}: Lapse's mv");
    }

    // Census over every `linked_exile_snapshot` array in every form.
    let journal_record = |path: &str| {
        path.starts_with("/zone_changes_this_turn/")
            && path.ends_with("/linked_exile_snapshot")
            && !path.contains("/trigger_source_context/")
    };
    let journal_context = |path: &str| {
        path.starts_with("/zone_changes_this_turn/")
            && path.ends_with("/trigger_source_context/linked_exile_snapshot")
    };
    assert_linked_exile_census(
        &raw,
        shock,
        &[
            ("zone_changes_this_turn record", &journal_record),
            ("zone_changes_this_turn context", &journal_context),
        ],
        "F1",
    );

    // CodeRabbit: the raw state names Shock; no opponent or spectator
    // projection, serialized or on the client wire, does anywhere.
    assert_no_path_names(&raw, "Shock", "F1");
}

/// The departure record Windbrisk Heights wrote when it left the battlefield.
fn heights_departure(state: &GameState, windbrisk: ObjectId) -> &ZoneChangeRecord {
    state
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == windbrisk && record.from_zone == Some(Zone::Battlefield))
        .expect("Windbrisk Heights' departure record")
}

/// The record's own linked-exile vector and its trigger-source context's copy.
fn record_linked_members(
    record: &ZoneChangeRecord,
) -> (&[LinkedExileSnapshot], &[LinkedExileSnapshot]) {
    let context = record
        .trigger_source_context
        .as_ref()
        .expect("a public departure record keeps its trigger-source context");
    (
        &record.linked_exile_snapshot,
        &context.linked_exile_snapshot,
    )
}

/// JSON paths of every string value, at any depth, that contains `needle`.
fn paths_naming(value: &serde_json::Value, needle: &str) -> Vec<String> {
    fn walk(value: &serde_json::Value, needle: &str, path: &str, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) if text.contains(needle) => out.push(path.to_string()),
            serde_json::Value::Object(map) => {
                for (name, child) in map {
                    walk(child, needle, &format!("{path}/{name}"), out);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, needle, &format!("{path}/{index}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, needle, "", &mut out);
    out
}

/// The four forms an opponent or a spectator receives: both projections and
/// both client wires (`state` of the wire envelope, so its paths equal the raw
/// state's).
fn hidden_viewer_forms(raw: &GameState) -> [(&'static str, serde_json::Value); 4] {
    let wire = |viewer: Option<PlayerId>| {
        serde_json::to_value(ClientGameStateRef::wrap(raw, viewer)).expect("client wire serializes")
    };
    [
        (
            "P1",
            serde_json::to_value(filter_state_for_viewer(raw, P1)).expect("P1 serializes"),
        ),
        (
            "spectator",
            serde_json::to_value(filter_state_for_unseated_viewer(raw))
                .expect("spectator serializes"),
        ),
        ("client wire P1", wire(Some(P1))),
        ("client wire spectator", wire(None)),
    ]
}

/// The state part of a viewer form (a client wire is `{ state, derived }`).
fn state_part(form: &serde_json::Value) -> &serde_json::Value {
    match (form.get("state"), form.get("derived")) {
        (Some(state), Some(_)) => state,
        _ => form,
    }
}

/// The raw state names `needle`; no opponent or spectator form names it
/// anywhere (the whole wire envelope, derived views included).
fn assert_no_path_names(raw: &GameState, needle: &str, label: &str) {
    let raw_value = serde_json::to_value(raw).expect("raw state serializes");
    assert!(
        !paths_naming(&raw_value, needle).is_empty(),
        "{label}: reach-guard: the raw state names {needle}"
    );
    for (view_label, form) in hidden_viewer_forms(raw) {
        let name = state_part(&form)
            .pointer(&format!(
                "/objects/{}/name",
                raw_object_named(raw, needle).0
            ))
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            name,
            Some(HIDDEN),
            "{label}/{view_label}: reach-guard: {needle} is hidden in this form"
        );
        let paths = paths_naming(&form, needle);
        assert!(
            paths.is_empty(),
            "{label}/{view_label}: the projection names {needle} at {paths:#?}"
        );
    }
}

fn raw_object_named(raw: &GameState, name: &str) -> ObjectId {
    raw.objects
        .iter()
        .find(|(_, object)| object.name == name)
        .map(|(id, _)| *id)
        .expect("the raw state holds the named object")
}

/// Census: walk every `linked_exile_snapshot` array, at any depth, in the raw
/// state and in each viewer form. Every required raw path latches `hidden`
/// with its real mana value. In each opponent / spectator form, an entry
/// naming `hidden` keeps its id and owner with mana value 0, every other entry
/// JSON-equals its raw entry, each array keeps its raw length at the EXACT
/// raw path, and a required path may not go missing. The authorized owner
/// (P0) sees every array JSON-equal to the raw one.
fn assert_linked_exile_census(
    raw: &GameState,
    hidden: ObjectId,
    required: &[RequiredPath<'_>],
    label: &str,
) {
    let hidden_id = serde_json::to_value(hidden).expect("id serializes");
    let names_hidden = |item: &serde_json::Value| item.get("exiled_id") == Some(&hidden_id);
    let mut raw_paths = Vec::new();
    walk_key_arrays(
        &serde_json::to_value(raw).expect("raw state serializes"),
        "linked_exile_snapshot",
        "",
        &mut raw_paths,
    );
    let raw_list: Vec<&String> = raw_paths.iter().map(|(path, _)| path).collect();
    for (name, matches) in required {
        assert!(
            raw_paths.iter().any(|(path, items)| matches(path)
                && items.iter().any(|item| names_hidden(item)
                    && item.get("mana_value").and_then(serde_json::Value::as_u64) != Some(0))),
            "{label}: reach-guard: no raw `{name}` path latches the hidden member's mana value; \
             raw paths: {raw_list:#?}"
        );
    }

    let views = hidden_viewer_forms(raw);
    for (view_label, form) in &views {
        let mut paths = Vec::new();
        walk_key_arrays(state_part(form), "linked_exile_snapshot", "", &mut paths);
        let list: Vec<&String> = paths.iter().map(|(path, _)| path).collect();
        for (path, items) in &paths {
            let Some((_, raw_items)) = raw_paths.iter().find(|(raw_path, _)| raw_path == path)
            else {
                panic!("{label}/{view_label}: `{path}` has no raw counterpart; raw: {raw_list:#?}");
            };
            assert_eq!(
                items.len(),
                raw_items.len(),
                "{label}/{view_label}: `{path}` length changed"
            );
            for (item, raw_item) in items.iter().zip(raw_items) {
                if names_hidden(raw_item) {
                    assert_eq!(
                        item.get("exiled_id"),
                        raw_item.get("exiled_id"),
                        "{label}/{view_label}: `{path}` hidden member id"
                    );
                    assert_eq!(
                        item.get("owner"),
                        raw_item.get("owner"),
                        "{label}/{view_label}: `{path}` hidden member owner"
                    );
                    assert_eq!(
                        item.get("mana_value").and_then(serde_json::Value::as_u64),
                        Some(0),
                        "{label}/{view_label}: `{path}` still discloses the hidden member's mana value"
                    );
                } else {
                    assert_eq!(
                        item, raw_item,
                        "{label}/{view_label}: `{path}` over-redacted a visible member"
                    );
                }
            }
        }
        for (raw_path, _) in &raw_paths {
            if required.iter().any(|(_, matches)| matches(raw_path)) {
                assert!(
                    paths.iter().any(|(path, _)| path == raw_path),
                    "{label}/{view_label}: in-scope path `{raw_path}` is missing; paths: {list:#?}"
                );
            }
        }
    }

    // The authorized looker keeps every array exactly.
    let mut owner_paths = Vec::new();
    walk_key_arrays(
        &serde_json::to_value(filter_state_for_viewer(raw, P0)).expect("P0 serializes"),
        "linked_exile_snapshot",
        "",
        &mut owner_paths,
    );
    for (raw_path, _) in &raw_paths {
        if required.iter().any(|(_, matches)| matches(raw_path)) {
            assert!(
                owner_paths.iter().any(|(path, _)| path == raw_path),
                "{label}/P0: in-scope path `{raw_path}` is missing"
            );
        }
    }
    for (path, items) in &owner_paths {
        assert!(
            raw_paths
                .iter()
                .any(|(raw_path, raw_items)| raw_path == path && raw_items == items),
            "{label}/P0: `{path}` differs from the raw state"
        );
    }
}

/// Test F3: a Hideaway source bounced to its owner's hand (Capsize) while its
/// face-down exiled card stays hidden. The source's own id is now hidden, so
/// its `linked_exile_lki` entry is dropped from the hidden-id projection and
/// its departure record is anonymised whole (an existing branch: these record
/// checks guard it and do not fail if this round's linked-exile fix is reverted).
#[test]
fn hideaway_source_bounced_to_hand_drops_linked_exile_entry() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let windbrisk = scenario.add_real_card(P0, "Windbrisk Heights", Zone::Hand, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let windbrisk_card_id = runner.state().objects[&windbrisk].card_id;

    assert!(
        play_windbrisk_and_hide(&mut runner, windbrisk, windbrisk_card_id, shock),
        "reach-guard: the real Hideaway ETB surfaced a DigChoice"
    );
    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    runner.cast(capsize).target_objects(&[windbrisk]).resolve();

    let raw = runner.state().clone();
    assert_eq!(
        raw.objects[&windbrisk].zone,
        Zone::Hand,
        "reach-guard: source bounced"
    );
    assert_eq!(raw.objects[&shock].zone, Zone::Exile, "reach-guard");
    assert!(
        raw.objects[&shock].face_down,
        "reach-guard: hidden face down"
    );
    let raw_members = raw
        .linked_exile_lki
        .get(&windbrisk)
        .expect("reach-guard: Windbrisk's bounce wrote linked_exile_lki");
    assert_eq!(
        raw_members
            .iter()
            .find(|m| m.exiled_id == shock)
            .map(|m| m.mana_value),
        Some(1),
        "reach-guard: the hidden member is recorded with Shock's mv"
    );

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&windbrisk].name, HIDDEN,
            "reach-guard ({label}): the source is hidden"
        );
        assert!(
            !view.linked_exile_lki.contains_key(&windbrisk),
            "{label}: linked_exile_lki still keyed by the hidden source"
        );
    }

    // Over-redaction guard: the owner sees their own hand card, so its entry stays.
    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(p0.objects[&windbrisk].name, "Windbrisk Heights");
    assert!(
        p0.linked_exile_lki.contains_key(&windbrisk),
        "P0 owner: a visible source keeps its linked-exile entry"
    );

    // CR 603.10a + CR 607.2a: the bounce's departure record and its synced
    // context latch the hidden member with its real mana value.
    let mv_of = |members: &[LinkedExileSnapshot]| {
        members
            .iter()
            .find(|m| m.exiled_id == shock)
            .map(|m| m.mana_value)
    };
    let (raw_record_members, raw_context_members) =
        record_linked_members(heights_departure(&raw, windbrisk));
    assert_eq!(mv_of(raw_record_members), Some(1), "reach-guard: record");
    assert_eq!(mv_of(raw_context_members), Some(1), "reach-guard: context");

    // The Heights' own id is hidden from P1 and a spectator, so its whole
    // record is anonymised (`redact_zone_change_record`, unchanged by this
    // fix): no linked vector and no source context survive.
    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&shock].name, HIDDEN,
            "reach-guard ({label}): Shock is hidden"
        );
        let record = heights_departure(&view, windbrisk);
        assert_eq!(record.name, HIDDEN, "reach-guard ({label}): hidden record");
        assert!(
            record.linked_exile_snapshot.is_empty(),
            "{label}: the hidden record keeps a linked-exile vector"
        );
        assert!(
            record.trigger_source_context.is_none(),
            "{label}: the hidden record keeps its source context"
        );
    }
    assert_no_hidden_mana_value(&raw, shock, "F3");

    // Authorized looker: P0 sees its own Heights and may still look at Shock.
    assert_eq!(
        p0.objects[&shock].name, "Shock",
        "reach-guard: P0 may still look at the hideaway card"
    );
    let (record_members, context_members) =
        record_linked_members(heights_departure(&p0, windbrisk));
    assert_eq!(record_members, raw_record_members, "P0 record unchanged");
    assert_eq!(context_members, raw_context_members, "P0 context unchanged");
    assert_eq!(mv_of(record_members), Some(1), "P0 record: Shock's mv");
    assert_eq!(mv_of(context_members), Some(1), "P0 context: Shock's mv");

    // CodeRabbit: the raw state names Shock; no opponent or spectator form does.
    assert_no_path_names(&raw, "Shock", "F3");
}

/// No `linked_exile_snapshot` entry naming `hidden`, in any opponent or
/// spectator form, discloses a non-zero mana value.
fn assert_no_hidden_mana_value(raw: &GameState, hidden: ObjectId, label: &str) {
    let hidden_id = serde_json::to_value(hidden).expect("id serializes");
    for (view_label, form) in hidden_viewer_forms(raw) {
        let mut paths = Vec::new();
        walk_key_arrays(state_part(&form), "linked_exile_snapshot", "", &mut paths);
        for (path, items) in &paths {
            for item in items
                .iter()
                .filter(|item| item.get("exiled_id") == Some(&hidden_id))
            {
                assert_eq!(
                    item.get("mana_value").and_then(serde_json::Value::as_u64),
                    Some(0),
                    "{label}/{view_label}: `{path}` discloses the hidden member's mana value"
                );
            }
        }
    }
}

/// Test F2: a damage source bounced to hand after dealing damage.
#[test]
fn damage_source_bounced_to_hand_blanks_damage_record_source() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mawcor = scenario.add_real_card(P0, "Mawcor", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let unsub = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(&mut runner, P0, &[(ManaType::Red, 1), (ManaType::Blue, 2)]);

    runner.cast(bolt).target_player(P1).resolve();
    runner.activate(mawcor, 0).target_player(P1).resolve();
    runner.cast(unsub).target_objects(&[mawcor]).resolve();

    let raw = runner.state().clone();
    assert_eq!(
        raw.objects[&mawcor].zone,
        Zone::Hand,
        "reach-guard: bounced"
    );
    let raw_record = raw
        .damage_dealt_this_turn
        .iter()
        .find(|r| r.source_id == mawcor)
        .expect("reach-guard: Mawcor's ping recorded");
    assert_eq!(raw_record.source_name, "Mawcor");
    assert!(raw_record.source_core_types.contains(&CoreType::Creature));
    assert_eq!(raw_record.amount, 1);

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(p1.objects[&mawcor].name, HIDDEN, "reach-guard: hidden");
    assert_eq!(
        p1.damage_dealt_this_turn.len(),
        raw.damage_dealt_this_turn.len(),
        "damage count preserved"
    );
    let record = p1
        .damage_dealt_this_turn
        .iter()
        .find(|r| r.source_id == mawcor)
        .expect("the hidden record is kept");
    assert_eq!(record.source_name, HIDDEN);
    assert!(record.source_core_types.is_empty());
    assert_eq!(record.source_mana_value, 0);
    assert_eq!(record.amount, 1, "amount is not identity");
    assert_eq!(
        record.target,
        TargetRef::Player(P1),
        "target is not identity"
    );
    let bolt_record = p1
        .damage_dealt_this_turn
        .iter()
        .find(|r| r.source_id == bolt)
        .expect("sibling: the Bolt's damage record");
    assert_eq!(
        bolt_record.source_name, "Lightning Bolt",
        "a public source keeps its name"
    );
}

/// Test F4: a declared attacker bounced to hand during combat.
#[test]
fn declared_attacker_bounced_to_hand_blanks_attack_declaration_record() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let unsub = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P1))])
        .expect("declaring the Bears must succeed");
    fund(&mut runner, P0, &[(ManaType::Blue, 2)]);
    runner.cast(unsub).target_objects(&[bears]).resolve();

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Hand, "reach-guard: bounced");
    let raw_record = raw
        .attacker_declarations_this_turn
        .iter()
        .find(|r| r.object_id == bears)
        .expect("reach-guard: the declaration was recorded");
    assert_eq!(raw_record.lki.name, "Grizzly Bears");

    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(p1.objects[&bears].name, HIDDEN, "reach-guard: hidden");
    assert_eq!(
        p1.attacker_declarations_this_turn.len(),
        raw.attacker_declarations_this_turn.len(),
        "declaration count preserved"
    );
    let record = p1
        .attacker_declarations_this_turn
        .iter()
        .find(|r| r.object_id == bears)
        .expect("the hidden record is kept");
    assert_eq!(record.lki.name, HIDDEN);
    assert_eq!(record.lki.mana_value, 0);
    assert!(record.lki.card_types.is_empty());
}

/// Test G (B1): the cast-history projection keeps the columns a type-filtered
/// cost reduction reads, so Demilich's displayed cost on the owner's filtered
/// state equals the raw value. This calls `display_spell_cost` directly on a
/// filtered state; it is not a production filtered-state call site.
#[test]
fn filtered_cast_history_keeps_type_filtered_cost_reduction() {
    let Some((runner, bolt, _, extras)) = bolt_tucked_by_memory_lapse(&["Demilich"]) else {
        return;
    };
    let demilich = extras[0];
    let raw = runner.state().clone();
    let two_blue = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        generic: 0,
    };
    assert_eq!(
        display_spell_cost(&raw, P0, demilich),
        Some(two_blue.clone()),
        "reach-guard: Demilich costs {{U}}{{U}} after two instants"
    );
    assert_eq!(
        cast_records_for(&raw, P0, bolt, false).len(),
        1,
        "reach-guard: the raw Bolt record links the Bolt"
    );

    let p0 = filter_state_for_viewer(&raw, P0);
    let blanked: Vec<_> = p0.spells_cast_this_turn_by_player[&P0]
        .iter()
        .filter(|r| r.name == HIDDEN)
        .collect();
    assert_eq!(blanked.len(), 1, "reach-guard: the Bolt record was blanked");
    assert_eq!(blanked[0].spell_object_id, None);

    assert_eq!(
        display_spell_cost(&p0, P0, demilich),
        Some(two_blue),
        "filtered cast history must still count the hidden instant for Demilich"
    );
}

fn counter_records_for(state: &GameState, id: ObjectId) -> Vec<CounterAddedRecord> {
    state
        .counter_added_this_turn
        .iter()
        .filter(|r| r.object_id == id)
        .cloned()
        .collect()
}

fn plus_one_counter_sum(state: &GameState) -> u32 {
    state
        .counter_added_this_turn
        .iter()
        .filter(|r| r.counter_type == CounterType::Plus1Plus1)
        .map(|r| r.count)
        .sum()
}

/// A hidden recipient's counter record loses every identifying column and keeps
/// who put how many of which counter on which id (the counted columns).
fn assert_counter_record_blanked(raw: &CounterAddedRecord, view: &CounterAddedRecord, label: &str) {
    assert_eq!(
        view.name, HIDDEN,
        "{label}: the counter record still names the hidden recipient"
    );
    assert!(view.core_types.is_empty(), "{label}: core types kept");
    assert!(view.subtypes.is_empty(), "{label}: subtypes kept");
    assert!(view.supertypes.is_empty(), "{label}: supertypes kept");
    assert!(view.keywords.is_empty(), "{label}: keywords kept");
    assert!(view.colors.is_empty(), "{label}: colors kept");
    assert!(view.counters.is_empty(), "{label}: counters kept");
    assert_eq!(view.power, None, "{label}: power kept");
    assert_eq!(view.toughness, None, "{label}: toughness kept");
    assert_eq!(view.mana_value, 0, "{label}: mana value kept");
    assert_eq!(
        (
            view.actor,
            view.object_id,
            &view.counter_type,
            view.count,
            view.controller,
            view.owner
        ),
        (
            raw.actor,
            raw.object_id,
            &raw.counter_type,
            raw.count,
            raw.controller,
            raw.owner
        ),
        "{label}: the counted columns are preserved"
    );
}

/// Test K1: CR 122.6 + CR 400.7: Battlegrowth's real counter writer records the
/// Grizzly Bears, then Unsummon returns them to their owner's hand. The
/// opponent and a spectator see the record kept but blanked; the owner, who
/// sees their own hand, keeps it; a visible sibling (Llanowar Elves) keeps it.
#[test]
fn counter_ledger_blanks_recipient_bounced_to_hand() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let growth_bears = scenario.add_real_card(P0, "Battlegrowth", Zone::Hand, db);
    let growth_elves = scenario.add_real_card(P0, "Battlegrowth", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[(ManaType::Green, 2), (ManaType::Blue, 1)],
    );

    commit_cast(&mut runner, growth_bears, Some(TargetRef::Object(bears)));
    settle(&mut runner);
    commit_cast(&mut runner, growth_elves, Some(TargetRef::Object(elves)));
    settle(&mut runner);
    commit_cast(&mut runner, unsummon, Some(TargetRef::Object(bears)));
    settle(&mut runner);

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Hand, "reach-guard: bounced");
    let raw_bears = counter_records_for(&raw, bears);
    assert_eq!(
        raw_bears.len(),
        1,
        "reach-guard: Battlegrowth's counter was recorded for the Bears"
    );
    let raw_record = &raw_bears[0];
    assert_eq!(raw_record.name, "Grizzly Bears");
    assert_eq!(raw_record.core_types, vec![CoreType::Creature]);
    assert_eq!(raw_record.counter_type, CounterType::Plus1Plus1);
    assert_eq!(raw_record.count, 1);
    assert_eq!(raw_record.mana_value, 2);
    let raw_elves = counter_records_for(&raw, elves);
    assert_eq!(raw_elves.len(), 1, "reach-guard: the sibling's record");
    assert_eq!(raw_elves[0].name, "Llanowar Elves");

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&bears].name, HIDDEN,
            "reach-guard ({label}): the recipient is hidden"
        );
        assert_eq!(
            view.counter_added_this_turn.len(),
            raw.counter_added_this_turn.len(),
            "{label}: the ledger keeps every record"
        );
        let records = counter_records_for(&view, bears);
        assert_eq!(records.len(), 1, "{label}: the hidden record is kept");
        assert_counter_record_blanked(raw_record, &records[0], label);
        assert_eq!(
            view.objects[&elves].name, "Llanowar Elves",
            "paired positive ({label}): the sibling is visible"
        );
        assert_eq!(
            counter_records_for(&view, elves),
            raw_elves,
            "{label}: a visible recipient keeps every column"
        );
    }

    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(p0.objects[&bears].name, "Grizzly Bears");
    assert_eq!(
        counter_records_for(&p0, bears),
        raw_bears,
        "the owner sees their own hand card's counter record"
    );
    assert_eq!(
        counter_records_for(runner.state(), bears)[0].name,
        "Grizzly Bears",
        "authoritative state is untouched by the projection"
    );

    // Re-entry: the recast Bears is public again and its record unredacted.
    fund(&mut runner, P0, &[(ManaType::Green, 2)]);
    runner.cast(bears).resolve();
    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Battlefield, "reach-guard");
    let p1 = filter_state_for_viewer(&raw, P1);
    assert_eq!(
        p1.objects[&bears].name, "Grizzly Bears",
        "reach-guard: the recast permanent is public"
    );
    assert_eq!(
        counter_records_for(&p1, bears),
        counter_records_for(&raw, bears),
        "a public recipient's counter record is not redacted"
    );
    assert_eq!(counter_records_for(&p1, bears)[0].name, "Grizzly Bears");
}

/// Test K2: CR 401.2: Time Ebb puts the countered-upon Bears on top of its
/// owner's library, hidden from every viewer including the owner.
#[test]
fn counter_ledger_blanks_recipient_tucked_into_library_for_every_viewer() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let growth = scenario.add_real_card(P0, "Battlegrowth", Zone::Hand, db);
    let ebb = scenario.add_real_card(P0, "Time Ebb", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    fund(
        &mut runner,
        P0,
        &[(ManaType::Green, 1), (ManaType::Blue, 3)],
    );

    commit_cast(&mut runner, growth, Some(TargetRef::Object(bears)));
    settle(&mut runner);
    commit_cast(&mut runner, ebb, Some(TargetRef::Object(bears)));
    settle(&mut runner);

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Library, "reach-guard");
    assert_eq!(library_top(&raw, P0), Some(bears), "reach-guard: on top");
    let raw_bears = counter_records_for(&raw, bears);
    assert_eq!(raw_bears.len(), 1, "reach-guard: the counter was recorded");
    assert_eq!(raw_bears[0].name, "Grizzly Bears");

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("P0 owner", filter_state_for_viewer(&raw, P0)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(view.objects[&bears].name, HIDDEN, "reach-guard ({label})");
        assert_eq!(
            view.counter_added_this_turn.len(),
            raw.counter_added_this_turn.len(),
            "{label}: the ledger keeps every record"
        );
        assert_eq!(
            plus_one_counter_sum(&view),
            plus_one_counter_sum(&raw),
            "{label}: the +1/+1 counter count is preserved"
        );
        let records = counter_records_for(&view, bears);
        assert_eq!(records.len(), 1, "{label}: the hidden record is kept");
        assert_counter_record_blanked(&raw_bears[0], &records[0], label);
    }
}

// ---------------------------------------------------------------------------
// Payment-source snapshots (`mana_spent_source_snapshots`)
// ---------------------------------------------------------------------------

/// Tap each land for real mana (CR 605.3b: a mana ability resolves at once) and
/// assert the pool holds exactly those lands' units.
fn activate_lands(runner: &mut GameRunner, lands: &[ObjectId]) {
    for &land in lands {
        runner.activate(land, 0).resolve();
    }
    let mut sources: Vec<ObjectId> = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .mana_pool
        .mana
        .iter()
        .map(|unit| unit.source_id)
        .collect();
    sources.sort();
    let mut expected = lands.to_vec();
    expected.sort();
    assert_eq!(
        sources, expected,
        "reach-guard: the pool holds one real unit from each land (not ObjectId(0))"
    );
}

fn snapshot_of(
    snapshots: &[ManaSpentSourceSnapshot],
    source: ObjectId,
) -> &ManaSpentSourceSnapshot {
    snapshots
        .iter()
        .find(|s| s.source_id == source)
        .unwrap_or_else(|| panic!("no payment snapshot for {source:?}: {snapshots:?}"))
}

/// Reach-guard / owner control: the vector is exactly {forest, island}, both named.
fn assert_payment_names_both(
    snapshots: &[ManaSpentSourceSnapshot],
    forest: ObjectId,
    island: ObjectId,
    label: &str,
) {
    assert_eq!(snapshots.len(), 2, "{label}: one snapshot per paid unit");
    assert_eq!(snapshot_of(snapshots, forest).lki.name, "Forest", "{label}");
    let island_snapshot = snapshot_of(snapshots, island);
    assert_eq!(island_snapshot.lki.name, "Island", "{label}");
    assert!(
        island_snapshot.lki.card_types.contains(&CoreType::Land),
        "{label}: the island snapshot records its Land type"
    );
}

/// The hidden source's entry keeps its id and slot and loses its identity; the
/// visible source's entry is untouched; length and order are unchanged.
fn assert_payment_redacted(
    raw: &[ManaSpentSourceSnapshot],
    view: &[ManaSpentSourceSnapshot],
    hidden: ObjectId,
    visible: ObjectId,
    label: &str,
) {
    assert_eq!(view.len(), raw.len(), "{label}: payment vector length kept");
    assert_eq!(
        view.iter().map(|s| s.source_id).collect::<Vec<_>>(),
        raw.iter().map(|s| s.source_id).collect::<Vec<_>>(),
        "{label}: source ids and order kept"
    );
    let hidden_snapshot = snapshot_of(view, hidden);
    assert_eq!(
        hidden_snapshot.lki.name, HIDDEN,
        "{label}: the hidden mana source is still named"
    );
    assert!(hidden_snapshot.lki.card_types.is_empty(), "{label}: types");
    assert!(hidden_snapshot.lki.subtypes.is_empty(), "{label}: subtypes");
    assert!(
        hidden_snapshot.lki.supertypes.is_empty(),
        "{label}: supertypes"
    );
    assert_eq!(
        snapshot_of(view, visible),
        snapshot_of(raw, visible),
        "{label}: the visible source's entry is kept"
    );
    assert_eq!(snapshot_of(view, visible).lki.name, "Forest", "{label}");
}

fn entry_record(state: &GameState, id: ObjectId) -> &ZoneChangeRecord {
    state
        .zone_changes_this_turn
        .iter()
        .find(|r| {
            r.object_id == id && r.from_zone == Some(Zone::Stack) && r.to_zone == Zone::Battlefield
        })
        .expect("the Stack -> Battlefield record")
}

fn record_payment(record: &ZoneChangeRecord) -> &[ManaSpentSourceSnapshot] {
    &record
        .trigger_source_context
        .as_ref()
        .expect("the record latches a trigger-source context")
        .mana_spent_source_snapshots
}

/// Test H1: CR 601.2h + CR 106.3 + CR 400.2: Grizzly Bears paid by a real
/// Forest and a real Island; Capsize then returns the Island to its owner's
/// hand. The public Bears keep both payment entries, but the opponent and a
/// spectator no longer see which card the Island entry was.
#[test]
fn visible_permanent_blanks_hidden_mana_source_in_payment_snapshots() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    activate_lands(&mut runner, &[forest, island]);
    commit_cast(&mut runner, bears, None);
    settle(&mut runner);

    let before = runner.state().clone();
    assert_eq!(
        before.objects[&bears].zone,
        Zone::Battlefield,
        "reach-guard"
    );
    let paid = before.objects[&bears].mana_spent_source_snapshots.clone();
    assert_payment_names_both(&paid, forest, island, "raw Bears");
    assert_eq!(before.objects[&bears].mana_spent_to_cast_amount, 2);
    // Baseline control: while both sources are public nothing is redacted.
    assert_eq!(
        filter_state_for_viewer(&before, P1).objects[&bears].mana_spent_source_snapshots,
        paid,
        "no over-redaction while every mana source is public"
    );

    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    commit_cast(&mut runner, capsize, Some(TargetRef::Object(island)));
    settle(&mut runner);

    let raw = runner.state().clone();
    assert_eq!(
        raw.objects[&island].zone,
        Zone::Hand,
        "reach-guard: bounced"
    );
    let raw_bears = &raw.objects[&bears];
    assert_payment_names_both(
        &raw_bears.mana_spent_source_snapshots,
        forest,
        island,
        "raw Bears after the bounce",
    );
    let raw_entry = entry_record(&raw, bears);
    assert_payment_names_both(
        record_payment(raw_entry),
        forest,
        island,
        "raw Bears Stack->Battlefield record",
    );
    assert!(
        raw.zone_changes_this_turn
            .iter()
            .any(|r| r.object_id == island && r.to_zone == Zone::Hand && r.name == "Island"),
        "reach-guard: the island's own departure was recorded"
    );

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&island].name, HIDDEN,
            "reach-guard ({label}): the mana source is hidden"
        );
        let view_bears = &view.objects[&bears];
        assert_eq!(view_bears.name, "Grizzly Bears", "{label}: Bears public");
        assert_payment_redacted(
            &raw_bears.mana_spent_source_snapshots,
            &view_bears.mana_spent_source_snapshots,
            island,
            forest,
            label,
        );
        assert_eq!(view_bears.mana_spent_to_cast_amount, 2, "{label}");
        assert_eq!(
            view_bears.mana_spent_to_cast, raw_bears.mana_spent_to_cast,
            "{label}"
        );
        assert_eq!(
            view_bears.colors_spent_to_cast, raw_bears.colors_spent_to_cast,
            "{label}: payment facts are kept"
        );
        let island_record = view
            .zone_changes_this_turn
            .iter()
            .find(|r| r.object_id == island && r.to_zone == Zone::Hand)
            .expect("the island's departure record is kept");
        assert_eq!(island_record.name, HIDDEN, "{label}: hidden-id record");
        assert!(island_record.trigger_source_context.is_none(), "{label}");
        assert_payment_redacted(
            record_payment(raw_entry),
            record_payment(entry_record(&view, bears)),
            island,
            forest,
            &format!("{label} Bears Stack->Battlefield record"),
        );
    }

    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(p0.objects[&island].name, "Island", "owner sees own hand");
    assert_eq!(
        p0.objects[&bears].mana_spent_source_snapshots, raw_bears.mana_spent_source_snapshots,
        "the owner keeps both payment entries"
    );
    assert_eq!(
        record_payment(entry_record(&p0, bears)),
        record_payment(raw_entry),
        "the owner keeps the record's payment entries"
    );
    assert_payment_names_both(
        &runner.state().objects[&bears].mana_spent_source_snapshots,
        forest,
        island,
        "authoritative state after projection",
    );
}

/// Pass priority once for the player who holds it.
fn pass(runner: &mut GameRunner) {
    runner
        .act(GameAction::PassPriority)
        .expect("passing priority must be accepted");
}

fn assert_priority(state: &GameState, player: PlayerId, label: &str) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player: p } if p == player),
        "{label}: expected Priority {{ {player:?} }}, got {:?}",
        state.waiting_for
    );
}

fn top_is_trigger(state: &GameState) -> bool {
    matches!(
        state.stack.back().map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility { .. })
    )
}

/// Board H3 (also census sub-case A): Elvish Visionary paid by a real Forest
/// and a real Island resolves; with its ETB trigger still on the stack,
/// Capsize returns the Island to P0's hand. Returns (runner, forest, island,
/// visionary).
fn visionary_etb_pending_after_island_bounced() -> Option<(GameRunner, ObjectId, ObjectId, ObjectId)>
{
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5b: the ETB draw must never deck P0.
    scenario.add_real_card(P0, "Shock", Zone::Library, db);
    scenario.add_real_card(P0, "Shock", Zone::Library, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let visionary = scenario.add_real_card(P0, "Elvish Visionary", Zone::Hand, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    activate_lands(&mut runner, &[forest, island]);
    commit_cast(&mut runner, visionary, None);
    for _ in 0..6 {
        let state = runner.state();
        if state.objects[&visionary].zone == Zone::Battlefield
            && top_is_trigger(state)
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            break;
        }
        pass(&mut runner);
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&visionary].zone,
        Zone::Battlefield,
        "reach-guard: the Visionary resolved"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "reach-guard: only the ETB is on the stack"
    );
    assert!(
        top_is_trigger(state),
        "reach-guard: the ETB trigger is on the stack"
    );
    assert_priority(state, P0, "reach-guard: P0 holds priority over the ETB");

    // `fund` adds pool units without passing priority, so the ETB stays put.
    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    commit_cast(&mut runner, capsize, Some(TargetRef::Object(island)));
    pass(&mut runner);
    pass(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&island].zone,
        Zone::Hand,
        "reach-guard: bounced"
    );
    assert_eq!(state.stack.len(), 1, "reach-guard: only Capsize resolved");
    assert!(
        top_is_trigger(state),
        "reach-guard: the ETB is still pending"
    );
    assert_priority(state, P0, "reach-guard: P0 holds priority again");
    Some((runner, forest, island, visionary))
}

/// The ETB stack entry's latched payment and its `trigger_event` record's payment.
fn etb_payments(state: &GameState) -> (&[ManaSpentSourceSnapshot], &[ManaSpentSourceSnapshot]) {
    let entry = state.stack.back().expect("the ETB entry");
    let StackEntryKind::TriggeredAbility {
        ability,
        trigger_event,
        ..
    } = &entry.kind
    else {
        panic!("top of stack is not a triggered ability: {:?}", entry.kind);
    };
    let ability_payment = &ability
        .trigger_source
        .as_ref()
        .expect("the ETB latched its source context")
        .mana_spent_source_snapshots;
    let Some(GameEvent::ZoneChanged { record, .. }) = trigger_event else {
        panic!("the ETB's trigger_event is not a ZoneChanged: {trigger_event:?}");
    };
    (ability_payment, record_payment(record))
}

/// Public stack attribution of the ETB entry (never suppressed).
fn etb_attribution(state: &GameState) -> (ObjectId, Option<String>, Option<String>, String) {
    let entry = state.stack.back().expect("the ETB entry");
    let StackEntryKind::TriggeredAbility {
        ability,
        description,
        source_name,
        ..
    } = &entry.kind
    else {
        panic!("top of stack is not a triggered ability");
    };
    (
        entry.source_id,
        ability.description.clone(),
        description.clone(),
        source_name.clone(),
    )
}

/// Test H3: CR 113.7a + CR 601.2h: a triggered ability's latched source context,
/// its trigger event's record, and the turn's zone-change journal all clone the
/// paying sources. After the Island is bounced, the opponent and a spectator
/// see each latched copy blanked for the Island and intact for the Forest,
/// while the public stack attribution is unchanged.
#[test]
fn latched_trigger_contexts_blank_hidden_mana_source() {
    let Some((runner, forest, island, visionary)) = visionary_etb_pending_after_island_bounced()
    else {
        return;
    };
    let raw = runner.state().clone();
    let (raw_ability, raw_event) = etb_payments(&raw);
    assert_payment_names_both(raw_ability, forest, island, "raw ETB trigger_source");
    assert_payment_names_both(raw_event, forest, island, "raw ETB trigger_event");
    let raw_entry = entry_record(&raw, visionary);
    assert_payment_names_both(
        record_payment(raw_entry),
        forest,
        island,
        "raw Visionary Stack->Battlefield record",
    );
    let raw_object = &raw.objects[&visionary].mana_spent_source_snapshots;
    assert_payment_names_both(raw_object, forest, island, "raw Visionary object");

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&island].name, HIDDEN,
            "reach-guard ({label}): the mana source is hidden"
        );
        let (ability, event) = etb_payments(&view);
        assert_payment_redacted(
            raw_ability,
            ability,
            island,
            forest,
            &format!("{label} ETB trigger_source"),
        );
        assert_payment_redacted(
            raw_event,
            event,
            island,
            forest,
            &format!("{label} ETB trigger_event"),
        );
        assert_payment_redacted(
            record_payment(raw_entry),
            record_payment(entry_record(&view, visionary)),
            island,
            forest,
            &format!("{label} Visionary record"),
        );
        assert_payment_redacted(
            raw_object,
            &view.objects[&visionary].mana_spent_source_snapshots,
            island,
            forest,
            &format!("{label} Visionary object"),
        );
        assert_eq!(
            etb_attribution(&view),
            etb_attribution(&raw),
            "{label}: public stack attribution is never suppressed"
        );
    }

    let p0 = filter_state_for_viewer(&raw, P0);
    let (ability, event) = etb_payments(&p0);
    assert_eq!(ability, raw_ability, "owner keeps the latched payment");
    assert_eq!(event, raw_event, "owner keeps the trigger event's payment");
}

/// Board H4 (also census sub-case B): Grizzly Bears paid by a real Forest and
/// a real Island is cast; Capsize returns the Island to P0's hand while the
/// Bears spell waits on the stack; Counterspell then counters the Bears, so
/// the real `record_departed_stack_spell` writer stores the departed spell.
/// Returns (runner, forest, island, bears).
fn bears_countered_after_island_bounced() -> Option<(GameRunner, ObjectId, ObjectId, ObjectId)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let counterspell = scenario.add_real_card(P0, "Counterspell", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    activate_lands(&mut runner, &[forest, island]);
    commit_cast(&mut runner, bears, None);
    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Stack, "reach-guard");
    assert_eq!(state.stack.len(), 1, "reach-guard: the Bears spell");
    assert_payment_names_both(
        &state.objects[&bears].mana_spent_source_snapshots,
        forest,
        island,
        "raw Bears spell",
    );

    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    commit_cast(&mut runner, capsize, Some(TargetRef::Object(island)));
    pass(&mut runner);
    pass(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&island].zone,
        Zone::Hand,
        "reach-guard: bounced"
    );
    assert_eq!(state.stack.len(), 1, "reach-guard: only Capsize resolved");
    assert_eq!(state.objects[&bears].zone, Zone::Stack, "reach-guard");
    assert_eq!(
        state.stack.back().map(|entry| entry.source_id),
        Some(bears),
        "reach-guard: the Bears spell is still on the stack"
    );
    assert_priority(state, P0, "reach-guard: P0 holds priority");

    fund(&mut runner, P0, &[(ManaType::Blue, 2)]);
    commit_cast(&mut runner, counterspell, Some(TargetRef::Object(bears)));
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach-guard: countered"
    );
    Some((runner, forest, island, bears))
}

/// Test H4: CR 608.2h + CR 601.2h: a countered spell's departed stack copy keeps
/// its payment snapshots. The Bears' key is public, so the entry is kept; only
/// the bounced Island's snapshot inside it is blanked for the opponent.
#[test]
fn departed_stack_spell_blanks_hidden_mana_source() {
    let Some((runner, forest, island, bears)) = bears_countered_after_island_bounced() else {
        return;
    };
    let raw = runner.state().clone();
    let raw_departed = raw
        .departed_stack_spells
        .get(&bears)
        .expect("reach-guard: the countered Bears were recorded as departed");
    assert!(!raw_departed.is_empty(), "reach-guard: one incarnation");
    for departed in raw_departed.values() {
        assert_payment_names_both(
            &departed.object.mana_spent_source_snapshots,
            forest,
            island,
            "raw departed Bears object",
        );
    }

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&island].name, HIDDEN,
            "reach-guard ({label}): the mana source is hidden"
        );
        let departed = view
            .departed_stack_spells
            .get(&bears)
            .expect("a public departed spell keeps its entry");
        assert_eq!(departed.len(), raw_departed.len(), "{label}");
        for (incarnation, raw_spell) in raw_departed.iter() {
            let spell = &departed[incarnation];
            assert_payment_redacted(
                &raw_spell.object.mana_spent_source_snapshots,
                &spell.object.mana_spent_source_snapshots,
                island,
                forest,
                &format!("{label} departed object"),
            );
            let raw_entry_payment = raw_spell
                .entry
                .ability()
                .and_then(|ability| ability.trigger_source.as_ref())
                .map(|context| &context.mana_spent_source_snapshots);
            let entry_payment = spell
                .entry
                .ability()
                .and_then(|ability| ability.trigger_source.as_ref())
                .map(|context| &context.mana_spent_source_snapshots);
            assert_eq!(
                entry_payment.map(Vec::len),
                raw_entry_payment.map(Vec::len),
                "{label}: departed entry payment presence and length kept"
            );
            if let (Some(raw_payment), Some(payment)) = (raw_entry_payment, entry_payment) {
                if raw_payment.iter().any(|s| s.source_id == island) {
                    assert_payment_redacted(
                        raw_payment,
                        payment,
                        island,
                        forest,
                        &format!("{label} departed entry"),
                    );
                }
            }
        }
    }

    let p0 = filter_state_for_viewer(&raw, P0);
    for (incarnation, raw_spell) in raw_departed.iter() {
        assert_eq!(
            p0.departed_stack_spells[&bears][incarnation]
                .object
                .mana_spent_source_snapshots,
            raw_spell.object.mana_spent_source_snapshots,
            "the owner keeps the departed spell's payment"
        );
    }
}

/// Collect every array under the key `key`, at any depth, with its JSON path.
fn walk_key_arrays(
    value: &serde_json::Value,
    key: &str,
    path: &str,
    out: &mut Vec<(String, Vec<serde_json::Value>)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                let child_path = format!("{path}/{name}");
                if name == key {
                    if let serde_json::Value::Array(items) = child {
                        out.push((child_path.clone(), items.clone()));
                    }
                }
                walk_key_arrays(child, key, &child_path, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_key_arrays(child, key, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

fn payment_paths(value: &serde_json::Value) -> Vec<(String, Vec<serde_json::Value>)> {
    let mut out = Vec::new();
    walk_key_arrays(value, "mana_spent_source_snapshots", "", &mut out);
    out
}

fn snapshot_name(item: &serde_json::Value) -> Option<&str> {
    item.pointer("/lki/name")
        .and_then(serde_json::Value::as_str)
}

fn items_from<'a>(
    items: &'a [serde_json::Value],
    source: &'a serde_json::Value,
) -> impl Iterator<Item = &'a serde_json::Value> {
    items
        .iter()
        .filter(move |item| item.get("source_id") == Some(source))
}

/// A named in-scope carrier path predicate the census requires in the raw state.
type RequiredPath<'a> = (&'a str, &'a dyn Fn(&str) -> bool);

/// Census H2: walk every `mana_spent_source_snapshots` array, at any depth, in
/// the raw state and in each viewer wire form, and require the hidden Island
/// to be blanked everywhere, each visible source kept under its name
/// everywhere, and every entry not naming the hidden Island equal to its raw
/// JSON (no over-redaction).
fn assert_payment_census(
    raw: &GameState,
    island: ObjectId,
    visible: &[(ObjectId, &str)],
    required: &[RequiredPath<'_>],
    label: &str,
) {
    let island_id = serde_json::to_value(island).expect("id serializes");
    let visible_ids: Vec<(serde_json::Value, &str)> = visible
        .iter()
        .map(|(id, name)| (serde_json::to_value(id).expect("id serializes"), *name))
        .collect();
    let raw_paths = payment_paths(&serde_json::to_value(raw).expect("raw state serializes"));
    let raw_list: Vec<&String> = raw_paths.iter().map(|(path, _)| path).collect();
    for (name, matches) in required {
        assert!(
            raw_paths.iter().any(|(path, items)| matches(path)
                && items_from(items, &island_id).any(|i| snapshot_name(i) == Some("Island"))),
            "{label}: reach-guard: no raw `{name}` path names the Island; raw paths: {raw_list:#?}"
        );
    }

    let views = [
        (
            "P1",
            serde_json::to_value(filter_state_for_viewer(raw, P1)).expect("P1 serializes"),
        ),
        (
            "spectator",
            serde_json::to_value(filter_state_for_unseated_viewer(raw))
                .expect("spectator serializes"),
        ),
        (
            "client wire P1",
            serde_json::to_value(ClientGameStateRef::wrap(raw, Some(P1)))
                .expect("client wire serializes"),
        ),
    ];
    for (view_label, view) in &views {
        let paths = payment_paths(view);
        let list: Vec<&String> = paths.iter().map(|(path, _)| path).collect();
        for (path, items) in &paths {
            for item in items_from(items, &island_id) {
                assert_eq!(
                    snapshot_name(item),
                    Some(HIDDEN),
                    "{label}/{view_label}: `{path}` still names the hidden Island; paths: {list:#?}"
                );
            }
        }
        for (raw_path, raw_items) in &raw_paths {
            let raw_island = items_from(raw_items, &island_id).count();
            let raw_visible: usize = visible_ids
                .iter()
                .map(|(id, _)| items_from(raw_items, id).count())
                .sum();
            if raw_island + raw_visible == 0 {
                continue;
            }
            let Some((_, items)) = paths
                .iter()
                .find(|(path, _)| path.ends_with(raw_path.as_str()))
            else {
                // A carrier the projection drops wholesale (e.g. the
                // `resolved_rules_journal` reset) cannot leak; an in-scope
                // carrier must survive with its slots.
                assert!(
                    !required.iter().any(|(_, matches)| matches(raw_path)),
                    "{label}/{view_label}: in-scope path `{raw_path}` is missing; paths: {list:#?}"
                );
                continue;
            };
            assert_eq!(
                items.len(),
                raw_items.len(),
                "{label}/{view_label}: `{raw_path}` length changed"
            );
            assert_eq!(
                items_from(items, &island_id).count(),
                raw_island,
                "{label}/{view_label}: `{raw_path}` island entries"
            );
            for (visible_id, name) in &visible_ids {
                let visible_items: Vec<_> = items_from(items, visible_id).collect();
                assert_eq!(
                    visible_items.len(),
                    items_from(raw_items, visible_id).count(),
                    "{label}/{view_label}: `{raw_path}` {name} entries"
                );
                for item in visible_items {
                    assert_eq!(
                        snapshot_name(item),
                        Some(*name),
                        "{label}/{view_label}: `{raw_path}` lost the visible {name}"
                    );
                }
            }
            for (raw_item, item) in raw_items.iter().zip(items) {
                if raw_item.get("source_id") != Some(&island_id) {
                    assert_eq!(
                        item, raw_item,
                        "{label}/{view_label}: `{raw_path}` over-redacted a visible entry"
                    );
                }
            }
        }
    }
}

/// Test H2: the payment-snapshot census over boards H3 (sub-case A) and H4
/// (sub-case B). A survivor anywhere fails with its path.
#[test]
fn payment_snapshot_census_blanks_hidden_source_in_every_wire_form() {
    let Some((runner, forest, island, visionary)) = visionary_etb_pending_after_island_bounced()
    else {
        return;
    };
    let objects_path = format!("/objects/{}/mana_spent_source_snapshots", visionary.0);
    let object_path = |path: &str| path == objects_path;
    let journal_path = |path: &str| path.starts_with("/zone_changes_this_turn/");
    let stack_ability_path = |path: &str| path.starts_with("/stack/") && path.contains("/ability/");
    let stack_event_path =
        |path: &str| path.starts_with("/stack/") && path.contains("/trigger_event/");
    assert_payment_census(
        runner.state(),
        island,
        &[(forest, "Forest")],
        &[
            ("objects[visionary]", &object_path),
            ("zone_changes_this_turn", &journal_path),
            ("stack ability", &stack_ability_path),
            ("stack trigger_event", &stack_event_path),
        ],
        "sub-case A",
    );

    let Some((runner, forest, island, bears)) = bears_countered_after_island_bounced() else {
        return;
    };
    let departed_prefix = format!("/departed_stack_spells/{}/", bears.0);
    let departed_object_path = |path: &str| {
        path.starts_with(&departed_prefix) && path.ends_with("/object/mana_spent_source_snapshots")
    };
    // Probed: the departed Bears' Spell-kind entry carries no ability payment,
    // so only its object is required; the H4 test asserts the entry
    // conditionally.
    assert_payment_census(
        runner.state(),
        island,
        &[(forest, "Forest")],
        &[
            ("departed_stack_spells[bears].object", &departed_object_path),
            ("zone_changes_this_turn", &journal_path),
        ],
        "sub-case B",
    );

    // Sub-case C: board H6 at the unanswered unless-payment prompt. The
    // visible same-name island_b and the four Forests must survive unchanged.
    let Some(TitanTaxBoard {
        runner,
        titan,
        forests,
        island_a,
        island_b,
    }) = frost_titan_tax_prompt_after_island_bounced()
    else {
        return;
    };
    let titan_path = format!("/objects/{}/mana_spent_source_snapshots", titan.0);
    let titan_object_path = |path: &str| path == titan_path;
    let prompt_path =
        |path: &str| path.starts_with("/waiting_for/") && path.contains("/pending_effect/");
    let mut visible: Vec<(ObjectId, &str)> = forests.iter().map(|id| (*id, "Forest")).collect();
    visible.push((island_b, "Island"));
    assert_payment_census(
        runner.state(),
        island_a,
        &visible,
        &[
            ("objects[titan]", &titan_object_path),
            ("zone_changes_this_turn", &journal_path),
            ("waiting_for pending_effect", &prompt_path),
        ],
        "sub-case C",
    );
}

/// Test H5: CR 601.2h + CR 400.2: the sacrifice ledger's record of a paid
/// creature latches its payment snapshots. Grizzly Bears paid by a real Forest
/// and a real Island are sacrificed to Goblin Bombardment, then Capsize returns
/// the Island to P0's hand: the public Bears' record keeps both slots, and only
/// the Island's identity is blanked for the opponent and a spectator.
#[test]
fn sacrifice_record_blanks_hidden_mana_source() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bombardment = scenario
        .add_creature(P0, "Goblin Bombardment", 0, 0)
        .as_enchantment()
        .from_oracle_text(GOBLIN_BOMBARDMENT)
        .id();
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    activate_lands(&mut runner, &[forest, island]);
    commit_cast(&mut runner, bears, None);
    settle(&mut runner);
    sacrifice_to_bombardment(&mut runner, bombardment, bears);
    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    commit_cast(&mut runner, capsize, Some(TargetRef::Object(island)));
    settle(&mut runner);

    let raw = runner.state().clone();
    assert_eq!(raw.objects[&bears].zone, Zone::Graveyard, "reach-guard");
    assert_eq!(
        raw.objects[&island].zone,
        Zone::Hand,
        "reach-guard: bounced"
    );
    let raw_record = sacrifice_named(&raw, bears).expect("reach-guard: the sacrifice record");
    assert_payment_names_both(
        record_payment(&raw_record),
        forest,
        island,
        "raw Bears sacrifice record",
    );

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&island].name, HIDDEN,
            "reach-guard ({label}): the mana source is hidden"
        );
        let record = sacrifice_named(&view, bears).expect("the public record is kept");
        assert_eq!(
            record.name, "Grizzly Bears",
            "{label}: a public sacrificed creature keeps its name"
        );
        assert_payment_redacted(
            record_payment(&raw_record),
            record_payment(&record),
            island,
            forest,
            &format!("{label} sacrifice record"),
        );
    }
    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(
        record_payment(&sacrifice_named(&p0, bears).expect("owner record")),
        record_payment(&raw_record),
        "the owner keeps the sacrifice record's payment"
    );
}

/// Board H6 (also census sub-case C): Frost Titan, paid by four real Forests and
/// two real Islands, resolves and taps P1's Grizzly Bears with its enters
/// trigger; Capsize returns one Island (`island_a`) to P0's hand while the other
/// (`island_b`) stays on the battlefield. P1 then casts Lightning Bolt at the
/// Titan, and the Titan's "counter that spell or ability unless its controller
/// pays {2}" trigger resolves into the unless-payment prompt (CR 118.12a), which
/// is left unanswered.
struct TitanTaxBoard {
    runner: GameRunner,
    titan: ObjectId,
    forests: Vec<ObjectId>,
    island_a: ObjectId,
    island_b: ObjectId,
}

fn frost_titan_tax_prompt_after_island_bounced() -> Option<TitanTaxBoard> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forests: Vec<ObjectId> = (0..4)
        .map(|_| scenario.add_real_card(P0, "Forest", Zone::Battlefield, db))
        .collect();
    let island_a = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let island_b = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let titan = scenario.add_real_card(P0, "Frost Titan", Zone::Hand, db);
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {4}{U}{U}: six real units, one per land, so the Titan's payment vector has
    // one entry per land.
    let mut lands = forests.clone();
    lands.extend([island_a, island_b]);
    activate_lands(&mut runner, &lands);
    commit_cast(&mut runner, titan, None);
    for _ in 0..6 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        pass(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reach-guard: the Titan's enters trigger asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        })
        .expect("target Grizzly Bears");
    for _ in 0..6 {
        let state = runner.state();
        if state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            break;
        }
        pass(&mut runner);
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&titan].zone,
        Zone::Battlefield,
        "reach-guard: the Titan resolved"
    );
    assert!(
        state.objects[&bears].tapped,
        "reach-guard: the enters trigger resolved"
    );
    assert!(state.stack.is_empty(), "reach-guard: the stack settled");
    assert_priority(state, P0, "reach-guard: P0 holds priority after the Titan");

    fund(&mut runner, P0, &[(ManaType::Blue, 3)]);
    commit_cast(&mut runner, capsize, Some(TargetRef::Object(island_a)));
    settle(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&island_a].zone,
        Zone::Hand,
        "reach-guard: island_a bounced"
    );
    assert_eq!(
        state.objects[&island_b].zone,
        Zone::Battlefield,
        "reach-guard: island_b stays public"
    );
    assert!(state.stack.is_empty(), "reach-guard: Capsize resolved");
    assert_priority(state, P0, "reach-guard: P0 holds priority after Capsize");

    pass(&mut runner);
    assert_priority(runner.state(), P1, "reach-guard: P1 receives priority");
    // Bolt's {R} plus the {2} tax, as in the issue #9282 Frost Titan precedent.
    fund(
        &mut runner,
        P1,
        &[(ManaType::Red, 1), (ManaType::Colorless, 2)],
    );
    commit_cast(&mut runner, bolt, Some(TargetRef::Object(titan)));
    for _ in 0..6 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        pass(&mut runner);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "reach-guard: the Titan's tax trigger reached its unless-payment prompt: {:?}",
        runner.state().waiting_for
    );
    Some(TitanTaxBoard {
        runner,
        titan,
        forests,
        island_a,
        island_b,
    })
}

/// The payment vector latched by the unless-payment prompt's retained ability.
fn prompt_payment(state: &GameState) -> &[ManaSpentSourceSnapshot] {
    let WaitingFor::UnlessPayment { pending_effect, .. } = &state.waiting_for else {
        panic!("expected UnlessPayment, got {:?}", state.waiting_for);
    };
    &pending_effect
        .trigger_source
        .as_ref()
        .expect("the tax trigger latched its source context")
        .mana_spent_source_snapshots
}

/// Only the hidden source's entry is blanked: same length, ids and order; the
/// hidden entry loses its identity; every other entry equals the raw entry.
fn assert_only_hidden_blanked(
    raw: &[ManaSpentSourceSnapshot],
    view: &[ManaSpentSourceSnapshot],
    hidden: ObjectId,
    label: &str,
) {
    assert!(
        raw.iter().any(|s| s.source_id == hidden),
        "{label}: reach-guard: the raw vector names the hidden source"
    );
    assert_eq!(view.len(), raw.len(), "{label}: payment vector length kept");
    assert_eq!(
        view.iter().map(|s| s.source_id).collect::<Vec<_>>(),
        raw.iter().map(|s| s.source_id).collect::<Vec<_>>(),
        "{label}: source ids and order kept"
    );
    for (raw_entry, entry) in raw.iter().zip(view) {
        if raw_entry.source_id == hidden {
            assert_eq!(
                entry.lki.name, HIDDEN,
                "{label}: the hidden mana source is still named"
            );
            assert!(entry.lki.card_types.is_empty(), "{label}: types");
            assert!(entry.lki.subtypes.is_empty(), "{label}: subtypes");
            assert!(entry.lki.supertypes.is_empty(), "{label}: supertypes");
        } else {
            assert_eq!(
                entry, raw_entry,
                "{label}: a visible source's entry is kept"
            );
        }
    }
}

/// Test H6: CR 118.12a + CR 601.2h + CR 113.7a + CR 400.2: the unless-payment
/// prompt of Frost Titan's tax trigger retains the trigger's resolved ability,
/// whose latched source context clones the Titan's payment snapshots. After a
/// paying Island returned to its owner's hand, the opponent, a spectator and the
/// opponent's client wire see that Island's entry blanked inside the prompt,
/// while the visible same-name Island and the Forests stay named, the public
/// prompt spine is unchanged, and the owner and the authoritative state keep the
/// full vector.
#[test]
fn frost_titan_tax_prompt_blanks_hidden_mana_source() {
    let Some(TitanTaxBoard {
        mut runner,
        titan,
        forests,
        island_a,
        island_b,
    }) = frost_titan_tax_prompt_after_island_bounced()
    else {
        return;
    };
    let raw = runner.state().clone();

    // Raw reach-guards, before any negative assertion.
    let titan_payment = &raw.objects[&titan].mana_spent_source_snapshots;
    let mut expected_sources = forests.clone();
    expected_sources.extend([island_a, island_b]);
    expected_sources.sort();
    let mut paid_sources: Vec<ObjectId> = titan_payment.iter().map(|s| s.source_id).collect();
    paid_sources.sort();
    assert_eq!(
        paid_sources, expected_sources,
        "reach-guard: the Titan's vector has one entry per paying land"
    );
    let island_entry = snapshot_of(titan_payment, island_a);
    assert_eq!(island_entry.lki.name, "Island", "reach-guard");
    assert!(
        island_entry.lki.card_types.contains(&CoreType::Land),
        "reach-guard"
    );
    let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = &raw.waiting_for
    else {
        panic!("reach-guard: expected UnlessPayment");
    };
    assert_eq!(*player, P1, "reach-guard: the Bolt's controller pays");
    assert_eq!(pending_effect.source_id, titan, "reach-guard");
    let raw_payment = prompt_payment(&raw).to_vec();
    assert_eq!(
        &raw_payment, titan_payment,
        "reach-guard: the prompt's ability latched the Titan's payment"
    );
    assert_eq!(snapshot_of(&raw_payment, island_a).lki.name, "Island");
    assert_eq!(snapshot_of(&raw_payment, island_b).lki.name, "Island");
    assert_eq!(raw.objects[&island_a].zone, Zone::Hand, "reach-guard");
    let raw_json = serde_json::to_value(&raw).expect("raw state serializes");
    let island_a_id = serde_json::to_value(island_a).expect("id serializes");
    assert!(
        payment_paths(&raw_json).iter().any(|(path, items)| {
            path.contains("/waiting_for/")
                && path.contains("/pending_effect/")
                && items_from(items, &island_a_id).any(|i| snapshot_name(i) == Some("Island"))
        }),
        "reach-guard: a raw waiting_for pending_effect path names island_a"
    );

    for (label, view) in [
        ("P1", filter_state_for_viewer(&raw, P1)),
        ("spectator", filter_state_for_unseated_viewer(&raw)),
    ] {
        assert_eq!(
            view.objects[&island_a].name, HIDDEN,
            "reach-guard ({label}): island_a is hidden"
        );
        assert_eq!(
            view.objects[&island_b].name, "Island",
            "reach-guard ({label}): the same-name island_b is visible"
        );
        assert_only_hidden_blanked(
            &raw_payment,
            prompt_payment(&view),
            island_a,
            &format!("{label} UnlessPayment pending_effect"),
        );
        let WaitingFor::UnlessPayment {
            player: view_player,
            cost: view_cost,
            pending_effect: view_effect,
            trigger_event: view_event,
            effect_description: view_description,
            remaining: view_remaining,
        } = &view.waiting_for
        else {
            panic!("{label}: the prompt is still UnlessPayment");
        };
        assert_eq!(view_player, player, "{label}: payer kept");
        assert_eq!(view_cost, cost, "{label}: cost kept");
        assert_eq!(view_event, trigger_event, "{label}: trigger event kept");
        assert_eq!(
            view_description, effect_description,
            "{label}: description kept"
        );
        assert_eq!(view_remaining, remaining, "{label}: remaining kept");
        assert_eq!(view_effect.source_id, pending_effect.source_id, "{label}");
        assert_eq!(view_effect.controller, pending_effect.controller, "{label}");
        assert_eq!(view_effect.effect, pending_effect.effect, "{label}");
        assert_eq!(
            view_effect.description, pending_effect.description,
            "{label}"
        );
        assert_eq!(view_effect.targets, pending_effect.targets, "{label}");
    }

    let wire = serde_json::to_value(ClientGameStateRef::wrap(&raw, Some(P1)))
        .expect("client wire serializes");
    let wire_paths = payment_paths(&wire);
    assert!(
        wire_paths
            .iter()
            .any(|(path, _)| path.contains("/waiting_for/") && path.contains("/pending_effect/")),
        "reach-guard: the client wire carries the prompt's payment vector"
    );
    for (path, items) in &wire_paths {
        for item in items_from(items, &island_a_id) {
            assert_eq!(
                snapshot_name(item),
                Some(HIDDEN),
                "client wire P1: `{path}` still names island_a"
            );
        }
    }

    let p0 = filter_state_for_viewer(&raw, P0);
    assert_eq!(p0.objects[&island_a].name, "Island", "owner sees own hand");
    assert_eq!(
        prompt_payment(&p0),
        raw_payment.as_slice(),
        "the owner keeps the prompt's full payment vector"
    );

    // Authority: projection edits only the clone, and the live prompt still resolves.
    assert_eq!(
        prompt_payment(runner.state()),
        raw_payment.as_slice(),
        "authoritative prompt is untouched by the projections"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays the {2} tax");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the authoritative prompt resolved: {:?}",
        runner.state().waiting_for
    );
}
