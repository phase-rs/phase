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
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AttachmentReferent, Effect, FilterProp, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
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
    assert_eq!(
        target_slots.len(),
        1,
        "copy retargeting exposes the root (creature) slot only"
    );
    target_slots[*current_slot].legal_alternatives.clone()
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
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("A -> B accepted");
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
/// creature to B would make the unchanged Equipment target illegal, so B is
/// not offered and a direct submission is rejected; keeping the targets works.
#[test]
fn copy_may_not_change_the_creature_away_from_its_legal_equipment_target() {
    let mut board = copy_board();
    let (a, b, c, eq1) = (board.a, board.b, board.c, board.eq1);
    let twincast = board.twincast;
    fiery_then(&mut board, &[a, eq1], |_| {}, twincast);
    let offered = copy_alternatives(&board.runner);
    assert!(
        offered.contains(&TargetRef::Object(a)),
        "reach guard: the slot's pool is populated"
    );
    assert!(!offered.contains(&TargetRef::Object(b)));
    assert!(!offered.contains(&TargetRef::Object(c)));
    assert!(board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
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
                new_targets: objects(&[b, eq1]),
            })
            .is_err(),
        "Equipment 1 would become illegal"
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[b, eq2]),
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
                new_targets: objects(&[b, eq3]),
            })
            .is_err(),
        "Equipment 3 is attached to A, not to the newly chosen B"
    );
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[a, eq3]),
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
            new_targets: objects(&[b, eq1]),
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
/// node's pins are id-keyed (one A pin for both positions), and the creature
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
            new_targets: objects(&[a, a]),
        })
        .expect("B -> A in the creature slot");
    let pins = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .map(|ability| ability.selected_target_incarnations.clone())
        .expect("probe on the stack");
    assert_eq!(
        pins.iter().filter(|pin| pin.object_id == a).count(),
        1,
        "reach guard: one id-keyed A pin serves both positions"
    );
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&eq_a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&eq_b].zone, Zone::Battlefield);
}

/// H2 negative (CR 400.7): a declared slot whose declaring node holds no pin,
/// or a stale pin, names nothing — the pin is never recovered from the live
/// row, so no Equipment is destroyed though the damage is dealt.
#[test]
fn missing_or_stale_pin_names_no_referent() {
    const TEXT: &str =
        "Target player loses 1 life. ~ deals 2 damage to target creature. Destroy all Equipment attached to that creature.";
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
                for pin in &mut node.selected_target_incarnations {
                    pin.incarnation += 100;
                }
            } else {
                node.selected_target_incarnations.clear();
            }
        }
        let outcome = commit.resolve();
        outcome.assert_life_delta(P1, -1);
        assert_eq!(
            outcome.zone_of(b.eq_a),
            Zone::Battlefield,
            "stale={stale}: no referent"
        );
    }
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
            node.selected_target_incarnations
                .iter()
                .find(|pin| pin.object_id == eq)
        })
        .map(|pin| pin.incarnation)
        .expect("the Equipment node pins Equipment 1")
}

/// Copy (CR 707.10c + CR 115.7d): the root-only copy choice never writes the
/// Equipment position, so the stale Equipment target stays unchanged (its pin
/// is kept) and is already illegal; B is offered and accepted. The copy hits B
/// (rider: exiled) and the new Equipment 1 is untouched by either spell.
#[test]
fn copy_may_change_the_creature_when_its_equipment_target_was_blinked() {
    let mut board = stale_equipment_board(|b| b.twincast, |b| b.a);
    let (a, b, eq1) = (board.a, board.b, board.eq1);
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
    let copy_id = board
        .runner
        .state()
        .stack
        .back()
        .map(|entry| entry.id)
        .expect("the copy is on the stack");
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
                new_targets: objects(&[b, eq1]),
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
            new_targets: objects(&[b, eq1]),
        })
        .expect("electing the new Equipment 1 on B is legal");
    assert_ne!(equipment_pin(&board.runner, fiery, eq1), 0, "re-pinned");
    board.runner.advance_until_stack_empty();
    let state = board.runner.state();
    assert_eq!(state.objects[&eq1].zone, Zone::Exile);
    assert_eq!(state.objects[&b].zone, Zone::Exile);
}

/// CR 115.1a + CR 601.2c: a controller-qualified dependent target ("target
/// Equipment you control / an opponent controls attached to that creature") is
/// not supported — its slot is built through per-player construction, which
/// does not compose with the declared-slot referent — so it keeps a gap. The
/// unqualified shape is the supported control.
#[test]
fn controller_qualified_dependent_target_keeps_its_gap() {
    for qualifier in ["you control ", "an opponent controls "] {
        let text = format!(
            "~ deals 5 damage to target creature. Exile up to one target Equipment {qualifier}attached to that creature."
        );
        let parsed = parse_oracle_text(&text, "Probe", &[], &types("Instant"), &[]);
        let effects = chain(&parsed.abilities[0]);
        assert!(
            matches!(effects[0], Effect::DealDamage { .. }),
            "{qualifier}: reach guard, the damage head parses, got {effects:?}"
        );
        // The exile path's fail-closed gap (`parse_exile_ast` declines the
        // whole clause), carrying the qualified fragment.
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::Unimplemented { name, description: Some(text), .. }
                    if name == "unparsed_verb_arguments"
                        && text.contains(&format!("Equipment {qualifier}attached to that creature"))
            )),
            "{qualifier}: must keep the qualified-clause gap, got {effects:?}"
        );
        assert!(
            !serde_json::to_string(&parsed.abilities[0])
                .unwrap()
                .contains("DeclaredTarget"),
            "{qualifier}: no declared-slot referent may be emitted"
        );
    }
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
fn ai_retarget_proposals(runner: &GameRunner) -> Vec<(Vec<TargetRef>, bool)> {
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
                .selected_target_incarnations
                .iter()
                .find(|pin| pin.object_id == id)
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
        proposals.iter().any(|(targets, _)| *targets == current),
        "the unchanged anchor is offered"
    );
    assert!(proposals.iter().all(|(_, accepted)| *accepted));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: current,
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
            new_targets: objects(&[a, eq1]),
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
                .any(|(targets, _)| *targets == vec![TargetRef::Object(b)]),
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
            .any(|(targets, _)| *targets == objects(&[a, eq1])),
        "reach guard: the anchor is proposed"
    );
    assert!(
        !proposals
            .iter()
            .any(|(targets, _)| *targets == objects(&[b, eq1])),
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
    node.selected_target_incarnations
        .iter()
        .find(|pin| pin.object_id == id)
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
            runner
                .act(GameAction::RetargetSpell {
                    new_targets: vec![TargetRef::Object(a)],
                })
                .expect("resubmitting A is accepted");
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
        proposals.iter().any(|(t, ok)| *t == anchor && *ok),
        "the AI's list holds the accepted anchor, got {proposals:?}"
    );
    assert!(proposals.iter().all(|(_, ok)| *ok));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: anchor,
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
    assert!(proposals.iter().any(|(t, ok)| *t == anchor && *ok));
    assert!(proposals.iter().all(|(_, ok)| *ok));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: anchor,
        })
        .expect("the anchor is accepted");
    assert_eq!(node_pin(&runner, spell, 1, eq), 0);
    assert_eq!(node_pin(&runner, spell, 3, ft), 0);

    let (mut runner, [spell, _a, c, eq, _l1, l2, ft]) = four_target_board();
    runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[c, eq, l2, ft]),
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

/// CR 115.3 + CR 115.7d + CR 400.7: resubmitting `[A, A]` would elect the new
/// A in the creature position, which (pins being keyed by object id) would
/// re-pin the artifact position the verdict retains — that reading cannot be
/// realized, so the exact resubmission is read as leaving every target
/// unchanged: accepted, the announced pin kept.
#[test]
fn same_object_in_two_positions_anchor_keeps_its_announced_pin() {
    const EXCHANGE: &str = "Exchange control of target artifact and target creature.";
    let (mut runner, spell, a) = same_object_two_positions_board(EXCHANGE, false);
    assert_eq!(root_pin(&runner, spell, a), 0, "reach guard");
    runner
        .act(GameAction::RetargetSpell {
            new_targets: objects(&[a, a]),
        })
        .expect("leaving every target unchanged is accepted");
    assert_eq!(root_pin(&runner, spell, a), 0, "the retained pin is kept");
}

/// A partial change cannot carry the same unrealizable reading: changing the
/// player while resubmitting `[A, A]` is refused, and nothing is re-pinned.
/// Control: the exact resubmission on the same board is accepted.
#[test]
fn same_object_in_two_positions_partial_change_is_refused() {
    const TEXT: &str =
        "Exchange control of target artifact and target creature. Target player loses 1 life.";
    let parsed = parse_oracle_text(TEXT, "Probe", &[], &types("Instant"), &[]);
    assert!(
        unimplemented_names(&[&parsed.abilities[0]]).is_empty(),
        "reach guard"
    );
    let (mut runner, spell, a) = same_object_two_positions_board(TEXT, true);
    let mut changed = objects(&[a, a]);
    changed.push(TargetRef::Player(P0));
    assert!(runner
        .act(GameAction::RetargetSpell {
            new_targets: changed,
        })
        .is_err());
    assert_eq!(root_pin(&runner, spell, a), 0);
    let mut anchor = objects(&[a, a]);
    anchor.push(TargetRef::Player(P1));
    runner
        .act(GameAction::RetargetSpell {
            new_targets: anchor,
        })
        .expect("the exact resubmission is accepted");
    assert_eq!(root_pin(&runner, spell, a), 0);
}
