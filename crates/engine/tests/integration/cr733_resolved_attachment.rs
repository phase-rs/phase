//! CR733 P2 coverage for the attachment family.
//!
//! `attach::attach_to`, `attach::attach_to_player`, and `attach::unattach` are
//! the three authorities every production attachment edit funnels through — the
//! `Attach` effect, equip and bestow resolution, ETB "enters attached", token
//! creation, counter-driven attach, `return_as_aura`, and the unattach costs —
//! but each wrote its mutation raw. A retained-prefix replay therefore had no
//! record of who was attached to whom.
//!
//! The three authorities share ONE command rather than three sibling variants:
//! they are the same graph mutation parameterized by the resulting host, and
//! `Option<AttachTarget>` — the type the object already stores — expresses
//! object host, player host, and unattached as leaf values.
//!
//! CR 613.7e + CR 701.3c is why the timestamp must be recorded: attaching to a
//! different host draws a NEW timestamp that orders the attachment against
//! continuous effects, so a replay that re-draws one silently reorders the layer
//! system.
//!
//! The test drives the REAL pipeline — a `GameAction::ActivateAbility` equip
//! activation resolving off the stack — so the edit is produced by the
//! production resolver, not by a direct call to the authority. The Equipment is
//! already on the battlefield, so its incarnation is stable across the action and
//! the recorded command can be replayed against the captured predecessor state.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;

const EQUIP_ORACLE: &str = "Equipped creature gets +1/+1.\nEquip {0}";

#[test]
fn equip_journals_an_exact_resolved_attachment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    let equipment = scenario
        .add_creature(P0, "Test Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(EQUIP_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Captured before the activation so the recorded command can be replayed
    // against the exact predecessor state it was resolved from.
    let pre_state = runner.state().clone();
    let journal_start = runner.state().resolved_rules_journal.entries().len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: equipment,
            ability_index: 0,
        })
        .expect("Equip {0} is activatable at sorcery speed with a legal host");
    runner.advance_until_stack_empty();

    // CR 301.5f: the Equipment is attached to the creature. Without these reach
    // guards the journal assertion below could pass vacuously on an equip that
    // never resolved.
    let state = runner.state();
    assert_eq!(
        state.objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
        "CR 301.5f: the resolved equip must attach the Equipment to the creature"
    );
    assert!(
        state.objects[&creature].attachments.contains(&equipment),
        "the host must list the Equipment as attached"
    );

    // The discriminating assertion: the edit is journaled as an exact resolved
    // command. A raw mutation records nothing here.
    let attachments: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::Attachment(command)
                if command.attachment.object_id == equipment =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        attachments.len(),
        1,
        "the attachment authority must journal exactly one resolved edit"
    );

    let attachment = &attachments[0];
    assert_eq!(
        attachment.expected_old_host, None,
        "CR 701.3c: the recorded transition is unattached -> the chosen host"
    );
    assert_eq!(
        attachment.resulting_host,
        Some(AttachTarget::Object(creature)),
        "the recorded host is the creature the equip resolved onto"
    );
    // CR 613.7e: the recorded timestamp is the one the attachment actually
    // received, not a value re-derived at replay time.
    assert_eq!(
        attachment.resulting_timestamp,
        Some(state.objects[&equipment].timestamp),
        "the journaled timestamp is the timestamp the attach installed"
    );

    // Replay-exactness: applying the recorded command to the pre-activation state
    // reproduces the same host and timestamp with no re-derivation — in
    // particular without drawing a fresh timestamp from `next_timestamp`.
    let mut replay = pre_state;
    engine::game::effects::attach::apply_resolved_attachment(&mut replay, attachment)
        .expect("the recorded attachment must replay against its captured predecessor");
    assert_eq!(
        replay.objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
        "replay installs the exact recorded host"
    );
    assert!(
        replay.objects[&creature].attachments.contains(&equipment),
        "replay installs the host-side attachment edge"
    );
    assert_eq!(
        replay.objects[&equipment].timestamp,
        attachment
            .resulting_timestamp
            .expect("an attach draws a timestamp"),
        "CR 613.7e: replay installs the recorded timestamp instead of re-drawing one"
    );

    // CR 613.7: installing a recorded timestamp is only half the contract — the
    // allocator must also be carried past it, or a later draw in the same replay
    // hands the same timestamp to a second object and CR 613.7 leaves the two
    // unordered within their layer. Asserted by DRAWING rather than by reading
    // the counter, so this pins the observable consequence and not the field.
    let installed = attachment
        .resulting_timestamp
        .expect("an attach draws a timestamp");
    let next_drawn = replay.next_timestamp();
    assert!(
        next_drawn > installed,
        "CR 613.7: replay installed timestamp {installed} but the next draw handed out \
         {next_drawn}; two objects sharing a timestamp are unordered within their layer"
    );
}

/// CR 701.3d: an unattach records the mirror-image edit — a host precondition
/// with no resulting host — and draws no timestamp, which is what makes the
/// timestamp `Option` a checkable invariant rather than an always-present field.
/// Driven through a real `Effect::UnattachAll` cast so the edit comes from the
/// production resolver.
#[test]
fn unattach_journals_a_host_removal_without_a_timestamp_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    let equipment = scenario
        .add_creature(P0, "Test Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(EQUIP_ORACLE)
        .id();

    let mut spell = scenario.add_spell_to_hand(P0, "Disarm", true);
    spell.with_ability(Effect::UnattachAll {
        attachment: TargetFilter::Any,
        target: TargetFilter::Typed(TypedFilter::creature()),
    });
    let spell_id = spell.id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: equipment,
            ability_index: 0,
        })
        .expect("Equip {0} is activatable at sorcery speed with a legal host");
    runner.advance_until_stack_empty();

    let attached_state = runner.state().clone();
    let journal_start = attached_state.resolved_rules_journal.entries().len();
    let attached_timestamp = attached_state.objects[&equipment].timestamp;
    let attached_next_timestamp = attached_state.next_timestamp;

    let outcome = runner.cast(spell_id).target_object(creature).resolve();

    let state = outcome.state();
    assert_eq!(
        state.objects[&equipment].attached_to, None,
        "CR 701.3d: the unattach authority clears the host"
    );

    let removals: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::Attachment(command)
                if command.attachment.object_id == equipment =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        removals.len(),
        1,
        "the unattach authority must journal exactly one resolved edit"
    );
    let removal = &removals[0];
    assert_eq!(
        removal.expected_old_host,
        Some(AttachTarget::Object(creature)),
        "the recorded precondition is the host the attachment was on"
    );
    assert_eq!(removal.resulting_host, None, "CR 701.3d: no resulting host");
    assert_eq!(
        removal.resulting_timestamp, None,
        "CR 613.7e applies to attaching to a new host, so an unattach draws none"
    );

    let mut replay = attached_state;
    engine::game::effects::attach::apply_resolved_attachment(&mut replay, removal)
        .expect("the recorded unattach must replay against its captured predecessor");
    assert_eq!(
        replay.objects[&equipment].attached_to, None,
        "replay clears the host"
    );
    assert!(
        !replay.objects[&creature].attachments.contains(&equipment),
        "replay removes the host-side attachment edge"
    );
    assert_eq!(
        replay.objects[&equipment].timestamp, attached_timestamp,
        "CR 613.7e: an unattach replay must not disturb the attachment's timestamp"
    );
    // The other side of the allocator contract: an unattach drew no timestamp,
    // so replaying one must not advance the draw counter either. This is what
    // keeps the advance bound to a recorded draw rather than applied blanket to
    // every attachment command.
    assert_eq!(
        replay.next_timestamp, attached_next_timestamp,
        "CR 613.7e: an unattach draws no timestamp, so replay advances no allocator"
    );
}

const MASTER_THIEF: &str = "When this creature enters, gain control of target artifact for as long as you control this creature.";
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";
const AURA_GRAFT: &str = "Gain control of target Aura that's attached to a permanent. Attach it to another permanent it can enchant.";
const GROWTH: &str = "Target creature gets +3/+3 until end of turn.";

struct ThiefBoard {
    runner: engine::game::scenario::GameRunner,
    thief: engine::types::identifiers::ObjectId,
    loot: engine::types::identifiers::ObjectId,
    aura: engine::types::identifiers::ObjectId,
    original_host: engine::types::identifiers::ObjectId,
    other_host: engine::types::identifiers::ObjectId,
    grafts: [engine::types::identifiers::ObjectId; 2],
    growth: engine::types::identifiers::ObjectId,
    exiled: engine::types::identifiers::ObjectId,
}

fn thief_board() -> ThiefBoard {
    use engine::game::scenario::P1;
    use engine::types::mana::ManaCost;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let loot = scenario.add_artifact_from_oracle(P1, "Loot", "").id();
    let thief = scenario
        .add_creature_to_hand_from_oracle(P0, "Master Thief", 2, 2, MASTER_THIEF)
        .with_mana_cost(ManaCost::zero())
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Control Magic", CONTROL_MAGIC)
        .with_subtypes(vec!["Aura"])
        .id();
    let original_host = scenario.add_vanilla(P1, 2, 2);
    let other_host = scenario.add_vanilla(P0, 2, 2);
    let grafts = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P1, "Aura Graft", true, AURA_GRAFT)
            .with_mana_cost(ManaCost::zero())
            .id()
    });
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let exiled = scenario
        .add_spell_to_exile(P0, "Giant Growth", true)
        .from_oracle_text(GROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, original_host);
    runner.cast(thief).target_object(loot).resolve();
    assert_eq!(runner.state().objects[&thief].controller, P0);
    assert_eq!(runner.state().objects[&loot].controller, P0);
    assert_eq!(runner.state().transient_continuous_effects.len(), 1);
    assert!(matches!(
        runner.state().transient_continuous_effects[0].duration,
        engine::types::ability::Duration::WhileControllingHost
    ));
    ThiefBoard {
        runner,
        thief,
        loot,
        aura,
        original_host,
        other_host,
        grafts,
        growth,
        exiled,
    }
}

fn park_graft(
    runner: &mut engine::game::scenario::GameRunner,
    spell: engine::types::identifiers::ObjectId,
    aura: engine::types::identifiers::ObjectId,
    old_host: engine::types::identifiers::ObjectId,
    new_host: engine::types::identifiers::ObjectId,
) -> engine::types::game_state::GameState {
    use engine::types::ability::EffectKind;
    use engine::types::game_state::WaitingFor;
    let caster = runner.state().objects[&spell].controller;
    if runner.state().priority_player != caster {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().priority_player, caster);
    let outcome = runner.cast(spell).target_object(aura).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            cards,
            effect_kind: EffectKind::Attach,
            ..
        } => {
            assert!(cards.contains(&new_host));
            assert!(!cards.contains(&old_host));
            assert!(cards.len() >= 2, "the real Attach continuation must park");
        }
        other => panic!("expected actual Aura Graft host choice, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(old_host))
    );
    runner.state().clone()
}

fn finish_graft(
    runner: &mut engine::game::scenario::GameRunner,
    start: usize,
    aura: engine::types::identifiers::ObjectId,
    new_host: engine::types::identifiers::ObjectId,
) -> engine::types::resolved_commands::ResolvedAttachmentCommand {
    runner
        .act(GameAction::SelectCards {
            cards: vec![new_host],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let edits: Vec<_> = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::Attachment(command) if command.attachment.object_id == aura => {
                Some(command.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].resulting_host,
        Some(AttachTarget::Object(new_host))
    );
    edits[0].clone()
}

fn assert_no_retirement_of(state: &engine::types::game_state::GameState, start: usize, id: u64) {
    use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
    assert!(!state.resolved_rules_journal.entries().iter().skip(start).any(|entry|
        matches!(entry.command.as_ref(), Some(ResolvedRulesCommand::ContinuousEffect(edit))
            if matches!(edit.as_ref(), ResolvedContinuousEffectEdit::Retire(command)
                if command.effects.iter().any(|effect| effect.id == id)))));
}

fn recorded_growth(
    state: &engine::types::game_state::GameState,
    start: usize,
) -> engine::types::resolved_commands::ResolvedContinuousEffectCommand {
    use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
    let commands: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Install(command)
                    if command.effect.source_name == "Giant Growth" =>
                {
                    Some(command.clone())
                }
                ResolvedContinuousEffectEdit::Install(_)
                | ResolvedContinuousEffectEdit::Retire(_) => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(commands.len(), 1);
    commands[0].clone()
}

fn legacy_install_journal(
    state: &engine::types::game_state::GameState,
) -> engine::types::resolved_commands::ResolvedRulesJournal {
    let mut value = serde_json::to_value(&state.resolved_rules_journal).unwrap();
    let mut converted = 0;
    for entry in value["entries"].as_array_mut().unwrap() {
        if let Some(install) = entry["command"]["ContinuousEffect"].get("Install").cloned() {
            entry["command"] = serde_json::json!({"ContinuousEffectInstall": install});
            converted += 1;
        }
    }
    assert!(converted > 0);
    serde_json::from_value(value).expect("the full old-install journal passes authority validation")
}

#[test]
fn master_thief_aura_graft_compound_replays_before_growth_and_legacy_install() {
    use engine::game::scenario::P1;
    use engine::types::game_state::LayersDirty;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
    for with_ring_recursion in [false, true] {
        let mut board = thief_board();
        let old = board.runner.state().transient_continuous_effects[0].clone();
        // A real control change invalidates this existing designation, exercising
        // the full finalizer's recursive Ring normalization boundary too.
        if with_ring_recursion {
            board
                .runner
                .state_mut()
                .ring_bearer
                .insert(P0, Some(board.thief));
            engine::game::layers::mark_layers_full(board.runner.state_mut());
            engine::game::layers::flush_layers(board.runner.state_mut());
            assert_eq!(
                board.runner.state().ring_bearer.get(&P0),
                Some(&Some(board.thief))
            );
        }
        let aura_ref =
            ObjectIncarnationRef::from_object(&board.runner.state().objects[&board.aura]);
        let prefix = park_graft(
            &mut board.runner,
            board.grafts[0],
            board.aura,
            board.original_host,
            board.thief,
        );
        let start = prefix.resolved_rules_journal.entries().len();
        assert_eq!(prefix.objects[&board.thief].controller, P0);
        assert_eq!(prefix.objects[&board.loot].controller, P0);
        assert_eq!(prefix.transient_continuous_effects.len(), 2);
        let graft = prefix
            .transient_continuous_effects
            .iter()
            .find(|e| e.source_id == board.grafts[0])
            .unwrap()
            .clone();
        assert_eq!(graft.duration, engine::types::ability::Duration::Permanent);
        assert_eq!(
            ObjectIncarnationRef::from_object(&prefix.objects[&board.aura]),
            aura_ref
        );
        let attachment = finish_graft(&mut board.runner, start, board.aura, board.thief);
        assert_eq!(board.runner.state().objects[&board.thief].controller, P1);
        assert_eq!(board.runner.state().objects[&board.loot].controller, P1);
        assert_eq!(board.runner.state().ring_bearer.get(&P0), None);
        assert_eq!(
            board
                .runner
                .state()
                .transient_continuous_effects
                .iter()
                .collect::<Vec<_>>(),
            vec![&graft]
        );
        assert_no_retirement_of(board.runner.state(), start, old.id);
        // The Graft's caster still holds priority after its host choice
        // resolves; pass it back to Growth's caster, as `park_graft` does.
        if board.runner.state().priority_player != P0 {
            board.runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(board.runner.state().priority_player, P0);
        board
            .runner
            .cast(board.growth)
            .target_object(board.thief)
            .resolve();
        let growth = recorded_growth(board.runner.state(), start);
        assert_eq!(growth.expected_installed_count, 1);
        for legacy in [false, true] {
            let mut replay = prefix.clone();
            let journal = replay.resolved_rules_journal.clone();
            let (attachment, growth) = if legacy {
                let decoded = legacy_install_journal(board.runner.state());
                let attachment = decoded
                    .entries()
                    .iter()
                    .skip(start)
                    .find_map(|entry| match entry.command.as_ref()? {
                        ResolvedRulesCommand::Attachment(command)
                            if command.attachment.object_id == board.aura =>
                        {
                            Some(command.clone())
                        }
                        _ => None,
                    })
                    .unwrap();
                let growth = decoded
                    .entries()
                    .iter()
                    .skip(start)
                    .find_map(|entry| match entry.command.as_ref()? {
                        ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                            ResolvedContinuousEffectEdit::Install(command)
                                if command.effect.source_name == "Giant Growth" =>
                            {
                                Some(command.clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap();
                (attachment, growth)
            } else {
                (attachment.clone(), growth.clone())
            };
            engine::game::effects::attach::apply_resolved_attachment(&mut replay, &attachment)
                .unwrap();
            // CR 611.2b: losing the Thief ends its grant at this compound boundary.
            assert_eq!(replay.objects[&board.thief].controller, P1);
            assert_eq!(replay.objects[&board.loot].controller, P1);
            assert_eq!(replay.layers_dirty, LayersDirty::Clean);
            assert_eq!(replay.ring_bearer.get(&P0), None);
            assert_eq!(
                replay
                    .transient_continuous_effects
                    .iter()
                    .collect::<Vec<_>>(),
                vec![&graft]
            );
            assert_eq!(replay.resolved_rules_journal, journal);
            replay
                .apply_resolved_continuous_effect_edit(&ResolvedContinuousEffectEdit::Install(
                    growth.clone(),
                ))
                .unwrap();
            engine::game::layers::evaluate_layers(&mut replay);
            assert_eq!(replay.resolved_rules_journal, journal);
            assert_eq!(
                replay
                    .transient_continuous_effects
                    .iter()
                    .collect::<Vec<_>>(),
                vec![&graft, &growth.effect]
            );
            assert_eq!(replay.objects[&board.thief].power, Some(5));
        }
    }
}

#[test]
fn compound_attachment_retires_non_tce_tails_without_swap_back_revival() {
    use engine::game::scenario::P1;
    use engine::types::ability::{
        CastingPermission, Duration, ReplacementCondition, ReplacementDefinition,
    };
    use engine::types::proposed_event::ProposedEvent;
    use engine::types::replacements::ReplacementEvent;
    let mut board = thief_board();
    // Typed compound-boundary fixture: Master Thief does not print these
    // replacement/permission riders. These are existing supported records.
    let mut controlled =
        ReplacementDefinition::new(ReplacementEvent::Untap).valid_card(TargetFilter::SelfRef);
    controlled.condition = Some(ReplacementCondition::ControllerControlsSource {
        source: board.thief,
        controller: P0,
    });
    let mut unrelated = controlled.clone();
    unrelated.condition = Some(ReplacementCondition::ControllerControlsSource {
        source: board.other_host,
        controller: P0,
    });
    {
        let host = board
            .runner
            .state_mut()
            .objects
            .get_mut(&board.loot)
            .unwrap();
        for def in [&controlled, &unrelated] {
            host.replacement_definitions.push(def.clone());
            std::sync::Arc::make_mut(&mut host.base_replacement_definitions).push(def.clone());
        }
        host.tapped = true;
    }
    let permission = |duration: Duration, source, player| -> CastingPermission {
        serde_json::from_value(serde_json::json!({
            "type": "PlayFromExile",
            "duration": duration, "source_id": source, "granted_to": player
        }))
        .unwrap()
    };
    let control_permission = permission(Duration::WhileControllingHost, board.thief, P0);
    let survivors = vec![
        permission(Duration::WhileHostOnBattlefield, board.thief, P0),
        permission(Duration::UntilHostLeavesPlay, board.thief, P0),
        permission(Duration::WhileControllingHost, board.other_host, P0),
        permission(Duration::WhileControllingHost, board.original_host, P1),
    ];
    {
        let exiled = board
            .runner
            .state_mut()
            .objects
            .get_mut(&board.exiled)
            .unwrap();
        exiled.casting_permissions.push(control_permission.clone());
        exiled.casting_permissions.extend(survivors.clone());
    }
    let presence_id = board
        .runner
        .state_mut()
        .add_transient_continuous_effect(
            board.thief,
            P0,
            Duration::WhileHostOnBattlefield,
            TargetFilter::SpecificObject {
                id: board.other_host,
            },
            vec![engine::types::ability::ContinuousModification::AddPower { value: 1 }],
            None,
        )
        .expect("the fixture's duration begins");
    let deadline_id = board
        .runner
        .state_mut()
        .add_transient_continuous_effect(
            board.thief,
            P0,
            Duration::UntilHostLeavesPlay,
            TargetFilter::SpecificObject {
                id: board.other_host,
            },
            vec![engine::types::ability::ContinuousModification::AddPower { value: 2 }],
            None,
        )
        .expect("the fixture's duration begins");
    engine::game::layers::mark_layers_full(board.runner.state_mut());
    engine::game::layers::flush_layers(board.runner.state_mut());
    assert_eq!(
        board.runner.state().objects[&board.other_host].power,
        Some(5)
    );
    let presence = board
        .runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|e| e.id == presence_id)
        .unwrap()
        .clone();
    let deadline = board
        .runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|e| e.id == deadline_id)
        .unwrap()
        .clone();
    let assert_tail = |state: &engine::types::game_state::GameState, present: bool| {
        let host = &state.objects[&board.loot];
        assert_eq!(
            host.replacement_definitions
                .as_slice()
                .contains(&controlled),
            present
        );
        assert_eq!(
            host.base_replacement_definitions.contains(&controlled),
            present
        );
        assert!(host.replacement_definitions.as_slice().contains(&unrelated));
        assert!(host.base_replacement_definitions.contains(&unrelated));
        let permissions = &state.objects[&board.exiled].casting_permissions;
        assert_eq!(permissions.contains(&control_permission), present);
        for survivor in &survivors {
            assert!(permissions.contains(survivor));
        }
        assert_eq!(permissions.len(), survivors.len() + usize::from(present));
        assert!(state
            .transient_continuous_effects
            .iter()
            .any(|e| e == &presence));
        assert!(state
            .transient_continuous_effects
            .iter()
            .any(|e| e == &deadline));
        assert_eq!(
            state.layers_dirty,
            engine::types::game_state::LayersDirty::Clean
        );
    };
    assert_tail(board.runner.state(), true);
    assert!(
        engine::game::casting::can_cast_object_now(board.runner.state(), P0, board.exiled),
        "the granted exiled instant is actually castable before control is lost"
    );
    let candidates = engine::game::replacement::find_applicable_replacements(
        board.runner.state(),
        &ProposedEvent::Untap {
            object_id: board.loot,
            applied: Default::default(),
        },
        engine::game::replacement::replacement_registry(),
    );
    assert_eq!(
        candidates
            .iter()
            .filter(|id| id.source == board.loot)
            .count(),
        2,
        "both seeded control gates are live before the first move"
    );
    let first_prefix = park_graft(
        &mut board.runner,
        board.grafts[0],
        board.aura,
        board.original_host,
        board.thief,
    );
    let first = finish_graft(
        &mut board.runner,
        first_prefix.resolved_rules_journal.entries().len(),
        board.aura,
        board.thief,
    );
    assert_tail(board.runner.state(), false);
    let mut first_replay = first_prefix;
    let first_journal = first_replay.resolved_rules_journal.clone();
    engine::game::effects::attach::apply_resolved_attachment(&mut first_replay, &first).unwrap();
    assert_tail(&first_replay, false);
    assert_eq!(first_replay.resolved_rules_journal, first_journal);
    assert_eq!(first_replay.objects[&board.thief].controller, P1);

    let second_prefix = park_graft(
        &mut board.runner,
        board.grafts[1],
        board.aura,
        board.thief,
        board.original_host,
    );
    assert_tail(&second_prefix, false);
    let second = finish_graft(
        &mut board.runner,
        second_prefix.resolved_rules_journal.entries().len(),
        board.aura,
        board.original_host,
    );
    assert_eq!(board.runner.state().objects[&board.thief].controller, P0);
    assert_tail(board.runner.state(), false);
    // Typed presence-vs-event control at the same retained compound boundary.
    // Phasing is not a zone departure, but it ends the presence duration.
    let mut phased = second_prefix.clone();
    phased.objects.get_mut(&board.thief).unwrap().phase_status =
        engine::game::game_object::PhaseStatus::PhasedOut {
            cause: engine::game::game_object::PhaseOutCause::Directly,
        };
    assert!(phased.objects[&board.exiled]
        .casting_permissions
        .contains(&survivors[0]));
    assert!(phased.objects[&board.exiled]
        .casting_permissions
        .contains(&survivors[1]));
    engine::game::effects::attach::apply_resolved_attachment(&mut phased, &second).unwrap();
    assert!(!phased
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == presence_id));
    assert!(phased
        .transient_continuous_effects
        .iter()
        .any(|e| e == &deadline));
    // CR 702.26f: presence-bound permissions end on phase-out; the explicit
    // zone-exit deadline remains unfulfilled.
    assert!(!phased.objects[&board.exiled]
        .casting_permissions
        .contains(&survivors[0]));
    assert!(phased.objects[&board.exiled]
        .casting_permissions
        .contains(&survivors[1]));
    phased.objects.get_mut(&board.thief).unwrap().phase_status =
        engine::game::game_object::PhaseStatus::PhasedIn;
    engine::game::layers::mark_layers_full(&mut phased);
    engine::game::layers::flush_layers(&mut phased);
    assert!(!phased
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == presence_id));
    assert!(phased
        .transient_continuous_effects
        .iter()
        .any(|e| e == &deadline));
    assert!(!phased.objects[&board.exiled]
        .casting_permissions
        .contains(&survivors[0]));
    let mut second_replay = second_prefix;
    let second_journal = second_replay.resolved_rules_journal.clone();
    engine::game::effects::attach::apply_resolved_attachment(&mut second_replay, &second).unwrap();
    assert_eq!(second_replay.objects[&board.thief].controller, P0);
    assert_tail(&second_replay, false);
    assert_eq!(second_replay.resolved_rules_journal, second_journal);
    // Replay both graph moves in sequence too: restoring the source's control
    // may never reconstruct the records removed at the intermediate boundary.
    engine::game::effects::attach::apply_resolved_attachment(&mut first_replay, &second).unwrap();
    assert_eq!(first_replay.objects[&board.thief].controller, P0);
    assert_tail(&first_replay, false);
}

#[test]
fn rootwater_attachment_owner_does_not_leak_into_later_aura_exit() {
    use engine::game::scenario::P1;
    use engine::types::ability::{AbilityKind, TargetRef};
    use engine::types::events::GameEvent;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use engine::types::mana::ManaCost;
    use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
    use engine::types::zones::Zone;
    const ROOTWATER: &str =
        "{T}: Gain control of target creature for as long as that creature is enchanted.";
    const HOLY: &str = "Enchant creature\nEnchanted creature gets +1/+2.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sources = [0, 1].map(|_| {
        scenario
            .add_creature_from_oracle(P0, "Rootwater Matriarch", 2, 3, ROOTWATER)
            .id()
    });
    let recipients = [0, 1].map(|_| scenario.add_vanilla(P1, 2, 2));
    let destination = scenario.add_vanilla(P0, 2, 2);
    let auras = [0, 1].map(|_| {
        scenario
            .add_enchantment_from_oracle(P0, "Holy Strength", HOLY)
            .with_subtypes(vec!["Aura"])
            .id()
    });
    let graft = scenario
        .add_spell_to_hand_from_oracle(P0, "Aura Graft", true, AURA_GRAFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let assert_attached_aura = |state: &GameState, aura: ObjectId, recipient: ObjectId| {
        assert_eq!(state.objects[&aura].zone, Zone::Battlefield);
        assert!(state.battlefield.contains(&aura));
        // CR 303.4b: the Aura must actually enchant this recipient in both graph directions.
        assert_eq!(
            state.objects[&aura].attached_to,
            Some(AttachTarget::Object(recipient))
        );
        assert!(state.objects[&recipient].attachments.contains(&aura));
    };
    // CR 704.5m: attach both setup Auras before either activation can run
    // state-based actions and put an unattached Aura into the graveyard.
    for (recipient, aura) in recipients.into_iter().zip(auras) {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, recipient);
    }
    for (recipient, aura) in recipients.into_iter().zip(auras) {
        assert_attached_aura(runner.state(), aura, recipient);
    }
    let mut old = Vec::new();
    for (source, recipient) in sources.into_iter().zip(recipients) {
        let index = runner.state().objects[&source]
            .abilities
            .iter()
            .position(|a| a.kind == AbilityKind::Activated)
            .unwrap();
        runner
            .activate(source, index)
            .target_object(recipient)
            .resolve();
        for (host, aura) in recipients.into_iter().zip(auras) {
            assert_attached_aura(runner.state(), aura, host);
        }
        assert!(runner.state().objects[&source].tapped);
        assert_eq!(runner.state().objects[&recipient].controller, P0);
        old.push(
            runner
                .state()
                .transient_continuous_effects
                .iter()
                .find(|e| e.source_id == source)
                .unwrap()
                .clone(),
        );
    }
    let prefix = park_graft(&mut runner, graft, auras[0], recipients[0], destination);
    let start = prefix.resolved_rules_journal.entries().len();
    assert_eq!(prefix.transient_continuous_effects.len(), 3);
    assert_eq!(
        old[0].duration_subject,
        Some(ObjectIncarnationRef::from_object(
            &prefix.objects[&recipients[0]]
        ))
    );
    let attachment = finish_graft(&mut runner, start, auras[0], destination);
    assert_eq!(attachment.attachment.object_id, auras[0]);
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    assert_eq!(runner.state().objects[&recipients[1]].controller, P0);
    assert!(!runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == old[0].id));
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| e == &old[1]));
    assert_no_retirement_of(runner.state(), start, old[0].id);
    // The independent legacy suffix must execute the same full ForAsLongAs
    // retirement inside Attachment, then accept a strict expected-two install.
    let mut legacy_live = engine::game::scenario::GameRunner::from_state(runner.state().clone());
    legacy_live
        .cast(growth)
        .target_object(recipients[0])
        .resolve();
    let legacy_growth = recorded_growth(legacy_live.state(), start);
    assert_eq!(legacy_growth.expected_installed_count, 2);
    let legacy_journal = legacy_install_journal(legacy_live.state());
    let mut legacy_replay = prefix.clone();
    let replay_journal = legacy_replay.resolved_rules_journal.clone();
    for entry in legacy_journal.entries().iter().skip(start) {
        match entry.command.as_ref() {
            Some(ResolvedRulesCommand::Attachment(command)) => {
                engine::game::effects::attach::apply_resolved_attachment(
                    &mut legacy_replay,
                    command,
                )
                .unwrap()
            }
            Some(ResolvedRulesCommand::ContinuousEffect(edit)) if matches!(edit.as_ref(), ResolvedContinuousEffectEdit::Install(c) if c.effect.source_name == "Giant Growth") => {
                legacy_replay
                    .apply_resolved_continuous_effect_edit(edit)
                    .unwrap()
            }
            _ => {}
        }
    }
    assert_eq!(legacy_replay.resolved_rules_journal, replay_journal);
    assert!(!legacy_replay
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == old[0].id));
    assert!(legacy_replay
        .transient_continuous_effects
        .iter()
        .any(|e| e == &legacy_growth.effect));

    // An ordinary subsequent boundary must resume standalone recording.
    assert_attached_aura(runner.state(), auras[0], destination);
    assert_attached_aura(runner.state(), auras[1], recipients[1]);
    let committed = runner.cast(removal).target_object(auras[1]).commit();
    let exit_prefix = committed.state().clone();
    assert_attached_aura(&exit_prefix, auras[0], destination);
    assert_attached_aura(&exit_prefix, auras[1], recipients[1]);
    assert_eq!(exit_prefix.objects[&removal].zone, Zone::Stack);
    let entry = exit_prefix
        .stack
        .back()
        .expect("Disenchant must be committed on the stack");
    assert_eq!(entry.source_id, removal);
    // CR 601.2c: inspect the committed target, not only the driver's declared intent.
    assert_eq!(
        entry
            .ability()
            .expect("Disenchant must carry its chosen target")
            .targets,
        vec![TargetRef::Object(auras[1])]
    );
    let exit_start = exit_prefix.resolved_rules_journal.entries().len();
    let exit = committed.resolve();
    exit.assert_zone(&[auras[1]], Zone::Graveyard);
    // CR 701.8a: this resolution must destroy B from the battlefield, not
    // merely observe an Aura that was already in the graveyard during setup.
    assert!(exit.events().iter().any(|event| matches!(event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            ..
        } if *object_id == auras[1]
    )));
    // CR 701.3d: leaving the battlefield severs both attachment graph edges.
    assert_eq!(exit.state().objects[&auras[1]].attached_to, None);
    assert!(!exit.state().objects[&recipients[1]]
        .attachments
        .contains(&auras[1]));
    assert!(!exit.state().battlefield.contains(&auras[1]));
    assert_eq!(runner.state().objects[&recipients[1]].controller, P1);
    runner.cast(growth).target_object(recipients[1]).resolve();
    let growth = recorded_growth(runner.state(), exit_start);
    assert_eq!(
        growth.expected_installed_count, 1,
        "Aura Graft's permanent grant survives"
    );
    let suffix: Vec<_> = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(exit_start)
        .filter_map(|entry| entry.command.as_ref())
        .filter(|command| match command {
            ResolvedRulesCommand::ZoneChange(c) => c.object.object_id == auras[1],
            ResolvedRulesCommand::ContinuousEffect(_) => true,
            _ => false,
        })
        .collect();
    assert_eq!(suffix.len(), 3);
    assert!(
        matches!(suffix[0], ResolvedRulesCommand::ZoneChange(command)
        if command.object == ObjectIncarnationRef::from_object(&exit_prefix.objects[&auras[1]])
            && command.from == Zone::Battlefield
            && command.to == Zone::Graveyard)
    );
    assert!(
        matches!(suffix[1], ResolvedRulesCommand::ContinuousEffect(edit)
        if matches!(edit.as_ref(), ResolvedContinuousEffectEdit::Retire(c) if c.effects == vec![old[1].clone()]))
    );
    assert_no_retirement_of(runner.state(), start, old[0].id);
    // These are semantic-subset replays at two actual prefixes. Intervening
    // Stack moves have unjournaled zone-history rows, so a single-prefix subset
    // would fail an unrelated zone-index precondition rather than test ownership.
    let mut attachment_replay = prefix;
    let attachment_journal = attachment_replay.resolved_rules_journal.clone();
    engine::game::effects::attach::apply_resolved_attachment(&mut attachment_replay, &attachment)
        .unwrap();
    assert_eq!(attachment_replay.objects[&recipients[0]].controller, P1);
    assert_eq!(attachment_replay.objects[&recipients[1]].controller, P0);
    assert!(!attachment_replay
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == old[0].id));
    assert!(attachment_replay
        .transient_continuous_effects
        .iter()
        .any(|e| e == &old[1]));
    assert_eq!(attachment_replay.resolved_rules_journal, attachment_journal);

    let mut replay = exit_prefix;
    let journal = replay.resolved_rules_journal.clone();
    assert!(!replay
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == old[0].id));
    assert!(replay
        .transient_continuous_effects
        .iter()
        .any(|e| e == &old[1]));
    for command in suffix {
        match command {
            ResolvedRulesCommand::ZoneChange(command) => {
                engine::game::zones::apply_resolved_zone_change(&mut replay, command).unwrap()
            }
            ResolvedRulesCommand::ContinuousEffect(edit) => {
                replay.apply_resolved_continuous_effect_edit(edit).unwrap()
            }
            _ => unreachable!(),
        }
    }
    engine::game::layers::evaluate_layers(&mut replay);
    assert_eq!(replay.resolved_rules_journal, journal);
    assert_eq!(replay.objects[&recipients[1]].controller, P1);
    assert_eq!(replay.objects[&recipients[1]].power, Some(5));
    assert_eq!(replay.transient_continuous_effects.len(), 2);
}
