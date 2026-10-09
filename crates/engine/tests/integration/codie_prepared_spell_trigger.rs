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
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, FilterProp, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
/// exiled, each controller gains its creature's power).
fn finish_codie_copy_and_retarget(
    runner: &mut GameRunner,
    codie: ObjectId,
    spell_id: ObjectId,
    x: ObjectId,
    y: ObjectId,
) {
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
