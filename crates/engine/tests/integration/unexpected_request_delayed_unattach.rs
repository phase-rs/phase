//! Unexpected Request: "Gain control of target creature until end of turn.
//! Untap that creature. It gains haste until end of turn. You may attach an
//! Equipment you control to that creature. If you do, unattach it at the
//! beginning of the next end step." (phase-rs/phase#9771)
//!
//! CR 701.3d: unattaching moves the Equipment off its host; it stays on the
//! battlefield. CR 603.7: the unattach is a delayed triggered ability created
//! as the spell resolves. CR 608.2c: "it" names the Equipment the Attach
//! instruction attached, which the delayed ability carries. CR 603.7c + CR
//! 400.7: an Equipment that left the battlefield and came back is a new object
//! the delayed ability no longer affects.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, OptionalPolicy, P0, P1};
use engine::types::ability::{ChoiceType, DelayedTriggerCondition, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const UNEXPECTED_REQUEST: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. You may attach an Equipment you control to that creature. If you do, unattach it at the beginning of the next end step.";
const SHUKO: &str = "Equipped creature gets +1/+0.\nEquip {0}";
const SANCTUARY_BLADE: &str = "As this Equipment becomes attached to a creature, choose a color.\nEquipped creature gets +2/+0 and has protection from the last chosen color.\nEquip {3}";
const PSYCHIC_PAPER: &str = "As this Equipment becomes attached to a creature, choose a creature card name and a creature type.\nEquipped creature has ward {1}, it can't be blocked, and its name and creature type are the last chosen name and creature type.\nEquip {2}";
const PLANAR_INCISION: &str = "Exile target artifact or creature, then return it to the battlefield under its owner's control with a +1/+1 counter on it.";
const TAMIYOS_COMPLEATION: &str = "Flash\nEnchant artifact, creature, or planeswalker\nWhen this Aura enters, tap enchanted permanent. If it's an Equipment, unattach it.\nEnchanted permanent loses all abilities and doesn't untap during its controller's untap step.";
const GALADHRIM_BOW: &str = "Flash\nWhen this Equipment enters, attach it to target creature you control. Untap that creature.\nEquipped creature gets +1/+2 and has reach.\nEquip {2}";

/// How the driver answers the resolution's prompts.
#[derive(Clone, Copy)]
struct Answers {
    /// How the driver answers the optional "you may attach" (CR 608.2d).
    attach: OptionalPolicy,
    /// The Equipment to pick when the attach offers a choice.
    pick: Option<ObjectId>,
}

const ACCEPT: Answers = Answers {
    attach: OptionalPolicy::Accept,
    pick: None,
};
const DECLINE: Answers = Answers {
    attach: OptionalPolicy::Decline,
    pick: None,
};

/// Every prompt the driver answered, with the player it was offered to.
#[derive(Default, Debug)]
struct Seen {
    optional_offers: Vec<PlayerId>,
    attach_choices: Vec<(PlayerId, Vec<ObjectId>)>,
    color_choices: Vec<PlayerId>,
    /// Card-name and creature-type choices, in the order they were asked.
    named_choices: Vec<(PlayerId, &'static str)>,
}

fn drive(r: &mut GameRunner, answers: Answers, seen: &mut Seen) {
    for _ in 0..48 {
        match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    return;
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                seen.optional_offers.push(player);
                let accept = match answers.attach {
                    OptionalPolicy::Accept => true,
                    OptionalPolicy::Decline => false,
                    OptionalPolicy::Stop => return,
                };
                r.act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer the attach");
            }
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                seen.attach_choices.push((player, cards.clone()));
                let pick = answers
                    .pick
                    .expect("the fixture names the Equipment to pick");
                r.act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("pick the Equipment");
            }
            WaitingFor::NamedChoice {
                player,
                choice_type: ChoiceType::Color { .. },
                options,
                ..
            } => {
                seen.color_choices.push(player);
                r.act(GameAction::ChooseOption {
                    choice: options[0].clone(),
                })
                .expect("choose a color");
            }
            WaitingFor::NamedChoice {
                player,
                choice_type: ChoiceType::CardName { .. },
                ..
            } => {
                seen.named_choices.push((player, "card name"));
                r.act(GameAction::ChooseOption {
                    choice: "Grizzly Bears".to_string(),
                })
                .expect("choose a card name");
            }
            WaitingFor::NamedChoice {
                player,
                choice_type: ChoiceType::CreatureType { .. },
                ..
            } => {
                seen.named_choices.push((player, "creature type"));
                r.act(GameAction::ChooseOption {
                    choice: "Bear".to_string(),
                })
                .expect("choose a creature type");
            }
            WaitingFor::DeclareAttackers { .. } => {
                r.act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("no attack");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the driver did not settle");
}

/// Advance from the current main phase into the end step and let the
/// beginning-of-end-step triggers resolve.
fn run_through_end_step(r: &mut GameRunner, seen: &mut Seen) {
    for _ in 0..4 {
        if r.state().phase == Phase::End {
            break;
        }
        r.advance_to_end_step();
        drive(r, DECLINE, seen);
    }
    assert_eq!(r.state().phase, Phase::End, "reach: the end step began");
    drive(r, DECLINE, seen);
    assert!(
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "the end-step triggers finished"
    );
}

fn attach_directly(r: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    r.state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    let host_obj = r.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&attachment) {
        host_obj.attachments.push(attachment);
    }
    r.state_mut().layers_dirty.mark_full();
}

fn host_of(r: &GameRunner, attachment: ObjectId) -> Option<AttachTarget> {
    r.state().objects[&attachment].attached_to
}

fn equipment(s: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    s.add_artifact_from_oracle(player, name, text)
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn shuko(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    equipment(s, player, "Shuko", SHUKO)
}

fn request_in_hand(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Unexpected Request", false, UNEXPECTED_REQUEST)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// The single delayed unattach Unexpected Request created, and the object it
/// names.
fn delayed_unattach_referent(r: &GameRunner) -> ObjectId {
    let [delayed] = r.state().delayed_triggers.as_slice() else {
        panic!(
            "exactly one delayed trigger, got {:?}",
            r.state().delayed_triggers
        );
    };
    assert_eq!(
        delayed.condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    assert!(matches!(delayed.ability.effect, Effect::UnattachAll { .. }));
    let [TargetRef::Object(id)] = delayed.ability.targets.as_slice() else {
        panic!(
            "the delayed unattach names one object, got {:?}",
            delayed.ability.targets
        );
    };
    *id
}

/// Casts Unexpected Request on the tapped `victim` and resolves it to an empty
/// stack, asserting the gain-control, untap and haste instructions happened and
/// that P0 alone was asked about the attach.
fn cast_request(r: &mut GameRunner, request: ObjectId, victim: ObjectId, answers: Answers) -> Seen {
    r.state_mut().objects.get_mut(&victim).unwrap().tapped = true;
    r.cast(request).target_object(victim).commit();
    let mut seen = Seen::default();
    drive(r, answers, &mut seen);
    assert!(
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the spell finished resolving"
    );
    let v = &r.state().objects[&victim];
    assert_eq!(
        (v.controller, v.tapped, v.has_keyword(&Keyword::Haste)),
        (P0, false, true),
        "reach: control, untap and haste resolved ({seen:?})"
    );
    assert_eq!(
        seen.optional_offers,
        vec![P0],
        "reach: P0 was offered the attach once"
    );
    seen
}

#[test]
fn unexpected_request_unattaches_at_next_end_step() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shuko = shuko(&mut s, P0);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();

    let mut seen = cast_request(&mut r, request, victim, ACCEPT);
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(victim)),
        "reach: Shuko was attached"
    );
    assert_eq!(
        delayed_unattach_referent(&r),
        shuko,
        "\"it\" names the Equipment, not the creature"
    );

    run_through_end_step(&mut r, &mut seen);
    // CR 701.3d: unattached, and still on the battlefield.
    assert_eq!(host_of(&r, shuko), None, "Shuko is unattached");
    assert_eq!(r.state().objects[&shuko].zone, Zone::Battlefield);
    assert!(
        r.state().delayed_triggers.is_empty(),
        "the one-shot delayed unattach fired"
    );
}

#[test]
fn declined_attach_creates_no_delayed_unattach() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shuko = shuko(&mut s, P0);
    let bears = s.add_vanilla(P0, 2, 2);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();
    attach_directly(&mut r, shuko, bears);

    let mut seen = cast_request(&mut r, request, victim, DECLINE);
    assert!(
        r.state().delayed_triggers.is_empty(),
        "\"If you do\": no attach, no delayed unattach"
    );

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(bears)),
        "the Equipment that was never moved keeps its host"
    );
}

/// Two Equipment are eligible, so the attach asks which one. Only the chosen
/// Equipment is "it"; the other one, and the opponent's Equipment already on
/// the stolen creature, are untouched.
#[test]
fn only_the_attached_equipment_is_unattached() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let first = equipment(&mut s, P0, "Shuko", SHUKO);
    let second = equipment(&mut s, P0, "Second Shuko", SHUKO);
    let theirs = equipment(&mut s, P1, "Their Shuko", SHUKO);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();
    attach_directly(&mut r, theirs, victim);

    let mut seen = cast_request(
        &mut r,
        request,
        victim,
        Answers {
            attach: OptionalPolicy::Accept,
            pick: Some(second),
        },
    );
    let [(chooser, offered)] = seen.attach_choices.as_slice() else {
        panic!("reach: the attach asked once which Equipment, got {seen:?}");
    };
    let mut offered = offered.clone();
    offered.sort();
    assert_eq!(
        (*chooser, offered),
        (P0, vec![first, second]),
        "reach: P0 chose between its own two Equipment"
    );
    assert_eq!(host_of(&r, second), Some(AttachTarget::Object(victim)));
    assert_eq!(host_of(&r, theirs), Some(AttachTarget::Object(victim)));
    assert_eq!(delayed_unattach_referent(&r), second);

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(
        host_of(&r, second),
        None,
        "the chosen Equipment is unattached"
    );
    assert_eq!(
        host_of(&r, first),
        None,
        "the unchosen Equipment never moved"
    );
    assert_eq!(
        host_of(&r, theirs),
        Some(AttachTarget::Object(victim)),
        "the opponent's Equipment is not \"it\""
    );
}

/// CR 701.3a + CR 608.2c: Sanctuary Blade's printed "as this becomes attached,
/// choose a color" is followed during the attach; the delayed unattach still
/// names the Blade. Only one Equipment is eligible, so no selection is asked.
#[test]
fn attach_time_choice_keeps_the_unattach_referent() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let blade = equipment(&mut s, P0, "Sanctuary Blade", SANCTUARY_BLADE);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();

    let mut seen = cast_request(&mut r, request, victim, ACCEPT);
    assert!(seen.attach_choices.is_empty(), "reach: no selection prompt");
    assert_eq!(
        seen.color_choices,
        vec![P0],
        "reach: P0 chose the Blade's color"
    );
    assert_eq!(host_of(&r, blade), Some(AttachTarget::Object(victim)));
    assert_eq!(delayed_unattach_referent(&r), blade);

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(host_of(&r, blade), None, "the Blade is unattached");
}

/// The two pauses together: P0 selects Sanctuary Blade from two Equipment, and
/// the attach then pauses for the Blade's color. The delayed unattach still
/// names the Blade, and Shuko is untouched.
#[test]
fn selected_equipment_with_attach_time_choice_keeps_the_unattach_referent() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let blade = equipment(&mut s, P0, "Sanctuary Blade", SANCTUARY_BLADE);
    let shuko = shuko(&mut s, P0);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();

    let mut seen = cast_request(
        &mut r,
        request,
        victim,
        Answers {
            attach: OptionalPolicy::Accept,
            pick: Some(blade),
        },
    );
    let [(chooser, offered)] = seen.attach_choices.as_slice() else {
        panic!("reach: the attach asked once which Equipment, got {seen:?}");
    };
    let mut offered = offered.clone();
    offered.sort();
    let mut both = vec![blade, shuko];
    both.sort();
    assert_eq!(
        (*chooser, offered),
        (P0, both),
        "reach: both Equipment offered to P0"
    );
    assert_eq!(
        seen.color_choices,
        vec![P0],
        "reach: P0 chose the Blade's color"
    );
    assert_eq!(host_of(&r, blade), Some(AttachTarget::Object(victim)));
    assert_eq!(delayed_unattach_referent(&r), blade);

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(host_of(&r, blade), None, "the Blade is unattached");
    assert_eq!(host_of(&r, shuko), None, "Shuko never moved");
}

/// Psychic Paper's attach-time replacement asks two questions (a card name,
/// then a creature type); the second is parked as its own continuation above
/// the first's post-replacement frame. The delayed unattach still names the
/// Paper, and Shuko is untouched.
#[test]
fn selected_equipment_with_two_attach_time_choices_keeps_the_unattach_referent() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let paper = equipment(&mut s, P0, "Psychic Paper", PSYCHIC_PAPER);
    let shuko = shuko(&mut s, P0);
    let victim = s
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let request = request_in_hand(&mut s);
    let mut r = s.build();
    r.state_mut().all_card_names = vec!["Grizzly Bears".to_string()].into();
    r.state_mut().all_creature_types.push("Bear".to_string());
    let paper_incarnation = r.state().objects[&paper].incarnation;

    let mut seen = cast_request(
        &mut r,
        request,
        victim,
        Answers {
            attach: OptionalPolicy::Accept,
            pick: Some(paper),
        },
    );
    let [(chooser, offered)] = seen.attach_choices.as_slice() else {
        panic!("reach: the attach asked once which Equipment, got {seen:?}");
    };
    let mut offered = offered.clone();
    offered.sort();
    let mut both = vec![paper, shuko];
    both.sort();
    assert_eq!(
        (*chooser, offered),
        (P0, both),
        "reach: both Equipment offered to P0"
    );
    assert_eq!(
        seen.named_choices,
        vec![(P0, "card name"), (P0, "creature type")],
        "reach: P0 chose the Paper's name, then its type, once each"
    );
    assert_eq!(host_of(&r, paper), Some(AttachTarget::Object(victim)));
    assert_eq!(host_of(&r, shuko), None, "reach: Shuko never moved");
    assert_eq!(delayed_unattach_referent(&r), paper);
    assert_eq!(
        r.state().delayed_triggers[0].ability.target_incarnations[0].incarnation,
        paper_incarnation,
        "the delayed unattach names the Paper's original incarnation"
    );

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(host_of(&r, paper), None, "the Paper is unattached");
    assert_eq!(r.state().objects[&paper].zone, Zone::Battlefield);
    assert_eq!(host_of(&r, shuko), None);
    assert_eq!(
        seen.named_choices.len(),
        2,
        "no name or type prompt was repeated"
    );
}

/// CR 603.7c + CR 400.7: Shuko left the battlefield and returned, so it is a
/// new object. Equipping the new object to P0's own creature is not undone.
#[test]
fn returned_equipment_is_a_new_object() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shuko = shuko(&mut s, P0);
    let bears = s.add_vanilla(P0, 2, 2);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let incision = s
        .add_spell_to_hand_from_oracle(P0, "Planar Incision", true, PLANAR_INCISION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();

    let mut seen = cast_request(&mut r, request, victim, ACCEPT);
    assert_eq!(host_of(&r, shuko), Some(AttachTarget::Object(victim)));
    assert_eq!(delayed_unattach_referent(&r), shuko);
    let incarnation_before = r.state().objects[&shuko].incarnation;

    r.cast(incision).target_object(shuko).commit();
    drive(&mut r, DECLINE, &mut seen);
    assert_eq!(r.state().objects[&shuko].zone, Zone::Battlefield);
    assert_ne!(
        r.state().objects[&shuko].incarnation,
        incarnation_before,
        "reach: Shuko left and returned as a new object"
    );
    assert_eq!(host_of(&r, shuko), None, "reach: it returned unattached");
    r.activate(shuko, 0).target_object(bears).resolve();
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(bears)),
        "reach: the new object was equipped to P0's creature"
    );

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(bears)),
        "the new object is not \"it\" and stays attached"
    );
}

/// Moving the same Equipment to another creature is not a zone change, so it
/// is still "it" (CR 603.7c), and CR 701.3d unattaches it from whatever it is
/// attached to.
#[test]
fn reequipped_equipment_is_still_unattached() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shuko = shuko(&mut s, P0);
    let bears = s.add_vanilla(P0, 2, 2);
    let victim = s.add_vanilla(P1, 2, 2);
    let request = request_in_hand(&mut s);
    let mut r = s.build();

    let mut seen = cast_request(&mut r, request, victim, ACCEPT);
    assert_eq!(host_of(&r, shuko), Some(AttachTarget::Object(victim)));
    let incarnation = r.state().objects[&shuko].incarnation;
    r.activate(shuko, 0).target_object(bears).resolve();
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(bears)),
        "reach: Shuko moved to P0's creature"
    );
    assert_eq!(
        r.state().objects[&shuko].incarnation,
        incarnation,
        "reach: equipping is not a zone change"
    );

    run_through_end_step(&mut r, &mut seen);
    assert_eq!(host_of(&r, shuko), None, "the same Equipment is unattached");
}

fn is_bow_trigger(entry: &engine::types::game_state::StackEntry, bow: ObjectId) -> bool {
    entry.source_id == bow && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
}

/// A printed Attach that is not followed by "unattach it" keeps its own
/// follower's referent: Galadhrim Bow's "Untap that creature" untaps the
/// creature, not the Bow.
#[test]
fn galadhrim_bow_untaps_the_creature_it_was_attached_to() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let bears = s.add_vanilla(P0, 2, 2);
    let bow = s
        .add_spell_to_hand(P0, "Galadhrim Bow", true)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["flash"], GALADHRIM_BOW)
        .id();
    let mut r = s.build();
    r.state_mut().objects.get_mut(&bears).unwrap().tapped = true;

    r.cast(bow).commit();
    // Resolve the Bow spell until its enters trigger is on the stack, answering
    // its target prompt if one is asked.
    for _ in 0..8 {
        if r.state()
            .stack
            .iter()
            .any(|entry| is_bow_trigger(entry, bow))
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match r.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { player, .. } => {
                assert_eq!(player, P0, "reach: P0 targets with the Bow's trigger");
                r.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bears)),
                })
                .expect("target Bears");
            }
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    let trigger = r
        .state()
        .stack
        .iter()
        .find(|entry| is_bow_trigger(entry, bow))
        .expect("reach: the Bow's enters trigger is on the stack");
    assert_eq!(
        trigger.ability().map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(bears)]),
        "reach: the trigger targets Bears"
    );
    let mut seen = Seen::default();
    drive(&mut r, DECLINE, &mut seen);
    assert!(
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the trigger finished resolving"
    );
    assert_eq!(
        host_of(&r, bow),
        Some(AttachTarget::Object(bears)),
        "reach: the Bow attached to Bears"
    );
    assert!(!r.state().objects[&bears].tapped, "Bears untapped");
    assert!(!r.state().objects[&bow].tapped, "the Bow was never tapped");
}

/// The same "unattach it" building block outside a delayed trigger: Tamiyo's
/// Compleation names the enchanted permanent.
/// Which permanent Tamiyo's Compleation enchants.
#[derive(Clone, Copy)]
enum CompleationHost {
    /// The Equipment attached to the creature.
    Equipment,
    /// The creature wearing that Equipment.
    Creature,
}

fn compleation_on(host: CompleationHost) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let shuko = shuko(&mut s, P1);
    let wearer = s.add_vanilla(P1, 2, 2);
    let aura = s
        .add_spell_to_hand(P0, "Tamiyo's Compleation", true)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["flash", "enchant"], TAMIYOS_COMPLEATION)
        .id();
    let mut r = s.build();
    attach_directly(&mut r, shuko, wearer);
    let target = match host {
        CompleationHost::Equipment => shuko,
        CompleationHost::Creature => wearer,
    };
    r.cast(aura).target_object(target).commit();
    let mut seen = Seen::default();
    drive(&mut r, DECLINE, &mut seen);
    assert!(
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: the Aura and its enters trigger finished resolving ({seen:?})"
    );
    assert!(
        r.state().objects[&target].tapped,
        "reach: the enters trigger tapped the enchanted permanent"
    );
    (r, aura, shuko, wearer)
}

#[test]
fn tamiyos_compleation_unattaches_an_enchanted_equipment() {
    let (r, aura, shuko, _) = compleation_on(CompleationHost::Equipment);
    assert_eq!(
        host_of(&r, shuko),
        None,
        "the enchanted Equipment is unattached"
    );
    assert_eq!(r.state().objects[&shuko].zone, Zone::Battlefield);
    assert_eq!(
        host_of(&r, aura),
        Some(AttachTarget::Object(shuko)),
        "the Aura stays on the Equipment"
    );
}

#[test]
fn tamiyos_compleation_leaves_a_creatures_equipment_alone() {
    let (r, aura, shuko, wearer) = compleation_on(CompleationHost::Creature);
    assert_eq!(host_of(&r, aura), Some(AttachTarget::Object(wearer)));
    assert_eq!(
        host_of(&r, shuko),
        Some(AttachTarget::Object(wearer)),
        "the enchanted permanent is not an Equipment, so nothing is unattached"
    );
}
