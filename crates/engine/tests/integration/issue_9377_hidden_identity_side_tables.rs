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
//! https://github.com/phase-rs/phase/issues/9377

use engine::game::casting::display_spell_cost;
use engine::game::combat::AttackTarget;
use engine::game::derived_views::derive_views;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, GameState, PayCostKind, WaitingFor,
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
/// hidden. `linked_exile_lki` keeps the member slot but loses its mana value.
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
}

/// Test F3: a Hideaway source bounced to its owner's hand (Capsize) while its
/// face-down exiled card stays hidden. The source's own id is now hidden, so
/// its `linked_exile_lki` entry is dropped from the hidden-id projection.
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
