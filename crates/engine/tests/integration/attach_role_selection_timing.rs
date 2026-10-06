//! CR 115.1a / CR 115.1c / CR 115.1d / CR 115.1e + CR 608.2d: the two roles of
//! an `Attach` instruction carry INDEPENDENT selection timing.
//!
//! `"attach any number of Equipment you control to target creature you control"`
//! (Beatrix, Loyal General; Ardenn, Intrepid Archaeologist) prints the word
//! "target" for the HOST only. The Equipment operand is a DESCRIBED choice made
//! while the effect resolves (CR 608.2d), from the LIVE battlefield population —
//! it is not an announced target (CR 115.10a) and must not claim an announcement
//! slot, and a host that itself matches the attachment filter must never be
//! consumed as the described operand.
//!
//! Revert discriminators:
//! 1. the trigger's announcement carries exactly ONE slot (the host); at base it
//!    carries two and the Equipment choice is locked at announcement;
//! 2. the resolution-time `EffectZoneChoice` for the attachment appears and
//!    offers the population that exists AT RESOLUTION (the response destroyed
//!    one Equipment after the announcement);
//! 3. the printed "any number of" cardinality lets the controller attach the
//!    whole (live) population, not exactly one object.
//!
//! Positive controls: a printed-target attachment (Brass Squire) still announces
//! BOTH roles, and a determined-operand clause ("attach it to ...") still
//! resolves without any resolution-time choice.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const BEATRIX: &str = "Vigilance (Attacking doesn't cause this creature to tap.)\nAt the beginning of combat on your turn, you may attach any number of Equipment you control to target creature you control.";
const ARDENN: &str = "At the beginning of combat on your turn, you may attach any number of Auras and Equipment you control to target permanent or player.\nPartner (You can have two commanders if both have partner.)";
const BRASS_SQUIRE: &str =
    "{T}: Attach target Equipment you control to target creature you control.";
const SHATTER: &str = "Destroy target artifact.";
const BALAN: &str = "First strike\nBalan has double strike as long as two or more Equipment are attached to it.\n{1}{W}: Attach all Equipment you control to Balan.";

/// One interjected instant cast, answered at the first matching active-player
/// priority window.
struct PriorityResponse {
    player: engine::types::player::PlayerId,
    spell: ObjectId,
    target: ObjectId,
}

/// Drive interactive windows until `stop` holds. `targets` answers declared
/// target prompts in declaration order; `attach_choices` answers a parked
/// resolution-time Attach choice (`WaitingFor::EffectZoneChoice` with
/// `effect_kind: EffectKind::Attach`) in FIFO order. Every other window panics,
/// so a missing prompt (revert) is an audible failure rather than a silent skip.
fn drive(
    runner: &mut GameRunner,
    targets: &mut Vec<ObjectId>,
    attach_choices: &mut Vec<ObjectId>,
    stop: impl Fn(&GameRunner) -> bool,
) {
    drive_with_response(runner, targets, attach_choices, None, stop);
}

fn drive_with_response(
    runner: &mut GameRunner,
    targets: &mut Vec<ObjectId>,
    attach_choices: &mut Vec<ObjectId>,
    mut response: Option<PriorityResponse>,
    mut stop: impl FnMut(&GameRunner) -> bool,
) {
    for _ in 0..160 {
        if stop(runner) {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            // CR 603.3b: the optional "you may" of a triggered ability.
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                if targets.is_empty() {
                    panic!(
                        "drive: a target prompt arrived with an empty queue: {:?}",
                        runner.state().waiting_for
                    );
                }
                GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(targets.remove(0))),
                }
            }
            // CR 115.10a + CR 608.2d: a described attachment operand is not a
            // target — it arrives as a resolution-time `EffectZoneChoice`.
            WaitingFor::EffectZoneChoice {
                cards,
                effect_kind: EffectKind::Attach,
                ..
            } => {
                if attach_choices.is_empty() {
                    panic!(
                        "drive: an Attach EffectZoneChoice arrived with an empty choice queue: \
                         cards={cards:?}"
                    );
                }
                GameAction::SelectCards {
                    cards: vec![attach_choices.remove(0)],
                }
            }
            WaitingFor::Priority { player } => {
                match response.take_if(|queued| queued.player == player) {
                    Some(PriorityResponse { spell, target, .. }) => {
                        targets.push(target);
                        GameAction::CastSpell {
                            object_id: spell,
                            card_id: runner.state().objects[&spell].card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        }
                    }
                    None => GameAction::PassPriority,
                }
            }
            other => panic!("drive: unexpected window {other:?}"),
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("drive: action rejected: {err:?}"));
    }
    panic!("drive: the stop condition was not reached within the iteration budget");
}

fn equipment(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, name, "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id()
}

/// CR 115.10a + CR 608.2d (maintainer finding): the described Equipment operand
/// is announced NOWHERE and chosen at resolution from the LIVE population.
#[test]
fn beatrix_equipment_is_chosen_at_resolution_from_the_live_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beatrix = scenario
        .add_creature_from_oracle(P0, "Beatrix, Loyal General", 2, 2, BEATRIX)
        .id();
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let equipment_a = equipment(&mut scenario, "Sword A");
    let equipment_b = equipment(&mut scenario, "Sword B");
    let opposing = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let shatter = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Shatter", true, SHATTER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);

    // Stop at the trigger's announcement prompt and inspect its slots.
    let mut targets: Vec<ObjectId> = Vec::new();
    let mut attach_choices: Vec<ObjectId> = Vec::new();
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        )
    });
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        unreachable!("drive stopped on a target prompt");
    };
    // REVERT DISCRIMINATOR 1: at base the clause is Stack-timed for BOTH roles,
    // so the Equipment claims a second announced slot and this fails.
    assert_eq!(
        target_slots.len(),
        1,
        "only the printed-target HOST may be announced; got slots {:?}",
        target_slots
            .iter()
            .map(|slot| slot.legal_targets.clone())
            .collect::<Vec<_>>()
    );
    let host_slot_legal = &target_slots[selection.current_slot].legal_targets;
    assert!(
        host_slot_legal.contains(&engine::types::ability::TargetRef::Object(host)),
        "the host creature must be the announced target, got {host_slot_legal:?}"
    );
    assert!(
        !host_slot_legal.contains(&engine::types::ability::TargetRef::Object(equipment_a))
            && !host_slot_legal.contains(&engine::types::ability::TargetRef::Object(equipment_b)),
        "the described Equipment must not appear among announced targets"
    );

    // Answer the announcement, then change the eligible population WHILE the
    // trigger is on the stack (the response destroys Equipment A).
    targets.push(host);
    let mut response = Some(PriorityResponse {
        player: P0,
        spell: shatter,
        target: equipment_a,
    });
    drive_with_response(
        &mut runner,
        &mut targets,
        &mut attach_choices,
        response.take(),
        |runner| {
            runner.state().objects[&equipment_a].zone != engine::types::zones::Zone::Battlefield
                || runner.state().stack.is_empty()
        },
    );
    assert_eq!(
        runner.state().objects[&equipment_a].zone,
        engine::types::zones::Zone::Graveyard,
        "reach-guard: the response really removed Equipment A before resolution"
    );

    // REVERT DISCRIMINATOR 2: the resolution-time choice must appear and offer
    // the LIVE population (Equipment B only — A is gone).
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            }
        )
    });
    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        unreachable!("drive stopped on the attach choice");
    };
    assert_eq!(
        cards,
        vec![equipment_b],
        "the resolution prompt must offer exactly the Equipment that exists at \
         resolution (A was destroyed in response)"
    );

    // Answer the choice and finish the trigger.
    attach_choices.push(equipment_b);
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    });

    assert_eq!(
        runner.state().objects[&equipment_b].attached_to,
        Some(AttachTarget::Object(host)),
        "the chosen Equipment must attach to the announced host"
    );
    assert_eq!(
        runner.state().objects[&beatrix].zone,
        engine::types::zones::Zone::Battlefield,
        "reach-guard: the trigger's source is still on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&opposing].zone,
        engine::types::zones::Zone::Battlefield,
        "reach-guard: the opponent's creature was never involved"
    );
}

/// CR 608.2d + CR 115.10a (the plan's B-1 hostile shape): a host that ITSELF
/// matches the attachment filter must never be consumed as the described
/// attachment operand. Ardenn's "target permanent or player" host can be the
/// very Equipment the described choice governs.
#[test]
fn ardenn_host_matching_the_attachment_filter_is_not_consumed_as_the_operand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Ardenn, Intrepid Archaeologist", 2, 2, ARDENN)
        .id();
    // CR 701.3a: an Equipment can only be attached to something it could equip,
    // so the hostile host must be a CREATURE that also carries the Equipment
    // subtype (the living-weapon class) — a legal "target permanent" that also
    // matches the attachment filter (`Or[Aura, Equipment] you control`).
    let host_equipment = scenario
        .add_creature(P0, "Living Sword", 1, 1)
        .as_artifact()
        // `as_artifact` strips the Creature type; an Equipment needs a legal
        // (creature) host, so restore it — the artifact-creature-Equipment class.
        .as_creature()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text("Equipped creature gets +1/+0.")
        .id();
    let equipment_b = equipment(&mut scenario, "Sword B");
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);

    let mut targets: Vec<ObjectId> = Vec::new();
    let mut attach_choices: Vec<ObjectId> = Vec::new();
    targets.push(host_equipment);
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            }
        )
    });

    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        unreachable!("drive stopped on the attach choice");
    };
    // REVERT DISCRIMINATOR (the B-1 gate): at base the
    // `explicit_attachment_target_chosen` shortcut sees the announced host in
    // `ability.targets` and matches it against the attachment filter, so NO
    // prompt is created at all and the host is read back as the operand.
    assert!(
        cards.contains(&equipment_b) && cards.contains(&host_equipment),
        "the described choice must offer the live Equipment population, got {cards:?}"
    );
    assert!(
        !(cards.len() == 1 && cards[0] == host_equipment),
        "the prompt must not degenerate to the announced host alone, got {cards:?}"
    );

    attach_choices.push(equipment_b);
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        runner.state().objects[&equipment_b].attached_to,
        Some(AttachTarget::Object(host_equipment)),
        "the selected Equipment must attach to the announced host"
    );
}

/// CR 107.1c + CR 608.2d: the printed "any number of" binds the WHOLE chosen set.
/// Two eligible Equipment at resolution, both answered, both attached — the
/// multi-select half of the described-attachment loop.
#[test]
fn beatrix_any_number_binds_the_whole_chosen_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Beatrix, Loyal General", 2, 2, BEATRIX)
        .id();
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let equipment_a = equipment(&mut scenario, "Sword A");
    let equipment_b = equipment(&mut scenario, "Sword B");
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);

    let mut targets: Vec<ObjectId> = vec![host];
    let mut attach_choices: Vec<ObjectId> = Vec::new();
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            }
        )
    });
    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        unreachable!("drive stopped on the attach choice");
    };
    assert!(
        cards.contains(&equipment_a) && cards.contains(&equipment_b),
        "both Equipment must be offered, got {cards:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![equipment_a, equipment_b],
        })
        .expect("selecting the whole eligible set must be legal");
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    });

    assert_eq!(
        runner.state().objects[&equipment_a].attached_to,
        Some(AttachTarget::Object(host)),
        "the first chosen Equipment must attach"
    );
    assert_eq!(
        runner.state().objects[&equipment_b].attached_to,
        Some(AttachTarget::Object(host)),
        "the second chosen Equipment must attach (the whole set binds)"
    );
}

/// CR 107.1c: "any number" includes zero — DECLINING Beatrix's parked attachment
/// choice attaches NOTHING. The parked choice must not fall back to the first
/// eligible object when the answer is an empty set.
#[test]
fn beatrix_declining_the_any_number_choice_attaches_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Beatrix, Loyal General", 2, 2, BEATRIX)
        .id();
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let equipment_a = equipment(&mut scenario, "Sword A");
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::BeginCombat);

    let mut targets: Vec<ObjectId> = vec![host];
    let mut attach_choices: Vec<ObjectId> = Vec::new();
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            }
        )
    });
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining an any-number attachment choice must be legal");
    drive(&mut runner, &mut targets, &mut attach_choices, |runner| {
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    });

    assert_eq!(
        runner.state().objects[&equipment_a].attached_to,
        None,
        "declining \"any number\" must attach nothing (CR 107.1c)"
    );
    assert!(
        runner.state().objects[&host].attachments.is_empty(),
        "the announced host must receive nothing, got {:?}",
        runner.state().objects[&host].attachments
    );
}

/// CR 107.1c + CR 608.2d: "attach all Equipment you control to Balan" is a
/// DETERMINED set — EVERY matching Equipment attaches with NO player choice.
/// Revert discriminator: without the determined-set path the resolution offers
/// a single-choice `EffectZoneChoice` (two eligible) and only one attaches.
#[test]
fn balan_attaches_every_matching_equipment_without_a_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::White,
                ObjectId(0),
                false,
                vec![],
            ),
        ],
    );
    let balan = scenario
        .add_creature_from_oracle(P0, "Balan, Wandering Knight", 3, 3, BALAN)
        .id();
    let equipment_a = equipment(&mut scenario, "Sword A");
    let equipment_b = equipment(&mut scenario, "Sword B");
    // A second controller's Equipment is NOT in "Equipment you control".
    let opposing_equipment = scenario
        .add_artifact_from_oracle(P1, "Sword C", "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&balan]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|d| d.contains("Attach all"))
        })
        .expect("Balan must carry the attach-all activated ability");
    runner.activate(balan, ability_index).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a determined set must not prompt; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&equipment_a].attached_to,
        Some(AttachTarget::Object(balan)),
        "the first matching Equipment must attach"
    );
    assert_eq!(
        runner.state().objects[&equipment_b].attached_to,
        Some(AttachTarget::Object(balan)),
        "the second matching Equipment must attach (the whole determined set)"
    );
    assert_eq!(
        runner.state().objects[&balan].attachments.len(),
        2,
        "reach-guard: exactly the controller's two Equipment are attached, got {:?}",
        runner.state().objects[&balan].attachments
    );
    assert_eq!(
        runner.state().objects[&opposing_equipment].attached_to,
        None,
        "an opponent's Equipment is outside the printed set"
    );
}

/// CR 608.2d: a determined set with nothing matching does nothing — no prompt,
/// no error, no attachment.
#[test]
fn balan_with_no_equipment_does_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::White,
                ObjectId(0),
                false,
                vec![],
            ),
        ],
    );
    let balan = scenario
        .add_creature_from_oracle(P0, "Balan, Wandering Knight", 3, 3, BALAN)
        .id();
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&balan]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|d| d.contains("Attach all"))
        })
        .expect("Balan must carry the attach-all activated ability");
    runner.activate(balan, ability_index).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an empty determined set must resolve quietly; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().objects[&balan].attachments.is_empty(),
        "nothing matches, so nothing attaches, got {:?}",
        runner.state().objects[&balan].attachments
    );
}

/// Paired positive control: a PRINTED-target attachment (Brass Squire) still
/// announces both roles, and its attach lands. This is the unchanged
/// counterpart that proves the new per-role gate is not a blanket demotion.
#[test]
fn brass_squire_still_announces_both_printed_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let squire = scenario
        .add_creature_from_oracle(P0, "Brass Squire", 1, 3, BRASS_SQUIRE)
        .id();
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let equipment_a = equipment(&mut scenario, "Sword A");
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&squire]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|d| d.contains("Attach"))
        })
        .expect("Brass Squire must carry the attach activated ability");

    runner
        .activate(squire, ability_index)
        .target_object(equipment_a)
        .target_object(host)
        .resolve();

    assert_eq!(
        runner.state().objects[&equipment_a].attached_to,
        Some(AttachTarget::Object(host)),
        "a printed-target attachment resolves through its announced slots, unchanged"
    );
}
