//! Codie, Ravenous Codex — "Whenever you cast a prepared spell, copy it. You may
//! choose new targets for the copy."
//!
//! Cast-pipeline tests for the `prepared spell` trigger qualifier
//! (`FilterProp::PrepareSpell`). The runtime authority is the
//! `prepared_copy_source` marker on the spell cast from a prepared permanent's
//! linked exile copy (CR 722.3c), inherited by copies of that spell (CR 722.3d),
//! and absent from Paradigm / cast-a-copy-of-a-card copies (CR 707.12).
//!
//! Codie is built from its verbatim Oracle text so these tests read the parser
//! under change. The trigger tests mark the prepare cards prepared through the
//! Debug `SetPrepared` action to isolate the trigger; the activated ability
//! ("Each creature you control becomes prepared", CR 115.10a: untargeted) and
//! the full activate-then-cast end-to-end flow are covered at the end of the
//! file without any Debug action.

use std::sync::Arc;

use engine::database::CardDatabase;
use engine::game::effects::paradigm::{arm_paradigm, enqueue_offer_if_any};
use engine::game::filter::{matches_target_filter, spell_record_matches_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, CountScope, Effect,
    FilterProp, QuantityExpr, QuantityRef, ResolvedAbility, StaticCondition, TargetFilter,
    TargetRef, TypedFilter, ZoneRef,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, SpellCastRecord, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;
use engine::types::TriggerMode;

use crate::support::shared_card_db;

/// Verbatim Oracle text of Codie, Ravenous Codex (FRA).
const CODIE_ORACLE: &str = "Whenever you cast a prepared spell, copy it. You may choose new targets for the copy.\n{W}{U}{B}{R}{G}, {T}: Each creature you control becomes prepared. (Only creatures with prepare spells can become prepared.)";

/// Verbatim Oracle text of Contemplation, the bystander whose SpellCast trigger
/// proves a cast event reached trigger collection.
const CONTEMPLATION_ORACLE: &str = "Whenever you cast a spell, you gain 1 life.";

/// Verbatim Oracle text of Shock, an ordinary spell cast from hand.
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

/// The committed integration fixture is required: a missing DB is an
/// environment failure, never a vacuous pass.
fn db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load for Codie prepared-spell tests")
}

fn mana(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
        .collect()
}

/// A preparation card from the fixture: its name, the name of its prepare
/// face, and where it starts.
struct PrepareCard {
    name: &'static str,
    prepare_face: &'static str,
    controller: PlayerId,
    zone: Zone,
}

const EMERITUS_P0: PrepareCard = PrepareCard {
    name: "Emeritus of Truce",
    prepare_face: "Swords to Plowshares",
    controller: P0,
    zone: Zone::Battlefield,
};

struct FixtureSpec {
    /// Number of Codies P0 controls (0, 1 or 2).
    codies: usize,
    prepare: PrepareCard,
    p0_pool: &'static [ManaType],
    p1_pool: &'static [ManaType],
    shock_in_hand: bool,
    counterspell_for_p1: bool,
}

impl FixtureSpec {
    fn swords(codies: usize) -> Self {
        Self {
            codies,
            prepare: EMERITUS_P0,
            p0_pool: &[ManaType::White],
            p1_pool: &[],
            shock_in_hand: false,
            counterspell_for_p1: false,
        }
    }
}

struct Fixture {
    runner: GameRunner,
    codies: Vec<ObjectId>,
    /// The preparation card (Emeritus of Truce unless the spec names another).
    prepare_source: ObjectId,
    /// P0's creature: the prepared Swords' original target.
    x: ObjectId,
    /// P1's creature: the copy's new target.
    y: ObjectId,
    shock: Option<ObjectId>,
    counterspell: Option<ObjectId>,
}

/// The second Codie carries a distinct name so the legend rule is not in play.
fn build_fixture(db: &CardDatabase, spec: FixtureSpec) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codies: Vec<ObjectId> = (0..spec.codies)
        .map(|index| {
            let name = if index == 0 {
                "Codie, Ravenous Codex".to_string()
            } else {
                format!("Codie, Ravenous Codex {}", index + 1)
            };
            scenario
                .add_creature_from_oracle(P0, &name, 1, 4, CODIE_ORACLE)
                .id()
        })
        .collect();
    let prepare_source = scenario.add_real_card(
        spec.prepare.controller,
        spec.prepare.name,
        spec.prepare.zone,
        db,
    );
    let x = scenario.add_creature(P0, "Exile Target", 2, 2).id();
    let y = scenario.add_creature(P1, "Retarget Target", 3, 3).id();
    let shock = spec.shock_in_hand.then(|| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
            .id()
    });
    let counterspell = spec
        .counterspell_for_p1
        .then(|| scenario.add_real_card(P1, "Counterspell", Zone::Hand, db));
    scenario.with_mana_pool(P0, mana(spec.p0_pool));
    if !spec.p1_pool.is_empty() {
        scenario.with_mana_pool(P1, mana(spec.p1_pool));
    }

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let back = runner
        .state()
        .objects
        .get(&prepare_source)
        .and_then(|object| object.back_face.clone())
        .unwrap_or_else(|| {
            panic!(
                "{} must hydrate its {} prepare face",
                spec.prepare.name, spec.prepare.prepare_face
            )
        });
    assert_eq!(back.name, spec.prepare.prepare_face);

    Fixture {
        runner,
        codies,
        prepare_source,
        x,
        y,
        shock,
        counterspell,
    }
}

fn set_prepared(runner: &mut GameRunner, object_id: ObjectId) {
    runner
        .act(GameAction::Debug(DebugAction::SetPrepared {
            object_id,
            prepared: true,
        }))
        .expect("Debug SetPrepared must mark the permanent prepared");
}

/// Prepare `source`, start the prepared cast of its targeted prepare spell, and
/// return the stack object id once the cast pauses for its target.
fn begin_prepared_cast(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    set_prepared(runner, source);
    start_prepared_cast(runner, source)
}

/// Start the prepared cast of an already-prepared `source`'s targeted prepare
/// spell (CR 722.3c) and return the stack object id once the cast pauses for
/// its target.
fn start_prepared_cast(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    runner
        .act(GameAction::CastPreparedCopy { source })
        .expect("CastPreparedCopy should start the prepared spell cast");

    let copy_id = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.object_id,
        other => panic!("prepared Swords cast must pause for a target, got {other:?}"),
    };
    let placeholder = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .expect("CR 601.2a announcement must create the exact prepared-copy stack entry");
    assert!(matches!(
        &placeholder.kind,
        StackEntryKind::Spell { ability: None, .. }
    ));
    assert!(runner.state().objects.contains_key(&copy_id));

    copy_id
}

/// Prepare `source` and start the prepared cast of its UNTARGETED prepare spell
/// (no `TargetSelection` is expected).
fn begin_prepared_untargeted_cast(runner: &mut GameRunner, source: ObjectId) {
    set_prepared(runner, source);
    runner
        .act(GameAction::CastPreparedCopy { source })
        .expect("CastPreparedCopy should start the prepared spell cast");
}

/// Drives the cast to its post-cast priority window. Returns the number of
/// `OrderTriggers` prompts answered with the identity order (CR 603.3b).
/// `spell_target` is `None` for an untargeted spell, which must never pause for
/// a target.
fn drive_cast_to_stack(runner: &mut GameRunner, spell_target: Option<ObjectId>) -> usize {
    let mut drained = 0;
    loop {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                let target = spell_target.expect("an untargeted spell must not ask for a target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("spell target selection should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("trigger target selection should succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::OrderTriggers { .. } => {
                drained +=
                    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during cast: {other:?}"),
        }
    }
    drained
}

fn assert_prepared_copy_finalized_on_stack(
    runner: &GameRunner,
    copy_id: ObjectId,
    spell_target: ObjectId,
) {
    assert!(stack_targets(runner.state(), copy_id).contains(&TargetRef::Object(spell_target)));
    assert_eq!(runner.state().objects[&copy_id].zone, Zone::Stack);
}

/// Flattened targets of the stack entry `id`.
fn stack_targets(state: &GameState, id: ObjectId) -> Vec<TargetRef> {
    let entry = state
        .stack
        .iter()
        .find(|entry| entry.id == id)
        .expect("the spell must have its own stack entry");
    let ability = entry
        .ability()
        .expect("the spell's stack entry must carry its finalized ability");
    engine::game::ability_utils::flatten_targets_in_chain(ability)
}

/// Distinct flattened targets of the stack entry `id`, in first-seen order.
fn distinct_stack_targets(state: &GameState, id: ObjectId) -> Vec<TargetRef> {
    let mut distinct = Vec::new();
    for target in stack_targets(state, id) {
        if !distinct.contains(&target) {
            distinct.push(target);
        }
    }
    distinct
}

/// Count the triggered abilities of `source` currently on the stack.
fn triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

fn codie_triggers(state: &GameState, codies: &[ObjectId]) -> usize {
    codies
        .iter()
        .map(|codie| triggers_from(state, *codie))
        .sum()
}

/// Reach guard for the negatives: the Codie object really carries the parsed
/// SpellCast trigger, narrowed to prepared spells.
fn assert_codie_watches_prepared_spells(state: &GameState, codie: ObjectId) {
    let watches = state.objects[&codie]
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition())
        .any(|definition| {
            matches!(definition.mode, TriggerMode::SpellCast)
                && matches!(
                    &definition.valid_card,
                    Some(TargetFilter::Typed(typed))
                        if typed.properties.contains(&FilterProp::PrepareSpell)
                )
        });
    assert!(
        watches,
        "Codie must carry its SpellCast trigger narrowed to prepared spells"
    );
}

fn prepare_spell_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::PrepareSpell]))
}

/// The four prepare-designation history queries, in order: `PrepareSpell`,
/// `TargetFilter::Not { PrepareSpell }`, `FilterProp::Not { PrepareSpell }`, and
/// `AnyOf { PrepareSpell, Modal }` (`Modal` is unevaluable on a record, so only
/// `PrepareSpell` can make the disjunction true).
fn prepare_history_filters() -> [TargetFilter; 4] {
    [
        prepare_spell_filter(),
        TargetFilter::Not {
            filter: Box::new(prepare_spell_filter()),
        },
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Not {
            prop: Box::new(FilterProp::PrepareSpell),
        }])),
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::AnyOf {
            props: vec![FilterProp::PrepareSpell, FilterProp::Modal],
        }])),
    ]
}

/// CR 722.3d: the readings of a spell cast as a prepare spell.
const PREPARED_READINGS: [bool; 4] = [true, false, false, true];
/// The readings of a spell not cast as a prepare spell (and not modal).
const ORDINARY_READINGS: [bool; 4] = [false, true, true, false];

/// Evaluate every `prepare_history_filters` query against one cast-history
/// record through the public spell-record filter authority.
fn prepare_readings(state: &GameState, record: &SpellCastRecord) -> [bool; 4] {
    prepare_history_filters()
        .map(|filter| spell_record_matches_filter(record, &filter, P0, &state.all_creature_types))
}

/// P0's "spells you've cast this turn" count, optionally filtered,
/// resolved through the production quantity resolver.
fn spells_cast_this_turn(state: &GameState, source: ObjectId, filter: Option<TargetFilter>) -> i32 {
    engine::game::quantity::resolve_quantity(
        state,
        &QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter,
            },
        },
        P0,
        source,
    )
}

/// `(object_id, original_id)` of every `SpellCopied` event in `events`.
fn spell_copied(events: &[GameEvent]) -> Vec<(ObjectId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::SpellCopied {
                object_id,
                original_id,
                ..
            } => Some((*object_id, *original_id)),
            _ => None,
        })
        .collect()
}

fn act_events(runner: &mut GameRunner, action: GameAction, what: &str) -> Vec<GameEvent> {
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{what}: {error:?}"))
        .events
}

/// Two priority passes resolve the top stack entry (CR 117.4); returns the
/// events of both actions.
fn pass_twice(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = act_events(runner, GameAction::PassPriority, "first priority pass");
    events.extend(act_events(
        runner,
        GameAction::PassPriority,
        "second priority pass resolves the top entry",
    ));
    events
}

fn assert_priority(runner: &GameRunner, player: PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::Priority { player: holder } if *holder == player => {}
        other => panic!("expected Priority for {player:?}, got {other:?}"),
    }
}

/// CR 722.3c: casting the prepared copy moves the linked exile copy to the
/// stack and unprepares the permanent; the cast spell keeps the
/// `prepared_copy_source` marker naming the permanent.
#[test]
fn codie_prepared_spell_marker_survives_cast() {
    let Fixture {
        mut runner,
        prepare_source: emeritus,
        x,
        ..
    } = build_fixture(db(), FixtureSpec::swords(0));
    let copy_id = begin_prepared_cast(&mut runner, emeritus);
    let drained = drive_cast_to_stack(&mut runner, Some(x));
    assert_eq!(drained, 0, "no trigger source is on the battlefield");
    assert_prepared_copy_finalized_on_stack(&runner, copy_id, x);

    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 1);
    let spell = &runner.state().objects[&copy_id];
    assert_eq!(spell.zone, Zone::Stack);
    assert_eq!(
        spell.prepared_copy_source,
        Some(emeritus),
        "the spell cast from the linked exile copy keeps its prepare marker on the stack"
    );
    // CR 722.3c: the permanent becomes unprepared as the spell becomes cast.
    assert!(runner.state().objects[&emeritus].prepared.is_none());
}

/// CR 722.3d + CR 707.10: a copy of a spell cast as a prepare spell inherits
/// the marker, and therefore is itself a prepared spell.
#[test]
fn codie_prepared_spell_copy_inherits_marker() {
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        ..
    } = build_fixture(db(), FixtureSpec::swords(1));
    let codie = codies[0];
    let spell_id = begin_prepared_cast(&mut runner, emeritus);
    let drained = drive_cast_to_stack(&mut runner, Some(x));
    assert_eq!(drained, 0, "one Codie trigger is a singleton group");
    assert_eq!(triggers_from(runner.state(), codie), 1);
    assert_eq!(runner.state().stack.len(), 2);

    let events = pass_twice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CopyRetarget { player: P0, .. }
        ),
        "CR 707.10c: the targeted copy opens the new-targets choice, got {:?}",
        runner.state().waiting_for
    );
    let copied = spell_copied(&events);
    assert_eq!(copied.len(), 1, "exactly one copy is created");
    let (copy_id, original_id) = copied[0];
    assert_eq!(original_id, spell_id);

    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keeping the copy's targets must be accepted");
    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 2);

    let copy = &runner.state().objects[&copy_id];
    assert_eq!(copy.zone, Zone::Stack);
    assert!(
        copy.prepared_copy_source.is_some(),
        "CR 722.3d: a copy of a prepare spell is a prepare spell"
    );
    assert_eq!(
        runner.state().objects[&spell_id].prepared_copy_source,
        Some(emeritus),
        "the original keeps its own marker"
    );
    assert!(
        matches_target_filter(
            runner.state(),
            copy_id,
            &prepare_spell_filter(),
            &FilterContext::from_source(runner.state(), codie),
        ),
        "CR 722.3d: the copy matches the prepared-spell filter"
    );
}

fn seed_targetless_paradigm_source(state: &mut GameState, card_num: u64, name: &str) -> ObjectId {
    let id = create_object(state, CardId(card_num), P0, name.to_string(), Zone::Exile);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(1);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    id
}

fn assert_fresh_paradigm_cast(state: &GameState, copy_id: ObjectId, journal_index: u32) {
    let occurrence = state.objects[&copy_id]
        .cast_occurrence
        .expect("the cast paradigm copy must carry fresh cast provenance");
    assert_eq!(occurrence.caster, P0);
    assert_eq!(occurrence.turn_journal_index, journal_index);
    assert_eq!(
        state.stack[0]
            .ability()
            .expect("the spell copy has a resolved ability")
            .cast_occurrence,
        Some(occurrence),
        "the stack ability graph must carry the same cast occurrence"
    );
    assert_eq!(
        state.spells_cast_this_turn_by_player[&P0][journal_index as usize].spell_object_id,
        Some(copy_id),
        "the occurrence must name the newly synthesized copy in the cast ledger"
    );
}

/// CR 707.12: a Paradigm copy is a cast copy of a card, not a spell cast as a
/// prepare spell — it never carries the prepare marker, even when its source
/// does, so Codie does not trigger on it.
#[test]
fn codie_prepared_spell_paradigm_copy_neither_carries_nor_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let contemplation = scenario
        .add_creature_from_oracle(P0, "Contemplation", 1, 1, CONTEMPLATION_ORACLE)
        .id();
    let mut runner = scenario.build();

    let source = {
        let state = runner.state_mut();
        let source = seed_targetless_paradigm_source(state, 100, "Paradigm Bolt");
        // Hostile: the source itself carries a prepare marker.
        state.objects.get_mut(&source).unwrap().prepared_copy_source = Some(ObjectId(999));
        arm_paradigm(state, source, P0, "Paradigm Bolt");
        assert!(
            enqueue_offer_if_any(state, P0),
            "one paradigm source must open a cast offer"
        );
        source
    };

    runner
        .act(GameAction::CastParadigmCopy { source })
        .expect("accepting the paradigm offer must succeed");
    let copy_id = runner.state().stack[0].id;
    // CR 603.3b: answer any ordering prompt before reading trigger counts.
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    assert_priority(&runner, P0);

    assert_eq!(
        runner.state().objects[&copy_id].prepared_copy_source,
        None,
        "CR 707.12: a Paradigm copy is not a prepare spell"
    );
    assert_fresh_paradigm_cast(runner.state(), copy_id, 0);
    // CR 707.12: the cast-history record of the Paradigm cast (identified by
    // `assert_fresh_paradigm_cast` above) records no prepare designation, so the
    // history readings are those of an ordinary spell.
    let record = &runner.state().spells_cast_this_turn_by_player[&P0][0];
    assert_eq!(record.prepared_copy_source, None);
    assert_eq!(prepare_readings(runner.state(), record), ORDINARY_READINGS);
    // Reach guard: the Paradigm route's SpellCast event reaches trigger collection.
    assert_eq!(
        triggers_from(runner.state(), contemplation),
        1,
        "the bystander's SpellCast trigger proves the Paradigm cast reached trigger machinery"
    );
    assert_eq!(
        triggers_from(runner.state(), codie),
        0,
        "Codie must not copy a Paradigm copy (not a prepared spell)"
    );
}

/// CR 707.12: `cast_copy_of_card` clears the prepare marker on the cast copy.
#[test]
fn codie_prepared_spell_cast_copy_of_card_clears_marker() {
    let mut state = GameState::new_two_player(7);
    let source = seed_targetless_paradigm_source(&mut state, 200, "Exiled Spell");
    state.objects.get_mut(&source).unwrap().prepared_copy_source = Some(ObjectId(999));
    let ability = ResolvedAbility::new(
        Effect::CastCopyOfCard {
            target: TargetFilter::None,
            cost: ManaCost::zero(),
            count: None,
        },
        vec![TargetRef::Object(source)],
        ObjectId(99),
        P0,
    );
    let mut events = Vec::new();

    engine::game::effects::cast_copy_of_card::resolve(&mut state, &ability, &mut events)
        .expect("casting a copy of the exiled card resolves");

    assert_eq!(state.stack.len(), 1, "the copy is on the stack");
    let copy_id = state.stack[0].id;
    assert_ne!(copy_id, source);
    let copy = &state.objects[&copy_id];
    assert_eq!(copy.zone, Zone::Stack);
    assert!(copy.cast_occurrence.is_some());
    assert_eq!(
        copy.prepared_copy_source, None,
        "CR 707.12: a cast copy of a card is not a prepare spell"
    );
    assert_eq!(
        state.objects[&source].prepared_copy_source,
        Some(ObjectId(999)),
        "the hostile source marker was present"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::SpellCast { object_id, .. } if *object_id == copy_id
    )));
    // CR 707.12: the cast-history record of this cast names the copy and
    // records no prepare designation, despite the hostile source marker.
    let record = state.spells_cast_this_turn_by_player[&P0]
        .last()
        .expect("casting a copy of a card is a real cast and is recorded");
    assert_eq!(record.spell_object_id, Some(copy_id));
    assert_eq!(record.prepared_copy_source, None);
    assert_eq!(prepare_readings(&state, record), ORDINARY_READINGS);
}

/// CR 601.2i + CR 722.3d + CR 707.10c: casting a prepared Swords to Plowshares
/// puts exactly one Codie trigger on the stack; it copies the spell, the copy is
/// retargeted, and both resolve. The copy (a prepare spell itself) is not cast
/// (CR 707.10), so it never re-triggers Codie.
#[test]
fn codie_prepared_spell_cast_copies_and_retargets() {
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        y,
        ..
    } = build_fixture(db(), FixtureSpec::swords(1));
    let codie = codies[0];
    let spell_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(x));
    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(
        triggers_from(runner.state(), codie),
        1,
        "exactly one Codie trigger for one prepared cast"
    );

    finish_codie_copy_and_retarget(&mut runner, codie, spell_id, x, y);
}

/// From `[spell (targeting x), Codie trigger]` with P0 holding priority: resolve
/// the trigger, retarget the copy to `y`, and resolve both spells. Asserts the
/// copy is made once from `spell_id`, the original keeps `x`, the copy targets
/// `y`, the copy does not retrigger Codie, and both Swords resolve (X and Y
/// exiled, each controller gains its creature's power). Returns the id of the
/// single copy (from its `SpellCopied` event).
fn finish_codie_copy_and_retarget(
    runner: &mut GameRunner,
    codie: ObjectId,
    spell_id: ObjectId,
    x: ObjectId,
    y: ObjectId,
) -> ObjectId {
    let mut events = pass_twice(runner);
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(target_slots.len(), 1);
            assert_eq!(target_slots[0].current, Some(TargetRef::Object(x)));
            assert!(target_slots[0]
                .legal_alternatives
                .contains(&TargetRef::Object(y)));
        }
        other => panic!("CR 707.10c: expected CopyRetarget, got {other:?}"),
    }
    events.extend(act_events(
        runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(y)),
        },
        "choose the copy's new target",
    ));
    assert_priority(runner, P0);

    let copied = spell_copied(&events);
    assert_eq!(copied.len(), 1, "exactly one SpellCopied");
    let (copy_id, original_id) = copied[0];
    assert_eq!(original_id, spell_id);
    assert_eq!(runner.state().stack.len(), 2);
    // The flattened chain repeats the target for Swords' "its controller gains
    // life" sub-ability, so compare the distinct targets.
    assert_eq!(
        distinct_stack_targets(runner.state(), spell_id),
        [TargetRef::Object(x)],
        "the original keeps its target"
    );
    // CR 707.10c: the copy's exile effect targets the newly chosen creature. (Its
    // "its controller gains life" sub-ability keeps the original's stored target
    // in the flattened chain; the life assertions below pin that it resolves
    // against the copy's new target.)
    let copy_root_targets = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .expect("the copy has its own stack entry");
    assert_eq!(copy_root_targets, [TargetRef::Object(y)]);
    let life_before: Vec<i32> = runner.state().players.iter().map(|p| p.life).collect();
    // CR 707.10: the copy matches PrepareSpell but is not cast, so no new trigger.
    assert!(matches_target_filter(
        runner.state(),
        copy_id,
        &prepare_spell_filter(),
        &FilterContext::from_source(runner.state(), codie),
    ));
    assert_eq!(triggers_from(runner.state(), codie), 0);

    runner.advance_until_stack_empty();
    let life_after: Vec<i32> = runner.state().players.iter().map(|p| p.life).collect();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&x].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&y].zone, Zone::Exile);
    // Swords exiles X (P0's 2/2): P0 gains 2. The copy exiles Y (P1's 3/3): P1
    // gains 3.
    assert_eq!(life_after[0] - life_before[0], 2, "P0 gains X's power");
    assert_eq!(life_after[1] - life_before[1], 3, "P1 gains Y's power");
    copy_id
}

/// Negative: an ordinary spell cast from hand is not a prepared spell.
#[test]
fn codie_prepared_spell_ignores_ordinary_cast_from_hand() {
    let mut spec = FixtureSpec::swords(1);
    spec.p0_pool = &[ManaType::Red];
    spec.shock_in_hand = true;
    let Fixture {
        mut runner,
        codies,
        x,
        shock,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];
    let shock = shock.expect("Shock requested");
    assert_codie_watches_prepared_spells(runner.state(), codie);

    let commit = runner.cast(shock).target_object(x).commit();
    let state = commit.state();
    // Reach guard: Shock really became a spell on the stack, unmarked.
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.objects[&shock].zone, Zone::Stack);
    assert_eq!(state.objects[&shock].prepared_copy_source, None);
    assert_eq!(
        triggers_from(state, codie),
        0,
        "Codie must not trigger on an ordinary spell"
    );
}

/// Negative (CR 722.2a): a preparation card cast from hand as its creature face
/// "has a prepare spell" but is not a spell cast as a prepare spell.
#[test]
fn codie_prepared_spell_ignores_preparation_card_cast_from_hand() {
    let mut spec = FixtureSpec::swords(1);
    spec.prepare = PrepareCard {
        zone: Zone::Hand,
        ..EMERITUS_P0
    };
    spec.p0_pool = &[ManaType::White, ManaType::White, ManaType::White];
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];
    assert_codie_watches_prepared_spells(runner.state(), codie);

    let commit = runner.cast(emeritus).commit();
    let state = commit.state();
    // Reach guard: the Emeritus creature spell is on the stack, carries its
    // prepare face, and no prepare marker.
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.objects[&emeritus].zone, Zone::Stack);
    assert!(state.objects[&emeritus].back_face.is_some());
    assert_eq!(state.objects[&emeritus].prepared_copy_source, None);
    assert_eq!(
        triggers_from(state, codie),
        0,
        "Codie must not trigger on a preparation card cast as its creature face"
    );
}

/// Negative (controller scope): an opponent's prepared cast does not trigger
/// Codie ("whenever YOU cast").
#[test]
fn codie_prepared_spell_ignores_opponent_prepared_cast() {
    let mut spec = FixtureSpec::swords(1);
    spec.prepare = PrepareCard {
        controller: P1,
        ..EMERITUS_P0
    };
    spec.p0_pool = &[];
    spec.p1_pool = &[ManaType::White];
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];

    set_prepared(&mut runner, emeritus);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert_priority(&runner, P1);
    runner
        .act(GameAction::CastPreparedCopy { source: emeritus })
        .expect("P1 starts the prepared cast");
    let spell_id = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.object_id,
        other => panic!("prepared Swords cast must pause for a target, got {other:?}"),
    };
    drive_cast_to_stack(&mut runner, Some(x));

    // Reach guard: P1's spell is a marked stack spell, so only controller
    // scope keeps Codie silent.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let spell = &runner.state().objects[&spell_id];
    assert_eq!(spell.zone, Zone::Stack);
    assert_eq!(spell.controller, P1);
    assert_eq!(spell.prepared_copy_source, Some(emeritus));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        triggers_from(runner.state(), codie),
        0,
        "Codie must not trigger on an opponent's prepared spell"
    );
}

/// CR 707.10c: an untargeted prepare spell is copied with no new-targets
/// prompt. Looped over two real untargeted prepare faces.
#[test]
fn codie_prepared_spell_untargeted_copy_has_no_retarget_prompt() {
    const UNTARGETED: [PrepareCard; 2] = [
        PrepareCard {
            name: "Goblin Glasswright",
            prepare_face: "Craft with Pride",
            controller: P0,
            zone: Zone::Battlefield,
        },
        PrepareCard {
            name: "Konstrari Improviser",
            prepare_face: "Soul Tether",
            controller: P0,
            zone: Zone::Battlefield,
        },
    ];
    for prepare in UNTARGETED {
        let card = prepare.name;
        let mut spec = FixtureSpec::swords(1);
        spec.prepare = prepare;
        spec.p0_pool = &[ManaType::Red, ManaType::Red, ManaType::Red];
        let Fixture {
            mut runner,
            codies,
            prepare_source,
            ..
        } = build_fixture(db(), spec);
        let codie = codies[0];
        assert_codie_watches_prepared_spells(runner.state(), codie);
        let artifact_tokens = |state: &GameState| {
            state
                .objects
                .values()
                .filter(|obj| {
                    obj.zone == Zone::Battlefield
                        && obj.controller == P0
                        && obj.is_token
                        && obj.card_types.core_types.contains(&CoreType::Artifact)
                })
                .count()
        };
        assert_eq!(artifact_tokens(runner.state()), 0, "{card}: no tokens yet");

        begin_prepared_untargeted_cast(&mut runner, prepare_source);
        drive_cast_to_stack(&mut runner, None);
        assert_priority(&runner, P0);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "{card}: spell + Codie trigger"
        );
        assert_eq!(triggers_from(runner.state(), codie), 1, "{card}");
        let spell_id = runner.state().stack[0].id;

        let events = pass_twice(&mut runner);
        assert_priority(&runner, P0);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "{card}: spell + copy, no retarget prompt"
        );
        let copied = spell_copied(&events);
        assert_eq!(copied.len(), 1, "{card}: exactly one SpellCopied");
        assert_eq!(copied[0].1, spell_id);
        assert_eq!(triggers_from(runner.state(), codie), 0, "{card}");

        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty(), "{card}");
        assert_eq!(
            artifact_tokens(runner.state()),
            2,
            "{card}: the original and the copy each create an artifact token"
        );
    }
}

/// Two Codies each copy the original once: two triggers, two copies, each with
/// `original_id` equal to the cast spell, and no copy re-triggers either Codie.
#[test]
fn codie_prepared_spell_two_codies_each_copy_original() {
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        ..
    } = build_fixture(db(), FixtureSpec::swords(2));
    let spell_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(x));
    // CR 603.3b: drain unconditionally before reading counts.
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 3);
    assert_eq!(codie_triggers(runner.state(), &codies), 2);
    assert_eq!(triggers_from(runner.state(), codies[0]), 1);
    assert_eq!(triggers_from(runner.state(), codies[1]), 1);

    // The top trigger resolves: CopyRetarget, keep the targets.
    let mut events = pass_twice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CopyRetarget { player: P0, .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    events.extend(act_events(
        &mut runner,
        GameAction::KeepAllCopyTargets,
        "keep the first copy's targets",
    ));
    assert_priority(&runner, P0);
    let first = spell_copied(&events);
    assert_eq!(first.len(), 1);
    let copy_one = first[0].0;
    // Reach guard, read at the copy's own creation: it is a marked stack spell.
    assert_eq!(runner.state().objects[&copy_one].zone, Zone::Stack);
    assert!(runner.state().objects[&copy_one]
        .prepared_copy_source
        .is_some());
    assert_eq!(
        codie_triggers(runner.state(), &codies),
        1,
        "only the second Codie trigger remains; the copy added none"
    );

    // Copy 1 resolves.
    pass_twice(&mut runner);
    assert_priority(&runner, P0);
    assert_eq!(codie_triggers(runner.state(), &codies), 1);

    // The remaining trigger resolves; the targeted copy opens its retarget
    // prompt (CR 707.10c), which keeps the targets.
    let mut events_two = pass_twice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CopyRetarget { player: P0, .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    events_two.extend(act_events(
        &mut runner,
        GameAction::KeepAllCopyTargets,
        "keep the second copy's targets",
    ));
    assert_priority(&runner, P0);
    let second = spell_copied(&events_two);
    assert_eq!(second.len(), 1);
    let copy_two = second[0].0;
    assert_eq!(runner.state().objects[&copy_two].zone, Zone::Stack);
    assert!(runner.state().objects[&copy_two]
        .prepared_copy_source
        .is_some());
    assert_eq!(codie_triggers(runner.state(), &codies), 0);

    assert_eq!(first[0].1, spell_id);
    assert_eq!(second[0].1, spell_id);
    assert_ne!(copy_one, copy_two);
}

/// CR 608.2h + CR 707.2: the original is countered in response; Codie's trigger
/// still copies it from its departed-spell record.
#[test]
fn codie_prepared_spell_trigger_copies_countered_original() {
    let mut spec = FixtureSpec::swords(1);
    spec.counterspell_for_p1 = true;
    spec.p1_pool = &[ManaType::Blue, ManaType::Blue];
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        counterspell,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];
    let counterspell = counterspell.expect("Counterspell requested");
    let spell_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(x));
    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(triggers_from(runner.state(), codie), 1);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to the Counterspell controller");
    assert_priority(&runner, P1);
    // `.commit()`, not `.resolve()`: stop with Counterspell on the stack.
    let commit = runner.cast(counterspell).target_object(spell_id).commit();
    assert_eq!(commit.state().stack.len(), 3);
    assert_eq!(triggers_from(commit.state(), codie), 1);

    for _ in 0..4 {
        if !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == counterspell)
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward Counterspell resolution");
    }
    // CR 701.6a: the prepared spell is countered; only Codie's trigger remains.
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(triggers_from(runner.state(), codie), 1);
    assert!(
        !runner.state().objects.contains_key(&spell_id),
        "CR 704.5e: the countered prepared copy ceases to exist"
    );
    assert!(
        runner.state().departed_stack_spells.contains_key(&spell_id),
        "the departed-spell record is the copy source"
    );

    let mut events = pass_twice(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(target_slots[0].current, Some(TargetRef::Object(x)));
        }
        other => panic!("expected CopyRetarget from the departed record, got {other:?}"),
    }
    events.extend(act_events(
        &mut runner,
        GameAction::KeepAllCopyTargets,
        "keep the copy's targets",
    ));
    assert_priority(&runner, P0);
    let copied = spell_copied(&events);
    assert_eq!(copied.len(), 1, "exactly one SpellCopied");
    let (copy_id, original_id) = copied[0];
    assert_eq!(original_id, spell_id);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].id, copy_id);
    assert_eq!(triggers_from(runner.state(), codie), 0);
    let copy = &runner.state().objects[&copy_id];
    assert_eq!(copy.zone, Zone::Stack);
    assert!(
        copy.prepared_copy_source.is_some(),
        "CR 722.3d: a copy of a prepare spell, made from the departed record, is a prepare spell"
    );
}

/// Exactly Codie's activation cost, {W}{U}{B}{R}{G}.
const WUBRG: &[ManaType] = &[
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
];

/// Codie's activation cost plus the {W} of Swords to Plowshares.
const WUBRG_AND_W: &[ManaType] = &[
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
    ManaType::White,
];

/// CR 602.2: activate `source`'s ability `ability_index` and answer every
/// prompt until priority returns. A `TargetSelection` is answered with the
/// first legal target so a run against a targeted parse keeps driving; callers
/// assert on the returned `WaitingFor` names, in visit order.
fn drive_activation(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
) -> Vec<&'static str> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("the activation must be accepted");
    let mut visited = Vec::new();
    for _ in 0..16 {
        visited.push(runner.state().waiting_for.variant_name());
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("answer the activation's target prompt");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay the activation cost from the pool");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => return visited,
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }
    panic!("the activation never returned to priority; visited {visited:?}");
}

/// P0 controls Codie, an unprepared Emeritus of Truce, an unprepared Konstrari
/// Improviser, a Goblin Glasswright that is already prepared, and a creature
/// with no prepare spell; P1 controls its own Emeritus of Truce. P0's pool pays
/// Codie's activation exactly.
struct MassFixture {
    runner: GameRunner,
    codie: ObjectId,
    emeritus_p0: ObjectId,
    konstrari_p0: ObjectId,
    glasswright_p0: ObjectId,
    faceless_p0: ObjectId,
    emeritus_p1: ObjectId,
}

fn build_mass_fixture(db: &CardDatabase) -> MassFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let emeritus_p0 = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let konstrari_p0 = scenario.add_real_card(P0, "Konstrari Improviser", Zone::Battlefield, db);
    let glasswright_p0 = scenario.add_real_card(P0, "Goblin Glasswright", Zone::Battlefield, db);
    let faceless_p0 = scenario.add_creature(P0, "Faceless Bystander", 2, 2).id();
    let emeritus_p1 = scenario.add_real_card(P1, "Emeritus of Truce", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, mana(WUBRG));

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    for id in [emeritus_p0, konstrari_p0, glasswright_p0, emeritus_p1] {
        assert!(
            runner.state().objects[&id].back_face.is_some(),
            "{} must hydrate its prepare face",
            runner.state().objects[&id].name
        );
    }
    assert!(runner.state().objects[&faceless_p0].back_face.is_none());
    // Konstrari Improviser "enters prepared" ("This creature enters prepared."),
    // and `add_real_card` moved it onto the battlefield through the zone
    // pipeline that applies that replacement. Unprepare it, which ceases that
    // copy, so Codie's activation is what prepares it here.
    assert!(runner.state().objects[&konstrari_p0].prepared.is_some());
    runner
        .act(GameAction::Debug(DebugAction::SetPrepared {
            object_id: konstrari_p0,
            prepared: false,
        }))
        .expect("Debug SetPrepared must unprepare the permanent");
    assert!(runner.state().objects[&konstrari_p0].prepared.is_none());
    assert!(linked_copies(runner.state(), konstrari_p0).is_empty());
    set_prepared(&mut runner, glasswright_p0);

    MassFixture {
        runner,
        codie,
        emeritus_p0,
        konstrari_p0,
        glasswright_p0,
        faceless_p0,
        emeritus_p1,
    }
}

/// CR 722.3c: every object in any zone linked to `source` as its prepare copy.
fn linked_copies(state: &GameState, source: ObjectId) -> Vec<ObjectId> {
    state
        .objects
        .values()
        .filter(|object| object.prepared_copy_source == Some(source))
        .map(|object| object.id)
        .collect()
}

/// The single object linked to `source`, asserted to be its exact CR 722.3c
/// copy: a non-token copy waiting in exile, owned and controlled by
/// `controller`, with the prepare face's name and face up (CR 406.3).
fn exact_linked_copy(
    state: &GameState,
    source: ObjectId,
    controller: PlayerId,
    prepare_face: &str,
) -> ObjectId {
    let linked = linked_copies(state, source);
    assert_eq!(
        linked.len(),
        1,
        "CR 722.3c: {source:?} must have exactly one linked copy, found {linked:?}"
    );
    let copy = &state.objects[&linked[0]];
    assert_eq!(copy.zone, Zone::Exile);
    assert!(state.exile.contains(&copy.id));
    assert!(copy.is_copy);
    assert!(!copy.is_token);
    assert_eq!(copy.owner, controller);
    assert_eq!(copy.controller, controller);
    assert_eq!(copy.name, prepare_face);
    assert!(!copy.face_down);
    copy.id
}

fn has_cast_prepared_copy(state: &GameState, source: ObjectId) -> bool {
    engine::ai_support::legal_actions(state)
        .iter()
        .any(|action| matches!(action, GameAction::CastPreparedCopy { source: s } if *s == source))
}

/// CR 115.10a + CR 722.3a: "{W}{U}{B}{R}{G}, {T}: Each creature you control
/// becomes prepared." names no target, so activation announces none; on
/// resolution each creature its controller controls that has a prepare spell
/// and is not already prepared becomes prepared. Codie (no prepare spell), a
/// faceless creature and the opponent's creature stay unprepared; the
/// already-prepared Glasswright gets no second designation.
#[test]
fn codie_activation_prepares_each_eligible_creature_without_targeting() {
    let MassFixture {
        mut runner,
        codie,
        emeritus_p0,
        konstrari_p0,
        glasswright_p0,
        faceless_p0,
        emeritus_p1,
    } = build_mass_fixture(db());
    assert!(
        runner.state().objects[&glasswright_p0].prepared.is_some(),
        "Glasswright starts prepared"
    );
    for id in [emeritus_p0, konstrari_p0, emeritus_p1, faceless_p0, codie] {
        assert!(runner.state().objects[&id].prepared.is_none());
    }
    // Baseline before the activation: the already-prepared Glasswright's copy.
    let glass_copy = exact_linked_copy(runner.state(), glasswright_p0, P0, "Craft with Pride");

    let visited = drive_activation(&mut runner, codie, 0);
    assert!(
        !visited.contains(&"TargetSelection"),
        "CR 115.10a: Codie's activation must not announce a target; visited {visited:?}"
    );
    assert_priority(&runner, P0);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert!(matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(entry.source_id, codie);
    let ability = entry
        .ability()
        .expect("the activated ability carries its resolved ability");
    assert!(
        engine::game::ability_utils::flatten_targets_in_chain(ability).is_empty(),
        "no target was chosen"
    );
    // CR 602.2b + CR 107.5: the cost is paid on activation.
    assert!(runner.state().objects[&codie].tapped);
    assert!(runner.state().players[0].mana_pool.mana.is_empty());

    // CR 117.4: both players pass and the ability resolves; priority returns to
    // P0 only after the state-based actions are checked.
    let events = pass_twice(&mut runner);
    assert_priority(&runner, P0);
    assert!(runner.state().stack.is_empty());
    let mut prepared_events: Vec<ObjectId> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::BecamePrepared { object_id } => Some(*object_id),
            _ => None,
        })
        .collect();
    prepared_events.sort_by_key(|id| id.0);
    let mut expected = vec![emeritus_p0, konstrari_p0];
    expected.sort_by_key(|id| id.0);
    assert_eq!(prepared_events, expected);
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: engine::types::ability::EffectKind::BecomePrepared,
            ..
        }
    )));

    assert!(runner.state().objects[&emeritus_p0].prepared.is_some());
    assert!(runner.state().objects[&konstrari_p0].prepared.is_some());
    // CR 722.3a: already prepared, so it can't gain the designation again (no
    // second `BecamePrepared` above).
    assert!(runner.state().objects[&glasswright_p0].prepared.is_some());
    for id in [emeritus_p1, faceless_p0, codie] {
        assert!(
            runner.state().objects[&id].prepared.is_none(),
            "{} must stay unprepared",
            runner.state().objects[&id].name
        );
    }

    // CR 722.3c + CR 704.5e: each copy the activation created survived the
    // state-based actions checked before priority returned. No castability is
    // asserted here: the pool is empty, so neither prepare spell is affordable.
    let copies_after_activation = |state: &GameState| {
        [
            exact_linked_copy(state, emeritus_p0, P0, "Swords to Plowshares"),
            exact_linked_copy(state, konstrari_p0, P0, "Soul Tether"),
            exact_linked_copy(state, glasswright_p0, P0, "Craft with Pride"),
        ]
    };
    let [emeritus_copy, konstrari_copy, glass_after] = copies_after_activation(runner.state());
    assert_eq!(
        glass_after, glass_copy,
        "no second Glasswright copy appears"
    );
    for id in [emeritus_p1, faceless_p0, codie] {
        assert!(linked_copies(runner.state(), id).is_empty());
    }
    // The fixture puts nothing else in exile.
    let mut exile: Vec<ObjectId> = runner.state().exile.iter().copied().collect();
    exile.sort_by_key(|id| id.0);
    let mut expected_exile = vec![glass_copy, emeritus_copy, konstrari_copy];
    expected_exile.sort_by_key(|id| id.0);
    assert_eq!(exile, expected_exile);

    // Idempotence: further state-based action checks keep the same copies.
    for _ in 0..2 {
        engine::game::sba::check_state_based_actions(runner.state_mut(), &mut Vec::new());
        assert_eq!(
            copies_after_activation(runner.state()),
            [emeritus_copy, konstrari_copy, glass_copy]
        );
    }
}

/// The full Codie flow, no Debug action: the activation prepares Emeritus of
/// Truce, its prepared Swords to Plowshares is cast (CR 722.3c), Codie's
/// trigger copies it (CR 722.3d + CR 707.10), the copy is retargeted
/// (CR 707.10c), and both resolve.
#[test]
fn codie_activation_then_prepared_cast_copies_and_retargets() {
    let mut spec = FixtureSpec::swords(1);
    spec.p0_pool = WUBRG_AND_W;
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        y,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];
    // Only the activation can prepare Emeritus in this test.
    assert!(runner.state().objects[&emeritus].prepared.is_none());

    let visited = drive_activation(&mut runner, codie, 0);
    assert!(
        !visited.contains(&"TargetSelection"),
        "CR 115.10a: Codie's activation must not announce a target; visited {visited:?}"
    );
    assert_priority(&runner, P0);
    pass_twice(&mut runner);
    assert_priority(&runner, P0);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().objects[&emeritus].prepared.is_some());
    let pool = &runner.state().players[0].mana_pool.mana;
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].color, ManaType::White);

    // CR 722.3c: the copy made at the activation is waiting in exile before
    // any cast, and it is castable for the {W} left in the pool.
    let persisted = exact_linked_copy(runner.state(), emeritus, P0, "Swords to Plowshares");
    assert!(has_cast_prepared_copy(runner.state(), emeritus));

    let spell_id = start_prepared_cast(&mut runner, emeritus);
    assert_eq!(
        spell_id, persisted,
        "CR 722.3c: the copy created when Emeritus became prepared is the object cast"
    );
    drive_cast_to_stack(&mut runner, Some(x));
    assert_priority(&runner, P0);
    {
        let state = runner.state();
        assert_eq!(state.stack.len(), 2);
        let spell_entry = state
            .stack
            .iter()
            .find(|entry| entry.id == persisted)
            .expect("the persisted copy is the spell on the stack");
        assert!(matches!(spell_entry.kind, StackEntryKind::Spell { .. }));
        assert_eq!(state.objects[&persisted].zone, Zone::Stack);
        assert_eq!(
            state.objects[&persisted].prepared_copy_source,
            Some(emeritus)
        );
        assert_eq!(
            triggers_from(state, codie),
            1,
            "CR 601.2i + CR 722.3d: casting the prepared spell triggers Codie"
        );
        assert!(state.players[0].mana_pool.mana.is_empty());
        assert!(
            state.objects[&emeritus].prepared.is_none(),
            "CR 722.3c + CR 601.2i: the permanent is unprepared as the spell becomes cast"
        );
        assert!(
            !state
                .objects
                .values()
                .any(|object| object.zone == Zone::Exile
                    && object.prepared_copy_source == Some(emeritus)),
            "no second copy is created in exile"
        );
        assert!(!has_cast_prepared_copy(state, emeritus));
        assert_eq!(state.spells_cast_this_turn_by_player[&P0].len(), 1);
    }

    finish_codie_copy_and_retarget(&mut runner, codie, spell_id, x, y);
    assert!(
        !runner
            .state()
            .objects
            .values()
            .any(|object| object.zone == Zone::Exile && object.prepared_copy_source.is_some()),
        "no linked copy remains in exile once the prepared spell was cast"
    );
}

/// CR 601.2i + CR 722.3c: backing out of the prepared cast at its target
/// prompt restores the designation and leaves the same copy waiting in exile,
/// through the state-based actions, for a later cast.
#[test]
fn codie_activation_then_cancelled_prepared_cast_keeps_the_same_copy() {
    let mut spec = FixtureSpec::swords(1);
    spec.p0_pool = WUBRG_AND_W;
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        ..
    } = build_fixture(db(), spec);
    let codie = codies[0];
    drive_activation(&mut runner, codie, 0);
    pass_twice(&mut runner);
    assert_priority(&runner, P0);
    let persisted = exact_linked_copy(runner.state(), emeritus, P0, "Swords to Plowshares");

    let announced = start_prepared_cast(&mut runner, emeritus);
    assert_eq!(announced, persisted);
    // Reach guard: the cast unprepared the permanent before the cancel.
    assert!(runner.state().objects[&emeritus].prepared.is_none());

    runner
        .act(GameAction::CancelCast)
        .expect("the prepared cast is cancellable at its target prompt");
    assert_priority(&runner, P0);
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut Vec::new());

    let state = runner.state();
    assert!(state.stack.is_empty());
    assert!(
        state.objects[&emeritus].prepared.is_some(),
        "CR 601.2i: the cancelled cast restores the prepared designation"
    );
    assert_eq!(
        exact_linked_copy(state, emeritus, P0, "Swords to Plowshares"),
        persisted,
        "the same copy waits in exile; none is recreated"
    );
    assert!(has_cast_prepared_copy(state, emeritus));
    assert_eq!(start_prepared_cast(&mut runner, emeritus), persisted);
}

/// The serialized shapes the protocol bump covers: Codie's activated ability
/// carries `scope: All` and its trigger's `valid_card` the `PrepareSpell` tag,
/// and both round-trip. A real card loaded from the fixture, whose stored AST
/// predates `scope`, reads the `Single` default.
#[test]
fn codie_serialized_shapes_carry_the_new_tags() {
    let db = db();
    let mut scenario = GameScenario::new();
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let tomekeeper = scenario.add_real_card(P0, "Biblioplex Tomekeeper", Zone::Battlefield, db);
    let runner = scenario.build();
    let state = runner.state();

    let ability = &state.objects[&codie].abilities[0];
    let json = serde_json::to_value(ability).expect("serialize Codie's activated ability");
    assert_eq!(json["effect"]["type"], "BecomePrepared");
    assert_eq!(json["effect"]["scope"]["type"], "All");
    let back: AbilityDefinition =
        serde_json::from_value(json).expect("Codie's activated ability round-trips");
    assert!(back == *ability, "the activated ability round-trips equal");

    let trigger = state.objects[&codie]
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition())
        .find(|definition| matches!(definition.mode, TriggerMode::SpellCast))
        .expect("Codie carries its SpellCast trigger");
    let json = serde_json::to_value(trigger).expect("serialize Codie's trigger");
    assert_eq!(json["valid_card"]["properties"][0]["type"], "PrepareSpell");
    let back: engine::types::ability::TriggerDefinition =
        serde_json::from_value(json).expect("Codie's trigger round-trips");
    assert_eq!(&back, trigger);

    let execute = state.objects[&tomekeeper]
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition())
        .find_map(|definition| definition.execute.as_deref())
        .expect("Biblioplex Tomekeeper carries its ETB trigger");
    assert_eq!(execute.mode_abilities.len(), 2);
    assert!(matches!(
        *execute.mode_abilities[0].effect,
        Effect::BecomePrepared {
            scope: engine::types::ability::EffectScope::Single,
            ..
        }
    ));
    assert!(matches!(
        *execute.mode_abilities[1].effect,
        Effect::BecomeUnprepared {
            scope: engine::types::ability::EffectScope::Single,
            ..
        }
    ));
}

/// P0 controls a prepared Emeritus of Truce (targeted Swords to Plowshares,
/// {W}) and a prepared Goblin Glasswright (untargeted Craft with Pride, a {R}
/// sorcery), a creature `x`; P1 controls a 3/3 `y`. P0 holds `shocks` copies of
/// the non-modal Shock ({R}) and a pool of exactly {W}{R}{R}. No Codie: the
/// cast-history ledger is read directly. `cast_limit`, when set, is a static on
/// a P0 creature.
struct LedgerFixture {
    runner: GameRunner,
    emeritus: ObjectId,
    glasswright: ObjectId,
    x: ObjectId,
    y: ObjectId,
    shocks: Vec<ObjectId>,
}

fn build_ledger_fixture(
    db: &CardDatabase,
    shocks: usize,
    cast_limit: Option<StaticMode>,
) -> LedgerFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let glasswright = scenario.add_real_card(P0, "Goblin Glasswright", Zone::Battlefield, db);
    let x = scenario.add_creature(P0, "Exile Target", 2, 2).id();
    let y = scenario.add_creature(P1, "Shock Target", 3, 3).id();
    if let Some(mode) = cast_limit {
        scenario
            .add_creature(P0, "Cast Limiter", 1, 1)
            .with_static(mode);
    }
    // Shock's printed {R}: the pool accounting below shows that only the cast
    // limit, never mana, keeps a Shock from being cast.
    let shocks: Vec<ObjectId> = (0..shocks)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Red],
                })
                .id()
        })
        .collect();
    scenario.with_mana_pool(P0, mana(&[ManaType::White, ManaType::Red, ManaType::Red]));

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    set_prepared(&mut runner, emeritus);
    set_prepared(&mut runner, glasswright);
    // Reach guard: each permanent is prepared with exactly one linked copy.
    exact_linked_copy(runner.state(), emeritus, P0, "Swords to Plowshares");
    exact_linked_copy(runner.state(), glasswright, P0, "Craft with Pride");

    LedgerFixture {
        runner,
        emeritus,
        glasswright,
        x,
        y,
        shocks,
    }
}

/// P0's cast-history records, in cast order.
fn p0_cast_records(state: &GameState) -> Vec<SpellCastRecord> {
    state
        .spells_cast_this_turn_by_player
        .get(&P0)
        .map(|records| records.iter().cloned().collect())
        .unwrap_or_default()
}

fn offers_cast_spell(state: &GameState, spell: ObjectId) -> bool {
    engine::ai_support::legal_actions(state).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    )
}

/// Cast the prepared Swords to Plowshares of `emeritus` at `target` and resolve
/// it (CR 117.4); returns the cast spell's id.
fn cast_and_resolve_prepared_swords(
    runner: &mut GameRunner,
    emeritus: ObjectId,
    target: ObjectId,
) -> ObjectId {
    let spell = start_prepared_cast(runner, emeritus);
    drive_cast_to_stack(runner, Some(target));
    assert_priority(runner, P0);
    assert_eq!(
        runner.state().objects[&spell].prepared_copy_source,
        Some(emeritus)
    );
    pass_twice(runner);
    assert_priority(runner, P0);
    assert!(runner.state().stack.is_empty());
    spell
}

/// Cast an ordinary Shock at `target` and resolve it.
fn cast_and_resolve_shock(runner: &mut GameRunner, shock: ObjectId, target: ObjectId) {
    {
        let commit = runner.cast(shock).target_object(target).commit();
        let state = commit.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(state.objects[&shock].zone, Zone::Stack);
        assert_eq!(state.objects[&shock].prepared_copy_source, None);
    }
    pass_twice(runner);
    assert_priority(runner, P0);
    assert!(runner.state().stack.is_empty());
}

/// CR 722.3d + CR 601.2i: each prepared cast records the prepare-spell
/// designation on the cast-history ledger and an ordinary cast records none, so
/// the positive, `TargetFilter::Not`, `FilterProp::Not` and `AnyOf` history
/// queries are exact per record and per quantity. Two prepared casts from two
/// different permanents surround an ordinary one.
#[test]
fn prepare_spell_designation_is_recorded_on_the_cast_ledger() {
    let LedgerFixture {
        mut runner,
        emeritus,
        glasswright,
        x,
        y,
        shocks,
    } = build_ledger_fixture(db(), 1, None);
    let shock = shocks[0];
    let glass_copy = exact_linked_copy(runner.state(), glasswright, P0, "Craft with Pride");

    let swords = cast_and_resolve_prepared_swords(&mut runner, emeritus, x);
    cast_and_resolve_shock(&mut runner, shock, y);
    // A sorcery-speed prepared cast, left on the stack.
    runner
        .act(GameAction::CastPreparedCopy {
            source: glasswright,
        })
        .expect("the prepared Craft with Pride is castable on an empty stack");
    drive_cast_to_stack(&mut runner, None);
    assert_priority(&runner, P0);

    let state = runner.state();
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.stack[0].id, glass_copy);
    assert!(state.players[0].mana_pool.mana.is_empty());
    let records = p0_cast_records(state);
    assert_eq!(
        records
            .iter()
            .map(|record| record.spell_object_id)
            .collect::<Vec<_>>(),
        [Some(swords), Some(shock), Some(glass_copy)],
        "one record per cast, in cast order"
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.prepared_copy_source)
            .collect::<Vec<_>>(),
        [Some(emeritus), None, Some(glasswright)],
        "CR 722.3d: each prepared cast names its own permanent; the ordinary cast names none"
    );
    assert_eq!(
        records
            .iter()
            .map(|record| prepare_readings(state, record))
            .collect::<Vec<_>>(),
        [PREPARED_READINGS, ORDINARY_READINGS, PREPARED_READINGS]
    );

    // CR 117.1: the same queries as a "spells you've cast this turn" count.
    assert_eq!(spells_cast_this_turn(state, emeritus, None), 3);
    assert_eq!(
        prepare_history_filters().map(|filter| spells_cast_this_turn(
            state,
            emeritus,
            Some(filter)
        )),
        [2, 1, 1, 2]
    );

    // Control for the inversion mechanism: a record without the designation
    // reads as non-prepare under both negations; one with it reads as prepare.
    assert_eq!(
        prepare_readings(state, &SpellCastRecord::default()),
        ORDINARY_READINGS
    );
    assert_eq!(
        prepare_readings(
            state,
            &SpellCastRecord {
                prepared_copy_source: Some(ObjectId(900)),
                ..Default::default()
            }
        ),
        PREPARED_READINGS
    );
}

/// CR 707.10 + CR 722.3d: Codie's copy of a prepared spell is a prepare spell
/// but is not cast, so the ledger holds exactly one prepare record for the one
/// prepared cast.
#[test]
fn codie_copy_of_a_prepared_spell_is_not_recorded_as_a_cast() {
    let Fixture {
        mut runner,
        codies,
        prepare_source: emeritus,
        x,
        y,
        ..
    } = build_fixture(db(), FixtureSpec::swords(1));
    let codie = codies[0];
    let spell_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(x));
    assert_priority(&runner, P0);
    assert_eq!(triggers_from(runner.state(), codie), 1);

    // Reach guard: the helper asserts exactly one `SpellCopied` from `spell_id`
    // and that the copy matched the prepared-spell filter on the stack.
    let copy_id = finish_codie_copy_and_retarget(&mut runner, codie, spell_id, x, y);
    assert_ne!(copy_id, spell_id);

    let state = runner.state();
    assert!(state.stack.is_empty());
    let records = p0_cast_records(state);
    assert_eq!(records.len(), 1, "CR 707.10: the copy is not cast");
    assert_eq!(records[0].spell_object_id, Some(spell_id));
    assert_eq!(records[0].prepared_copy_source, Some(emeritus));
    assert_eq!(prepare_readings(state, &records[0]), PREPARED_READINGS);
    assert_eq!(
        spells_cast_this_turn(state, emeritus, Some(prepare_spell_filter())),
        1
    );
    assert_eq!(spells_cast_this_turn(state, emeritus, None), 1);
}

/// A typed `PerTurnCastLimit` (CR 101.2 + CR 604.1) that allows each player one
/// spell per turn that is not a prepare spell (no printed card; it drives the
/// cast-limit consumer of the history filter): a prepared cast does not use up
/// the allowance, an ordinary cast does.
fn non_prepare_cast_limit() -> StaticMode {
    StaticMode::PerTurnCastLimit {
        who: ProhibitionScope::AllPlayers,
        max: 1,
        spell_filter: Some(TargetFilter::Not {
            filter: Box::new(prepare_spell_filter()),
        }),
    }
}

/// History side: after a prepared cast, an ordinary Shock is still castable
/// (the prepared record is not counted as non-prepare); after one ordinary
/// cast, a second affordable Shock is blocked.
#[test]
fn negated_prepare_cast_limit_does_not_count_a_prepared_cast() {
    let LedgerFixture {
        mut runner,
        emeritus,
        x,
        y,
        shocks,
        ..
    } = build_ledger_fixture(db(), 2, Some(non_prepare_cast_limit()));
    let (shock, second_shock) = (shocks[0], shocks[1]);
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        shock
    ));

    cast_and_resolve_prepared_swords(&mut runner, emeritus, x);
    let records = p0_cast_records(runner.state());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].prepared_copy_source, Some(emeritus));
    // Discriminating: the prepared record is not a non-prepare cast.
    assert!(
        engine::game::casting::can_cast_object_now(runner.state(), P0, shock),
        "a prepared cast must not use up the non-prepare allowance"
    );
    assert!(offers_cast_spell(runner.state(), shock));

    cast_and_resolve_shock(&mut runner, shock, y);
    // The limit instrument works: one Red remains, so only the limit blocks
    // the second Shock.
    let pool = &runner.state().players[0].mana_pool.mana;
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].color, ManaType::Red);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        second_shock
    ));
    assert!(!offers_cast_spell(runner.state(), second_shock));
}

/// Candidate side: after an ordinary cast used up the non-prepare allowance,
/// the prepared Swords (projected from the linked copy still waiting in exile)
/// is still castable, and its cast is recorded as a prepare spell.
#[test]
fn negated_prepare_cast_limit_allows_the_prepared_candidate() {
    let LedgerFixture {
        mut runner,
        emeritus,
        x,
        y,
        shocks,
        ..
    } = build_ledger_fixture(db(), 2, Some(non_prepare_cast_limit()));
    let (shock, second_shock) = (shocks[0], shocks[1]);

    cast_and_resolve_shock(&mut runner, shock, y);
    // The limit instrument works: {W}{R} remain, so only the limit blocks the
    // second Shock.
    assert_eq!(runner.state().players[0].mana_pool.mana.len(), 2);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        second_shock
    ));

    // Discriminating: the prepared candidate is a prepare spell.
    assert!(
        has_cast_prepared_copy(runner.state(), emeritus),
        "the prepared Swords must stay castable under a non-prepare limit"
    );
    cast_and_resolve_prepared_swords(&mut runner, emeritus, x);
    let records = p0_cast_records(runner.state());
    assert_eq!(
        records
            .iter()
            .map(|record| record.prepared_copy_source)
            .collect::<Vec<_>>(),
        [None, Some(emeritus)]
    );
}

/// "Whenever an opponent casts a prepared spell": Codie's "a prepared spell"
/// designation on the opponent-caster branch (`<who> casts a ...`). No printed
/// card carries this trigger; the watcher drives that branch's designation peel
/// through the live cast-trigger matcher.
const OPPONENT_PREPARED_WATCHER_ORACLE: &str =
    "Whenever an opponent casts a prepared spell, you gain 1 life.";

/// P0 controls the opponent-prepared-spell watcher and a 2/2 `x`;
/// `prepare_owner` controls Emeritus of Truce (targeted Swords to Plowshares,
/// {W}); P1 controls a 3/3 `y` and holds a {R} Shock.
struct WatcherFixture {
    runner: GameRunner,
    watcher: ObjectId,
    emeritus: ObjectId,
    x: ObjectId,
    y: ObjectId,
    shock: ObjectId,
}

fn build_watcher_fixture(
    db: &CardDatabase,
    prepare_owner: PlayerId,
    p0_pool: &[ManaType],
    p1_pool: &[ManaType],
) -> WatcherFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_from_oracle(
            P0,
            "Prepared Watcher",
            1,
            1,
            OPPONENT_PREPARED_WATCHER_ORACLE,
        )
        .id();
    let emeritus =
        scenario.add_real_card(prepare_owner, "Emeritus of Truce", Zone::Battlefield, db);
    let x = scenario.add_creature(P0, "Exile Target", 2, 2).id();
    let y = scenario.add_creature(P1, "Retarget Target", 3, 3).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, SHOCK_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    if !p0_pool.is_empty() {
        scenario.with_mana_pool(P0, mana(p0_pool));
    }
    if !p1_pool.is_empty() {
        scenario.with_mana_pool(P1, mana(p1_pool));
    }

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    WatcherFixture {
        runner,
        watcher,
        emeritus,
        x,
        y,
        shock,
    }
}

/// Parse reach guard: the watcher carries a SpellCast trigger scoped to an
/// opponent caster and narrowed to prepared spells (CR 722.3d).
fn assert_watches_opponent_prepared_spells(state: &GameState, watcher: ObjectId) {
    let opponent_caster = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::Opponent),
    ));
    let watches = state.objects[&watcher]
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition())
        .any(|definition| {
            matches!(definition.mode, TriggerMode::SpellCast)
                && definition.valid_target == opponent_caster
                && matches!(
                    &definition.valid_card,
                    Some(TargetFilter::Typed(typed))
                        if typed.properties.contains(&FilterProp::PrepareSpell)
                )
        });
    assert!(
        watches,
        "the watcher must carry its opponent-scoped SpellCast trigger narrowed to prepared spells"
    );
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// Positive (CR 722.3d + CR 603.2): an opponent's prepared cast triggers the
/// watcher exactly once, and the trigger resolves for the watcher's controller.
#[test]
fn opponent_prepared_spell_watcher_fires_on_opponent_prepared_cast() {
    let WatcherFixture {
        mut runner,
        watcher,
        emeritus,
        y,
        ..
    } = build_watcher_fixture(db(), P1, &[], &[ManaType::White]);
    assert_watches_opponent_prepared_spells(runner.state(), watcher);
    let p0_life = life(runner.state(), P0);
    let p1_life = life(runner.state(), P1);

    set_prepared(&mut runner, emeritus);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert_priority(&runner, P1);
    // P1 aims its Swords at its own creature so the "its controller gains life"
    // rider can never touch P0's life total.
    let spell = start_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(y));

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.objects[&spell].zone, Zone::Stack);
    assert_eq!(state.objects[&spell].controller, P1);
    assert_eq!(state.objects[&spell].prepared_copy_source, Some(emeritus));
    assert_eq!(state.stack.len(), 2);
    assert_eq!(
        triggers_from(state, watcher),
        1,
        "an opponent's prepared spell must trigger the watcher once"
    );

    // P1 and P0 pass: the watcher's trigger (top of the stack) resolves.
    pass_twice(&mut runner);
    let state = runner.state();
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.stack[0].id, spell);
    assert_eq!(life(state, P0), p0_life + 1);
    assert_eq!(life(state, P1), p1_life);
}

/// Paired negative (the discriminating leg): an opponent's ordinary spell cast
/// from hand is not a prepared spell, so the watcher stays silent.
#[test]
fn opponent_prepared_spell_watcher_ignores_opponent_ordinary_cast() {
    let WatcherFixture {
        mut runner,
        watcher,
        x,
        shock,
        ..
    } = build_watcher_fixture(db(), P1, &[], &[ManaType::Red]);
    let p0_life = life(runner.state(), P0);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert_priority(&runner, P1);
    {
        let commit = runner.cast(shock).target_object(x).commit();
        let state = commit.state();
        // Reach guard: P1's Shock really became an unmarked spell on the stack.
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.objects[&shock].zone, Zone::Stack);
        assert_eq!(state.objects[&shock].controller, P1);
        assert_eq!(state.objects[&shock].prepared_copy_source, None);
        assert_eq!(
            triggers_from(state, watcher),
            0,
            "the watcher must not trigger on an opponent's ordinary spell"
        );
        assert_eq!(state.stack.len(), 1);
    }
    // Checked after the runtime negative so that a dropped designation reports
    // the false trigger first; the test needs both to pass.
    assert_watches_opponent_prepared_spells(runner.state(), watcher);

    pass_twice(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(life(runner.state(), P0), p0_life);
}

/// Paired negative (caster scope): the watcher's controller casting its own
/// prepared spell is not an opponent's cast.
#[test]
fn opponent_prepared_spell_watcher_ignores_own_prepared_cast() {
    let WatcherFixture {
        mut runner,
        watcher,
        emeritus,
        y,
        ..
    } = build_watcher_fixture(db(), P0, &[ManaType::White], &[]);
    assert_watches_opponent_prepared_spells(runner.state(), watcher);
    let p0_life = life(runner.state(), P0);

    let spell = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, Some(y));
    assert_priority(&runner, P0);

    // Reach guard: P0's spell is a marked prepared spell on the stack, so only
    // the opponent caster scope keeps the watcher silent.
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Stack);
    assert_eq!(state.objects[&spell].controller, P0);
    assert_eq!(state.objects[&spell].prepared_copy_source, Some(emeritus));
    assert_eq!(state.stack.len(), 1);
    assert_eq!(
        triggers_from(state, watcher),
        0,
        "the watcher must not trigger on its controller's prepared spell"
    );

    // Swords resolves: P1 (y's controller) gains life, P0 does not.
    pass_twice(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(life(runner.state(), P0), p0_life);
}

// ---------------------------------------------------------------------------
// CR 108.2 + CR 109.1 + CR 722.3c: the retained prepare copy is a copy of a
// card, not a card. It remains in exile while its permanent stays prepared
// (the CR 704.5e exception), and every query for CARDS in exile must not see
// it, while object-level exile enumeration (casting it, the CR 800.4a sweep)
// still does.
// ---------------------------------------------------------------------------

/// Verbatim Oracle text of Crackling Drake.
const CRACKLING_DRAKE_ORACLE: &str = "Flying\nCrackling Drake's power is equal to the total number of instant and sorcery cards you own in exile and in your graveyard.\nWhen this creature enters, draw a card.";

/// Verbatim Oracle text of Ketramose, the New Dawn.
const KETRAMOSE_ORACLE: &str = "Menace, lifelink, indestructible\nKetramose can't attack or block unless there are seven or more cards in exile.\nWhenever one or more cards are put into exile from graveyards and/or the battlefield during your turn, you draw a card and lose 1 life.";

/// Whether `expr` reads a count of cards in exile.
fn reads_exile_card_count(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Exile,
                    ..
                },
        } => true,
        QuantityExpr::Sum { exprs } => exprs.iter().any(reads_exile_card_count),
        _ => false,
    }
}

fn power(runner: &mut GameRunner, id: ObjectId) -> Option<i32> {
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner.state().objects[&id].power
}

fn exile_ids(state: &GameState) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state.exile.iter().copied().collect();
    ids.sort_by_key(|id| id.0);
    ids
}

/// P0 controls Codie, an unprepared Emeritus of Truce and Crackling Drake
/// (built from its verbatim Oracle text). Real instants: P0's Lightning Bolt in
/// exile, P0's Shock in the graveyard, P1's Opt in exile, and P0's Counterspell
/// in hand. P0's pool pays Codie's activation exactly.
struct DrakeBoard {
    runner: GameRunner,
    codie: ObjectId,
    emeritus: ObjectId,
    drake: ObjectId,
    bolt: ObjectId,
    opt: ObjectId,
    counterspell: ObjectId,
}

fn build_drake_board(db: &CardDatabase) -> DrakeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let drake = scenario
        .add_creature_from_oracle(P0, "Crackling Drake", 0, 4, CRACKLING_DRAKE_ORACLE)
        .id();
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Exile, db);
    scenario.add_real_card(P0, "Shock", Zone::Graveyard, db);
    let opt = scenario.add_real_card(P1, "Opt", Zone::Exile, db);
    let counterspell = scenario.add_real_card(P0, "Counterspell", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(WUBRG));

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_eq!(
        runner.state().objects[&emeritus]
            .back_face
            .as_ref()
            .map(|back| back.name.as_str()),
        Some("Swords to Plowshares"),
        "Emeritus of Truce must hydrate its prepare face"
    );
    DrakeBoard {
        runner,
        codie,
        emeritus,
        drake,
        bolt,
        opt,
        counterspell,
    }
}

/// Codie's activation prepares Emeritus of Truce; returns the exact linked
/// Swords to Plowshares copy once priority returns with an empty stack.
fn activate_codie_preparing_emeritus(board: &mut DrakeBoard) -> ObjectId {
    let visited = drive_activation(&mut board.runner, board.codie, 0);
    assert!(
        !visited.contains(&"TargetSelection"),
        "CR 115.10a: Codie's activation announces no target; visited {visited:?}"
    );
    // CR 117.4: both players pass and the activated ability resolves.
    pass_twice(&mut board.runner);
    assert_priority(&board.runner, P0);
    let state = board.runner.state();
    assert!(state.stack.is_empty());
    assert!(state.objects[&board.codie].tapped);
    assert!(state.players[0].mana_pool.mana.is_empty());
    assert!(state.objects[&board.emeritus].prepared.is_some());
    exact_linked_copy(state, board.emeritus, P0, "Swords to Plowshares")
}

/// The maintainer's discriminating regression. Crackling Drake's power is "the
/// total number of instant and sorcery cards you own in exile and in your
/// graveyard". Codie's activation prepares Emeritus of Truce, whose CR 722.3c
/// Swords to Plowshares copy (an Instant owned by P0) remains in exile. That
/// copy is a copy of a card, not a card (CR 108.2 + CR 109.1), so the Drake
/// still counts only Lightning Bolt in exile and Shock in the graveyard.
#[test]
fn exile_card_population_crackling_drake_ignores_the_retained_prepare_copy() {
    let mut board = build_drake_board(db());
    let drake = board.drake;

    // Reach guards: the Drake's parsed CDA reads cards in exile, and the board
    // holds exactly Bolt (P0's) and Opt (P1's) in exile.
    let reads_exile = board.runner.state().objects[&drake]
        .static_definitions
        .as_slice()
        .iter()
        .flat_map(|definition| definition.modifications.iter())
        .any(|modification| {
            matches!(
                modification,
                ContinuousModification::SetDynamicPower { value } if reads_exile_card_count(value)
            )
        });
    assert!(
        reads_exile,
        "Crackling Drake must parse its power CDA over cards in exile"
    );
    assert_eq!(
        exile_ids(board.runner.state()),
        exile_ids_of(&[board.bolt, board.opt])
    );
    // Bolt (exile) + Shock (graveyard); P1's Opt is not P0's.
    assert_eq!(power(&mut board.runner, drake), Some(2));

    let copy = activate_codie_preparing_emeritus(&mut board);
    let state = board.runner.state();
    assert!(state.objects[&copy]
        .card_types
        .core_types
        .contains(&CoreType::Instant));
    // Positive reach guard: a non-card Instant owned by P0 now sits in exile.
    assert_eq!(
        exile_ids(state),
        exile_ids_of(&[board.bolt, board.opt, copy])
    );

    // CR 108.2 + CR 109.1: the copy is not a card; the Drake still reads 2.
    assert_eq!(
        power(&mut board.runner, drake),
        Some(2),
        "the retained Swords copy must not count as an instant card in exile"
    );

    // Idempotence: further state-based action checks keep the copy (the
    // CR 722.3c retention is preserved) and the reading.
    for _ in 0..2 {
        engine::game::sba::check_state_based_actions(board.runner.state_mut(), &mut Vec::new());
        assert_eq!(
            exact_linked_copy(
                board.runner.state(),
                board.emeritus,
                P0,
                "Swords to Plowshares"
            ),
            copy
        );
        assert_eq!(power(&mut board.runner, drake), Some(2));
    }

    // Paired positive: a real instant card P0 owns entering exile counts.
    engine::game::zones::move_to_zone(
        board.runner.state_mut(),
        board.counterspell,
        Zone::Exile,
        &mut Vec::new(),
    );
    assert!(board.runner.state().exile.contains(&board.counterspell));
    assert_eq!(power(&mut board.runner, drake), Some(3));
}

fn exile_ids_of(ids: &[ObjectId]) -> Vec<ObjectId> {
    let mut ids = ids.to_vec();
    ids.sort_by_key(|id| id.0);
    ids
}

/// Ketramose's "unless there are seven or more cards in exile" counts every
/// card in exile, regardless of owner. Six real cards plus two retained
/// prepare copies (one per player) are SIX cards: Ketramose can't attack; the
/// seventh real card lets it attack.
#[test]
fn exile_card_population_unfiltered_all_scope_count() {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ketramose = scenario
        .add_creature(P0, "Ketramose, the New Dawn", 4, 4)
        .from_oracle_text(KETRAMOSE_ORACLE)
        .id();
    let emeritus_p0 = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let emeritus_p1 = scenario.add_real_card(P1, "Emeritus of Truce", Zone::Battlefield, db);
    for name in ["Shock", "Opt", "Lightning Bolt"] {
        scenario.add_real_card(P0, name, Zone::Exile, db);
    }
    for name in ["Counterspell", "Grizzly Bears", "Bear Cub"] {
        scenario.add_real_card(P1, name, Zone::Exile, db);
    }
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Reach guard: the parsed restriction reads every card in exile.
    let count = runner.state().objects[&ketramose]
        .static_definitions
        .as_slice()
        .iter()
        .find_map(
            |definition| match (&definition.mode, &definition.condition) {
                (StaticMode::CantAttackOrBlock, Some(StaticCondition::Not { condition })) => {
                    match condition.as_ref() {
                        StaticCondition::QuantityComparison { lhs, .. } => Some(lhs.clone()),
                        _ => None,
                    }
                }
                _ => None,
            },
        )
        .expect("Ketramose must parse its exile-count attack restriction");
    assert!(matches!(
        &count,
        QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Exile,
                card_types,
                filter: None,
                scope: CountScope::All,
            },
        } if card_types.is_empty()
    ));

    set_prepared(&mut runner, emeritus_p0);
    set_prepared(&mut runner, emeritus_p1);
    // Two retained copies of two owners (multi-authority).
    exact_linked_copy(runner.state(), emeritus_p0, P0, "Swords to Plowshares");
    exact_linked_copy(runner.state(), emeritus_p1, P1, "Swords to Plowshares");
    assert_eq!(runner.state().exile.len(), 8);

    let scoped = |scope: CountScope| QuantityExpr::Ref {
        qty: QuantityRef::ZoneCardCount {
            zone: ZoneRef::Exile,
            card_types: Vec::new(),
            filter: None,
            scope,
        },
    };
    let read = |runner: &GameRunner, expr: &QuantityExpr| {
        engine::game::quantity::resolve_quantity(runner.state(), expr, P0, ketramose)
    };
    let can_attack = |runner: &mut GameRunner| {
        engine::game::layers::evaluate_layers(runner.state_mut());
        engine::game::combat::validate_attackers(runner.state(), &[ketramose]).is_ok()
    };

    // Six real cards; the copies are not cards (CR 108.2 + CR 109.1).
    assert_eq!(read(&runner, &count), 6);
    assert_eq!(read(&runner, &scoped(CountScope::Opponents)), 3);
    assert_eq!(read(&runner, &scoped(CountScope::Owner)), 3);
    assert!(
        !can_attack(&mut runner),
        "six cards in exile: Ketramose can't attack"
    );

    // Paired positive: a seventh real card enables the attack.
    engine::game::zones::move_to_zone(runner.state_mut(), divination, Zone::Exile, &mut Vec::new());
    assert_eq!(read(&runner, &count), 7);
    assert_eq!(read(&runner, &scoped(CountScope::Owner)), 4);
    assert!(
        can_attack(&mut runner),
        "seven cards in exile: Ketramose can attack"
    );
}

/// The object-level contract the maintainer asked to keep: the retained copy
/// is still an object in exile that can be cast through its CR 722.3c
/// permission after every card-population change.
#[test]
fn exile_card_population_object_level_cast_is_preserved() {
    let mut board = build_drake_board(db());
    let copy = activate_codie_preparing_emeritus(&mut board);
    assert!(board.runner.state().exile.contains(&copy));
    board.runner.state_mut().players[0]
        .mana_pool
        .add(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    assert!(
        has_cast_prepared_copy(board.runner.state(), board.emeritus),
        "CR 722.3c: the prepared Swords copy stays castable from exile"
    );
}

/// CR 800.4a consumer-contract regression (not revert-discriminating: the copy
/// would also cease once its permanent left). The leave-the-game sweep reads
/// every object a departing player owns: P0's real card stays in exile owned
/// by P0, P0's prepared permanent leaves the battlefield, the CR 722.3c
/// retention lapses, and the copy ceases to exist (CR 704.5e).
#[test]
fn exile_card_population_leave_the_game_sweep_still_reaches_objects() {
    let db = db();
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::free_for_all(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Exile, db);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    set_prepared(&mut runner, emeritus);
    // Paired positive: before the departure the copy exists and is linked.
    let copy = exact_linked_copy(runner.state(), emeritus, P0, "Swords to Plowshares");

    let state = runner.state_mut();
    engine::game::elimination::eliminate_player(state, P0, &mut Vec::new());
    engine::game::sba::check_state_based_actions(state, &mut Vec::new());

    assert!(state.exile.contains(&bolt));
    assert_eq!(state.objects[&bolt].owner, P0);
    assert!(!state.battlefield.contains(&emeritus));
    assert!(
        !state.exile.contains(&copy),
        "the copy left exile with its departed owner's other objects"
    );
    assert!(linked_copies(state, emeritus)
        .iter()
        .all(|id| state.objects[id].zone != Zone::Exile));
}

// ---------------------------------------------------------------------------
// The prepare spell is a copiable value (CR 722.2b). Croaking Counterpart
// ("Create a token that's a copy of target non-Frog creature, except it's a
// 1/1 green Frog.") copying a preparation creature makes a token with the same
// prepare spell, so Codie's activation prepares the token and its CR 722.3c
// copy has only the prepare spell's characteristics (the CR 722.3c example).
// ---------------------------------------------------------------------------

/// A real card on the battlefield and the prepare face it must hydrate.
struct CounterpartSource {
    controller: PlayerId,
    name: &'static str,
    prepare_face: Option<&'static str>,
}

/// P0 controls Codie and a non-flying bystander; each `sources` card is on its
/// controller's battlefield; Croaking Counterpart is in P0's hand and P0's
/// pool pays its {1}{G}{U} exactly.
struct CounterpartBoard {
    runner: GameRunner,
    codie: ObjectId,
    bystander: ObjectId,
    sources: Vec<ObjectId>,
    counterpart: ObjectId,
}

fn build_counterpart_board(db: &CardDatabase, sources: &[CounterpartSource]) -> CounterpartBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let bystander = scenario.add_creature(P0, "Ground Bystander", 2, 2).id();
    let source_ids: Vec<ObjectId> = sources
        .iter()
        .map(|source| scenario.add_real_card(source.controller, source.name, Zone::Battlefield, db))
        .collect();
    let counterpart = scenario.add_real_card(P0, "Croaking Counterpart", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(&[ManaType::Green, ManaType::Blue, ManaType::Red]));

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // CR 205.3m: seed the creature-type vocabulary from the card corpus, as a
    // production game load does, so "it's a 1/1 green Frog" can replace the
    // copied creature types.
    runner.state_mut().all_creature_types = db.creature_type_vocabulary().to_vec();
    for (source, id) in sources.iter().zip(&source_ids) {
        let back = runner.state().objects[id].back_face.as_ref();
        assert_eq!(
            back.map(|face| face.name.as_str()),
            source.prepare_face,
            "{} must hydrate exactly its printed prepare face",
            source.name
        );
        if source.prepare_face.is_some() {
            assert_eq!(
                back.and_then(|face| face.layout_kind),
                Some(engine::types::card::LayoutKind::Prepare)
            );
        }
    }
    CounterpartBoard {
        runner,
        codie,
        bystander,
        sources: source_ids,
        counterpart,
    }
}

fn add_p0_mana(runner: &mut GameRunner, colors: &[ManaType]) {
    for color in colors {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            *color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Casts Croaking Counterpart targeting `target` and returns the single token
/// it created, asserting the copy exceptions (CR 707.9b) and the token's owner
/// and controller (CR 111.2). `etb_players` answers a player target of the
/// token's own enters trigger, if the copied creature has one.
fn cast_counterpart(
    board: &mut CounterpartBoard,
    target: ObjectId,
    etb_players: &[PlayerId],
) -> ObjectId {
    let name = board.runner.state().objects[&target].name.clone();
    let outcome = board
        .runner
        .cast(board.counterpart)
        .target_object(target)
        .target_players(etb_players)
        .resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { player } if *player == P0
    ));
    let state = board.runner.state();
    assert!(state.stack.is_empty());
    assert!(state.players[0].mana_pool.mana.is_empty());
    let tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].name == name)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "Croaking Counterpart creates one token copy"
    );
    let token = &state.objects[&tokens[0]];
    // CR 111.2: the player who creates a token owns and controls it.
    assert_eq!(token.owner, P0);
    assert_eq!(token.controller, P0);
    // CR 707.9b: "except it's a 1/1 green Frog".
    assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
    assert_eq!(token.color, vec![engine::types::mana::ManaColor::Green]);
    assert_eq!(token.card_types.subtypes, vec!["Frog"]);
    tokens[0]
}

/// CR 115.10a + CR 117.4: Codie's untargeted activation, paid from a fresh
/// {W}{U}{B}{R}{G}, resolves and priority returns to P0 with an empty stack.
fn activate_codie(runner: &mut GameRunner, codie: ObjectId) -> Vec<GameEvent> {
    add_p0_mana(runner, WUBRG);
    let visited = drive_activation(runner, codie, 0);
    assert!(
        !visited.contains(&"TargetSelection"),
        "CR 115.10a: Codie's activation announces no target; visited {visited:?}"
    );
    let events = pass_twice(runner);
    assert_priority(runner, P0);
    assert!(runner.state().stack.is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: engine::types::ability::EffectKind::BecomePrepared,
            ..
        }
    )));
    events
}

/// The board after Codie's activation prepared both the Counterpart token and
/// its uncopied source.
struct PreparedCounterpart {
    board: CounterpartBoard,
    token: ObjectId,
    token_copy: ObjectId,
    source_copy: ObjectId,
}

/// Counterpart copies a P0 preparation creature, then Codie's activation
/// prepares the token AND the uncopied source (the paired control). Each has
/// exactly one linked CR 722.3c copy in exile, which has only the prepare
/// spell's characteristics: a one-pip `cost` instant of colour `color`, never
/// the token's 1/1 green Frog exceptions (the CR 722.3c example).
fn counterpart_token_prepared_by_codie(
    source_name: &'static str,
    prepare_face: &'static str,
    cost: ManaCostShard,
    color: engine::types::mana::ManaColor,
    etb_players: &[PlayerId],
) -> PreparedCounterpart {
    let mut board = build_counterpart_board(
        db(),
        &[CounterpartSource {
            controller: P0,
            name: source_name,
            prepare_face: Some(prepare_face),
        }],
    );
    let source = board.sources[0];
    let token = cast_counterpart(&mut board, source, etb_players);
    {
        let state = board.runner.state();
        // CR 722.2b: the token's copiable values include the prepare spell.
        let back = state.objects[&token]
            .back_face
            .as_ref()
            .expect("CR 722.2b: the token copy has the source's prepare spell");
        assert_eq!(back.name, prepare_face);
        assert_eq!(
            back.layout_kind,
            Some(engine::types::card::LayoutKind::Prepare)
        );
        assert_eq!(
            Some(back),
            state.objects[&source].back_face.as_ref(),
            "the token's prepare spell is the source's"
        );
        // CR 707.2: the copy does not gain the designation by being created.
        assert!(state.objects[&token].prepared.is_none());
        assert!(state.objects[&source].prepared.is_none());
        assert!(linked_copies(state, token).is_empty());
    }

    let events = activate_codie(&mut board.runner, board.codie);
    let state = board.runner.state();
    let mut prepared: Vec<ObjectId> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::BecamePrepared { object_id } => Some(*object_id),
            _ => None,
        })
        .collect();
    prepared.sort_by_key(|id| id.0);
    let mut expected = vec![source, token];
    expected.sort_by_key(|id| id.0);
    assert_eq!(prepared, expected);
    assert!(state.objects[&token].prepared.is_some());
    assert!(state.objects[&source].prepared.is_some());
    assert!(state.objects[&board.codie].prepared.is_none());
    assert!(state.objects[&board.bystander].prepared.is_none());

    let token_copy = exact_linked_copy(state, token, P0, prepare_face);
    let source_copy = exact_linked_copy(state, source, P0, prepare_face);
    assert_ne!(token_copy, source_copy);
    assert_eq!(
        exile_ids(state),
        exile_ids_of(&[token_copy, source_copy]),
        "one retained copy per prepared permanent and nothing else in exile"
    );

    // CR 722.3c: the token's copy has only the prepare spell's characteristics.
    let copy = &state.objects[&token_copy];
    assert_eq!(copy.card_types.core_types, vec![CoreType::Instant]);
    assert!(copy.card_types.subtypes.is_empty(), "not a Frog");
    assert_eq!(copy.color, vec![color], "not green");
    assert_eq!((copy.power, copy.toughness), (None, None), "not a 1/1");
    assert_eq!(
        copy.mana_cost,
        ManaCost::Cost {
            generic: 0,
            shards: vec![cost],
        }
    );
    // The paired uncopied control's copy agrees on every characteristic.
    let control = &state.objects[&source_copy];
    assert_eq!(copy.name, control.name);
    assert_eq!(copy.card_types, control.card_types);
    assert_eq!(copy.color, control.color);
    assert_eq!(copy.mana_cost, control.mana_cost);
    assert!(*copy.abilities == *control.abilities);
    assert_eq!(copy.prepared_copy_source, Some(token));
    assert_eq!(control.prepared_copy_source, Some(source));

    // Being prepared changes nothing about the token itself.
    let token_obj = &state.objects[&token];
    assert_eq!((token_obj.power, token_obj.toughness), (Some(1), Some(1)));
    assert_eq!(token_obj.card_types.subtypes, vec!["Frog"]);

    // CR 722.3c + CR 704.5e: the copies survive further state-based actions.
    engine::game::sba::check_state_based_actions(board.runner.state_mut(), &mut Vec::new());
    assert_eq!(
        exact_linked_copy(board.runner.state(), token, P0, prepare_face),
        token_copy
    );

    PreparedCounterpart {
        board,
        token,
        token_copy,
        source_copy,
    }
}

/// The maintainer's board: Croaking Counterpart copies Encouraging Aviator,
/// Codie prepares the Frog token, the token's linked copy is a blue instant
/// named Jump (the CR 722.3c example), it is cast, Codie copies it
/// (CR 722.3d), and both resolve. The uncopied Aviator is the paired control.
#[test]
fn copiable_prepare_face_counterpart_token_of_aviator_is_prepared_by_codie() {
    let PreparedCounterpart {
        mut board,
        token,
        token_copy,
        source_copy,
    } = counterpart_token_prepared_by_codie(
        "Encouraging Aviator",
        "Jump",
        ManaCostShard::Blue,
        engine::types::mana::ManaColor::Blue,
        &[],
    );
    let aviator = board.sources[0];
    let codie = board.codie;
    let bystander = board.bystander;
    let flying = |runner: &mut GameRunner, id: ObjectId| {
        engine::game::layers::evaluate_layers(runner.state_mut());
        engine::game::keywords::has_keyword(
            &runner.state().objects[&id],
            &engine::types::keywords::Keyword::Flying,
        )
    };
    assert!(!flying(&mut board.runner, codie));
    assert!(!flying(&mut board.runner, bystander));

    // Positive castability for {U}: both prepared permanents offer the cast.
    assert!(!has_cast_prepared_copy(board.runner.state(), token));
    add_p0_mana(&mut board.runner, &[ManaType::Blue]);
    assert!(has_cast_prepared_copy(board.runner.state(), token));
    assert!(has_cast_prepared_copy(board.runner.state(), aviator));

    // CR 722.3c + CR 601.2i: cast the token's copy; it is the object cast and
    // the token is unprepared as the spell becomes cast.
    let spell = start_prepared_cast(&mut board.runner, token);
    assert_eq!(spell, token_copy);
    drive_cast_to_stack(&mut board.runner, Some(codie));
    assert_priority(&board.runner, P0);
    {
        let state = board.runner.state();
        assert_eq!(state.objects[&spell].zone, Zone::Stack);
        assert_eq!(state.objects[&spell].prepared_copy_source, Some(token));
        assert!(state.objects[&token].prepared.is_none());
        assert_eq!(
            triggers_from(state, codie),
            1,
            "CR 722.3d: casting the token's prepared spell triggers Codie"
        );
        // The control is untouched: still prepared with its own copy.
        assert!(state.objects[&aviator].prepared.is_some());
        assert_eq!(exact_linked_copy(state, aviator, P0, "Jump"), source_copy);
    }

    // CR 707.10c: Codie's copy of Jump is retargeted to the bystander.
    pass_twice(&mut board.runner);
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::CopyRetarget { player, .. } if player == P0
    ));
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bystander)),
        })
        .expect("choose the copy's new target");
    board.runner.advance_until_stack_empty();
    assert!(board.runner.state().stack.is_empty());
    // "Target creature gains flying until end of turn." resolved twice.
    assert!(flying(&mut board.runner, codie));
    assert!(flying(&mut board.runner, bystander));
}

/// The same class through Emeritus of Truce, whose prepare spell is the white
/// instant Swords to Plowshares. The token's own enters trigger ("target player
/// creates a 1/1 ... Inkling") targets P0; P1 controls no creature, so that
/// trigger does not prepare the token, and Codie's activation does.
#[test]
fn copiable_prepare_face_counterpart_token_of_emeritus_is_prepared_by_codie() {
    let PreparedCounterpart {
        mut board, token, ..
    } = counterpart_token_prepared_by_codie(
        "Emeritus of Truce",
        "Swords to Plowshares",
        ManaCostShard::White,
        engine::types::mana::ManaColor::White,
        &[P0],
    );
    assert!(!has_cast_prepared_copy(board.runner.state(), token));
    add_p0_mana(&mut board.runner, &[ManaType::White]);
    assert!(
        has_cast_prepared_copy(board.runner.state(), token),
        "CR 722.3c: the token's Swords copy is castable for {{W}}"
    );
}

/// A token copy of a creature without a prepare spell has none (CR 722.3a:
/// it can't become prepared). Paired positive on the same board: the same
/// activation prepares an uncopied Encouraging Aviator.
#[test]
fn copiable_prepare_face_non_prepare_token_stays_unprepared() {
    let mut board = build_counterpart_board(
        db(),
        &[
            CounterpartSource {
                controller: P0,
                name: "Grizzly Bears",
                prepare_face: None,
            },
            CounterpartSource {
                controller: P0,
                name: "Encouraging Aviator",
                prepare_face: Some("Jump"),
            },
        ],
    );
    let bears = board.sources[0];
    let aviator = board.sources[1];
    let token = cast_counterpart(&mut board, bears, &[]);
    assert!(board.runner.state().objects[&token].back_face.is_none());

    activate_codie(&mut board.runner, board.codie);
    let state = board.runner.state();
    assert!(state.objects[&token].prepared.is_none());
    assert!(linked_copies(state, token).is_empty());
    assert!(state.objects[&bears].prepared.is_none());
    assert!(state.objects[&aviator].prepared.is_some());
    let aviator_copy = exact_linked_copy(state, aviator, P0, "Jump");
    assert_eq!(exile_ids(state), vec![aviator_copy]);
}

/// CR 707.2 + CR 110.5: the prepared designation is not a copiable value.
/// Counterpart copying an already-prepared Aviator makes an unprepared token
/// with no linked copy, while the Aviator keeps its single copy; the token
/// can still become prepared because it has the prepare spell (CR 722.3a).
#[test]
fn copiable_prepare_face_designation_is_not_copied() {
    let mut board = build_counterpart_board(
        db(),
        &[CounterpartSource {
            controller: P0,
            name: "Encouraging Aviator",
            prepare_face: Some("Jump"),
        }],
    );
    let aviator = board.sources[0];
    set_prepared(&mut board.runner, aviator);
    let aviator_copy = exact_linked_copy(board.runner.state(), aviator, P0, "Jump");

    let token = cast_counterpart(&mut board, aviator, &[]);
    {
        let state = board.runner.state();
        assert!(state.objects[&token].prepared.is_none());
        assert!(linked_copies(state, token).is_empty());
        assert_eq!(exact_linked_copy(state, aviator, P0, "Jump"), aviator_copy);
        // Reach guard: the token has the prepare spell, so it could be prepared.
        assert_eq!(
            state.objects[&token]
                .back_face
                .as_ref()
                .map(|face| face.name.as_str()),
            Some("Jump")
        );
    }

    let events = activate_codie(&mut board.runner, board.codie);
    let prepared: Vec<ObjectId> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::BecamePrepared { object_id } => Some(*object_id),
            _ => None,
        })
        .collect();
    // CR 722.3a: the already-prepared Aviator can't gain the designation again.
    assert_eq!(prepared, vec![token]);
    let state = board.runner.state();
    let token_copy = exact_linked_copy(state, token, P0, "Jump");
    assert_eq!(exact_linked_copy(state, aviator, P0, "Jump"), aviator_copy);
    assert_eq!(exile_ids(state), exile_ids_of(&[aviator_copy, token_copy]));
}

/// Owner versus controller: P0's Counterpart copies P1's Aviator. The token is
/// P0's (CR 111.2), so P0's Codie prepares it and its copy is P0's; P1's
/// Aviator is not a creature P0 controls and stays unprepared.
#[test]
fn copiable_prepare_face_token_owner_is_the_creator() {
    let mut board = build_counterpart_board(
        db(),
        &[CounterpartSource {
            controller: P1,
            name: "Encouraging Aviator",
            prepare_face: Some("Jump"),
        }],
    );
    let aviator = board.sources[0];
    let token = cast_counterpart(&mut board, aviator, &[]);
    activate_codie(&mut board.runner, board.codie);
    let state = board.runner.state();
    assert!(state.objects[&token].prepared.is_some());
    let token_copy = exact_linked_copy(state, token, P0, "Jump");
    assert!(state.objects[&aviator].prepared.is_none());
    assert!(linked_copies(state, aviator).is_empty());
    assert_eq!(exile_ids(state), vec![token_copy]);
}

/// The token's prepare spell is serialized token state: a save/load and the
/// card-database rehydration keep it unchanged, and the reloaded token is
/// prepared by Codie. (Not revert-discriminating after the reload: the
/// rehydration's `populate_back_face_if_dfc` would also give a token whose
/// `printed_ref` names a preparation card its printed other face; the
/// discriminating assertion is the pre-save `back_face`.)
#[test]
fn copiable_prepare_face_survives_serialization_and_rehydration() {
    let db = db();
    let mut board = build_counterpart_board(
        db,
        &[CounterpartSource {
            controller: P0,
            name: "Encouraging Aviator",
            prepare_face: Some("Jump"),
        }],
    );
    let aviator = board.sources[0];
    let token = cast_counterpart(&mut board, aviator, &[]);
    let before = board.runner.state().objects[&token]
        .back_face
        .clone()
        .expect("CR 722.2b: the token has the prepare spell before saving");

    let json = serde_json::to_string(board.runner.state()).expect("state serializes");
    let mut restored: GameState = serde_json::from_str(&json).expect("state deserializes");
    engine::game::rehydrate_game_from_card_db(&mut restored, db);
    assert_eq!(restored.objects[&token].back_face.as_ref(), Some(&before));

    let mut runner = GameRunner::from_state(restored);
    activate_codie(&mut runner, board.codie);
    assert!(runner.state().objects[&token].prepared.is_some());
    exact_linked_copy(runner.state(), token, P0, "Jump");
}

// ---------------------------------------------------------------------------
// A prepare spell is not another face. CR 701.27c: only permanents represented
// by double-faced tokens or cards can transform; CR 701.27d: transforming into
// an instant or sorcery face does nothing. The Counterpart token copy of
// Encouraging Aviator carries Jump (an instant) as a copiable value
// (CR 722.2b) but is a single-faced token, and the printed Aviator is a
// single-faced preparation card, so neither transforms. Delver of Secrets, a
// real transforming double-faced card on the same board, is the positive
// control through the same entry points.
// ---------------------------------------------------------------------------

/// The Counterpart token copy of P0's Encouraging Aviator, the printed Aviator,
/// and P0's Delver of Secrets (the transforming control).
struct TransformBoard {
    runner: GameRunner,
    aviator: ObjectId,
    delver: ObjectId,
    token: ObjectId,
}

fn transform_board() -> TransformBoard {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let bystander = scenario.add_creature(P0, "Ground Bystander", 2, 2).id();
    let aviator = scenario.add_real_card(P0, "Encouraging Aviator", Zone::Battlefield, db);
    let delver = scenario.add_real_card(P0, "Delver of Secrets", Zone::Battlefield, db);
    let counterpart = scenario.add_real_card(P0, "Croaking Counterpart", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(&[ManaType::Green, ManaType::Blue, ManaType::Red]));
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().all_creature_types = db.creature_type_vocabulary().to_vec();
    {
        let state = runner.state();
        assert_eq!(
            state.objects[&delver]
                .back_face
                .as_ref()
                .map(|face| (face.name.as_str(), face.layout_kind)),
            Some((
                "Insectile Aberration",
                Some(engine::types::card::LayoutKind::Transform)
            )),
            "the control is a real transforming double-faced card"
        );
    }
    let mut board = CounterpartBoard {
        runner,
        codie,
        bystander,
        sources: vec![aviator, delver],
        counterpart,
    };
    let token = cast_counterpart(&mut board, aviator, &[]);
    let back = board.runner.state().objects[&token]
        .back_face
        .as_ref()
        .expect("CR 722.2b: the token copy has the prepare spell");
    assert_eq!(back.name, "Jump");
    assert_eq!(back.card_types.core_types, vec![CoreType::Instant]);
    TransformBoard {
        runner: board.runner,
        aviator,
        delver,
        token,
    }
}

/// The characteristics a transform would change, plus the face flag.
type Face = (
    String,
    Vec<CoreType>,
    Vec<String>,
    Option<i32>,
    Option<i32>,
    bool,
    Option<String>,
);

fn face(state: &GameState, id: ObjectId) -> Face {
    let object = &state.objects[&id];
    (
        object.name.clone(),
        object.card_types.core_types.clone(),
        object.card_types.subtypes.clone(),
        object.power,
        object.toughness,
        object.transformed,
        object.back_face.as_ref().map(|back| back.name.clone()),
    )
}

/// CR 701.27c + CR 701.27d through `GameAction::Transform` at P0's priority:
/// the reducer rejects the action for the token and the printed Aviator, and
/// neither changes; the same action transforms the Delver control.
#[test]
fn copiable_prepare_face_token_cannot_transform_by_action() {
    let TransformBoard {
        mut runner,
        aviator,
        delver,
        token,
    } = transform_board();
    assert_priority(&runner, P0);
    let token_before = face(runner.state(), token);
    let aviator_before = face(runner.state(), aviator);
    assert_eq!(token_before.2, vec!["Frog"]);
    assert_eq!((token_before.3, token_before.4), (Some(1), Some(1)));
    assert_eq!(token_before.6.as_deref(), Some("Jump"));

    for id in [token, aviator] {
        assert!(
            runner.act(GameAction::Transform { object_id: id }).is_err(),
            "CR 701.27c + CR 701.27d: a prepare spell is not a face to transform into"
        );
    }
    assert_eq!(face(runner.state(), token), token_before);
    assert_eq!(face(runner.state(), aviator), aviator_before);
    assert_priority(&runner, P0);

    runner
        .act(GameAction::Transform { object_id: delver })
        .expect("the transforming double-faced control transforms");
    let delver_after = &runner.state().objects[&delver];
    assert_eq!(delver_after.name, "Insectile Aberration");
    assert!(delver_after.transformed);
    assert_eq!(face(runner.state(), token), token_before);
}

/// CR 701.27c + CR 701.27d through the transform effect resolver: a targeted
/// "transform target creature" at the token or the printed Aviator does
/// nothing (and is not an error), while the same instruction transforms the
/// Delver control; a mass transform leaves both prepare-spell holders alone.
#[test]
fn copiable_prepare_face_token_ignores_transform_effect() {
    let TransformBoard {
        mut runner,
        aviator,
        delver,
        token,
    } = transform_board();
    let token_before = face(runner.state(), token);
    let aviator_before = face(runner.state(), aviator);
    let transform = |runner: &mut GameRunner, scope, targets: Vec<TargetRef>| {
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope,
            },
            targets,
            ObjectId(9_001),
            P0,
        );
        let mut events = Vec::new();
        engine::game::effects::resolve_effect(runner.state_mut(), &ability, &mut events)
            .expect("a transform that does nothing is not an error");
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::Transformed { object_id } => Some(*object_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let single = engine::types::ability::EffectScope::Single;

    for id in [token, aviator] {
        assert!(transform(&mut runner, single, vec![TargetRef::Object(id)]).is_empty());
    }
    assert_eq!(face(runner.state(), token), token_before);
    assert_eq!(face(runner.state(), aviator), aviator_before);

    // Positive control through the same resolver.
    assert_eq!(
        transform(&mut runner, single, vec![TargetRef::Object(delver)]),
        vec![delver]
    );
    assert_eq!(runner.state().objects[&delver].name, "Insectile Aberration");

    // "Transform all creatures": only the double-faced Delver turns back over.
    assert_eq!(
        transform(
            &mut runner,
            engine::types::ability::EffectScope::All,
            vec![]
        ),
        vec![delver]
    );
    assert_eq!(runner.state().objects[&delver].name, "Delver of Secrets");
    assert_eq!(face(runner.state(), token), token_before);
    assert_eq!(face(runner.state(), aviator), aviator_before);
}

/// CR 722.2b + CR 707.2 + CR 613.1a: the prepare spell is a copiable value, so a
/// permanent that is a Layer-1 copy (a Clone) of Encouraging Aviator has it and
/// Codie's activation prepares it (CR 722.3a), with a linked copy named Jump
/// (CR 722.3c). Paired controls on the same board: the uncopied Aviator is
/// prepared too, and an Aviator that is itself a copy of a creature with no
/// prepare spell has none and stays unprepared.
#[test]
fn layer1_copy_of_a_preparation_creature_is_prepared_by_codie() {
    let db = db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie, Ravenous Codex", 1, 4, CODIE_ORACLE)
        .id();
    let aviator = scenario.add_real_card(P0, "Encouraging Aviator", Zone::Battlefield, db);
    let aviator_copying_plain =
        scenario.add_real_card(P0, "Encouraging Aviator", Zone::Battlefield, db);
    let clone = scenario.add_creature(P0, "Clone", 0, 0).id();
    let copy_of_clone = scenario.add_creature(P0, "Second Clone", 0, 0).id();
    let plain = scenario.add_creature(P0, "Plain Donor", 2, 2).id();
    scenario.with_mana_pool(P0, mana(WUBRG));
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(runner.state().objects[&aviator].back_face.is_some());
    assert!(runner.state().objects[&clone].back_face.is_none());

    let become_copy = |runner: &mut GameRunner, recipient: ObjectId, donor: ObjectId| {
        let ability = engine::types::ability::ResolvedAbility::new(
            engine::types::ability::Effect::BecomeCopy {
                recipient: engine::types::ability::CopyRecipient::Source,
                target: engine::types::ability::TargetFilter::Any,
                duration: Some(engine::types::ability::Duration::UntilEndOfTurn),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(donor)],
            recipient,
            P0,
        );
        engine::game::effects::become_copy::resolve(runner.state_mut(), &ability, &mut Vec::new())
            .expect("the Layer-1 copy effect installs");
        engine::game::layers::evaluate_layers(runner.state_mut());
    };
    become_copy(&mut runner, clone, aviator);
    become_copy(&mut runner, aviator_copying_plain, plain);
    // Copy of a copy: the donor's CURRENT copiable values (Aviator's, via the
    // first Clone) carry the prepare spell forward.
    become_copy(&mut runner, copy_of_clone, clone);
    assert_eq!(
        runner.state().objects[&copy_of_clone].name,
        "Encouraging Aviator"
    );
    assert_eq!(runner.state().objects[&clone].name, "Encouraging Aviator");
    assert_eq!(
        runner.state().objects[&aviator_copying_plain].name,
        "Plain Donor"
    );
    assert!(runner.state().objects[&clone].back_face.is_none());

    let visited = drive_activation(&mut runner, codie, 0);
    assert!(!visited.contains(&"TargetSelection"));
    pass_twice(&mut runner);
    assert!(runner.state().stack.is_empty());

    let state = runner.state();
    assert!(
        state.objects[&aviator].prepared.is_some(),
        "positive control"
    );
    assert!(
        state.objects[&clone].prepared.is_some(),
        "CR 722.2b: the Clone's copied prepare spell makes it eligible"
    );
    exact_linked_copy(state, clone, P0, "Jump");
    assert!(
        state.objects[&copy_of_clone].prepared.is_some(),
        "CR 707.2: a copy of the Clone has the copied prepare spell too"
    );
    exact_linked_copy(state, copy_of_clone, P0, "Jump");
    assert!(
        state.objects[&aviator_copying_plain].prepared.is_none(),
        "CR 722.2b: a copy of a creature with no prepare spell has none"
    );
    assert!(linked_copies(state, aviator_copying_plain).is_empty());
}
