//! "<type> attached to that creature" — the declared-slot attachment referent.
//!
//! Light of Judgment (FIN) — "Light of Judgment deals 6 damage to target
//! creature. Destroy up to one Equipment attached to that creature." — and its
//! class: Turn to Slag, Blastfire Bolt, Fires of Mount Doom ("Destroy all
//! Equipment attached to that creature") and Fiery Annihilation ("Exile up to
//! one target Equipment attached to that creature. If that creature would die
//! this turn, exile it instead.").
//!
//! "that creature" names the object announced for an earlier declared target
//! slot, so the relation lowers to
//! `FilterProp::AttachedTo { to: AttachmentReferent::DeclaredTarget { slot } }`
//! and the referent is read by slot, never as "the first object target".
//!
//! CR set (each verified against `docs/MagicCompRules.txt`):
//! CR 115.3 (one object may fill several "target" instances), CR 115.10a
//! (only the word "target" makes a target), CR 601.2c (announcing targets),
//! CR 608.2b (illegal targets; information about an illegal target is not
//! determined), CR 608.2c (instructions in order), CR 608.2d (choices made
//! while applying the effect), CR 608.2h (last known information), CR 614.1a
//! (replacement effects), CR 701.3a (attach), CR 701.8a (destroy),
//! CR 702.12b (indestructible), CR 704.3 (no state-based actions during resolution),
//! CR 400.7 (a moved object is a new object).
//!
//! Oracle text is verbatim from Scryfall.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach;
use engine::game::game_object::AttachTarget;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, preview_interaction, submit_interaction,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, AttachmentReferent, Effect, FilterProp, QuantityExpr,
    QuantityRef, TapStateChange, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionPreviewRequest, InteractionPreviewStatus,
    InteractionReasonCode, InteractionResponse, InteractionSessionId, InteractionSubmission,
    PreviewRequestId,
};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

pub(crate) const LIGHT_OF_JUDGMENT: &str =
    "Light of Judgment deals 6 damage to target creature. Destroy up to one Equipment attached to that creature.";
pub(crate) const TURN_TO_SLAG: &str =
    "Turn to Slag deals 5 damage to target creature. Destroy all Equipment attached to that creature.";
pub(crate) const BLASTFIRE_BOLT: &str =
    "Blastfire Bolt deals 5 damage to target creature. Destroy all Equipment attached to that creature.";
pub(crate) const FIRES_OF_MOUNT_DOOM: &str = "When Fires of Mount Doom enters, it deals 2 damage to target creature an opponent controls. Destroy all Equipment attached to that creature.\n{2}{R}: Exile the top card of your library. You may play that card this turn. When you play a card this way, Fires of Mount Doom deals 2 damage to each player.";
pub(crate) const FIERY_ANNIHILATION: &str = "Fiery Annihilation deals 5 damage to target creature. Exile up to one target Equipment attached to that creature. If that creature would die this turn, exile it instead.";
const TREEFOLK_MYSTIC: &str = "Whenever this creature blocks or becomes blocked by a creature, destroy all Auras attached to that creature.";
const CORROSIVE_OOZE: &str = "Whenever this creature blocks or becomes blocked by an equipped creature, destroy all Equipment attached to that creature at end of combat.";
const SHACKLES_OF_TREACHERY: &str = "Gain control of target creature until end of turn. Untap that creature. Until end of turn, it gains haste and \"Whenever this creature deals damage, destroy target Equipment attached to it.\"";

fn types(core: &str) -> Vec<String> {
    vec![core.to_string()]
}

/// Every effect of `def`'s `sub_ability` line, head first.
pub(crate) fn chain(def: &AbilityDefinition) -> Vec<&Effect> {
    std::iter::successors(Some(def), |d| d.sub_ability.as_deref())
        .map(|d| &*d.effect)
        .collect()
}

fn unimplemented_names(defs: &[&AbilityDefinition]) -> Vec<String> {
    defs.iter()
        .flat_map(|d| chain(d))
        .filter_map(|e| match e {
            Effect::Unimplemented { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The declared-slot referents carried anywhere on `filter`'s typed legs.
pub(crate) fn declared_slots(filter: &TargetFilter) -> Vec<usize> {
    let mut slots = Vec::new();
    fn walk(f: &TargetFilter, out: &mut Vec<usize>) {
        match f {
            TargetFilter::Typed(tf) => {
                for p in &tf.properties {
                    if let FilterProp::AttachedTo {
                        to: AttachmentReferent::DeclaredTarget { slot },
                    } = p
                    {
                        out.push(*slot);
                    }
                }
            }
            TargetFilter::Or { filters } | TargetFilter::And { filters } => {
                filters.iter().for_each(|x| walk(x, out))
            }
            _ => {}
        }
    }
    walk(filter, &mut slots);
    slots
}

/// SHAPE: Light of Judgment's second sentence is a resolution-time selection of
/// up to one Equipment attached to declared slot 0, then destruction of exactly
/// the chosen set (CR 115.10a + CR 608.2d).
#[test]
fn light_of_judgment_lowers_to_resolution_choice_of_slot_zero_attachments() {
    let parsed = parse_oracle_text(
        LIGHT_OF_JUDGMENT,
        "Light of Judgment",
        &[],
        &types("Instant"),
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let effects = chain(&parsed.abilities[0]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard: no gap may remain, got {effects:?}"
    );
    assert!(matches!(effects[0], Effect::DealDamage { .. }));
    let Effect::ChooseObjectsIntoTrackedSet {
        filter, min, max, ..
    } = effects[1]
    else {
        panic!("second node must be the resolution-time choice, got {effects:?}");
    };
    assert_eq!((*min, *max), (0, Some(1)), "up to one");
    assert_eq!(declared_slots(filter), vec![0]);
    assert!(
        matches!(
            effects[2],
            Effect::DestroyAll {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "third node destroys exactly the chosen set, got {effects:?}"
    );
    assert!(
        parsed.abilities[0]
            .sub_ability
            .as_ref()
            .is_some_and(|s| s.multi_target.is_none()),
        "the untargeted choice must not leak a multi-target spec"
    );
}

/// SHAPE: the "all" siblings lower to a `DestroyAll` over slot 0's attachments.
#[test]
fn destroy_all_equipment_attached_to_that_creature_siblings() {
    for (oracle, name) in [
        (TURN_TO_SLAG, "Turn to Slag"),
        (BLASTFIRE_BOLT, "Blastfire Bolt"),
    ] {
        let parsed = parse_oracle_text(oracle, name, &[], &types("Sorcery"), &[]);
        let effects = chain(&parsed.abilities[0]);
        assert!(
            unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
            "{name}: reach guard, got {effects:?}"
        );
        let Effect::DestroyAll { target, .. } = effects[1] else {
            panic!("{name}: expected DestroyAll, got {effects:?}");
        };
        assert_eq!(declared_slots(target), vec![0], "{name}");
    }
}

/// SHAPE: the ETB trigger body binds "that creature" to the trigger's own
/// declared slot 0.
#[test]
fn fires_of_mount_doom_trigger_body_binds_slot_zero() {
    let parsed = parse_oracle_text(
        FIRES_OF_MOUNT_DOOM,
        "Fires of Mount Doom",
        &[],
        &types("Enchantment"),
        &[],
    );
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("ETB trigger body");
    let effects = chain(execute);
    assert!(
        unimplemented_names(&[execute]).is_empty(),
        "reach guard, got {effects:?}"
    );
    let Effect::DestroyAll { target, .. } = effects[1] else {
        panic!("expected DestroyAll, got {effects:?}");
    };
    assert_eq!(declared_slots(target), vec![0]);
}

/// RUNTIME (CR 603.3d + CR 601.2c + CR 608.2b + CR 701.8a): Fires of Mount
/// Doom is cast and enters; its ETB trigger announces "target creature an
/// opponent controls" through `TriggerTargetSelection` (the trigger path, whose
/// writer journals the trigger body's pin, not the cast path). On resolution it
/// deals 2 damage to that creature and destroys exactly the Equipment attached
/// to it: Equipment on another creature and unattached Equipment survive.
#[test]
fn fires_of_mount_doom_etb_destroys_only_the_targets_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 7).id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 7).id();
    let on_victim = equipment(&mut scenario, P1, "On Victim");
    let on_other = equipment(&mut scenario, P1, "On Other");
    let loose = equipment(&mut scenario, P1, "Unattached");
    let fires = scenario
        .add_spell_to_hand_from_oracle(P0, "Fires of Mount Doom", false, FIRES_OF_MOUNT_DOOM)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), on_victim, victim);
    attach::attach_to(runner.state_mut(), on_other, other);
    runner.cast(fires).commit();
    let mut offered: Option<Vec<TargetRef>> = None;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                offered = Some(target_slots[selection.current_slot].legal_targets.clone());
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .expect("the victim is a legal trigger target");
            }
            WaitingFor::Priority { .. } => {
                if offered.is_some() && runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    let offered = offered.expect("reach guard: the ETB trigger asked for its target");
    assert!(offered.contains(&TargetRef::Object(victim)));
    assert!(offered.contains(&TargetRef::Object(other)));
    assert!(
        !offered.contains(&TargetRef::Object(mine)),
        "an opponent's creature only"
    );
    let state = runner.state();
    assert_eq!(
        state.objects[&fires].zone,
        Zone::Battlefield,
        "Fires entered"
    );
    assert_eq!(
        state.objects[&victim].damage_marked, 2,
        "2 damage to the target"
    );
    assert_eq!(state.objects[&other].damage_marked, 0);
    assert_eq!(
        state.objects[&on_victim].zone,
        Zone::Graveyard,
        "the target's Equipment is destroyed"
    );
    assert_eq!(state.objects[&on_other].zone, Zone::Battlefield);
    assert_eq!(state.objects[&loose].zone, Zone::Battlefield);
}

/// SHAPE: Fiery Annihilation's Equipment target is narrowed to slot 0's
/// attachments, and the death-exile rider is bound to the creature's slot, not
/// to the Equipment node that precedes it (CR 614.1a).
#[test]
fn fiery_annihilation_narrows_equipment_slot_and_binds_rider_to_creature_slot() {
    let parsed = parse_oracle_text(
        FIERY_ANNIHILATION,
        "Fiery Annihilation",
        &[],
        &types("Instant"),
        &[],
    );
    let effects = chain(&parsed.abilities[0]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard, got {effects:?}"
    );
    let Effect::ChangeZone { target, .. } = effects[1] else {
        panic!("expected the Equipment exile, got {effects:?}");
    };
    assert_eq!(declared_slots(target), vec![0]);
    let Effect::AddTargetReplacement { target, .. } = effects[2] else {
        panic!("expected the death-exile rider, got {effects:?}");
    };
    assert_eq!(*target, TargetFilter::ParentTargetSlot { index: 0 });
}

/// Out of class: "that creature" names no earlier declared target slot (a
/// trigger's other combatant; a granted trigger's own source). The fail-closed
/// gap stays. Paired with a reach guard that the clause was reached.
#[test]
fn referents_without_a_declared_slot_keep_their_gap() {
    for (oracle, name, core, gap) in [
        (
            TREEFOLK_MYSTIC,
            "Treefolk Mystic",
            "Creature",
            "attached_to_qualifier",
        ),
        (
            CORROSIVE_OOZE,
            "Corrosive Ooze",
            "Creature",
            "attached_to_qualifier",
        ),
        (
            SHACKLES_OF_TREACHERY,
            "Shackles of Treachery",
            "Sorcery",
            "unparsed_verb_arguments",
        ),
    ] {
        let parsed = parse_oracle_text(oracle, name, &[], &types(core), &[]);
        let json = serde_json::to_string(&parsed).expect("serialize parse");
        assert!(
            json.contains(gap),
            "{name}: the attachment clause must keep its `{gap}` gap: {json}"
        );
        assert!(
            !json.contains("DeclaredTarget"),
            "{name}: no declared-slot referent may be invented: {json}"
        );
    }
}

/// Two earlier creature targets make "that creature" ambiguous: no
/// nearest-declarer rule, the gap stays. Reach guard: the unambiguous control
/// with distinct nouns binds.
#[test]
fn ambiguous_antecedent_keeps_gap_and_distinct_nouns_bind() {
    let ambiguous = "Tap target creature. ~ deals 1 damage to target creature. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(ambiguous, "Probe", &[], &types("Sorcery"), &[]);
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(json.contains("attached_to_qualifier"), "{json}");
    assert!(!json.contains("DeclaredTarget"), "{json}");

    let distinct = "Tap target artifact. ~ deals 1 damage to target creature. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(distinct, "Probe", &[], &types("Sorcery"), &[]);
    let effects = chain(&parsed.abilities[0]);
    let Some(Effect::DestroyAll { target, .. }) = effects.last().copied() else {
        panic!("expected DestroyAll, got {effects:?}");
    };
    assert_eq!(declared_slots(target), vec![1], "slot 1, not the artifact");
}

/// CR 601.2c: a player slot counts in the declared numbering, so the creature
/// is slot 1 (C1, mixed player + object chain).
#[test]
fn mixed_player_and_object_chain_numbers_the_creature_slot_one() {
    let text = "Target player loses 1 life. ~ deals 2 damage to target creature. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(text, "Probe", &[], &types("Sorcery"), &[]);
    let effects = chain(&parsed.abilities[0]);
    let Some(Effect::DestroyAll { target, .. }) = effects.last().copied() else {
        panic!("expected DestroyAll, got {effects:?}");
    };
    assert_eq!(declared_slots(target), vec![1]);
}

/// H3: a "Choose target X and target Y" head's LOCAL slot index is never
/// trusted; the referent is re-resolved through the chain-wide registry. A
/// variable-count prefix keeps the gap.
#[test]
fn two_target_head_after_a_fixed_prefix_uses_the_chain_wide_slot() {
    let fixed = "Tap target land. Choose target creature and target artifact. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(fixed, "Probe", &[], &types("Sorcery"), &[]);
    let effects = chain(&parsed.abilities[0]);
    let Some(Effect::DestroyAll { target, .. }) = effects.last().copied() else {
        panic!("expected DestroyAll, got {effects:?}");
    };
    assert_eq!(
        declared_slots(target),
        vec![1],
        "the creature is chain slot 1"
    );

    let variable = "Tap up to one target land. Choose target creature and target artifact. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(variable, "Probe", &[], &types("Sorcery"), &[]);
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(json.contains("attached_to_qualifier"), "{json}");
    assert!(!json.contains("DeclaredTarget"), "{json}");
}

/// CR 700.2: declared-slot numbering is mode-local, so a mode never admits the
/// referent. Reach guard: the same body outside a mode binds.
#[test]
fn modal_mode_declines_the_declared_slot_referent() {
    let body = "Target creature gets -1/-1 until end of turn. Destroy all Equipment attached to that creature.";
    let parsed = parse_oracle_text(body, "Probe", &[], &types("Sorcery"), &[]);
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(json.contains("DeclaredTarget"), "control binds: {json}");

    let modal = format!("Choose one —\n• Draw a card.\n• {body}");
    let parsed = parse_oracle_text(&modal, "Probe", &[], &types("Sorcery"), &[]);
    let json = serde_json::to_string(&parsed).unwrap();
    assert!(!json.contains("DeclaredTarget"), "{json}");
    assert!(json.contains("attached_to_qualifier"), "{json}");
}

// ---------------------------------------------------------------------------
// Runtime rows (cast pipeline)
// ---------------------------------------------------------------------------

fn equipment(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    name: &str,
) -> ObjectId {
    scenario
        .add_artifact_from_oracle(owner, name, "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn aura(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    name: &str,
) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(
            owner,
            name,
            "Enchant creature\nEnchanted creature gets +1/+1.",
        )
        .with_subtypes(vec!["Aura"])
        .id()
}

fn free_spell(scenario: &mut GameScenario, name: &str, instant: bool, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn eligible(waiting: &WaitingFor) -> (Vec<ObjectId>, u32, Option<u32>) {
    let WaitingFor::ChooseObjectsSelection {
        eligible, min, max, ..
    } = waiting
    else {
        panic!("expected the resolution-time Equipment choice, got {waiting:?}");
    };
    let mut ids: Vec<ObjectId> = eligible
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    ids.sort();
    (ids, *min, *max)
}

fn choose(runner: &mut GameRunner, chosen: &[ObjectId]) {
    runner
        .act(GameAction::SelectTargets {
            targets: chosen.iter().map(|id| TargetRef::Object(*id)).collect(),
        })
        .expect("the selection must be accepted");
    runner.advance_until_stack_empty();
}

fn give_hexproof(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.base_keywords.push(Keyword::Hexproof);
    obj.keywords.push(Keyword::Hexproof);
}

struct LojBoard {
    runner: GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    other: ObjectId,
    eq_a: ObjectId,
    aura_b: ObjectId,
    eq_c: ObjectId,
    eq_d: ObjectId,
}

/// The opponent's 2/7 `victim` carries Equipment A and Aura B; Equipment C is on
/// another creature; Equipment D is unattached.
fn loj_board(oracle: &str, name: &str, victim_toughness: i32) -> LojBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Victim", 2, victim_toughness)
        .id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let eq_a = equipment(&mut scenario, P1, "Equipment A");
    let aura_b = aura(&mut scenario, P1, "Aura B");
    let eq_c = equipment(&mut scenario, P1, "Equipment C");
    let eq_d = equipment(&mut scenario, P1, "Equipment D");
    let spell = free_spell(&mut scenario, name, true, oracle);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq_a, victim);
    attach::attach_to(runner.state_mut(), aura_b, victim);
    attach::attach_to(runner.state_mut(), eq_c, other);
    LojBoard {
        runner,
        spell,
        victim,
        other,
        eq_a,
        aura_b,
        eq_c,
        eq_d,
    }
}

/// R1 (CR 115.10a + CR 608.2d): the choice is made while the spell resolves and
/// offers exactly the Equipment attached to the targeted creature — not its
/// Aura, not Equipment on another creature, not unattached Equipment.
#[test]
fn light_of_judgment_offers_only_the_targets_equipment_and_destroys_the_choice() {
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    let (ids, min, max) = eligible(outcome.final_waiting_for());
    assert_eq!(ids, vec![b.eq_a], "only the target's Equipment is offered");
    assert_eq!((min, max), (0, Some(1)), "up to one");
    assert_eq!(
        outcome.state().objects[&b.victim].damage_marked,
        6,
        "reach guard: the first sentence resolved before the choice (CR 608.2c)"
    );
    choose(&mut b.runner, &[b.eq_a]);
    let state = b.runner.state();
    assert_eq!(state.objects[&b.eq_a].zone, Zone::Graveyard);
    for survivor in [b.aura_b, b.eq_c, b.eq_d] {
        assert_eq!(state.objects[&survivor].zone, Zone::Battlefield);
    }
}

/// R2: "up to one" admits choosing nothing; and a target with no Equipment
/// still raises the (empty) choice, a legal zero selection.
#[test]
fn light_of_judgment_choosing_none_destroys_nothing_and_unequipped_target_is_legal() {
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    let (ids, _, _) = eligible(outcome.final_waiting_for());
    assert_eq!(ids, vec![b.eq_a], "reach guard");
    choose(&mut b.runner, &[]);
    assert_eq!(b.runner.state().objects[&b.eq_a].zone, Zone::Battlefield);
    assert!(
        b.runner.state().stack.is_empty(),
        "the spell finished resolving"
    );

    // Unequipped target: the choice is offered with nothing eligible.
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.other]).resolve();
    let (ids, _, max) = eligible(outcome.final_waiting_for());
    assert_eq!(ids, vec![b.eq_c], "Other carries Equipment C");
    assert_eq!(max, Some(1));
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    attach::attach_to(b.runner.state_mut(), b.eq_a, b.other);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    let (ids, _, max) = eligible(outcome.final_waiting_for());
    assert!(ids.is_empty(), "an unequipped target offers nothing");
    assert_eq!(max, Some(0), "CR 609.3: the achievable maximum is zero");
    choose(&mut b.runner, &[]);
    assert_eq!(b.runner.state().objects[&b.victim].damage_marked, 6);
}

/// R3 (CR 608.2d, ruling 1): the Equipment is chosen while the spell resolves,
/// so an Equipment moved away in response is not offered and one attached in
/// response is.
#[test]
fn light_of_judgment_reads_attachments_at_resolution_not_at_cast() {
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    let mut commit = b.runner.cast(b.spell).target_objects(&[b.victim]).commit();
    attach::attach_to(commit.state_mut(), b.eq_a, b.other);
    attach::attach_to(commit.state_mut(), b.eq_d, b.victim);
    let outcome = commit.resolve();
    let (ids, _, _) = eligible(outcome.final_waiting_for());
    assert_eq!(
        ids,
        vec![b.eq_d],
        "the response-attached Equipment D, not A"
    );
}

/// R4 (CR 608.2b, ruling 3): the creature becomes an illegal target in
/// response, so the spell does not resolve and no Equipment is destroyed.
#[test]
fn light_of_judgment_illegal_target_destroys_nothing() {
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 7);
    let mut commit = b.runner.cast(b.spell).target_objects(&[b.victim]).commit();
    give_hexproof(commit.state_mut(), b.victim);
    let outcome = commit.resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no choice is raised, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 0);
    assert_eq!(outcome.zone_of(b.eq_a), Zone::Battlefield);
    assert_eq!(
        outcome.zone_of(b.spell),
        Zone::Graveyard,
        "reach guard: it was cast"
    );
}

/// R5 (CR 704.3): lethal damage does not remove the creature until after the
/// spell resolves, so its Equipment is still offered and destroyed.
#[test]
fn light_of_judgment_lethal_damage_still_offers_the_equipment() {
    let mut b = loj_board(LIGHT_OF_JUDGMENT, "Light of Judgment", 2);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    let (ids, _, _) = eligible(outcome.final_waiting_for());
    assert_eq!(ids, vec![b.eq_a]);
    choose(&mut b.runner, &[b.eq_a]);
    assert_eq!(b.runner.state().objects[&b.eq_a].zone, Zone::Graveyard);
    assert_eq!(
        b.runner.state().objects[&b.victim].zone,
        Zone::Graveyard,
        "the creature dies to state-based actions after resolution"
    );
}

/// R6 (CR 702.12b): indestructible Equipment can be chosen but is not destroyed.
/// R7: the Equipment's controller is irrelevant — the caster's own Equipment on
/// the opponent's creature is offered too.
#[test]
fn light_of_judgment_indestructible_and_foreign_controller_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 7).id();
    let sturdy = scenario
        .add_artifact_from_oracle(P1, "Sturdy", "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .indestructible()
        .id();
    let mine = equipment(&mut scenario, P0, "Mine");
    let spell = free_spell(&mut scenario, "Light of Judgment", true, LIGHT_OF_JUDGMENT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), sturdy, victim);
    attach::attach_to(runner.state_mut(), mine, victim);
    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    let (ids, _, _) = eligible(outcome.final_waiting_for());
    let mut expected = vec![sturdy, mine];
    expected.sort();
    assert_eq!(
        ids, expected,
        "both Equipment are offered, whoever controls them"
    );
    choose(&mut runner, &[sturdy]);
    assert_eq!(
        runner.state().objects[&sturdy].zone,
        Zone::Battlefield,
        "indestructible Equipment survives (CR 702.12b)"
    );
    assert_eq!(runner.state().objects[&mine].zone, Zone::Battlefield);
}

/// Turn to Slag: every Equipment on the target is destroyed; Equipment on
/// another creature and the target's Aura survive.
#[test]
fn turn_to_slag_destroys_all_equipment_on_the_target_only() {
    let mut b = loj_board(TURN_TO_SLAG, "Turn to Slag", 7);
    let second = b.eq_d;
    attach::attach_to(b.runner.state_mut(), second, b.victim);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(outcome.zone_of(b.eq_a), Zone::Graveyard);
    assert_eq!(outcome.zone_of(second), Zone::Graveyard);
    assert_eq!(
        outcome.zone_of(b.eq_c),
        Zone::Battlefield,
        "other creature's Equipment"
    );
    assert_eq!(
        outcome.zone_of(b.aura_b),
        Zone::Battlefield,
        "an Aura is not Equipment"
    );
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 5);
}

/// T9 (CR 608.2h): the referent was moved by a LEGAL earlier instruction of the
/// same resolution, so "Equipment attached to that creature" reads the exact
/// exit record of that creature — its former Equipment is destroyed, Equipment
/// elsewhere is not.
#[test]
fn referent_moved_by_an_earlier_instruction_reads_its_exit_record() {
    const TEXT: &str = "Exile target creature. Destroy all Equipment attached to that creature.";
    let mut b = loj_board(TEXT, "Probe", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    assert_eq!(
        outcome.zone_of(b.victim),
        Zone::Exile,
        "reach guard: exiled first"
    );
    assert_eq!(
        outcome.zone_of(b.eq_a),
        Zone::Graveyard,
        "the exiled creature's former Equipment is destroyed (CR 608.2h)"
    );
    assert_eq!(outcome.zone_of(b.eq_c), Zone::Battlefield);
    assert_eq!(outcome.zone_of(b.eq_d), Zone::Battlefield);
}

/// C1 (CR 601.2c): the creature is declared slot 1 behind a player slot; the
/// referent reads slot 1. Control: the player slot stays legal while the
/// creature becomes illegal — the spell resolves, but the Equipment part needs
/// information about the illegal creature and does nothing (CR 608.2b).
#[test]
fn mixed_player_object_chain_reads_slot_one_and_illegal_later_slot_supplies_nothing() {
    const TEXT: &str = "Target player loses 1 life. ~ deals 2 damage to target creature. Destroy all Equipment attached to that creature.";
    let mut b = loj_board(TEXT, "Probe", 7);
    let outcome = b
        .runner
        .cast(b.spell)
        .target_player(P1)
        .target_objects(&[b.victim])
        .resolve();
    outcome.assert_life_delta(P1, -1);
    assert_eq!(outcome.zone_of(b.eq_a), Zone::Graveyard);
    assert_eq!(outcome.zone_of(b.eq_c), Zone::Battlefield);

    let mut b = loj_board(TEXT, "Probe", 7);
    let mut commit = b
        .runner
        .cast(b.spell)
        .target_player(P1)
        .target_objects(&[b.victim])
        .commit();
    give_hexproof(commit.state_mut(), b.victim);
    let outcome = commit.resolve();
    outcome.assert_life_delta(P1, -1);
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 0);
    assert_eq!(
        outcome.zone_of(b.eq_a),
        Zone::Battlefield,
        "an illegal referent supplies no information (CR 608.2b)"
    );
}

/// C2 (CR 115.3): with distinct nouns, slot 0 (an artifact) and slot 1 (a
/// creature) may be the same object or different objects; the referent is
/// slot 1, never "the first object target".
#[test]
fn same_object_in_two_slots_and_split_slots_read_slot_one() {
    const TEXT: &str = "Tap target artifact. ~ deals 1 damage to target creature. Destroy all Equipment attached to that creature.";
    let build = || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario
            .add_creature(P1, "Artifact Creature A", 2, 7)
            .as_artifact()
            .as_creature()
            .id();
        let bee = scenario.add_creature(P1, "Creature B", 2, 7).id();
        let eq_a = equipment(&mut scenario, P1, "On A");
        let eq_b = equipment(&mut scenario, P1, "On B");
        let spell = free_spell(&mut scenario, "Probe", false, TEXT);
        let mut runner = scenario.build();
        attach::attach_to(runner.state_mut(), eq_a, a);
        attach::attach_to(runner.state_mut(), eq_b, bee);
        (runner, spell, a, bee, eq_a, eq_b)
    };
    // (a) A fills both slots.
    let (mut runner, spell, a, _bee, eq_a, eq_b) = build();
    let outcome = runner.cast(spell).target_objects(&[a, a]).resolve();
    assert!(
        outcome.state().objects[&a].tapped,
        "reach guard: slot 0 is A"
    );
    assert_eq!(outcome.zone_of(eq_a), Zone::Graveyard);
    assert_eq!(outcome.zone_of(eq_b), Zone::Battlefield);
    // (b) A in slot 0, B in slot 1: B's Equipment, not A's.
    let (mut runner, spell, a, bee, eq_a, eq_b) = build();
    let outcome = runner.cast(spell).target_objects(&[a, bee]).resolve();
    assert!(
        outcome.state().objects[&a].tapped,
        "reach guard: slot 0 is A"
    );
    assert_eq!(outcome.zone_of(eq_b), Zone::Graveyard, "slot 1's Equipment");
    assert_eq!(
        outcome.zone_of(eq_a),
        Zone::Battlefield,
        "not the first object's"
    );
}

// ---------------------------------------------------------------------------
// Fiery Annihilation (whole card)
// ---------------------------------------------------------------------------

/// Move `id` from the battlefield to its owner's graveyard ("would die",
/// CR 700.4) through the zone pipeline, so replacement effects apply.
fn kill(runner: &mut GameRunner, id: ObjectId) {
    let mut events: Vec<GameEvent> = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Graveyard, id),
        &mut events,
    );
}

/// Pass priority (declaring no attackers or blockers) until `turn` begins.
fn drive_to_turn(runner: &mut GameRunner, turn: u32) {
    for _ in 0..300 {
        if runner.state().turn_number >= turn {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::<(ObjectId, AttackTarget)>::new(),
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("unexpected prompt while driving: {other:?}"),
        };
        runner.act(action).expect("driver action accepted");
    }
    panic!("turn {turn} never began");
}

/// F7 (CR 601.2c): once the creature is chosen, the Equipment slot offers only
/// Equipment attached to THAT creature — not the caster's own unattached
/// Equipment, not Equipment on a different creature.
#[test]
fn fiery_annihilation_equipment_slot_offers_only_the_chosen_creatures_equipment() {
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 7);
    let mine = b.eq_d;
    // The caster's own unattached Equipment.
    {
        let state = b.runner.state_mut();
        let obj = state.objects.get_mut(&mine).unwrap();
        obj.controller = P0;
        obj.owner = P0;
    }
    let card_id = b.runner.state().objects[&b.spell].card_id;
    b.runner
        .act(GameAction::CastSpell {
            object_id: b.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast");
    b.runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b.victim)),
        })
        .expect("choose the creature");
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &b.runner.state().waiting_for
    else {
        panic!(
            "expected the Equipment slot, got {:?}",
            b.runner.state().waiting_for
        );
    };
    let offered = &selection.current_legal_targets;
    assert!(
        target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(b.eq_a)),
        "reach guard: the static slot admits the Equipment"
    );
    assert_eq!(
        offered,
        &vec![TargetRef::Object(b.eq_a)],
        "only the chosen creature's Equipment"
    );
    assert!(!offered.contains(&TargetRef::Object(mine)));
    assert!(!offered.contains(&TargetRef::Object(b.eq_c)));
    assert!(
        b.runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(b.eq_c)),
            })
            .is_err(),
        "submitting another creature's Equipment is rejected"
    );
}

/// F1 + F5 + F2 (CR 614.1a): with the Equipment chosen and still attached, it
/// is exiled; lethal damage then exiles the CREATURE instead of putting it into
/// the graveyard — the rider is bound to the creature's slot, not the
/// Equipment node it follows.
#[test]
fn fiery_annihilation_exiles_equipment_and_rider_exiles_the_creature() {
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 2);
    let outcome = b
        .runner
        .cast(b.spell)
        .target_objects(&[b.victim, b.eq_a])
        .resolve();
    assert_eq!(
        outcome.zone_of(b.eq_a),
        Zone::Exile,
        "the Equipment is exiled"
    );
    assert_eq!(
        outcome.zone_of(b.victim),
        Zone::Exile,
        "the creature would die to lethal damage and is exiled instead"
    );
    assert_eq!(outcome.zone_of(b.eq_c), Zone::Battlefield);

    // F1: no Equipment chosen — damage and the rider still apply.
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 2);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    assert_eq!(outcome.zone_of(b.victim), Zone::Exile);
    assert_eq!(
        outcome.zone_of(b.eq_a),
        Zone::Battlefield,
        "not chosen, not exiled"
    );
}

/// F3 (CR 608.2b, ruling 3): the Equipment moved to another creature in
/// response is no longer attached to the target, so it is an illegal target and
/// is not exiled; the creature is still dealt damage and the rider still
/// applies to it.
#[test]
fn fiery_annihilation_moved_equipment_is_not_exiled_but_damage_and_rider_apply() {
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 2);
    let mut commit = b
        .runner
        .cast(b.spell)
        .target_objects(&[b.victim, b.eq_a])
        .commit();
    attach::attach_to(commit.state_mut(), b.eq_a, b.other);
    let outcome = commit.resolve();
    assert_eq!(outcome.zone_of(b.eq_a), Zone::Battlefield);
    assert_eq!(
        outcome.state().objects[&b.eq_a].attached_to,
        Some(AttachTarget::Object(b.other))
    );
    assert_eq!(
        outcome.zone_of(b.victim),
        Zone::Exile,
        "damage + rider applied"
    );
}

/// F4 (CR 608.2b, ruling 2): the creature becomes an illegal target while the
/// Equipment is otherwise selectable. The Equipment part needs information
/// about the illegal creature, so nothing is exiled, no damage is dealt, and no
/// rider is installed.
#[test]
fn fiery_annihilation_illegal_creature_exiles_nothing_and_installs_no_rider() {
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 7);
    let mut commit = b
        .runner
        .cast(b.spell)
        .target_objects(&[b.victim, b.eq_a])
        .commit();
    give_hexproof(commit.state_mut(), b.victim);
    let outcome = commit.resolve();
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 0);
    assert_eq!(outcome.zone_of(b.eq_a), Zone::Battlefield);
    assert_eq!(
        outcome.zone_of(b.spell),
        Zone::Graveyard,
        "reach guard: cast"
    );
    kill(&mut b.runner, b.victim);
    assert_eq!(
        b.runner.state().objects[&b.victim].zone,
        Zone::Graveyard,
        "no rider was installed"
    );
}

/// F6 (ruling 4): the rider applies if the creature would die this turn FOR
/// ANY REASON — a later, independent destruction exiles it; the Equipment
/// stays. F8: after the turn ends, the "this turn" rider has expired.
#[test]
fn fiery_annihilation_rider_applies_to_any_death_this_turn_and_expires() {
    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 5);
    kill(&mut b.runner, b.victim);
    assert_eq!(b.runner.state().objects[&b.victim].zone, Zone::Exile);
    assert_eq!(b.runner.state().objects[&b.eq_a].zone, Zone::Battlefield);

    let mut b = loj_board(FIERY_ANNIHILATION, "Fiery Annihilation", 7);
    let outcome = b.runner.cast(b.spell).target_objects(&[b.victim]).resolve();
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 5);
    let next = b.runner.state().turn_number + 1;
    drive_to_turn(&mut b.runner, next);
    kill(&mut b.runner, b.victim);
    assert_eq!(
        b.runner.state().objects[&b.victim].zone,
        Zone::Graveyard,
        "the rider expired with the turn"
    );
}

// ---------------------------------------------------------------------------
// Copies and "choose new targets" (CR 707.10c, CR 115.7d, CR 115.7e)
// ---------------------------------------------------------------------------

const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const REDIRECT: &str = "You may choose new targets for target spell.";

/// Fiery Annihilation's opponent board: creature A (2/7) carries Equipment 1;
/// creature B (2/2, dies to 5 damage) carries Equipment 2; creature C (2/7)
/// carries nothing; Equipment 3 is unattached. Twincast and Redirect are in
/// hand alongside Fiery Annihilation, all free.
struct CopyBoard {
    runner: GameRunner,
    fiery: ObjectId,
    twincast: ObjectId,
    redirect: ObjectId,
    a: ObjectId,
    b: ObjectId,
    c: ObjectId,
    eq1: ObjectId,
    eq2: ObjectId,
    eq3: ObjectId,
}

fn copy_board() -> CopyBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let b = scenario.add_creature(P1, "Creature B", 2, 2).id();
    let c = scenario.add_creature(P1, "Creature C", 2, 7).id();
    let eq1 = equipment(&mut scenario, P1, "Equipment 1");
    let eq2 = equipment(&mut scenario, P1, "Equipment 2");
    let eq3 = equipment(&mut scenario, P1, "Equipment 3");
    let fiery = free_spell(
        &mut scenario,
        "Fiery Annihilation",
        true,
        FIERY_ANNIHILATION,
    );
    let twincast = free_spell(&mut scenario, "Twincast", true, TWINCAST);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq1, a);
    attach::attach_to(runner.state_mut(), eq2, b);
    CopyBoard {
        runner,
        fiery,
        twincast,
        redirect,
        a,
        b,
        c,
        eq1,
        eq2,
        eq3,
    }
}

/// Pass priority and accept "you may" until the stack-object choice the test
/// is about (`CopyRetarget` / `RetargetChoice`) is open.
fn drive_to_retarget_prompt(runner: &mut GameRunner) {
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::CopyRetarget { .. } | WaitingFor::RetargetChoice { .. } => return,
            ref other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("no retarget prompt opened");
}

/// Cast Fiery Annihilation at `targets`, then cast `responder` targeting it and
/// drive to the responder's retarget prompt.
fn fiery_then<R>(
    board: &mut CopyBoard,
    targets: &[ObjectId],
    before_response: impl FnOnce(&mut engine::types::game_state::GameState) -> R,
    responder: ObjectId,
) {
    board
        .runner
        .cast(board.fiery)
        .target_objects(targets)
        .commit();
    let _ = before_response(board.runner.state_mut());
    board
        .runner
        .cast(responder)
        .target_objects(&[board.fiery])
        .commit();
    drive_to_retarget_prompt(&mut board.runner);
}

fn copy_alternatives(runner: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::CopyRetarget {
        target_slots,
        current_slot,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected CopyRetarget, got {:?}",
            runner.state().waiting_for
        );
    };
    target_slots[*current_slot].legal_alternatives.clone()
}

/// The copy walk's positions (CR 707.10c: every chain-addressed declared
/// target, so Fiery Annihilation's Equipment sub-target too) and its cursor.
fn copy_walk(runner: &GameRunner) -> (usize, usize, bool, bool) {
    let WaitingFor::CopyRetarget {
        target_slots,
        current_slot,
        can_keep_rest,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected CopyRetarget, got {:?}",
            runner.state().waiting_for
        );
    };
    (
        target_slots.len(),
        *current_slot,
        target_slots[*current_slot].can_keep,
        *can_keep_rest,
    )
}

/// H1 safe positive (CR 707.10c + CR 115.7d): Equipment 1 was moved onto C
/// before the copy's new-target choice, so it is already an illegal target and
/// stays unchanged; changing the copy's creature A -> B is legal. The copy
/// damages B, which would die and is exiled instead (the rider follows the
/// copy's creature); Equipment 1 is untouched.
#[test]
fn copy_may_change_the_creature_when_its_equipment_target_is_already_illegal() {
    let mut board = copy_board();
    let (a, b, c, eq1) = (board.a, board.b, board.c, board.eq1);
    let twincast = board.twincast;
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| attach::attach_to(state, eq1, c),
        twincast,
    );
    assert!(
        copy_alternatives(&board.runner).contains(&TargetRef::Object(b)),
        "B is offered: Equipment 1 is no longer legal either way"
    );
    assert_eq!(
        copy_walk(&board.runner),
        (2, 0, true, true),
        "the copy walk addresses the creature and the Equipment"
    );
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B accepted");
    assert!(
        copy_walk(&board.runner).2,
        "CR 707.10c + CR 115.7d: the already-illegal Equipment may stay"
    );
    board
        .runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("Equipment 1 kept");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&b].zone, Zone::Exile, "copy's rider is on B");
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&eq1].attached_to,
        Some(AttachTarget::Object(c))
    );
    assert_eq!(state.objects[&a].damage_marked, 5, "the original hit A");
}

/// H1 (no Equipment target): a copy whose original chose no Equipment may
/// change its creature freely; the copy's damage and rider follow B.
#[test]
fn copy_without_an_equipment_target_changes_the_creature() {
    let mut board = copy_board();
    let (a, b, eq2) = (board.a, board.b, board.eq2);
    let twincast = board.twincast;
    fiery_then(&mut board, &[a], |_| {}, twincast);
    assert!(copy_alternatives(&board.runner).contains(&TargetRef::Object(b)));
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B accepted");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&b].zone, Zone::Exile);
    assert_eq!(state.objects[&eq2].zone, Zone::Battlefield, "not chosen");
    assert_eq!(state.objects[&a].damage_marked, 5);
}

/// H1 negative (CR 115.7d: new targets "must not cause any unchanged targets to
/// become illegal"): with Equipment 1 still attached to A, changing the copy's
/// creature to C (which carries no Equipment) would make the unchanged
/// Equipment target illegal with no Equipment to change it to, so C is not
/// offered and a direct submission is rejected; keeping the targets works. B
/// is offered only because its own Equipment 2 completes the change
/// (`copy_may_change_the_creature_and_its_equipment_together`).
#[test]
fn copy_may_not_change_the_creature_away_from_its_legal_equipment_target() {
    let mut board = copy_board();
    let (a, b, c, eq1) = (board.a, board.b, board.c, board.eq1);
    let twincast = board.twincast;
    fiery_then(&mut board, &[a, eq1], |_| {}, twincast);
    let offered = copy_alternatives(&board.runner);
    assert_eq!(
        copy_walk(&board.runner),
        (2, 0, true, true),
        "reach guard: keeping A is answerable"
    );
    assert!(offered.contains(&TargetRef::Object(b)));
    assert!(!offered.contains(&TargetRef::Object(c)));
    assert!(board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(c)),
        })
        .is_err());
    board
        .runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keeping the targets is legal");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&eq1].zone, Zone::Exile, "the copy exiled it");
    assert_eq!(state.objects[&a].damage_marked, 10, "both hit A");
    assert_eq!(state.objects[&b].damage_marked, 0);
}

/// M1 (CR 707.10c + CR 115.7d + CR 115.7e): a copy may change its creature and
/// its Equipment sub-target together — A -> B with Equipment 1 -> Equipment 2.
/// After choosing B, keeping Equipment 1 is not answerable (it would make a
/// legal unchanged target illegal), neither per position nor as "keep the
/// rest"; the only offered Equipment is the one attached to B (not the
/// unattached Equipment 3, not A's Equipment 1).
#[test]
fn copy_may_change_the_creature_and_its_equipment_together() {
    let mut board = copy_board();
    let (a, b, eq1, eq2, eq3) = (board.a, board.b, board.eq1, board.eq2, board.eq3);
    let twincast = board.twincast;
    fiery_then(&mut board, &[a, eq1], |_| {}, twincast);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B is offered: Equipment 2 completes it");
    assert_eq!(
        copy_walk(&board.runner),
        (2, 1, false, false),
        "keeping Equipment 1 is not answerable after A -> B"
    );
    assert_eq!(
        copy_alternatives(&board.runner),
        vec![TargetRef::Object(eq2)],
        "only B's Equipment is offered"
    );
    for refused in [
        GameAction::ChooseTarget { target: None },
        GameAction::KeepAllCopyTargets,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(eq1)),
        },
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(eq3)),
        },
    ] {
        assert!(
            GameRunner::from_state(board.runner.state().clone())
                .act(refused.clone())
                .is_err(),
            "{refused:?} is refused"
        );
    }
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(eq2)),
        })
        .expect("Equipment 1 -> Equipment 2 accepted");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(
        state.objects[&b].zone,
        Zone::Exile,
        "the copy's rider is on B"
    );
    assert_eq!(
        state.objects[&eq2].zone,
        Zone::Exile,
        "the copy exiled Equipment 2"
    );
    assert_eq!(
        state.objects[&eq1].zone,
        Zone::Exile,
        "the original exiled Equipment 1"
    );
    assert_eq!(state.objects[&eq3].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&a].damage_marked, 5,
        "only the original hit A"
    );
}

/// Move `id` to exile and back (a blink). The engine keeps the storage id and
/// bumps the incarnation (CR 400.7: it is a new object).
fn blink(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    let before = state.objects[&id].incarnation;
    let mut events: Vec<GameEvent> = Vec::new();
    move_object_for_test(
        state,
        ZoneMoveRequest::effect(id, Zone::Exile, id),
        &mut events,
    );
    move_object_for_test(
        state,
        ZoneMoveRequest::effect(id, Zone::Battlefield, id),
        &mut events,
    );
    assert_eq!(state.objects[&id].zone, Zone::Battlefield, "blinked back");
    assert!(
        state.objects[&id].incarnation > before,
        "reach guard: same id, new incarnation"
    );
}

/// Stale-referent probe (CR 400.7 + CR 115.7d): A is blinked after Fiery
/// Annihilation was announced and Equipment 1 is attached to the NEW A before
/// the copy's choice. The copy's unchanged A reference names the OLD object,
/// so Equipment 1 was not a legal target "attached to that creature" — it is
/// already illegal, and changing the creature to B is allowed. (Control: the
/// fresh, current A of `copy_may_not_change_the_creature_away_from_its_legal_equipment_target`
/// refuses B.)
#[test]
fn copy_stale_announced_referent_is_not_rebound_to_a_new_same_id_object() {
    let mut board = copy_board();
    let (a, b, eq1) = (board.a, board.b, board.eq1);
    let twincast = board.twincast;
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| {
            blink(state, a);
            attach::attach_to(state, eq1, a);
        },
        twincast,
    );
    assert_eq!(
        board.runner.state().objects[&eq1].attached_to,
        Some(AttachTarget::Object(a)),
        "reach guard: Equipment 1 is on the new A"
    );
    assert!(
        copy_alternatives(&board.runner).contains(&TargetRef::Object(b)),
        "the old-A announcement is not rebound to the new A"
    );
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B accepted");
    board
        .runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("the already-illegal Equipment 1 is kept");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&b].zone, Zone::Exile);
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
}

fn retarget_pools(runner: &GameRunner) -> Vec<Vec<TargetRef>> {
    let WaitingFor::RetargetChoice { slot_pools, .. } = &runner.state().waiting_for else {
        panic!(
            "expected RetargetChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    slot_pools.clone()
}

fn objects(ids: &[ObjectId]) -> Vec<TargetRef> {
    ids.iter().map(|id| TargetRef::Object(*id)).collect()
}

/// RetargetChoice (CR 115.7d + CR 115.7e): the Equipment position's pool spans
/// every creature the same submission may elect, and the final set is checked
/// exactly — A -> B keeping Equipment 1 is refused; A -> B with Equipment 1 ->
/// Equipment 2 is accepted; A kept with Equipment 1 -> Equipment 3 (moved onto
/// A) is accepted.
#[test]
fn choose_new_targets_checks_the_edited_selection_as_a_whole() {
    // (a) refused, then (b) accepted on the same prompt.
    let mut board = copy_board();
    let (a, b, eq1, eq2) = (board.a, board.b, board.eq1, board.eq2);
    let redirect = board.redirect;
    fiery_then(&mut board, &[a, eq1], |_| {}, redirect);
    let pools = retarget_pools(&board.runner);
    assert_eq!(pools.len(), 2, "creature and Equipment positions");
    assert!(pools[1].contains(&TargetRef::Object(eq2)), "B's Equipment");
    assert!(pools[1].contains(&TargetRef::Object(eq1)), "A's Equipment");
    assert!(
        board
            .runner
            .act(GameAction::RetargetSpell {
                new_targets: objects(&[b, eq1]).into_iter().map(Some).collect(),
            })
            .is_err(),
        "Equipment 1 would become illegal"
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[b, eq2]).into_iter().map(Some).collect(),
        })
        .expect("A -> B with B's Equipment");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&eq2].zone, Zone::Exile);
    assert_eq!(state.objects[&b].zone, Zone::Exile, "rider on B");
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
    assert_eq!(state.objects[&a].damage_marked, 0);

    // (c) A kept, Equipment 1 -> Equipment 3 (also on A).
    let mut board = copy_board();
    let (a, eq1, eq3) = (board.a, board.eq1, board.eq3);
    let redirect = board.redirect;
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| attach::attach_to(state, eq3, a),
        redirect,
    );
    let b = board.b;
    assert!(
        retarget_pools(&board.runner)[1].contains(&TargetRef::Object(eq3)),
        "reach guard: Equipment 3 is in the widened pool"
    );
    assert!(
        board
            .runner
            .act(GameAction::RetargetSpell {
                new_targets: objects(&[b, eq3]).into_iter().map(Some).collect(),
            })
            .is_err(),
        "Equipment 3 is attached to A, not to the newly chosen B"
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[a, eq3]).into_iter().map(Some).collect(),
        })
        .expect("Equipment 1 -> Equipment 3 on A");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&eq3].zone, Zone::Exile);
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
    assert_eq!(state.objects[&a].damage_marked, 5);
}

/// RetargetChoice (CR 115.7d first clause): Equipment 1 was detached before
/// the prompt, so it is already illegal and may stay unchanged while A -> B.
#[test]
fn choose_new_targets_keeps_an_already_illegal_equipment_target() {
    let mut board = copy_board();
    let (a, b, c, eq1) = (board.a, board.b, board.c, board.eq1);
    let redirect = board.redirect;
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| attach::attach_to(state, eq1, c),
        redirect,
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[b, eq1]).into_iter().map(Some).collect(),
        })
        .expect("an already-illegal unchanged target may stay");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&b].zone, Zone::Exile);
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Pins and chain-wide numbering
// ---------------------------------------------------------------------------

/// H2 (CR 115.3 + CR 115.7d): one two-target node selects artifact creature A
/// and creature B; choosing new targets puts A in the creature slot too. The
/// pins are positional (each A occurrence carries its own pin), and the creature
/// slot still reads A — A's Equipment is destroyed, B's is not.
#[test]
fn two_target_head_retargeted_onto_one_object_reads_its_pin() {
    const TEXT: &str =
        "Choose target artifact and target creature. Destroy all Equipment attached to that creature.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature(P1, "Artifact Creature A", 2, 7)
        .as_artifact()
        .as_creature()
        .id();
    let bee = scenario.add_creature(P1, "Creature B", 2, 7).id();
    let eq_a = equipment(&mut scenario, P1, "On A");
    let eq_b = equipment(&mut scenario, P1, "On B");
    let spell = free_spell(&mut scenario, "Probe", true, TEXT);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq_a, a);
    attach::attach_to(runner.state_mut(), eq_b, bee);
    runner.cast(spell).target_objects(&[a, bee]).commit();
    runner.cast(redirect).target_objects(&[spell]).commit();
    drive_to_retarget_prompt(&mut runner);
    runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[a, a]).into_iter().map(Some).collect(),
        })
        .expect("B -> A in the creature slot");
    let pins = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .map(|ability| {
            let mut pins = ability.aligned_target_pins();
            if let Some(sub) = ability.sub_ability.as_deref() {
                pins.extend(sub.aligned_target_pins());
            }
            pins
        })
        .expect("probe on the stack");
    assert_eq!(
        pins.iter()
            .filter(|pin| pin.is_some_and(|pin| pin.object_id == a))
            .count(),
        2,
        "reach guard: each A occurrence (artifact and creature slots) carries its own pin"
    );
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&eq_a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&eq_b].zone, Zone::Battlefield);
}

/// H2 negative (CR 400.7 + CR 608.2b): the declaring damage node's pin for its
/// creature is mutated after announcement. The two arms take different routes,
/// and each is asserted on its own:
///
/// - STALE pin, the ILLEGAL-TARGET CONTROL: the occurrence names an
///   incarnation that is not the live object, so the node fails its own
///   legality check when the spell resolves. Its slot is a hole: no damage, and
///   the attachment reader returns at the illegal-slot check without reading.
/// - MISSING pin, a REACHED reader: an unpinned occurrence is not judged stale,
///   so the damage IS dealt (the slot is legal and the reader runs), but the
///   reader has no announcement pin and never recovers one from the live row.
///   It names no referent, and Equipment A survives.
///
/// The player part resolves in both. Positive guard: the unmodified control
/// deals the 2 damage and destroys Equipment A. A stale pin that DOES reach the
/// reader (stale only after the legality check) is
/// `stale_pin_after_legality_reads_the_exit_record_not_the_live_object`.
#[test]
fn missing_or_stale_pin_names_no_referent() {
    const TEXT: &str =
        "Target player loses 1 life. ~ deals 2 damage to target creature. Destroy all Equipment attached to that creature.";
    let mut control = loj_board(TEXT, "Probe", 7);
    let outcome = control
        .runner
        .cast(control.spell)
        .target_player(P1)
        .target_objects(&[control.victim])
        .resolve();
    assert_eq!(
        outcome.state().objects[&control.victim].damage_marked,
        2,
        "positive guard: an intact pin deals the damage"
    );
    assert_eq!(
        outcome.zone_of(control.eq_a),
        Zone::Graveyard,
        "positive guard"
    );
    for stale in [false, true] {
        let mut b = loj_board(TEXT, "Probe", 7);
        let mut commit = b
            .runner
            .cast(b.spell)
            .target_player(P1)
            .target_objects(&[b.victim])
            .commit();
        {
            let state = commit.state_mut();
            let entry = state
                .stack
                .iter_mut()
                .find(|entry| entry.id == b.spell)
                .expect("probe on the stack");
            let ability = entry.ability_mut().expect("spell ability");
            let node = ability.sub_ability.as_deref_mut().expect("damage node");
            if stale {
                for pin in node.target_pins.iter_mut().flatten() {
                    pin.incarnation += 100;
                }
            } else {
                node.target_pins.clear();
            }
        }
        let outcome = commit.resolve();
        outcome.assert_life_delta(P1, -1);
        assert_eq!(
            outcome.state().objects[&b.victim].damage_marked,
            if stale { 0 } else { 2 },
            "stale={stale}: a stale pin makes the node's own target illegal \
             (CR 608.2b); a missing pin leaves it legal and reaches the reader"
        );
        assert_eq!(
            outcome.zone_of(b.eq_a),
            Zone::Battlefield,
            "stale={stale}: no referent"
        );
    }
}

/// Append `tail` at the end of `def`'s sub-ability chain.
fn append_sub_ability(def: &mut AbilityDefinition, tail: AbilityDefinition) {
    match def.sub_ability.as_deref_mut() {
        Some(next) => append_sub_ability(next, tail),
        None => def.sub_ability = Some(Box::new(tail)),
    }
}

/// H2, the REACHED stale reader (CR 608.2b + CR 608.2h + CR 400.7): the
/// declared creature is legal when the spell starts to resolve, and only goes
/// stale DURING resolution. An engine-composed spell exiles target creature
/// and returns it (a new object, CR 400.7), then "Destroy all Equipment
/// attached to that creature". The reader runs with the announcement pin,
/// which no longer names the live object. It never recovers the live row:
/// it reads the exit record of exactly the pinned incarnation (CR 608.2h), so
/// Equipment A, attached when the creature left, is destroyed; Equipment C on
/// another creature survives. Reach guards: the creature really left and came
/// back (the exile node was not pruned), and slot 0 was not an illegal hole.
#[test]
fn stale_pin_after_legality_reads_the_exit_record_not_the_live_object() {
    const BLINK: &str =
        "Exile target creature, then return that card to the battlefield under its owner's control.";
    const DESTROY_ATTACHED: &str =
        "Tap target creature. Destroy all Equipment attached to that creature.";
    let types = types("Instant");
    let mut root = parse_oracle_text(BLINK, "Blink Probe", &[], &types, &[])
        .abilities
        .remove(0);
    assert!(
        unimplemented_names(&[&root]).is_empty(),
        "reach guard: the blink parses supported"
    );
    let destroy = parse_oracle_text(DESTROY_ATTACHED, "Destroy Probe", &[], &types, &[])
        .abilities
        .remove(0)
        .sub_ability
        .expect("the destroy node");
    let Effect::DestroyAll { target, .. } = destroy.effect.as_ref() else {
        panic!("expected DestroyAll, got {:?}", destroy.effect);
    };
    assert_eq!(declared_slots(target), vec![0], "reach guard: reads slot 0");
    append_sub_ability(&mut root, *destroy);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 7).id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let eq_a = equipment(&mut scenario, P1, "Equipment A");
    let eq_c = equipment(&mut scenario, P1, "Equipment C");
    let spell = scenario
        .add_spell_to_hand(P0, "Blink Probe", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(root)
        .id();
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq_a, victim);
    attach::attach_to(runner.state_mut(), eq_c, other);
    let before = runner.state().objects[&victim].incarnation;
    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    let state = outcome.state();
    assert_eq!(
        state.objects[&victim].zone,
        Zone::Battlefield,
        "reach: returned"
    );
    assert!(
        state.objects[&victim].incarnation > before,
        "reach guard: the creature left and returned during resolution"
    );
    assert_eq!(
        state.objects[&eq_a].zone,
        Zone::Graveyard,
        "the pinned incarnation's exit record names Equipment A"
    );
    assert_eq!(state.objects[&eq_c].zone, Zone::Battlefield);
}

/// H2 (CR 608.2b + CR 400.7): the creature is blinked before resolution and its
/// new incarnation picks up Equipment. The initial legality check marks the old
/// incarnation's slot illegal, so the reader returns None before either live or
/// LKI matching — the player part still resolves; nothing is destroyed.
#[test]
fn blinked_referent_is_illegal_and_supplies_no_attachments() {
    const TEXT: &str =
        "Target player loses 1 life. ~ deals 2 damage to target creature. Destroy all Equipment attached to that creature.";
    let mut b = loj_board(TEXT, "Probe", 7);
    let mut commit = b
        .runner
        .cast(b.spell)
        .target_player(P1)
        .target_objects(&[b.victim])
        .commit();
    blink(commit.state_mut(), b.victim);
    attach::attach_to(commit.state_mut(), b.eq_d, b.victim);
    let outcome = commit.resolve();
    outcome.assert_life_delta(P1, -1);
    assert_eq!(outcome.state().objects[&b.victim].damage_marked, 0);
    assert_eq!(outcome.zone_of(b.eq_d), Zone::Battlefield, "new attachment");
    assert_eq!(
        outcome.zone_of(b.eq_a),
        Zone::Battlefield,
        "the old incarnation's attachment (its exit record) is not read either"
    );
}

/// H3 (CR 601.2c): a two-target head after a fixed one-target prefix. "that
/// creature" is chain slot 1 (the creature), not the head's local index 0
/// (which in the whole chain is the land). The creature's Equipment is
/// destroyed; Equipment on another creature survives.
#[test]
fn two_target_head_after_a_prefix_destroys_the_creatures_equipment() {
    const TEXT: &str = "Tap target land. Choose target creature and target artifact. Destroy all Equipment attached to that creature.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Red);
    let x = scenario.add_creature(P1, "Creature X", 2, 7).id();
    let other = scenario.add_creature(P1, "Other", 2, 7).id();
    let eq_x = equipment(&mut scenario, P1, "On X");
    let eq_other = equipment(&mut scenario, P1, "On Other");
    let art = scenario
        .add_artifact_from_oracle(P1, "Plain Artifact", "")
        .id();
    let spell = free_spell(&mut scenario, "Probe", false, TEXT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq_x, x);
    attach::attach_to(runner.state_mut(), eq_other, other);
    let outcome = runner.cast(spell).target_objects(&[land, x, art]).resolve();
    assert!(outcome.state().objects[&land].tapped, "reach guard: slot 0");
    assert_eq!(outcome.zone_of(eq_x), Zone::Graveyard);
    assert_eq!(outcome.zone_of(eq_other), Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Saved-game compatibility (legacy `AttachedToSource` / `AttachedToRecipient` /
// `AttachedToPlayer` tags inside stored abilities and statics)
// ---------------------------------------------------------------------------

/// Rewrite every canonical `{"type":"AttachedTo","to":{..}}` in a serialized
/// state into the pre-refactor tag it replaced, returning the rewrite count.
fn legacyize(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            // `FilterProp::AttachedTo { to }` ? not the unit `TargetFilter::AttachedTo`.
            if map.get("type").and_then(|t| t.as_str()) == Some("AttachedTo")
                && map.contains_key("to")
            {
                let to = map.get("to").cloned().expect("checked above");
                let legacy = match to.get("type").and_then(|t| t.as_str()) {
                    Some("Source") => serde_json::json!({ "type": "AttachedToSource" }),
                    Some("Recipient") => serde_json::json!({ "type": "AttachedToRecipient" }),
                    Some("Player") => serde_json::json!({
                        "type": "AttachedToPlayer",
                        "player": to.get("player").cloned().expect("player"),
                    }),
                    other => panic!("no legacy form for {other:?}"),
                };
                *value = legacy;
                return 1;
            }
            map.values_mut().map(legacyize).sum()
        }
        serde_json::Value::Array(items) => items.iter_mut().map(legacyize).sum(),
        _ => 0,
    }
}

fn count_tag(value: &serde_json::Value, tag: &str) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            let hit = map.get("type").and_then(|t| t.as_str()) == Some(tag)
                && (tag != "AttachedTo" || map.contains_key("to"));
            usize::from(hit) + map.values().map(|v| count_tag(v, tag)).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter().map(|v| count_tag(v, tag)).sum(),
        _ => 0,
    }
}

/// Restore through the engine's own save/undo/P2P pipeline.
fn restore(json: &str) -> GameRunner {
    let state = serde_json::from_str::<PersistedGameState>(json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

fn persisted(runner: &GameRunner) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes")
}

/// Save `runner` in the pre-refactor encoding and load it back. Asserts the
/// fixture really carries `legacy_tag`, and that the loaded state serializes
/// canonical-new (no legacy tag survives a save).
fn reload_legacy(runner: &GameRunner, legacy_tag: &str) -> GameRunner {
    let mut value = persisted(runner);
    let canonical = count_tag(&value, "AttachedTo");
    assert!(canonical > 0, "reach guard: the state stores the relation");
    assert_eq!(legacyize(&mut value), canonical);
    assert!(
        count_tag(&value, legacy_tag) > 0,
        "fixture carries {legacy_tag}"
    );
    assert_eq!(count_tag(&value, "AttachedTo"), 0, "fixture is all-legacy");
    let reloaded = restore(&value.to_string());
    let saved = persisted(&reloaded);
    assert_eq!(
        count_tag(&saved, "AttachedTo"),
        canonical,
        "saved canonical"
    );
    for old in [
        "AttachedToSource",
        "AttachedToRecipient",
        "AttachedToPlayer",
    ] {
        assert_eq!(count_tag(&saved, old), 0, "no {old} is written");
    }
    reloaded
}

fn power_toughness(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(state);
    let obj = &state.objects[&id];
    (
        obj.power.unwrap_or_default(),
        obj.toughness.unwrap_or_default(),
    )
}

/// Source (CR 701.3a + CR 613.4c): a 1/1 that "gets +2/+2 for each Aura and
/// Equipment attached to" itself, carrying one Equipment (+1/+0) and one Aura
/// (+1/+1): 1 + 2*2 + 1 + 1 = 7 power, 1 + 2*2 + 1 = 6 toughness. A legacy save
/// loads and computes the same.
#[test]
fn legacy_attached_to_source_save_loads_and_counts_the_same() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champ = scenario
        .add_creature_from_oracle(
            P0,
            "Probe Champion",
            1,
            1,
            "Probe Champion gets +2/+2 for each Aura and Equipment attached to Probe Champion.",
        )
        .id();
    let eq = equipment(&mut scenario, P0, "Gear");
    let au = aura(&mut scenario, P0, "Blessing");
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq, champ);
    attach::attach_to(runner.state_mut(), au, champ);
    assert_eq!(power_toughness(&mut runner, champ), (7, 6), "baseline");
    let mut reloaded = reload_legacy(&runner, "AttachedToSource");
    assert_eq!(power_toughness(&mut reloaded, champ), (7, 6));
    // The relation stays live after the load: one fewer attachment, -2/-2 and
    // the Equipment's own +1/+0.
    let mut events: Vec<GameEvent> = Vec::new();
    move_object_for_test(
        reloaded.state_mut(),
        ZoneMoveRequest::effect(eq, Zone::Graveyard, eq),
        &mut events,
    );
    assert_eq!(power_toughness(&mut reloaded, champ), (4, 4));
}

/// Recipient (CR 303.4b + CR 613.4c): Strong Back-style "Enchanted creature
/// gets +2/+2 for each Aura and Equipment attached to it" on a 1/1 that also
/// carries one Equipment (+1/+0): 1 + 2*2 + 1 = 6 power, 1 + 2*2 = 5 toughness.
#[test]
fn legacy_attached_to_recipient_save_loads_and_counts_the_same() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 1, 1).id();
    let back = scenario
        .add_enchantment_from_oracle(
            P0,
            "Probe Back",
            "Enchant creature\nEnchanted creature gets +2/+2 for each Aura and Equipment attached to it.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let eq = equipment(&mut scenario, P0, "Gear");
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), back, host);
    attach::attach_to(runner.state_mut(), eq, host);
    assert_eq!(power_toughness(&mut runner, host), (6, 5), "baseline");
    let mut reloaded = reload_legacy(&runner, "AttachedToRecipient");
    assert_eq!(power_toughness(&mut reloaded, host), (6, 5));
}

/// Player (CR 303.4b + CR 701.3a): Curse of Thirst on P1 with a second Curse
/// attached to P1 — at P1's upkeep it deals damage equal to the number of
/// Curses attached to that player (2).
#[test]
fn legacy_attached_to_player_save_loads_and_counts_the_same() {
    let build = || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let thirst = scenario
            .add_enchantment_from_oracle(
                P0,
                "Curse of Thirst",
                "Enchant player\nAt the beginning of enchanted player's upkeep, Curse of Thirst deals damage to that player equal to the number of Curses attached to them.",
            )
            .with_subtypes(vec!["Aura", "Curse"])
            .id();
        let other = scenario
            .add_enchantment_from_oracle(P0, "Curse of Probing", "Enchant player")
            .with_subtypes(vec!["Aura", "Curse"])
            .id();
        let mut runner = scenario.build();
        attach::attach_to_player(runner.state_mut(), thirst, P1);
        attach::attach_to_player(runner.state_mut(), other, P1);
        runner
    };
    let through_upkeep = |runner: &mut GameRunner| {
        let before = runner.life(P1);
        let next = runner.state().turn_number + 1;
        drive_to_turn(runner, next);
        assert_eq!(runner.state().active_player, P1, "P1's turn");
        for _ in 0..20 {
            // Stop once the upkeep is over (P1's library is empty).
            if runner.state().phase == Phase::Draw {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
        before - runner.life(P1)
    };
    let mut baseline = build();
    assert_eq!(through_upkeep(&mut baseline), 2, "baseline");
    let mut reloaded = reload_legacy(&build(), "AttachedToPlayer");
    assert_eq!(through_upkeep(&mut reloaded), 2);
}

/// Nested legacy tags (inside `Not` and `AnyOf`) decode to the canonical form,
/// and a canonical-new filter round-trips unchanged.
#[test]
fn legacy_attachment_tags_decode_at_any_depth_and_new_form_round_trips() {
    let legacy = serde_json::json!({
        "type": "Typed",
        "type_filters": [{ "Subtype": "Equipment" }],
        "controller": null,
        "properties": [
            { "type": "Not", "prop": { "type": "AttachedToSource" } },
            { "type": "AnyOf", "props": [
                { "type": "AttachedToRecipient" },
                { "type": "AttachedToPlayer", "player": "EnchantedPlayer" }
            ] }
        ]
    });
    let decoded: TargetFilter = serde_json::from_value(legacy).expect("legacy decodes");
    let canonical = serde_json::to_value(&decoded).unwrap();
    assert_eq!(count_tag(&canonical, "AttachedTo"), 3);
    for old in [
        "AttachedToSource",
        "AttachedToRecipient",
        "AttachedToPlayer",
    ] {
        assert_eq!(count_tag(&canonical, old), 0);
    }
    let again: TargetFilter = serde_json::from_value(canonical.clone()).expect("new decodes");
    assert_eq!(again, decoded);
    assert_eq!(serde_json::to_value(&again).unwrap(), canonical);
    let declared = serde_json::json!({
        "type": "AttachedTo", "to": { "type": "DeclaredTarget", "slot": 1 }
    });
    let prop: FilterProp = serde_json::from_value(declared.clone()).expect("decodes");
    assert_eq!(
        prop,
        FilterProp::AttachedTo {
            to: AttachmentReferent::DeclaredTarget { slot: 1 }
        }
    );
    assert_eq!(serde_json::to_value(&prop).unwrap(), declared);
}

// ---------------------------------------------------------------------------
// Die-exile rider with a compound subject ("that creature or planeswalker")
// ---------------------------------------------------------------------------

const SCORCHING_DRAGONFIRE: &str = "Scorching Dragonfire deals 3 damage to target creature or planeswalker. If that creature or planeswalker would die this turn, exile it instead.";

/// CR 614.1a + CR 608.2c: the subject "that creature or planeswalker" names no
/// unique slot noun, but the damage clause holds the chain's only declared
/// slot, so the rider keeps its `Any` route (no other antecedent exists) and
/// a creature dealt lethal damage is exiled.
#[test]
fn single_declarer_compound_subject_rider_keeps_its_route_and_exiles() {
    let parsed = parse_oracle_text(
        SCORCHING_DRAGONFIRE,
        "Scorching Dragonfire",
        &[],
        &types("Instant"),
        &[],
    );
    let effects = chain(&parsed.abilities[0]);
    assert!(
        matches!(
            effects[1],
            Effect::AddTargetReplacement {
                target: TargetFilter::Any,
                ..
            }
        ),
        "rider stays on the damage target, got {effects:?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let spell = free_spell(
        &mut scenario,
        "Scorching Dragonfire",
        true,
        SCORCHING_DRAGONFIRE,
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bear]).resolve();
    assert_eq!(outcome.zone_of(bear), Zone::Exile);
}

/// Control: an unresolvable subject with ANOTHER declared slot before the
/// preceding clause stays a strict gap — the `Any` route would install the
/// replacement on the artifact the preceding clause tapped.
#[test]
fn compound_subject_rider_with_an_intervening_declarer_fails_closed() {
    const TEXT: &str = "~ deals 2 damage to target creature or planeswalker. Tap target artifact. If that creature or planeswalker would die this turn, exile it instead.";
    let parsed = parse_oracle_text(TEXT, "Probe", &[], &types("Instant"), &[]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]])
            .contains(&"die_exile_rider_antecedent".to_string()),
        "got {:?}",
        chain(&parsed.abilities[0])
    );
}

// ---------------------------------------------------------------------------
// Round 2: stale Equipment target; controller-qualified dependent target
// ---------------------------------------------------------------------------

/// Fiery Annihilation announced at A / Equipment 1; then Equipment 1 is
/// blinked (a new object, CR 400.7) and the new Equipment 1 is attached to
/// `host`. Returns the board with the responder's prompt open.
fn stale_equipment_board(
    responder: fn(&CopyBoard) -> ObjectId,
    host: fn(&CopyBoard) -> ObjectId,
) -> CopyBoard {
    let mut board = copy_board();
    let (a, eq1) = (board.a, board.eq1);
    let who = responder(&board);
    let host = host(&board);
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| {
            blink(state, eq1);
            attach::attach_to(state, eq1, host);
        },
        who,
    );
    assert_eq!(
        board.runner.state().objects[&eq1].attached_to,
        Some(AttachTarget::Object(host)),
        "reach guard: the new Equipment 1 is on its host"
    );
    board
}

/// The incarnation the spell `spell`'s Equipment node pins for `eq`.
fn equipment_pin(runner: &GameRunner, spell: ObjectId, eq: ObjectId) -> u64 {
    runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .and_then(|ability| ability.sub_ability.as_deref())
        .and_then(|node| {
            node.target_occurrences()
                .into_iter()
                .find_map(|(target, pin)| (target == TargetRef::Object(eq)).then_some(pin))
                .flatten()
        })
        .map(|pin| pin.incarnation)
        .expect("the Equipment node pins Equipment 1")
}

/// Copy (CR 707.10c + CR 115.7d): the stale Equipment target is already
/// illegal, so B is offered and accepted and the Equipment position may be
/// kept explicitly — it stays unchanged with its announced pin. The copy hits B
/// (rider: exiled) and the new Equipment 1 is untouched by either spell.
#[test]
fn copy_may_change_the_creature_when_its_equipment_target_was_blinked() {
    let mut board = stale_equipment_board(|b| b.twincast, |b| b.a);
    let (a, b, eq1, fiery) = (board.a, board.b, board.eq1, board.fiery);
    let WaitingFor::CopyRetarget { copy_id, .. } = board.runner.state().waiting_for else {
        panic!("expected the copy walk");
    };
    assert_ne!(
        copy_id, fiery,
        "the walk is the Twincast copy, not the original"
    );
    assert!(
        board
            .runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == fiery),
        "reach guard: the original is still on the stack below the copy"
    );
    assert!(
        copy_alternatives(&board.runner).contains(&TargetRef::Object(b)),
        "the stale Equipment target does not block B"
    );
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B accepted");
    board
        .runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("the stale Equipment target is kept");
    assert_eq!(
        equipment_pin(&board.runner, fiery, eq1),
        0,
        "control: the original's own announced pin"
    );
    assert_eq!(
        equipment_pin(&board.runner, copy_id, eq1),
        0,
        "the copy keeps the announced Equipment pin"
    );
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&b].zone, Zone::Exile);
    assert_eq!(state.objects[&a].damage_marked, 5, "the original hit A");
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
}

/// Choose new targets (CR 115.7a + CR 115.7e + CR 400.7): resubmitting the
/// same id for a target whose announced object is gone ELECTS the new object
/// (phase-rs/phase#8355 H2), so it is validated as a changed target against
/// the final selection. With the new Equipment 1 on A and the creature moved
/// to B it is illegal: the submission is refused and nothing is re-pinned.
#[test]
fn choose_new_targets_refuses_a_same_id_re_election_illegal_for_the_new_creature() {
    let mut board = stale_equipment_board(|b| b.redirect, |b| b.a);
    let (b, eq1, fiery) = (board.b, board.eq1, board.fiery);
    assert_eq!(equipment_pin(&board.runner, fiery, eq1), 0, "reach guard");
    assert!(
        board
            .runner
            .act(GameAction::RetargetSpell {
                new_targets: objects(&[b, eq1]).into_iter().map(Some).collect(),
            })
            .is_err(),
        "the new Equipment 1 is not attached to B"
    );
    assert_eq!(
        equipment_pin(&board.runner, fiery, eq1),
        0,
        "no pin revival on a refused submission"
    );
}

/// Discriminator for the test above: with the new Equipment 1 on B, the same
/// submission is a legal election of the new object — accepted, re-pinned,
/// and Fiery Annihilation exiles it along with B.
#[test]
fn choose_new_targets_accepts_a_same_id_re_election_legal_for_the_new_creature() {
    let mut board = stale_equipment_board(|b| b.redirect, |b| b.b);
    let (b, eq1, fiery) = (board.b, board.eq1, board.fiery);
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[b, eq1]).into_iter().map(Some).collect(),
        })
        .expect("electing the new Equipment 1 on B is legal");
    assert_ne!(equipment_pin(&board.runner, fiery, eq1), 0, "re-pinned");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&eq1].zone, Zone::Exile);
    assert_eq!(state.objects[&b].zone, Zone::Exile);
}

/// CR 115.1a + CR 601.2c + CR 109.5: a controller-qualified dependent target
/// ("target Equipment you control / an opponent controls attached to that
/// creature") composes both constraints. Creature A wears Equipment Mine
/// (controlled by the caster) and Equipment Theirs; creature B wears Equipment
/// B. After A is announced, "you control" offers only Mine and "an opponent
/// controls" only Theirs; the chosen one is exiled, and the damage hits A.
#[test]
fn controller_qualified_dependent_target_composes_with_the_referent() {
    for (qualifier, mine_offered) in [("you control ", true), ("an opponent controls ", false)] {
        let text = format!(
            "~ deals 5 damage to target creature. Exile up to one target Equipment {qualifier}attached to that creature."
        );
        let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
        assert!(
            unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
            "{qualifier}: the qualified clause parses supported"
        );
        assert!(
            serde_json::to_string(&parsed.abilities[0])
                .unwrap()
                .contains("DeclaredTarget"),
            "{qualifier}: the declared-slot referent is emitted"
        );

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
        let b = scenario.add_creature(P1, "Creature B", 2, 7).id();
        let mine = equipment(&mut scenario, P0, "Equipment Mine");
        let theirs = equipment(&mut scenario, P1, "Equipment Theirs");
        let eq_b = equipment(&mut scenario, P1, "Equipment B");
        let spell = free_spell(&mut scenario, "Probe", true, &text);
        let mut runner = scenario.build();
        attach::attach_to(runner.state_mut(), mine, a);
        attach::attach_to(runner.state_mut(), theirs, a);
        attach::attach_to(runner.state_mut(), eq_b, b);
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("cast begins");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(a)),
            })
            .expect("announce A");
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!(
                "{qualifier}: expected the Equipment slot, got {:?}",
                runner.state().waiting_for
            );
        };
        let (offered, chosen) = if mine_offered {
            (mine, theirs)
        } else {
            (theirs, mine)
        };
        assert_eq!(
            selection.current_legal_targets,
            vec![TargetRef::Object(offered)],
            "{qualifier}: only that controller's Equipment on A"
        );
        assert!(
            GameRunner::from_state(runner.state().clone())
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(chosen)),
                })
                .is_err(),
            "{qualifier}: the other controller's Equipment is refused"
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(offered)),
            })
            .expect("announce the qualified Equipment");
        runner.advance_until_stack_empty();
        let state = runner.state();
        assert_eq!(state.objects[&a].damage_marked, 5);
        assert_eq!(state.objects[&offered].zone, Zone::Exile);
        assert_eq!(state.objects[&chosen].zone, Zone::Battlefield);
        assert_eq!(state.objects[&eq_b].zone, Zone::Battlefield);
    }
}

/// The unqualified shape stays the supported control.
#[test]
fn unqualified_dependent_target_control() {
    let parsed = parse_oracle_text(
        "~ deals 5 damage to target creature. Exile up to one target Equipment attached to that creature.",
        "Probe",
        &[],
        &types("Instant"),
        &[],
    );
    assert!(unimplemented_names(&[&parsed.abilities[0]]).is_empty());
    let Effect::ChangeZone { target, .. } = chain(&parsed.abilities[0])[1] else {
        panic!("expected the Equipment exile");
    };
    assert_eq!(declared_slots(target), vec![0]);
}

// ---------------------------------------------------------------------------
// Round 4: incarnation-aware retarget authority (prompt admission, dependent
// pools, AI issuance)
// ---------------------------------------------------------------------------

/// Every `RetargetSpell` the AI's raw candidate generation issues for the open
/// prompt, each applied to a clone of the state: `(proposal, accepted)`.
fn ai_retarget_proposals(runner: &GameRunner) -> Vec<(Vec<Option<TargetRef>>, bool)> {
    engine::ai_support::candidate_actions(runner.state())
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::RetargetSpell { new_targets } => Some(new_targets),
            _ => None,
        })
        .map(|new_targets| {
            let mut probe = GameRunner::from_state(runner.state().clone());
            let accepted = probe
                .act(GameAction::RetargetSpell {
                    new_targets: new_targets.clone(),
                })
                .is_ok();
            (new_targets, accepted)
        })
        .collect()
}

/// The incarnation the root of `spell` pins for `id`.
fn root_pin(runner: &GameRunner, spell: ObjectId, id: ObjectId) -> u64 {
    runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .and_then(|ability| {
            ability
                .target_occurrences()
                .into_iter()
                .find_map(|(target, pin)| (target == TargetRef::Object(id)).then_some(pin))
                .flatten()
        })
        .map(|pin| pin.incarnation)
        .expect("the root pins the object")
}

/// CR 115.7a + CR 115.7d + CR 400.7: the only artifact was blinked and its new
/// object has hexproof, so it is no legal choice — resubmitting its id is the
/// RETAINED target, unchanged with its announced pin. "Choose new targets"
/// therefore accepts leaving every target as it is: the prompt discharges, the
/// old artifact pin stays, and the spell resolves with the artifact illegal.
#[test]
fn choose_new_targets_retains_a_target_whose_new_object_is_not_a_legal_choice() {
    const TEXT: &str = "Tap target artifact. Target player loses 1 life.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P1, "Only Artifact", "")
        .id();
    let spell = free_spell(&mut scenario, "Probe", true, TEXT);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    runner
        .cast(spell)
        .target_objects(&[artifact])
        .target_player(P1)
        .commit();
    let announced = root_pin(&runner, spell, artifact);
    blink(runner.state_mut(), artifact);
    give_hexproof(runner.state_mut(), artifact);
    runner.cast(redirect).target_objects(&[spell]).commit();
    drive_to_retarget_prompt(&mut runner);
    let current = match &runner.state().waiting_for {
        WaitingFor::RetargetChoice {
            current_targets, ..
        } => current_targets.clone(),
        other => panic!("expected RetargetChoice, got {other:?}"),
    };
    let proposals = ai_retarget_proposals(&runner);
    assert!(
        proposals
            .iter()
            .any(|(targets, _)| targets.len() == current.len()
                && targets.iter().all(Option::is_none)),
        "the unchanged anchor is offered"
    );
    assert!(proposals.iter().all(|(_, accepted)| *accepted));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![None; current.len()],
        })
        .expect("leaving every target unchanged is accepted");
    assert_eq!(root_pin(&runner, spell, artifact), announced, "pin kept");
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&artifact].tapped);
    assert_eq!(runner.life(P1), 19);
}

/// CR 115.7e: both the creature and its Equipment left and returned, and the
/// new Equipment is on the new creature. Resubmitting both ids elects both new
/// objects — the Equipment is offered in its position's pool and the final
/// selection is legal, so it is accepted and both are affected.
#[test]
fn choose_new_targets_offers_and_accepts_a_same_id_re_election_of_both_targets() {
    let mut board = copy_board();
    let (a, eq1, redirect, fiery) = (board.a, board.eq1, board.redirect, board.fiery);
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| {
            blink(state, a);
            blink(state, eq1);
            attach::attach_to(state, eq1, a);
        },
        redirect,
    );
    let pools = retarget_pools(&board.runner);
    assert!(
        pools[1].contains(&TargetRef::Object(eq1)),
        "the new Equipment 1 is offered"
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[a, eq1]).into_iter().map(Some).collect(),
        })
        .expect("electing both new objects is legal");
    assert_ne!(equipment_pin(&board.runner, fiery, eq1), 0, "re-pinned");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&a].damage_marked, 5);
    assert_eq!(state.objects[&eq1].zone, Zone::Exile);
}

/// The AI never issues a retarget the reducer rejects. Lightning-Bolt-like
/// spell at A; A blinked and its new object given hexproof; then "choose new
/// targets": every raw proposal (the anchor included) is accepted. Control: the
/// same board without the blink.
#[test]
fn ai_retarget_proposals_are_all_accepted_after_a_blink() {
    const BOLT: &str = "~ deals 3 damage to any target.";
    for blinked in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
        let b = scenario.add_creature(P1, "Creature B", 2, 7).id();
        let bolt = free_spell(&mut scenario, "Bolt", true, BOLT);
        let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
        let mut runner = scenario.build();
        runner.cast(bolt).target_objects(&[a]).commit();
        if blinked {
            blink(runner.state_mut(), a);
            give_hexproof(runner.state_mut(), a);
        }
        runner.cast(redirect).target_objects(&[bolt]).commit();
        drive_to_retarget_prompt(&mut runner);
        let proposals = ai_retarget_proposals(&runner);
        assert!(
            proposals
                .iter()
                .any(|(targets, _)| *targets == vec![Some(TargetRef::Object(b))]),
            "blinked={blinked}: reach guard, [B] is proposed"
        );
        assert!(
            proposals.iter().all(|(_, accepted)| *accepted),
            "blinked={blinked}: every proposal is accepted, got {proposals:?}"
        );
    }
}

/// The AI never issues a combination the final-set check rejects: with
/// Equipment 1 still on A, `[B, Equipment 1]` would make the unchanged
/// Equipment target illegal (CR 115.7d), so it is not proposed.
#[test]
fn ai_retarget_proposals_respect_the_dependent_final_set_check() {
    let mut board = copy_board();
    let (a, b, eq1, redirect) = (board.a, board.b, board.eq1, board.redirect);
    fiery_then(&mut board, &[a, eq1], |_| {}, redirect);
    let proposals = ai_retarget_proposals(&board.runner);
    assert!(
        proposals
            .iter()
            .any(|(targets, _)| *targets == vec![None, None]),
        "reach guard: the anchor is proposed"
    );
    assert!(
        !proposals
            .iter()
            .any(|(targets, _)| *targets == vec![Some(TargetRef::Object(b)), None]),
        "[B, Equipment 1] is not proposed"
    );
    assert!(proposals.iter().all(|(_, accepted)| *accepted));
}

// ---------------------------------------------------------------------------
// Round 5: compatibility encoding; the all-unchanged response (CR 115.7d)
// ---------------------------------------------------------------------------

/// The incarnation node `depth` (0 = root, following `sub_ability`) of
/// `spell` pins for `id`.
fn node_pin(runner: &GameRunner, spell: ObjectId, depth: usize, id: ObjectId) -> u64 {
    let mut node = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .expect("spell on the stack");
    for _ in 0..depth {
        node = node.sub_ability.as_deref().expect("chain node");
    }
    node.target_occurrences()
        .into_iter()
        .find_map(|(target, pin)| (target == TargetRef::Object(id)).then_some(pin))
        .flatten()
        .map(|pin| pin.incarnation)
        .expect("node pins the object")
}

/// R4-P1 (CR 115.7a + CR 115.7d): the same "choose new targets" answer means
/// the same thing whether the prompt carries its per-position addresses or is
/// a compatibility payload without them. Bolt at A; A blinked; with hexproof
/// the new A is no legal choice, so resubmitting `[A]` keeps the announced pin;
/// without hexproof it elects the new A and re-pins it.
#[test]
fn compatibility_and_full_retarget_prompts_share_one_changed_verdict() {
    const BOLT: &str = "~ deals 3 damage to any target.";
    for hexproof in [true, false] {
        for compat in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
            scenario.add_creature(P1, "Creature B", 2, 7);
            let bolt = free_spell(&mut scenario, "Bolt", true, BOLT);
            let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
            let mut runner = scenario.build();
            runner.cast(bolt).target_objects(&[a]).commit();
            let announced = root_pin(&runner, bolt, a);
            blink(runner.state_mut(), a);
            if hexproof {
                give_hexproof(runner.state_mut(), a);
            }
            runner.cast(redirect).target_objects(&[bolt]).commit();
            drive_to_retarget_prompt(&mut runner);
            if compat {
                // A payload predating the per-position fields: both default
                // to empty (`#[serde(default)]`).
                let mut value = serde_json::to_value(&runner.state().waiting_for).unwrap();
                let fields = value
                    .as_object_mut()
                    .and_then(|outer| outer.values_mut().find_map(|v| v.as_object_mut()))
                    .filter(|inner| inner.contains_key("slots"))
                    .or(None);
                let fields = match fields {
                    Some(fields) => fields,
                    None => value.as_object_mut().expect("tagged object"),
                };
                assert!(fields.remove("slots").is_some(), "reach guard: slots field");
                assert!(fields.remove("slot_pools").is_some());
                runner.state_mut().waiting_for = serde_json::from_value(value).unwrap();
                let WaitingFor::RetargetChoice {
                    slots, slot_pools, ..
                } = &runner.state().waiting_for
                else {
                    panic!("restored prompt");
                };
                assert!(slots.is_empty() && slot_pools.is_empty());
            }
            // CR 115.7d: keeping is explicit (`None`); choosing A (`Some`) is
            // an election of the current A, legal only without hexproof.
            let elect = GameAction::RetargetSpell {
                new_targets: vec![Some(TargetRef::Object(a))],
            };
            if hexproof {
                assert!(
                    GameRunner::from_state(runner.state().clone())
                        .act(elect)
                        .is_err(),
                    "compat={compat}: electing the hexproof A is refused"
                );
                runner
                    .act(GameAction::RetargetSpell {
                        new_targets: vec![None],
                    })
                    .expect("keeping A is accepted");
            } else {
                runner.act(elect).expect("electing the new A is accepted");
            }
            let pin = root_pin(&runner, bolt, a);
            if hexproof {
                assert_eq!(pin, announced, "compat={compat}: retained, pin kept");
            } else {
                assert_ne!(pin, announced, "compat={compat}: elected, re-pinned");
            }
        }
    }
}

const EQUIPMENT_AND_AURA: &str = "Tap target creature. Exile target Equipment attached to that creature. Exile target Aura attached to that creature.";

/// CR 115.7d: two dependents on one creature. Equipment 1 was blinked onto C;
/// the Aura is still on A and C has no Aura. Electing the new Equipment 1 is
/// illegal with A, and moving to C would make the unchanged Aura illegal — but
/// leaving every target unchanged is always legal: the all-unchanged response
/// is accepted, every pin is kept, and the spell resolves with the stale
/// Equipment target illegal.
#[test]
fn leaving_every_target_unchanged_is_always_accepted() {
    let parsed = parse_oracle_text(EQUIPMENT_AND_AURA, "Probe", &[], &types("Instant"), &[]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard: supported, got {:?}",
        chain(&parsed.abilities[0])
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let c = scenario.add_creature(P1, "Creature C", 2, 7).id();
    let eq1 = equipment(&mut scenario, P1, "Equipment 1");
    let aura1 = aura(&mut scenario, P1, "Aura 1");
    let spell = free_spell(&mut scenario, "Probe", true, EQUIPMENT_AND_AURA);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq1, a);
    attach::attach_to(runner.state_mut(), aura1, a);
    runner.cast(spell).target_objects(&[a, eq1, aura1]).commit();
    blink(runner.state_mut(), eq1);
    attach::attach_to(runner.state_mut(), eq1, c);
    runner.cast(redirect).target_objects(&[spell]).commit();
    drive_to_retarget_prompt(&mut runner);
    let anchor = objects(&[a, eq1, aura1]);
    let proposals = ai_retarget_proposals(&runner);
    assert!(
        proposals
            .iter()
            .any(|(t, ok)| *t == vec![None; anchor.len()] && *ok),
        "the AI's list holds the accepted anchor, got {proposals:?}"
    );
    assert!(proposals.iter().all(|(_, ok)| *ok));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![None; anchor.len()],
        })
        .expect("leaving every target unchanged is accepted");
    assert_eq!(node_pin(&runner, spell, 1, eq1), 0, "stale pin kept");
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(state.objects[&a].tapped);
    assert_eq!(
        state.objects[&eq1].zone,
        Zone::Battlefield,
        "stale: illegal"
    );
    assert_eq!(state.objects[&aura1].zone, Zone::Exile);
}

const CREATURE_AND_LAND: &str = "Tap target creature. Exile target Equipment attached to that creature. Tap target land. Exile target Fortification attached to that land.";

/// The verifier's four-target board: Equipment and Fortification blinked onto
/// C and L2. Returns the runner with the "choose new targets" prompt open and
/// `(spell, a, c, eq, l1, l2, ft)`.
fn four_target_board() -> (GameRunner, [ObjectId; 7]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let c = scenario.add_creature(P1, "Creature C", 2, 7).id();
    let l1 = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Red);
    let l2 = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Red);
    let eq = equipment(&mut scenario, P1, "Equipment");
    let ft = scenario
        .add_artifact_from_oracle(P1, "Fortification", "")
        .with_subtypes(vec!["Fortification"])
        .id();
    let spell = free_spell(&mut scenario, "Probe", true, CREATURE_AND_LAND);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq, a);
    attach::attach_to(runner.state_mut(), ft, l1);
    runner.cast(spell).target_objects(&[a, eq, l1, ft]).commit();
    blink(runner.state_mut(), eq);
    blink(runner.state_mut(), ft);
    attach::attach_to(runner.state_mut(), eq, c);
    attach::attach_to(runner.state_mut(), ft, l2);
    assert_eq!(
        runner.state().objects[&ft].attached_to,
        Some(AttachTarget::Object(l2)),
        "reach guard: the new Fortification is on L2"
    );
    runner.cast(redirect).target_objects(&[spell]).commit();
    drive_to_retarget_prompt(&mut runner);
    (runner, [spell, a, c, eq, l1, l2, ft])
}

/// R4-P2 (CR 115.7d + CR 115.7e): the prompt opens; the all-unchanged response
/// is accepted with every pin kept, and the AI holds it; the complete election
/// `[C, Equipment, L2, Fortification]` is accepted too.
#[test]
fn four_target_prompt_opens_and_accepts_the_anchor_and_the_full_election() {
    let (mut runner, [spell, a, _c, eq, l1, _l2, ft]) = four_target_board();
    let anchor = objects(&[a, eq, l1, ft]);
    let proposals = ai_retarget_proposals(&runner);
    assert!(proposals
        .iter()
        .any(|(t, ok)| *t == vec![None; anchor.len()] && *ok));
    assert!(proposals.iter().all(|(_, ok)| *ok));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![None; anchor.len()],
        })
        .expect("the anchor is accepted");
    assert_eq!(node_pin(&runner, spell, 1, eq), 0);
    assert_eq!(node_pin(&runner, spell, 3, ft), 0);

    let (mut runner, [spell, _a, c, eq, _l1, l2, ft]) = four_target_board();
    runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[c, eq, l2, ft]).into_iter().map(Some).collect(),
        })
        .expect("the complete election is accepted");
    assert_ne!(node_pin(&runner, spell, 1, eq), 0);
    assert_ne!(node_pin(&runner, spell, 3, ft), 0);
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&eq].zone, Zone::Exile);
    assert_eq!(state.objects[&ft].zone, Zone::Exile);
    assert!(state.objects[&c].tapped && state.objects[&l2].tapped);
}

// ---------------------------------------------------------------------------
// Round 6: one object in two positions of one node (id-keyed pins)
// ---------------------------------------------------------------------------

/// Exchange-style spell targeting artifact creature A in BOTH positions of one
/// node; A then leaves and returns (a new object) and loses the artifact type,
/// so the new A is a legal choice for the creature position only. Returns the
/// runner with "choose new targets" open, plus `(spell, a)`.
fn same_object_two_positions_board(
    text: &str,
    with_player: bool,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature(P1, "Artifact Creature A", 2, 7)
        .as_artifact()
        .as_creature()
        .id();
    let spell = free_spell(&mut scenario, "Probe", true, text);
    let redirect = free_spell(&mut scenario, "Redirect", true, REDIRECT);
    let mut runner = scenario.build();
    let commit = runner.cast(spell).target_objects(&[a, a]);
    let commit = if with_player {
        commit.target_player(P1)
    } else {
        commit
    };
    commit.commit();
    blink(runner.state_mut(), a);
    runner.state_mut().add_transient_continuous_effect(
        a,
        P1,
        engine::types::ability::Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: a },
        vec![engine::types::ability::ContinuousModification::RemoveType {
            core_type: engine::types::CoreType::Artifact,
        }],
        None,
    );
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(!runner.state().objects[&a]
        .card_types
        .core_types
        .contains(&engine::types::CoreType::Artifact));
    runner.cast(redirect).target_objects(&[spell]).commit();
    drive_to_retarget_prompt(&mut runner);
    let WaitingFor::RetargetChoice { slot_pools, .. } = &runner.state().waiting_for else {
        panic!("expected RetargetChoice");
    };
    assert!(
        !slot_pools[0].contains(&TargetRef::Object(a))
            && slot_pools[1].contains(&TargetRef::Object(a)),
        "reach guard: the new A is legal for the creature position only"
    );
    (runner, spell, a)
}

/// CR 115.3 + CR 115.7d + CR 400.7: keeping both positions (`[keep, keep]`)
/// leaves every target unchanged: accepted, the announced pin kept. Choosing
/// `A` at the artifact position is an election of the new A, which is not an
/// artifact, so `[A, A]` is refused — a keep is said explicitly, never
/// inferred from a resubmitted id.
#[test]
fn same_object_in_two_positions_anchor_keeps_its_announced_pin() {
    const EXCHANGE: &str = "Exchange control of target artifact and target creature.";
    let (mut runner, spell, a) = same_object_two_positions_board(EXCHANGE, false);
    assert_eq!(root_pin(&runner, spell, a), 0, "reach guard");
    assert!(
        GameRunner::from_state(runner.state().clone())
            .act(GameAction::RetargetSpell {
                new_targets: objects(&[a, a]).into_iter().map(Some).collect(),
            })
            .is_err(),
        "electing the new A at the artifact position is refused"
    );
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![None, None],
        })
        .expect("leaving every target unchanged is accepted");
    assert_eq!(root_pin(&runner, spell, a), 0, "the retained pin is kept");
}

/// CR 115.3 + CR 115.7d + CR 400.7 (positional occurrence pins): a partial
/// change `[keep, A, P0]` — keep the artifact position, elect A at the
/// creature position, change the player — is realizable. Each occurrence carries its own pin, so the artifact position
/// RETAINS the announced (old) A with its announced pin while the creature
/// position ELECTS the new A with a fresh pin; electing at one position no
/// longer re-pins the other. Before positional pins this reading was refused.
#[test]
fn same_object_in_two_positions_partial_change_elects_per_position() {
    const TEXT: &str =
        "Exchange control of target artifact and target creature. Target player loses 1 life.";
    let parsed = parse_oracle_text(TEXT, "Probe", &[], &types("Instant"), &[]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard"
    );
    let (mut runner, spell, a) = same_object_two_positions_board(TEXT, true);
    let new_incarnation = runner.state().objects[&a].incarnation;
    assert_ne!(new_incarnation, 0, "reach guard: A is a new object");
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![
                None,
                Some(TargetRef::Object(a)),
                Some(TargetRef::Player(P0)),
            ],
        })
        .expect("retain old A, elect new A, change the player");
    let root = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .expect("probe on the stack");
    let pins: Vec<Option<u64>> = root
        .aligned_target_pins()
        .into_iter()
        .map(|pin| pin.map(|pin| pin.incarnation))
        .collect();
    assert_eq!(
        pins,
        vec![Some(0), Some(new_incarnation)],
        "the artifact position keeps its announced pin; the creature position \
         carries the elected incarnation"
    );
}

// ---------------------------------------------------------------------------
// Derived quantity slot reading a declared slot (CR 608.2b)
// ---------------------------------------------------------------------------

/// An engine-defined two-node spell: "Tap target creature." then gain life equal
/// to the mana value of target Equipment attached to that creature — the second
/// slot is DERIVED from the quantity (`TargetObjectManaValue`) and its filter
/// reads declared slot 0. Returns `(runner, spell, a, e, c)` with `[A, E]`
/// announced, Equipment E (mana value 1) on creature A, and creature C.
fn derived_mana_value_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let c = scenario.add_creature(P1, "Creature C", 2, 7).id();
    let e = scenario
        .add_artifact_from_oracle(P1, "Equipment E", "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let equipment_of_slot_0 = TargetFilter::Typed(
        TypedFilter::default()
            .subtype("Equipment".to_string())
            .properties(vec![FilterProp::AttachedTo {
                to: AttachmentReferent::DeclaredTarget { slot: 0 },
            }]),
    );
    let mut tap = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: engine::types::ability::EffectScope::Single,
            state: TapStateChange::Tap,
        },
    );
    tap.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::TargetObjectManaValue {
                    filter: Box::new(equipment_of_slot_0),
                },
            },
            player: TargetFilter::Controller,
        },
    )));
    let spell = scenario
        .add_spell_to_hand(P0, "Derived Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(tap)
        .id();
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), e, a);
    runner.cast(spell).target_objects(&[a, e]).commit();
    let declared = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .map(|ability| {
            std::iter::successors(Some(ability), |node| node.sub_ability.as_deref())
                .flat_map(|node| node.targets.clone())
                .collect::<Vec<_>>()
        })
        .expect("spell on the stack");
    assert_eq!(
        declared,
        objects(&[a, e]),
        "reach guard: both slots announced"
    );
    (runner, spell, a, e, c)
}

/// CR 608.2b: the creature became an illegal target, so the derived Equipment
/// slot — "attached to that creature" — gets no information from it and is
/// illegal too: with every target illegal the spell does nothing (no tap, no
/// life). Controls: a legal creature taps it and gains 1; the Equipment moved
/// off the creature is illegal while the tap still happens.
#[test]
fn derived_slot_reading_an_illegal_declared_slot_is_illegal() {
    let (mut runner, _spell, a, _e, _c) = derived_mana_value_board();
    give_hexproof(runner.state_mut(), a);
    let before = runner.life(P0);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&a].tapped);
    assert_eq!(runner.life(P0), before, "no life from an illegal referent");

    let (mut runner, _spell, a, _e, _c) = derived_mana_value_board();
    let before = runner.life(P0);
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&a].tapped, "control: tapped");
    assert_eq!(runner.life(P0), before + 1, "control: mana value 1");

    let (mut runner, _spell, a, e, c) = derived_mana_value_board();
    attach::attach_to(runner.state_mut(), e, c);
    let before = runner.life(P0);
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&a].tapped, "control: tapped");
    assert_eq!(
        runner.life(P0),
        before,
        "control: detached Equipment is illegal"
    );
}

// ---------------------------------------------------------------------------
// Matt's M2: a partial retarget keeps an old, now-illegal target (CR 115.7d)
// ---------------------------------------------------------------------------

/// Fiery Annihilation at `[A, Equipment 1]`; Equipment 1 then leaves and
/// returns (a new object, CR 400.7) and is attached to `host`; Redirect opens
/// "choose new targets". The blink and re-attachment are the test helpers
/// (`blink`, `attach::attach_to`) standing in for Planar Incision and Magnetic
/// Theft.
fn m2_board(host: impl FnOnce(&CopyBoard) -> ObjectId) -> CopyBoard {
    let mut board = copy_board();
    let (a, eq1, redirect) = (board.a, board.eq1, board.redirect);
    let host = host(&board);
    fiery_then(
        &mut board,
        &[a, eq1],
        |state| {
            blink(state, eq1);
            attach::attach_to(state, eq1, host);
        },
        redirect,
    );
    assert_eq!(
        board.runner.state().objects[&eq1].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: the returned Equipment 1 is on its new host"
    );
    board
}

/// M2 (CR 115.7d: "the player may leave any number of the targets unchanged,
/// even if those targets would be illegal"): change the creature to B and KEEP
/// the departed Equipment 1. `[B, keep]` is accepted with Equipment 1's
/// announced pin kept; B takes 5 and the returned Equipment 1 is untouched.
#[test]
fn m2_partial_retarget_keeps_the_departed_equipment() {
    let board = m2_board(|board| board.a);
    let (b, eq1, fiery) = (board.b, board.eq1, board.fiery);
    let mut runner = board.runner;
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(b)), None],
        })
        .expect("[B, keep] is accepted");
    assert_eq!(
        equipment_pin(&runner, fiery, eq1),
        0,
        "the announced pin is kept"
    );
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(
        state.objects[&b].zone,
        Zone::Exile,
        "B took 5 and was exiled"
    );
    assert_eq!(
        state.objects[&eq1].zone,
        Zone::Battlefield,
        "Equipment 1 untouched"
    );
}

/// M2-elect (CR 400.7 + CR 115.7e): with the returned Equipment 1 on B,
/// choosing it is an election of the new object (re-pinned), legal for B.
#[test]
fn m2_partial_retarget_may_elect_the_returned_equipment_on_the_new_creature() {
    let board = m2_board(|board| board.b);
    let (b, eq1, fiery) = (board.b, board.eq1, board.fiery);
    let mut runner = board.runner;
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(b)), Some(TargetRef::Object(eq1))],
        })
        .expect("[B, Equipment 1] elects the returned Equipment");
    assert_ne!(
        equipment_pin(&runner, fiery, eq1),
        0,
        "re-pinned to the new object"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&eq1].zone, Zone::Exile);
}

/// M2-neg (CR 115.7e): with the returned Equipment 1 on A, electing it for B is
/// refused (it is not attached to that creature); keeping it is accepted.
#[test]
fn m2_partial_retarget_refuses_electing_equipment_not_on_the_new_creature() {
    let board = m2_board(|board| board.a);
    let (b, eq1) = (board.b, board.eq1);
    let mut runner = board.runner;
    assert!(runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(b)), Some(TargetRef::Object(eq1))],
        })
        .is_err());
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(b)), None],
        })
        .expect("control: keeping Equipment 1 is accepted");
}

/// M2 through the interaction surface: the RetargetChoice opportunity offers a
/// KEEP choice per position; `[B, keep Equipment 1]` previews and submits; a
/// keep id at another position, or twice, is not a legal answer.
#[test]
fn m2_partial_retarget_through_the_interaction_surface() {
    let board = m2_board(|board| board.a);
    let (b, eq1) = (board.b, board.eq1);
    let mut state = board.runner.state().clone();
    let WaitingFor::RetargetChoice {
        legal_new_targets,
        current_targets,
        ..
    } = state.waiting_for.clone()
    else {
        panic!("expected RetargetChoice");
    };
    bind_interaction_authority(&mut state, InteractionSessionId("m2".to_string())).expect("bind");
    let filtered = filter_state_for_viewer(&state, P0);
    let view = derive_viewer_interaction(&state, &filtered, P0);
    let opportunity = view
        .opportunities
        .iter()
        .find(|opportunity| {
            matches!(
                opportunity.response,
                InteractionOpportunityResponse::Schema { .. }
            )
        })
        .expect("a schema opportunity");
    let InteractionOpportunityResponse::Schema { candidates, .. } = &opportunity.response else {
        unreachable!()
    };
    let positions = current_targets.len();
    assert_eq!(
        candidates.len(),
        legal_new_targets.len() + positions,
        "every pool member, then one KEEP per position"
    );
    let keep = |position: usize| candidates[legal_new_targets.len() + position].id.clone();
    assert!(
        serde_json::to_string(&candidates[legal_new_targets.len() + 1])
            .unwrap()
            .contains("keep"),
        "reach: the KEEP choice is labelled"
    );
    let b_choice = candidates[legal_new_targets
        .iter()
        .position(|target| *target == TargetRef::Object(b))
        .expect("B is offered")]
    .id
    .clone();
    let request = |id: &str, choice_ids| InteractionPreviewRequest {
        request_id: PreviewRequestId(id.to_string()),
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Sequence { choice_ids },
    };
    for (id, choice_ids) in [
        ("wrong-position", vec![keep(1), keep(1)]),
        ("swapped", vec![b_choice.clone(), keep(0)]),
    ] {
        assert_eq!(
            preview_interaction(&state, P0, &request(id, choice_ids)).status,
            InteractionPreviewStatus::Rejected {
                reason: InteractionReasonCode::ConstraintUnsatisfied,
            },
            "{id}: a KEEP answers only its own position"
        );
    }
    let accepted = request("m2", vec![b_choice.clone(), keep(1)]);
    assert!(
        !matches!(
            preview_interaction(&state, P0, &accepted).status,
            InteractionPreviewStatus::Rejected { .. }
        ),
        "[B, keep] previews"
    );
    submit_interaction(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id: opportunity.interaction_id.clone(),
            response: accepted.response.clone(),
        },
    )
    .expect("[B, keep] submits");
    let mut runner = GameRunner::from_state(state);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&b].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&eq1].zone, Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// Keep actions preserve pins (CR 400.7 + CR 115.7d + CR 707.10c)
// ---------------------------------------------------------------------------

/// Fiery Annihilation at A; A is then blinked (a new object, CR 400.7) before
/// Twincast. Keeping the copy's creature, by "keep the rest"
/// (`KeepAllCopyTargets`) or by keeping the position (`ChooseTarget(None)`),
/// leaves it UNCHANGED with its announced pin: the copy still names the
/// departed A, so the returned A takes no damage. Choosing A is a distinct
/// election of the returned object (re-pinned) and the copy hits it: the
/// control proving the election is available, and that keep never silently
/// becomes it.
#[test]
fn keeping_a_blinked_creature_keeps_its_announced_pin() {
    for action in ["keep-rest", "keep-position", "elect"] {
        let mut board = copy_board();
        let (a, fiery, twincast) = (board.a, board.fiery, board.twincast);
        fiery_then(&mut board, &[a], |state| blink(state, a), twincast);
        let WaitingFor::CopyRetarget {
            copy_id,
            target_slots,
            current_slot,
            can_keep_rest,
            ..
        } = board.runner.state().waiting_for.clone()
        else {
            panic!("{action}: expected the copy walk");
        };
        assert_ne!(copy_id, fiery, "{action}: the walk is the Twincast copy");
        assert_eq!(root_pin(&board.runner, copy_id, a), 0, "{action}: reach");
        assert!(
            target_slots[current_slot].can_keep && can_keep_rest,
            "{action}: keeping is answerable"
        );
        assert!(
            target_slots[current_slot]
                .legal_alternatives
                .contains(&TargetRef::Object(a)),
            "{action}: electing the returned A is offered (distinct)"
        );
        match action {
            "keep-rest" => {
                board
                    .runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the rest");
            }
            "keep-position" => {
                board
                    .runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("keep A");
                if matches!(
                    board.runner.state().waiting_for,
                    WaitingFor::CopyRetarget { .. }
                ) {
                    board
                        .runner
                        .act(GameAction::KeepAllCopyTargets)
                        .expect("keep the rest");
                }
            }
            _ => {
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(a)),
                    })
                    .expect("elect the returned A");
                if matches!(
                    board.runner.state().waiting_for,
                    WaitingFor::CopyRetarget { .. }
                ) {
                    board
                        .runner
                        .act(GameAction::KeepAllCopyTargets)
                        .expect("keep the rest");
                }
            }
        }
        let returned = board.runner.state().objects[&a].incarnation;
        let elected = action == "elect";
        assert_eq!(
            root_pin(&board.runner, copy_id, a),
            if elected { returned } else { 0 },
            "{action}: keep preserves the announced pin; elect re-pins"
        );
        board.runner.advance_until_stack_empty();
        assert_eq!(
            board.runner.state().objects[&a].damage_marked,
            if elected { 5 } else { 0 },
            "{action}: only an elected returned A is hit"
        );
    }
}

// ---------------------------------------------------------------------------
// Forced "change a target" (CR 115.7a + CR 115.7b + CR 115.7e)
// ---------------------------------------------------------------------------

const SPELLSKITE: &str = "{U/P}: Change a target of target spell or ability to this creature. ({U/P} can be paid with either {U} or 2 life.)";

/// Fiery Annihilation at A (with Equipment 1 when `with_equipment`); P0 then
/// activates its own Spellskite, paying 2 life, which forces "a target" of
/// Fiery to Spellskite. Returns (runner, a, eq1, spellskite) after Fiery
/// resolves.
fn spellskite_on_fiery(with_equipment: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let eq1 = equipment(&mut scenario, P1, "Equipment 1");
    let spellskite = scenario
        .add_creature_from_oracle(P0, "Spellskite", 0, 4, SPELLSKITE)
        .id();
    let fiery = free_spell(
        &mut scenario,
        "Fiery Annihilation",
        true,
        FIERY_ANNIHILATION,
    );
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), eq1, a);
    let targets: Vec<ObjectId> = if with_equipment {
        vec![a, eq1]
    } else {
        vec![a]
    };
    runner.cast(fiery).target_objects(&targets).commit();
    runner
        .act(GameAction::ActivateAbility {
            source_id: spellskite,
            ability_index: 0,
        })
        .expect("activate Spellskite");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::PhyrexianPayment { .. }
    ) {
        runner
            .act(GameAction::SubmitPhyrexianChoices {
                choices: vec![engine::types::game_state::ShardChoice::PayLife],
            })
            .expect("pay 2 life");
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: Spellskite's ability is on Fiery"
    );
    // Resolve Spellskite's ability, then Fiery.
    for _ in 0..2 {
        let top = runner.state().stack.back().map(|e| e.id).unwrap();
        for _ in 0..8 {
            if !runner.state().stack.iter().any(|e| e.id == top) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
    }
    (runner, a, eq1, spellskite)
}

/// CR 115.7a + CR 115.7e: with Equipment 1 (attached to A) also targeted,
/// changing Fiery's creature to Spellskite would make the unchanged Equipment
/// target illegal (it is not attached to Spellskite). Only the final set is
/// evaluated, so that change is not legal; the original targets are
/// unchanged: A takes 5 and Equipment 1 is exiled. Control: with no Equipment
/// targeted, the creature target changes to Spellskite, which takes 5 and is
/// exiled by the rider.
#[test]
fn forced_change_a_target_keeps_the_dependent_equipment_target_legal() {
    let (runner, a, eq1, spellskite) = spellskite_on_fiery(true);
    let state = runner.state();
    assert_eq!(
        state.objects[&a].damage_marked, 5,
        "the creature target is unchanged"
    );
    assert_eq!(
        state.objects[&eq1].zone,
        Zone::Exile,
        "Equipment 1 is still exiled"
    );
    assert_eq!(state.objects[&spellskite].zone, Zone::Battlefield);

    let (runner, a, eq1, spellskite) = spellskite_on_fiery(false);
    let state = runner.state();
    assert_eq!(
        state.objects[&a].damage_marked, 0,
        "control: redirected away from A"
    );
    assert_eq!(
        state.objects[&spellskite].zone,
        Zone::Exile,
        "control: Spellskite takes 5 and the rider exiles it"
    );
    assert_eq!(state.objects[&eq1].zone, Zone::Battlefield);
}

/// CR 109.5: "you" in "target Equipment you control attached to that creature"
/// is the spell's controller even when an earlier slot targets a player. With
/// P1 chosen as the player target, only the caster's Equipment Mine on A is
/// offered (P1's Equipment Theirs is refused), and the chosen Mine is exiled
/// at resolution: admission and resolution agree. Choosing P0 as the player
/// is the control.
#[test]
fn you_control_dependent_target_binds_the_caster_not_an_earlier_player_target() {
    const TEXT: &str = "Target player loses 1 life. ~ deals 5 damage to target creature. Exile up to one target Equipment you control attached to that creature.";
    let parsed = parse_oracle_text(TEXT, "Probe", &[], &types("Instant"), &[]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard: the labelled synthetic parses supported"
    );
    for player in [P1, P0] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
        let mine = equipment(&mut scenario, P0, "Equipment Mine");
        let theirs = equipment(&mut scenario, P1, "Equipment Theirs");
        let spell = free_spell(&mut scenario, "Probe", true, TEXT);
        let mut runner = scenario.build();
        attach::attach_to(runner.state_mut(), mine, a);
        attach::attach_to(runner.state_mut(), theirs, a);
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("cast begins");
        for target in [TargetRef::Player(player), TargetRef::Object(a)] {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(target),
                })
                .expect("announce");
        }
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!(
                "{player:?}: expected the Equipment slot, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            selection.current_legal_targets,
            vec![TargetRef::Object(mine)],
            "{player:?}: only the caster's Equipment on A"
        );
        assert!(
            GameRunner::from_state(runner.state().clone())
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(theirs)),
                })
                .is_err(),
            "{player:?}: the targeted player's Equipment is refused"
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(mine)),
            })
            .expect("announce Mine");
        runner.advance_until_stack_empty();
        let state = runner.state();
        assert_eq!(
            state.objects[&mine].zone,
            Zone::Exile,
            "{player:?}: Mine exiled"
        );
        assert_eq!(state.objects[&theirs].zone, Zone::Battlefield);
        assert_eq!(state.objects[&a].damage_marked, 5);
    }
}

/// CR 109.5 + CR 108.3 + CR 115.1a: an earlier "target player" slot, then
/// "Exile up to one target Equipment <qualifier> attached to that creature", on one board where creature A wears Mine (the caster's), Theirs
/// (P1's) and Lent (owned by the caster, controlled by P1). "you control" is
/// {Mine} whatever player was targeted; "an opponent controls" is {Theirs,
/// Lent}; "you own" is {Mine, Lent}. Exactly that set is offered, the rest are
/// refused, and the chosen Equipment alone is exiled. (Every other qualifier
/// fails closed: `unadmitted_qualified_dependent_target_keeps_its_gap`; the
/// targeted Destroy shape too: `targeted_declared_slot_destroy_keeps_its_gap`.)
#[test]
fn admitted_qualified_dependent_exile_target_binds_each_qualifier() {
    {
        let verb = "Exile";
        for qualifier in ["you control", "an opponent controls", "you own"] {
            for player in [P1, P0] {
                let text = format!(
                    "Target player loses 1 life. ~ deals 5 damage to target creature. {verb} up to one target Equipment {qualifier} attached to that creature."
                );
                let label = format!("{verb} / {qualifier} / {player:?}");
                let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
                assert!(
                    unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
                    "{label}: reach guard, parses supported"
                );
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
                let mine = equipment(&mut scenario, P0, "Equipment Mine");
                let theirs = equipment(&mut scenario, P1, "Equipment Theirs");
                let lent = equipment(&mut scenario, P0, "Equipment Lent");
                let spell = free_spell(&mut scenario, "Probe", true, &text);
                let mut runner = scenario.build();
                {
                    let obj = runner.state_mut().objects.get_mut(&lent).unwrap();
                    obj.base_controller = Some(P1);
                    obj.controller = P1;
                }
                for equipment in [mine, theirs, lent] {
                    attach::attach_to(runner.state_mut(), equipment, a);
                }
                runner
                    .act(GameAction::CastSpell {
                        object_id: spell,
                        card_id: runner.state().objects[&spell].card_id,
                        targets: vec![],
                        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    })
                    .expect("cast begins");
                for target in [TargetRef::Player(player), TargetRef::Object(a)] {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(target),
                        })
                        .unwrap_or_else(|e| panic!("{label}: announce: {e:?}"));
                }
                let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for
                else {
                    panic!(
                        "{label}: expected the Equipment slot, got {:?}",
                        runner.state().waiting_for
                    );
                };
                let offered: Vec<ObjectId> = match qualifier {
                    "you control" => vec![mine],
                    "an opponent controls" => vec![theirs, lent],
                    _ => vec![mine, lent],
                };
                assert_eq!(
                    sorted(selection.current_legal_targets.clone()),
                    sorted(offered.iter().copied().map(TargetRef::Object).collect()),
                    "{label}: exactly the qualified Equipment on A"
                );
                for refused in [mine, theirs, lent]
                    .into_iter()
                    .filter(|e| !offered.contains(e))
                {
                    assert!(
                        GameRunner::from_state(runner.state().clone())
                            .act(GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(refused)),
                            })
                            .is_err(),
                        "{label}: unqualified Equipment refused"
                    );
                }
                let chosen = *offered.last().unwrap();
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(chosen)),
                    })
                    .expect("announce the qualified Equipment");
                assert!(
                    !matches!(
                        runner.state().waiting_for,
                        WaitingFor::TargetSelection { .. }
                    ),
                    "{label}: no further slot (no duplicate companion player slot)"
                );
                runner.advance_until_stack_empty();
                let state = runner.state();
                assert_eq!(state.objects[&chosen].zone, Zone::Exile, "{label}: exiled");
                for other in [mine, theirs, lent].into_iter().filter(|e| *e != chosen) {
                    assert_eq!(state.objects[&other].zone, Zone::Battlefield, "{label}");
                }
                assert_eq!(state.objects[&a].damage_marked, 5, "{label}");
                assert_eq!(state.objects[&a].zone, Zone::Battlefield, "{label}");
            }
        }
    }
}

/// CR 109.5 + CR 608.2c: a qualified dependent target admits exactly "you
/// control", "you own" and "an opponent controls". Every other control or
/// ownership qualifier ("they control", "that player controls", "controlled
/// by those opponents") names a player no controller reference binds yet, so
/// the whole clause keeps its strict gap (`unparsed_verb_arguments`, as Light
/// of Judgment's clause had before this class): no declared-slot referent and
/// no widened, unqualified Equipment target, for exile and destroy alike. The three admitted
/// phrasings are the controls.
#[test]
fn unadmitted_qualified_dependent_target_keeps_its_gap() {
    for (head, qualifier) in [
        ("Target player", "that player controls"),
        ("Target opponent", "that player controls"),
        ("Target opponent", "that opponent controls"),
        ("Target player", "controlled by that player"),
        ("Target player", "they control"),
        ("Target player", "controlled by those players"),
        ("Target opponent", "controlled by those opponents"),
    ] {
        for verb in ["Exile", "Destroy"] {
            let text = format!(
                "{head} loses 1 life. ~ deals 5 damage to target creature. {verb} up to one target Equipment {qualifier} attached to that creature."
            );
            let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
            let gaps: Vec<(String, String)> = chain(&parsed.abilities[0])
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Unimplemented { name, description } => {
                        Some((name.clone(), description.clone().unwrap_or_default()))
                    }
                    _ => None,
                })
                .collect();
            let label = format!("{verb} / {head} / {qualifier}");
            assert_eq!(
                gaps,
                vec![(
                    "unparsed_verb_arguments".to_string(),
                    format!("{verb} target Equipment {qualifier} attached to that creature"),
                )],
                "{label}: the whole clause is the strict gap"
            );
            assert!(
                chain(&parsed.abilities[0]).iter().all(|effect| !matches!(
                    effect,
                    Effect::ChangeZone { .. } | Effect::Destroy { .. }
                )),
                "{label}: no widened exile or destroy"
            );
            assert!(
                !serde_json::to_string(&parsed.abilities[0])
                    .unwrap()
                    .contains("DeclaredTarget"),
                "{label}: no declared-slot referent"
            );
        }
    }
    for qualifier in ["you control", "you own", "an opponent controls"] {
        let text = format!(
            "Target player loses 1 life. ~ deals 5 damage to target creature. Exile up to one target Equipment {qualifier} attached to that creature."
        );
        let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
        assert!(
            unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
            "{qualifier}: control parses supported"
        );
        assert!(serde_json::to_string(&parsed.abilities[0])
            .unwrap()
            .contains("DeclaredTarget"));
    }
}

/// The parsed "Target player loses 1 life. ~ deals 5 damage to target
/// creature. Exile up to one target Equipment you control attached to that
/// creature." with its exile target's filter replaced by `shape(you_leg)`.
fn typed_exile_probe(shape: impl Fn(TypedFilter) -> TargetFilter) -> AbilityDefinition {
    use engine::types::ability::ControllerRef;
    const TEXT: &str = "Target player loses 1 life. ~ deals 5 damage to target creature. Exile up to one target Equipment you control attached to that creature.";
    let mut head = parse_oracle_text(TEXT, "Typed Probe", &[], &types("Instant"), &[])
        .abilities
        .remove(0);
    let mut tail = &mut head;
    while !matches!(tail.effect.as_ref(), Effect::ChangeZone { .. }) {
        tail = tail.sub_ability.as_mut().expect("the exile node");
    }
    let Effect::ChangeZone { target, .. } = tail.effect.as_mut() else {
        unreachable!()
    };
    let TargetFilter::Typed(you) = target.clone() else {
        panic!("reach: a typed leg");
    };
    assert_eq!(you.controller, Some(ControllerRef::You), "reach");
    *target = shape(you);
    head
}

/// The typed probe's board: creatures A and B (P1); Mine (P0) and Theirs (P1)
/// attached to A; On B (P0) attached to B; Loose (P1) unattached.
struct TypedBoard {
    runner: GameRunner,
    spell: ObjectId,
    a: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
    on_b: ObjectId,
    loose: ObjectId,
}

/// The typed probe's board, before casting.
fn typed_board(head: &AbilityDefinition) -> TypedBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Creature A", 2, 7).id();
    let b = scenario.add_creature(P1, "Creature B", 2, 7).id();
    let mine = equipment(&mut scenario, P0, "Equipment Mine");
    let theirs = equipment(&mut scenario, P1, "Equipment Theirs");
    let on_b = equipment(&mut scenario, P0, "Equipment On B");
    let loose = equipment(&mut scenario, P1, "Equipment Loose");
    let spell = scenario
        .add_spell_to_hand(P0, "Typed Probe", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(head.clone())
        .id();
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), mine, a);
    attach::attach_to(runner.state_mut(), theirs, a);
    attach::attach_to(runner.state_mut(), on_b, b);
    TypedBoard {
        runner,
        spell,
        a,
        mine,
        theirs,
        on_b,
        loose,
    }
}

/// Answers the walk from creature A on: P1, then A, then a companion player
/// slot (if one is surfaced) with `companion`.
fn announce_player_and_host(runner: &mut GameRunner, a: ObjectId, companion: PlayerId) {
    for target in [TargetRef::Player(P1), TargetRef::Object(a)] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target),
            })
            .expect("announce");
    }
    if let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for {
        if !selection.current_legal_targets.is_empty()
            && selection
                .current_legal_targets
                .iter()
                .all(|t| matches!(t, TargetRef::Player(_)))
        {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(companion)),
                })
                .expect("announce the companion player");
        }
    }
}

/// Casts the typed probe and announces P1 and creature A, answering a
/// companion player slot (if one is surfaced) with `companion`.
fn cast_typed_probe(head: &AbilityDefinition, companion: PlayerId) -> TypedBoard {
    let mut board = typed_board(head);
    let (spell, a) = (board.spell, board.a);
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: board.runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast begins");
    announce_player_and_host(&mut board.runner, a, companion);
    board
}

/// CR 601.2c: the typed probe announced as a populated chain (P1, creature A,
/// the companion player on the Equipment node), the slots built on it, and the
/// object candidates of the Equipment slot. A populated chain is how a
/// relative player reference reads its player while slots are built.
fn populated_probe(
    board: &TypedBoard,
    head: &AbilityDefinition,
    companion: PlayerId,
) -> (
    engine::types::ability::ResolvedAbility,
    Vec<engine::types::game_state::TargetSelectionSlot>,
    Vec<TargetRef>,
) {
    use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
    let mut announced = build_resolved_from_def(head, board.spell, P0);
    announced.targets = vec![TargetRef::Player(P1)];
    let creature = announced
        .sub_ability
        .as_mut()
        .expect("reach: creature node");
    creature.targets = vec![TargetRef::Object(board.a)];
    let equipment = creature
        .sub_ability
        .as_mut()
        .expect("reach: Equipment node");
    equipment.targets = vec![TargetRef::Player(companion)];
    let slots = build_target_slots(board.runner.state(), &announced).expect("slots build");
    let offered = slots
        .iter()
        .filter(|slot| {
            matches!(
                slot.effect_kind,
                engine::types::ability::EffectKind::ChangeZone
            )
        })
        .flat_map(|slot| {
            slot.legal_targets
                .iter()
                .filter(|t| matches!(t, TargetRef::Object(_)))
                .cloned()
        })
        .collect();
    (announced, slots, offered)
}

fn sorted(mut targets: Vec<TargetRef>) -> Vec<TargetRef> {
    targets.sort_by_key(|t| format!("{t:?}"));
    targets
}

/// CR 109.5 (typed composition, not printed Oracle): a flat `Or` of admitted
/// legs, "Equipment you control" OR "Equipment an opponent controls", each
/// attached to that creature, binds both legs to the ability's controller and
/// unions them, so the offered set does not depend on the legs' order: Mine
/// and Theirs. Equipment on another creature and loose Equipment are refused,
/// and each offered choice resolves (exiled).
#[test]
fn flat_qualified_or_offers_the_same_union_in_either_order() {
    use engine::types::ability::ControllerRef;
    for you_first in [true, false] {
        let head = typed_exile_probe(|you| {
            let mut opponent = you.clone();
            opponent.controller = Some(ControllerRef::Opponent);
            let (you, opponent) = (TargetFilter::Typed(you), TargetFilter::Typed(opponent));
            TargetFilter::Or {
                filters: if you_first {
                    vec![you, opponent]
                } else {
                    vec![opponent, you]
                },
            }
        });
        for elect_theirs in [false, true] {
            let label = format!("you_first={you_first} elect_theirs={elect_theirs}");
            let TypedBoard {
                mut runner,
                spell: _,
                a,
                mine,
                theirs,
                on_b,
                loose,
            } = cast_typed_probe(&head, P1);
            let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
                panic!("{label}: expected the Equipment slot");
            };
            assert_eq!(
                sorted(selection.current_legal_targets.clone()),
                sorted(vec![TargetRef::Object(mine), TargetRef::Object(theirs)]),
                "{label}: the union of the bound legs"
            );
            for refused in [on_b, loose] {
                assert!(
                    GameRunner::from_state(runner.state().clone())
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(refused)),
                        })
                        .is_err(),
                    "{label}: not attached to A, refused"
                );
            }
            let chosen = if elect_theirs { theirs } else { mine };
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(chosen)),
                })
                .expect("announce the Equipment");
            runner.advance_until_stack_empty();
            let state = runner.state();
            assert_eq!(state.objects[&chosen].zone, Zone::Exile, "{label}: exiled");
            for other in [mine, theirs, on_b, loose]
                .into_iter()
                .filter(|o| *o != chosen)
            {
                assert_eq!(state.objects[&other].zone, Zone::Battlefield, "{label}");
            }
            assert_eq!(state.objects[&a].damage_marked, 5, "{label}");
        }
    }
}

/// Builds a typed exile-target shape from the parsed "you control" leg.
type ShapeFn = Box<dyn Fn(TypedFilter) -> TargetFilter>;

/// CR 109.5 + CR 601.2c (typed compositions, not printed Oracle): a
/// declared-slot referent binds only a flat admitted leg or a flat `Or` of
/// them. A leg naming a target player, as its controller or as its owner
/// (`Owned { TargetPlayer | TargetOpponent }`), or a nested `Or`/`And`
/// composite, has no binding authority yet, so the slot offers no candidates and accepts
/// none (no guessed binding), whichever player is the companion; the
/// optional slot is declined and nothing is exiled.
#[test]
fn unbindable_declared_slot_shapes_offer_no_candidates() {
    use engine::types::ability::ControllerRef;
    let target_player = |you: &TypedFilter| {
        let mut leg = you.clone();
        leg.controller = Some(ControllerRef::TargetPlayer);
        TargetFilter::Typed(leg)
    };
    let unqualified = |you: &TypedFilter| {
        let mut leg = you.clone();
        leg.controller = None;
        TargetFilter::Typed(leg)
    };
    let owned_by = |you: &TypedFilter, owner: ControllerRef| {
        let mut leg = you.clone();
        leg.controller = None;
        leg.properties.push(FilterProp::Owned { controller: owner });
        TargetFilter::Typed(leg)
    };
    let shapes: Vec<(&str, ShapeFn)> = vec![
        (
            "Owned{TargetPlayer}",
            Box::new(move |you| owned_by(&you, ControllerRef::TargetPlayer)),
        ),
        (
            "Owned{TargetOpponent}",
            Box::new(move |you| owned_by(&you, ControllerRef::TargetOpponent)),
        ),
        (
            "standalone TargetPlayer",
            Box::new(move |you| target_player(&you)),
        ),
        (
            "flat Or[You, TargetPlayer]",
            Box::new(move |you| TargetFilter::Or {
                filters: vec![TargetFilter::Typed(you.clone()), target_player(&you)],
            }),
        ),
        (
            "nested Or[Or[You, TargetPlayer]]",
            Box::new(move |you| TargetFilter::Or {
                filters: vec![TargetFilter::Or {
                    filters: vec![TargetFilter::Typed(you.clone()), target_player(&you)],
                }],
            }),
        ),
        (
            "And[Or[You, TargetPlayer], unqualified]",
            Box::new(move |you| TargetFilter::And {
                filters: vec![
                    TargetFilter::Or {
                        filters: vec![TargetFilter::Typed(you.clone()), target_player(&you)],
                    },
                    unqualified(&you),
                ],
            }),
        ),
        (
            "And[TargetPlayer, You]",
            Box::new(move |you| TargetFilter::And {
                filters: vec![target_player(&you), TargetFilter::Typed(you)],
            }),
        ),
        (
            "nested Or[Or[You]]",
            Box::new(move |you| TargetFilter::Or {
                filters: vec![TargetFilter::Or {
                    filters: vec![TargetFilter::Typed(you)],
                }],
            }),
        ),
    ];
    for (name, shape) in &shapes {
        let head = typed_exile_probe(shape);
        for companion in [P1, P0] {
            if name.contains("TargetOpponent") && companion == P0 {
                continue;
            }
            let label = format!("{name} companion={companion:?}");
            let (_, _, populated) = populated_probe(&typed_board(&head), &head, companion);
            assert!(
                populated.is_empty(),
                "{label}: no candidates on a populated chain, got {populated:?}"
            );
            let TypedBoard {
                mut runner,
                spell: _,
                a,
                mine,
                theirs,
                on_b,
                loose,
            } = cast_typed_probe(&head, companion);
            if let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for {
                assert!(
                    selection.current_legal_targets.is_empty(),
                    "{label}: no candidates, got {:?}",
                    selection.current_legal_targets
                );
                for refused in [mine, theirs, on_b, loose] {
                    assert!(
                        GameRunner::from_state(runner.state().clone())
                            .act(GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(refused)),
                            })
                            .is_err(),
                        "{label}: refused"
                    );
                }
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .unwrap_or_else(|e| panic!("{label}: decline: {e:?}"));
            }
            runner.advance_until_stack_empty();
            let state = runner.state();
            for equipment in [mine, theirs, on_b, loose] {
                assert_eq!(
                    state.objects[&equipment].zone,
                    Zone::Battlefield,
                    "{label}: nothing exiled"
                );
            }
            assert_eq!(
                state.objects[&a].damage_marked, 5,
                "{label}: the damage applies"
            );
        }
    }
}

/// CR 108.3 + CR 109.5 + CR 601.2c (typed composition, not printed Oracle):
/// an unqualified-controller leg owned by the target player or opponent
/// (`Owned { TargetPlayer | TargetOpponent }`) has no binding authority, like
/// a target-player controller. On a populated chain (where the reference reads
/// the announced player) the Equipment slot offers nothing at construction
/// and at selection, every Equipment is refused, and nothing is exiled. The
/// declaring owner's `Owned { You }` is the control on the same route: it
/// offers the caster's Equipment on every candidate host at construction,
/// Mine alone at selection, and exiles Mine.
#[test]
fn relatively_owned_declared_slot_target_offers_nothing_on_a_populated_chain() {
    use engine::game::ability_utils::begin_target_selection_for_ability;
    use engine::types::ability::ControllerRef;
    for owner in [
        ControllerRef::TargetPlayer,
        ControllerRef::TargetOpponent,
        ControllerRef::You,
    ] {
        let admitted = owner == ControllerRef::You;
        let head = typed_exile_probe(|you| {
            let mut leg = you;
            leg.controller = None;
            leg.properties.push(FilterProp::Owned {
                controller: owner.clone(),
            });
            TargetFilter::Typed(leg)
        });
        for companion in [P1, P0] {
            if owner == ControllerRef::TargetOpponent && companion == P0 {
                continue;
            }
            let label = format!("Owned{{{owner:?}}} companion={companion:?}");
            let mut board = typed_board(&head);
            let (announced, slots, offered) = populated_probe(&board, &head, companion);
            let expected: Vec<TargetRef> = if admitted {
                vec![TargetRef::Object(board.mine)]
            } else {
                vec![]
            };
            // Construction unions over every candidate of the declared slot
            // (A and B); selection narrows to the chosen host.
            let constructed: Vec<TargetRef> = if admitted {
                sorted(vec![
                    TargetRef::Object(board.mine),
                    TargetRef::Object(board.on_b),
                ])
            } else {
                vec![]
            };
            assert_eq!(sorted(offered), constructed, "{label}: slot construction");

            let (spell, a) = (board.spell, board.a);
            let runner = &mut board.runner;
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id: runner.state().objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                })
                .expect("cast begins");
            let progress =
                begin_target_selection_for_ability(runner.state(), &announced, &slots, &[])
                    .expect("reach: populated selection begins");
            let WaitingFor::TargetSelection {
                pending_cast,
                target_slots,
                selection,
                ..
            } = &mut runner.state_mut().waiting_for
            else {
                panic!("{label}: reach: the production pending cast");
            };
            *pending_cast.ability = announced;
            *target_slots = slots;
            *selection = progress;
            announce_player_and_host(runner, a, companion);
            let at_equipment: Vec<TargetRef> = match &runner.state().waiting_for {
                WaitingFor::TargetSelection { selection, .. } => selection
                    .current_legal_targets
                    .iter()
                    .filter(|t| matches!(t, TargetRef::Object(_)))
                    .cloned()
                    .collect(),
                _ => vec![],
            };
            assert_eq!(at_equipment, expected, "{label}: selection");
            let all = [board.mine, board.theirs, board.on_b, board.loose];
            for equipment in all {
                let accepted = matches!(
                    runner.state().waiting_for,
                    WaitingFor::TargetSelection { .. }
                ) && GameRunner::from_state(runner.state().clone())
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(equipment)),
                    })
                    .is_ok();
                assert_eq!(
                    accepted,
                    admitted && equipment == board.mine,
                    "{label}: {equipment:?} acceptance"
                );
            }
            if let WaitingFor::TargetSelection { pending_cast, .. } =
                &mut runner.state_mut().waiting_for
            {
                // The populated seeds were the probe's context; the commit
                // appends the walk's own choices.
                let mut node = Some(pending_cast.ability.as_mut());
                while let Some(ability) = node {
                    ability.targets.clear();
                    node = ability.sub_ability.as_deref_mut();
                }
                let chosen = admitted.then_some(TargetRef::Object(board.mine));
                runner
                    .act(GameAction::ChooseTarget { target: chosen })
                    .unwrap_or_else(|e| panic!("{label}: commit: {e:?}"));
            }
            runner.advance_until_stack_empty();
            let state = runner.state();
            for equipment in all {
                let exiled = admitted && equipment == board.mine;
                assert_eq!(
                    state.objects[&equipment].zone,
                    if exiled {
                        Zone::Exile
                    } else {
                        Zone::Battlefield
                    },
                    "{label}: {equipment:?}"
                );
            }
        }
    }
}

/// CR 115.1a + CR 601.2c: a targeted destroy of a declared-slot referent
/// ("Destroy [up to one] target Equipment [you control | an opponent controls
/// | you own] attached to that creature") has no supported lowering and no
/// printed producer, so the whole clause keeps its strict gap: no destroy
/// effect and no declared-slot referent. Light of Judgment's untargeted
/// "Destroy up to one Equipment attached to that creature" is the control.
#[test]
fn targeted_declared_slot_destroy_keeps_its_gap() {
    for head in ["Destroy target", "Destroy up to one target"] {
        for qualifier in ["", "you control ", "an opponent controls ", "you own "] {
            let label = format!("{head} / {qualifier}");
            let text = format!(
                "Target player loses 1 life. ~ deals 5 damage to target creature. {head} Equipment {qualifier}attached to that creature."
            );
            let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
            let gaps: Vec<(String, String)> = chain(&parsed.abilities[0])
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Unimplemented { name, description } => {
                        Some((name.clone(), description.clone().unwrap_or_default()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                gaps,
                vec![(
                    "unparsed_verb_arguments".to_string(),
                    format!("Destroy target Equipment {qualifier}attached to that creature"),
                )],
                "{label}: the whole clause is the strict gap"
            );
            assert!(
                chain(&parsed.abilities[0])
                    .iter()
                    .all(|effect| !matches!(effect, Effect::Destroy { .. })),
                "{label}: no inert destroy"
            );
            assert!(
                !serde_json::to_string(&parsed.abilities[0])
                    .unwrap()
                    .contains("DeclaredTarget"),
                "{label}: no declared-slot referent"
            );
        }
    }
    let parsed = parse_oracle_text(
        "Target player loses 1 life. ~ deals 5 damage to target creature. Destroy up to one Equipment attached to that creature.",
        "Probe",
        &[],
        &types("Instant"),
        &[],
    );
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "the untargeted resolution choice stays supported"
    );
}
