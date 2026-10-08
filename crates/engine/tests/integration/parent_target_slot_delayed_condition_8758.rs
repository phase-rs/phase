//! A delayed trigger whose CONDITION names an earlier declared target by slot
//! (`TargetFilter::ParentTargetSlot { index }`) watches that object and no
//! other. CR 603.7c: "A delayed triggered ability that refers to a particular
//! object still affects it even if the object changes characteristics.
//! However, if that object is no longer in the zone it's expected to be in at
//! the time the delayed triggered ability resolves, the ability won't affect
//! it."
//!
//! Stolen Uniform is the one printed carrier: "Choose target creature you
//! control and target Equipment. … When you lose control of that Equipment
//! this turn, if it's attached to a creature you control, unattach it." The
//! Equipment is slot 1 of the whole chain, but the clause that installs the
//! trigger is three instructions down, and each instruction only inherits its
//! immediate parent's targets. Bound against that one-element list, slot 1
//! was out of range and degraded to `TargetFilter::Any` (issue #8758), so the
//! trigger fired on the FIRST permanent you lost control of this turn,
//! whichever it was.
//!
//! The slot is now resolved through the shared chain-root slot authority,
//! `targeting::resolve_live_parent_slot_from_root`, which also carries the
//! CR 608.2b legality stamp: a slot whose target was illegal as the spell
//! resolved names nothing. A condition none of whose alternatives can match
//! any more installs no trigger, exactly as a bare `ParentTarget` over an
//! empty parent set already does — but a dead slot takes only its own branch
//! with it: an `Or` keeps its other branches and a `WhenNextEvent` keeps its
//! `or_trigger` (CR 608.2b: "Other parts of the effect for which those targets
//! are not illegal may still affect them"). No printed card carries a compound
//! slot condition, so the compound cases below rewrite Stolen Uniform's parsed
//! chain — the condition and, to make a firing observable without an attach,
//! the delayed effect (draw a card) — and cast the result through the ordinary
//! pipeline. The player-axis case grafts that rewritten clause onto "target
//! player draws a card", so the slot resolves to a player and the bound leaf
//! is matched by `trigger_matchers::player_matches_filter`.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, EffectKind, QuantityExpr,
    TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::rules::{
    cast_spell_action, drive_with_response, drive_with_target_refs, PriorityResponse,
};

const STOLEN_UNIFORM: &str = "Choose target creature you control and target Equipment. Gain \
control of that Equipment until end of turn. Attach it to the chosen creature. When you lose \
control of that Equipment this turn, if it's attached to a creature you control, unattach it.";

const STEAL: &str = "Gain control of target creature until end of turn.";

const ARTIFACT_HEXPROOF: &str = "Target artifact you control gains hexproof until end of turn.";

/// Ephemerate's shape on an artifact: the Equipment leaves and returns as a
/// new object before Stolen Uniform resolves (CR 400.7).
const BLINK_ARTIFACT: &str =
    "Exile target artifact, then return it to the battlefield under its owner's control.";

/// Reach guard: `source` resolved an instruction of `kind`, whatever it then
/// affected.
fn resolved(events: &[GameEvent], kind: EffectKind, source: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: resolved_kind, source_id, .. }
                if *resolved_kind == kind && *source_id == source
        )
    })
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn attached_to(runner: &GameRunner, id: ObjectId) -> Option<AttachTarget> {
    runner.state().objects[&id].attached_to
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// The `valid_card` filters of the one installed `WhenNextEvent` delayed
/// trigger — the primary trigger's and, when present, the `or_trigger`'s — or
/// `None` when no delayed trigger is installed at all.
fn installed_valid_cards(runner: &GameRunner) -> Option<(TargetFilter, Option<TargetFilter>)> {
    let triggers = &runner.state().delayed_triggers;
    assert!(
        triggers.len() <= 1,
        "Stolen Uniform installs at most one delayed trigger, found {}",
        triggers.len()
    );
    let installed = triggers.first()?;
    let DelayedTriggerCondition::WhenNextEvent {
        trigger,
        or_trigger,
        ..
    } = &installed.condition
    else {
        panic!(
            "Stolen Uniform's delayed trigger is a WhenNextEvent, found {:?}",
            installed.condition
        );
    };
    let valid_card = |trigger: &TriggerDefinition| {
        trigger
            .valid_card
            .clone()
            .expect("the lose-control condition carries a valid_card filter")
    };
    Some((valid_card(trigger), or_trigger.as_deref().map(valid_card)))
}

/// The primary trigger's `valid_card`, or `None` when nothing is installed.
fn installed_valid_card(runner: &GameRunner) -> Option<TargetFilter> {
    installed_valid_cards(runner).map(|(primary, _)| primary)
}

/// The `CreateDelayedTrigger` instruction of a parsed chain, if any.
fn delayed_clause(ability: &mut AbilityDefinition) -> Option<&mut AbilityDefinition> {
    if matches!(*ability.effect, Effect::CreateDelayedTrigger { .. }) {
        return Some(ability);
    }
    ability.sub_ability.as_deref_mut().and_then(delayed_clause)
}

/// Stolen Uniform's parsed chain with its `CreateDelayedTrigger` clause
/// rewritten: `rewrite` edits the condition, and the delayed effect becomes
/// "draw a card" so a firing is observable whether or not the attach happened.
fn stolen_uniform_with_condition(
    rewrite: impl FnOnce(&mut DelayedTriggerCondition),
) -> Vec<AbilityDefinition> {
    let mut parsed = parse_oracle_text(
        STOLEN_UNIFORM,
        "Stolen Uniform",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let root = parsed
        .abilities
        .first_mut()
        .expect("reach guard: Stolen Uniform parses to one spell ability");
    let clause = delayed_clause(root).expect("reach guard: the chain ends in CreateDelayedTrigger");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = &mut *clause.effect
    else {
        unreachable!("delayed_clause matched CreateDelayedTrigger");
    };
    rewrite(condition);
    **effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    parsed.abilities
}

/// The parsed `WhenNextEvent` of Stolen Uniform, for the rewrites below.
fn when_next_event(
    condition: &mut DelayedTriggerCondition,
) -> (&mut TriggerDefinition, &mut Option<Box<TriggerDefinition>>) {
    let DelayedTriggerCondition::WhenNextEvent {
        trigger,
        or_trigger,
        ..
    } = condition
    else {
        panic!("Stolen Uniform's delayed trigger is a WhenNextEvent, found {condition:?}");
    };
    (trigger, or_trigger)
}

fn slot(index: usize) -> TargetFilter {
    TargetFilter::ParentTargetSlot { index }
}

/// A spell whose only declared target is a player: slot 0 of its chain
/// resolves to a `TargetRef::Player`.
const PROBE: &str = "Target player draws a card.";

/// "Target player draws a card" followed by Stolen Uniform's delayed clause,
/// its lose-control trigger swapped for a becomes-target trigger whose
/// `valid_subject_player` is `subject`: "when a player matching `subject`
/// next becomes the target of a spell or ability this turn, draw a card".
fn probe_with_subject(subject: TargetFilter) -> Vec<AbilityDefinition> {
    let mut uniform = stolen_uniform_with_condition(|condition| {
        let (trigger, _) = when_next_event(condition);
        let mut becomes_target = TriggerDefinition::new(TriggerMode::BecomesTarget);
        becomes_target.valid_subject_player = Some(subject);
        *trigger = becomes_target;
    });
    let mut clause = delayed_clause(&mut uniform[0])
        .expect("reach guard: the rewritten chain still ends in CreateDelayedTrigger")
        .clone();
    clause.sub_ability = None;
    let mut parsed = parse_oracle_text(PROBE, "Probe", &[], &["Instant".to_string()], &[]);
    let root = parsed
        .abilities
        .first_mut()
        .expect("reach guard: the probe parses to one spell ability");
    assert!(
        root.sub_ability.is_none(),
        "reach guard: the probe is a single instruction"
    );
    root.sub_ability = Some(Box::new(clause));
    parsed.abilities
}

/// The primary trigger's `valid_subject_player` of the one installed
/// `WhenNextEvent` delayed trigger, or `None` when nothing is installed.
fn installed_subject_player(runner: &GameRunner) -> Option<TargetFilter> {
    let installed = runner.state().delayed_triggers.first()?;
    let DelayedTriggerCondition::WhenNextEvent { trigger, .. } = &installed.condition else {
        panic!(
            "the probe's delayed trigger is a WhenNextEvent, found {:?}",
            installed.condition
        );
    };
    Some(
        trigger
            .valid_subject_player
            .clone()
            .expect("the becomes-target condition carries a valid_subject_player filter"),
    )
}

/// Two creatures you control, an Equipment the opponent controls, Stolen
/// Uniform in your hand, and two instants in the opponent's hand: `response`
/// (`response_oracle`) and `steal` (a steal), in your precombat main phase.
struct Board {
    runner: GameRunner,
    spell: ObjectId,
    mine: ObjectId,
    other: ObjectId,
    blade: ObjectId,
    response: ObjectId,
    steal: ObjectId,
}

fn board(response_oracle: &str) -> Board {
    board_with(response_oracle, None)
}

/// [`board`] with Stolen Uniform's parsed abilities replaced by `abilities`
/// (see [`stolen_uniform_with_condition`]).
fn board_with(response_oracle: &str, abilities: Option<Vec<AbilityDefinition>>) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let other = scenario.add_creature(P0, "Other", 1, 1).id();
    let blade = scenario
        .add_creature(P1, "Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = match abilities {
        None => scenario
            .add_spell_to_hand_from_oracle(P0, "Stolen Uniform", true, STOLEN_UNIFORM)
            .id(),
        Some(abilities) => {
            let mut card = scenario.add_spell_to_hand(P0, "Stolen Uniform", true);
            for ability in abilities {
                card.with_ability_definition(ability);
            }
            card.id()
        }
    };
    let response = scenario
        .add_spell_to_hand_from_oracle(P1, "Response", true, response_oracle)
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Steal", true, STEAL)
        .id();
    // A library to draw from, so a rewritten "draw a card" firing is a card in
    // hand and not a loss by an empty library (CR 704.5b).
    scenario.add_spell_to_library_top(P0, "Filler", true);
    scenario.add_spell_to_library_top(P0, "Filler", true);
    Board {
        runner: scenario.build(),
        spell,
        mine,
        other,
        blade,
        response,
        steal,
    }
}

/// Cast Stolen Uniform on (`mine`, `blade`) with no response and let it
/// resolve; returns the events. Reach-guards the attach so the later
/// assertions cannot pass on a spell that did nothing.
fn resolve_uniform(board: &mut Board) -> Vec<GameEvent> {
    let Board {
        runner,
        spell,
        mine,
        blade,
        ..
    } = board;
    let cast = cast_spell_action(runner, *spell);
    let events = drive_with_response(runner, cast, &[*mine, *blade], None);
    assert_eq!(
        controller(runner, *blade),
        P0,
        "reach guard: you gain control of the Equipment"
    );
    assert_eq!(
        attached_to(runner, *blade),
        Some(AttachTarget::Object(*mine)),
        "reach guard: the Equipment is attached to the chosen creature"
    );
    events
}

/// Cast the board's Stolen Uniform on (`mine`, `blade`) with the opponent's
/// `response` cast at `blade` in response; returns the events. Reach-guards
/// that the spell resolved (the creature slot stays legal) and that the
/// installing clause ran — its refusal reports the same `CreateDelayedTrigger`
/// event as an install, so a missing trigger afterwards is a refusal, not a
/// chain that stopped early.
fn resolve_uniform_with_response(board: &mut Board) -> Vec<GameEvent> {
    let Board {
        runner,
        spell,
        mine,
        blade,
        response,
        ..
    } = board;
    let cast = cast_spell_action(runner, *spell);
    let events = drive_with_response(
        runner,
        cast,
        &[*mine, *blade],
        Some(PriorityResponse {
            player: P1,
            instant: *response,
            target: *blade,
        }),
    );
    assert!(
        resolved(&events, EffectKind::GainControl, *spell),
        "reach guard: the creature you control is still legal, so the spell resolves"
    );
    assert!(
        resolved(&events, EffectKind::CreateDelayedTrigger, *spell),
        "reach guard: the installing clause ran"
    );
    events
}

/// Hand priority to the opponent in your main phase with an empty stack, then
/// have them cast `instant` at `target` and let it resolve.
fn opponent_casts(runner: &mut GameRunner, instant: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to the opponent");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach guard: the opponent holds priority in your main phase"
    );
    let cast = cast_spell_action(runner, instant);
    drive_with_response(runner, cast, &[target], None)
}

/// Drive the current turn to its end and into the next turn's upkeep, passing
/// every priority window and declaring no attackers or blockers. Cleanup
/// (CR 514.2) is where the until-end-of-turn control ends.
fn advance_past_cleanup(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..64 {
        if runner.state().turn_number > turn && runner.state().phase == Phase::Upkeep {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected window while ending the turn: {other:?}"),
        };
        runner.act(action).expect("end the turn");
    }
    panic!("the next upkeep was not reached within the window budget");
}

/// CR 603.7c: the installed condition names the Equipment itself — slot 1 of
/// the chain — not `Any`.
#[test]
fn stolen_uniforms_delayed_trigger_names_the_equipment_it_took() {
    let mut board = board(STEAL);
    resolve_uniform(&mut board);

    assert_eq!(
        installed_valid_card(&board.runner),
        Some(TargetFilter::SpecificObject { id: board.blade }),
        "the lose-control condition is bound to the Equipment at declared slot 1"
    );
}

/// The game-visible half of the same fact. Losing control of a DIFFERENT
/// permanent this turn is not "losing control of that Equipment": the
/// Equipment stays attached and the trigger stays armed. At cleanup the
/// until-end-of-turn control ends (CR 514.2), you lose control of the
/// Equipment, and only then does it come off.
#[test]
fn losing_control_of_another_permanent_does_not_unattach_the_uniform() {
    let mut board = board(STEAL);
    resolve_uniform(&mut board);
    let Board {
        mut runner,
        mine,
        other,
        blade,
        response,
        ..
    } = board;

    opponent_casts(&mut runner, response, other);
    assert_eq!(
        controller(&runner, other),
        P1,
        "reach guard: the opponent stole the other creature"
    );
    assert_eq!(
        attached_to(&runner, blade),
        Some(AttachTarget::Object(mine)),
        "losing control of another permanent must not unattach the Equipment"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the one-shot trigger must still be armed for the Equipment's own control loss"
    );

    advance_past_cleanup(&mut runner);
    assert_eq!(
        controller(&runner, blade),
        P1,
        "reach guard: the until-end-of-turn control ended at cleanup"
    );
    assert_eq!(
        attached_to(&runner, blade),
        None,
        "losing control of the Equipment itself unattaches it"
    );
}

/// CR 608.2b: "If part of the effect requires information about an illegal
/// target, it fails to determine any such information. Any part of the effect
/// that requires that information won't happen." The Equipment gains
/// hexproof in response, so slot 1 is illegal as the spell resolves: you gain
/// control of nothing, attach nothing, and a trigger that would watch "that
/// Equipment" has nothing to watch — it is not installed, rather than
/// installed as a watch on every permanent.
#[test]
fn an_equipment_that_became_illegal_installs_no_delayed_trigger() {
    let mut board = board(ARTIFACT_HEXPROOF);
    resolve_uniform_with_response(&mut board);
    let Board { runner, blade, .. } = board;

    assert!(
        runner.state().objects[&blade].has_keyword(&Keyword::Hexproof),
        "reach guard: the response gave the Equipment hexproof"
    );
    assert_eq!(
        controller(&runner, blade),
        P1,
        "reach guard: an illegal Equipment stays under its controller"
    );
    assert_eq!(
        installed_valid_card(&runner),
        None,
        "a condition naming an illegal slot installs no delayed trigger"
    );
}

/// CR 400.7: the Equipment is blinked in response, so the object the spell
/// targeted no longer exists — the slot's referent is stale (and, having left
/// its zone, illegal under CR 608.2b). It names nothing, and the condition
/// must not widen to `Any` in its place.
#[test]
fn an_equipment_blinked_before_resolution_installs_no_delayed_trigger() {
    let mut board = board(BLINK_ARTIFACT);
    let events = resolve_uniform_with_response(&mut board);
    let Board { runner, blade, .. } = board;

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == blade
        )),
        "reach guard: the response exiled the Equipment before the spell resolved"
    );
    assert_eq!(
        installed_valid_card(&runner),
        None,
        "a condition naming a stale slot installs no delayed trigger"
    );
}

/// CR 608.2b: an `Or` over a dead slot and a live one keeps the live branch.
/// The condition is rewritten to "when you lose control of that Equipment or
/// that creature"; the Equipment turns illegal in response, and the trigger
/// still installs — bound to the creature alone — and fires when the creature
/// is stolen.
#[test]
fn an_or_over_a_dead_slot_keeps_its_live_branch() {
    let abilities = stolen_uniform_with_condition(|condition| {
        let (trigger, _) = when_next_event(condition);
        trigger.valid_card = Some(TargetFilter::Or {
            filters: vec![slot(1), slot(0)],
        });
    });
    let mut board = board_with(ARTIFACT_HEXPROOF, Some(abilities));
    resolve_uniform_with_response(&mut board);
    let Board {
        mut runner,
        mine,
        steal,
        ..
    } = board;

    assert_eq!(
        installed_valid_card(&runner),
        Some(TargetFilter::SpecificObject { id: mine }),
        "the dead Equipment branch is dropped and the live creature branch is bound"
    );

    let before = hand_size(&runner, P0);
    opponent_casts(&mut runner, steal, mine);
    assert_eq!(
        controller(&runner, mine),
        P1,
        "reach guard: the opponent stole the creature"
    );
    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "losing control of the creature fires the surviving branch"
    );
}

/// The `And` counterpart at the same entrance: a dead slot inside an `And`
/// leaves nothing that can match, so nothing is installed.
#[test]
fn an_and_over_a_dead_slot_installs_nothing() {
    let abilities = stolen_uniform_with_condition(|condition| {
        let (trigger, _) = when_next_event(condition);
        trigger.valid_card = Some(TargetFilter::And {
            filters: vec![slot(1), slot(0)],
        });
    });
    let mut board = board_with(ARTIFACT_HEXPROOF, Some(abilities));
    resolve_uniform_with_response(&mut board);

    assert_eq!(
        installed_valid_card(&board.runner),
        None,
        "an `And` with a dead member can never match and installs nothing"
    );
}

/// CR 608.2b: a `WhenNextEvent` whose primary trigger names the dead slot but
/// whose `or_trigger` names the live creature keeps the whole delayed trigger:
/// the primary is bound dead (`None`), the alternative is bound to the
/// creature, and stealing the creature fires it.
#[test]
fn an_or_trigger_over_a_live_slot_keeps_the_trigger_installed() {
    let abilities = stolen_uniform_with_condition(|condition| {
        let (trigger, or_trigger) = when_next_event(condition);
        let mut alternative = trigger.clone();
        alternative.valid_card = Some(slot(0));
        *or_trigger = Some(Box::new(alternative));
    });
    let mut board = board_with(ARTIFACT_HEXPROOF, Some(abilities));
    resolve_uniform_with_response(&mut board);
    let Board {
        mut runner,
        mine,
        steal,
        ..
    } = board;

    assert_eq!(
        installed_valid_cards(&runner),
        Some((
            TargetFilter::None,
            Some(TargetFilter::SpecificObject { id: mine })
        )),
        "the dead primary stays installed as a filter that matches nothing; the alternative is bound"
    );

    let before = hand_size(&runner, P0);
    opponent_casts(&mut runner, steal, mine);
    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "losing control of the creature fires the alternative"
    );
}

/// The refusal partner: both the primary trigger and the `or_trigger` name the
/// dead slot, so no alternative can match and nothing is installed.
#[test]
fn an_or_trigger_over_a_dead_slot_too_installs_nothing() {
    let abilities = stolen_uniform_with_condition(|condition| {
        let (trigger, or_trigger) = when_next_event(condition);
        *or_trigger = Some(Box::new(trigger.clone()));
    });
    let mut board = board_with(ARTIFACT_HEXPROOF, Some(abilities));
    resolve_uniform_with_response(&mut board);

    assert_eq!(
        installed_valid_cards(&board.runner),
        None,
        "a delayed trigger none of whose alternatives can match installs nothing"
    );
}

/// CR 608.2c ("read the whole text") on the player axis: the slot resolves
/// to the targeted player and the condition keeps its shape,
/// `Not { SpecificPlayer }` — "a player other than that one". Targeting the
/// named player again does NOT fire it; targeting
/// any other player does. Before the player-axis matcher walked `Not`, the
/// composed filter fell through to the wildcard and the first targeting of
/// ANY player — the excluded one included — fired the trigger.
#[test]
fn a_not_over_a_bound_player_slot_excludes_only_that_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let probe = {
        let mut card = scenario.add_spell_to_hand(P0, "Probe", true);
        for ability in probe_with_subject(TargetFilter::Not {
            filter: Box::new(slot(0)),
        }) {
            card.with_ability_definition(ability);
        }
        card.id()
    };
    let at_them = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe", true, PROBE)
        .id();
    let at_me = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe", true, PROBE)
        .id();
    // Libraries to draw from (CR 704.5b): the probes make the opponent draw
    // twice and you once, and a firing draws once more.
    for _ in 0..3 {
        scenario.add_spell_to_library_top(P0, "Filler", true);
        scenario.add_spell_to_library_top(P1, "Filler", true);
    }
    let mut runner = scenario.build();

    let cast = cast_spell_action(&runner, probe);
    let events = drive_with_target_refs(&mut runner, cast, &[TargetRef::Player(P1)]);
    assert!(
        resolved(&events, EffectKind::CreateDelayedTrigger, probe),
        "reach guard: the installing clause ran"
    );
    assert_eq!(
        installed_subject_player(&runner),
        Some(TargetFilter::Not {
            filter: Box::new(TargetFilter::SpecificPlayer { id: P1 }),
        }),
        "the player slot is bound to the targeted player and keeps its `Not`"
    );
    assert!(
        runner.state().waiting_for == WaitingFor::Priority { player: P0 }
            && runner.state().phase == Phase::PreCombatMain,
        "reach guard: you hold priority again in your main phase"
    );

    let before = hand_size(&runner, P0);
    let cast = cast_spell_action(&runner, at_them);
    drive_with_target_refs(&mut runner, cast, &[TargetRef::Player(P1)]);
    assert_eq!(
        hand_size(&runner, P0),
        before - 1,
        "targeting the excluded player must not fire the trigger"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the one-shot trigger must still be armed"
    );

    let before = hand_size(&runner, P0);
    let cast = cast_spell_action(&runner, at_me);
    drive_with_target_refs(&mut runner, cast, &[TargetRef::Player(P0)]);
    assert_eq!(
        hand_size(&runner, P0),
        before - 1 + 1 + 1,
        "targeting any other player fires the trigger: the probe's draw and the trigger's"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot trigger fired and left"
    );
}
