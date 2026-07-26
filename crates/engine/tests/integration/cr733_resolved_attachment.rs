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
}
