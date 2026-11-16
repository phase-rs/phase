//! CR 509.3e: "becomes blocked by N or more creatures" triggers (#9761).
//!
//! The class: Seifer, Balamb Rival ("Whenever a creature attacking one of your
//! opponents becomes blocked by two or more creatures, that attacking creature
//! gains deathtouch until end of turn.") and Hedron Blade ("Whenever equipped
//! creature becomes blocked by one or more colorless creatures, it gains
//! deathtouch until end of turn."). Card texts are the printed Oracle (Scryfall).

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    BlockerCountThreshold, Comparator, ControllerRef, FilterProp, TargetFilter, TriggerDefinition,
    TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::{AttackTargetFilter, TriggerMode};
use engine::types::zones::Zone;

const SEIFER: &str = "First strike\nWhenever you attack a player, goad target creature that player controls.\nWhenever a creature attacking one of your opponents becomes blocked by two or more creatures, that attacking creature gains deathtouch until end of turn.";
const HEDRON_BLADE: &str = "Equipped creature gets +1/+1.\nWhenever equipped creature becomes blocked by one or more colorless creatures, it gains deathtouch until end of turn. (Any amount of damage it deals to a creature is enough to destroy it.)\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)";
const SNAPCASTER: &str = "Flash\nWhen this creature enters, target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const MIRROR_MATCH: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const MOONLACE: &str = "Target spell or permanent becomes colorless.";
const MAGNETIC_THEFT: &str =
    "Attach target Equipment to target creature. (Control of the Equipment doesn't change.)";
const SLIP_OUT_THE_BACK: &str = "Put a +1/+1 counter on target creature. It phases out. (Treat it and anything attached to it as though they don't exist until its controller's next turn.)";
const DREAD_SLAVER: &str = "Whenever a creature dealt damage by this creature this turn dies, return it to the battlefield under your control. That creature is a black Zombie in addition to its other colors and types.";
const NO_QUARTER: &str = "Whenever a creature becomes blocked by a creature with lesser power, destroy the blocking creature.\nWhenever a creature blocks a creature with lesser power, destroy the attacking creature.";

const P2: PlayerId = PlayerId(2);

// ---------------------------------------------------------------------------
// Drivers
// ---------------------------------------------------------------------------

/// Answer one prompt: pass priority, order triggers as listed, or choose each
/// trigger target slot's first legal target.
fn step(r: &mut GameRunner) {
    match r.state().waiting_for.clone() {
        WaitingFor::Priority { .. } => {
            r.act(GameAction::PassPriority).expect("pass priority");
        }
        WaitingFor::OrderTriggers { triggers, .. } => {
            r.act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("order triggers");
        }
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            let target = selection
                .current_legal_targets
                .first()
                .cloned()
                .or_else(|| {
                    target_slots
                        .get(selection.current_slot)
                        .and_then(|slot| slot.legal_targets.first().cloned())
                });
            r.act(GameAction::ChooseTarget { target })
                .expect("choose trigger target");
        }
        other => panic!("unexpected prompt {other:?}"),
    }
}

fn drive(r: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..400 {
        if stop(r) {
            return;
        }
        step(r);
    }
    panic!("the game never reached the expected point");
}

/// Answer trigger prompts without passing priority: the trigger window.
fn to_priority(r: &mut GameRunner) {
    drive(r, |r| {
        matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

fn priority_for(r: &mut GameRunner, player: PlayerId) {
    drive(
        r,
        |r| matches!(r.state().waiting_for, WaitingFor::Priority { player: p } if p == player),
    );
}

fn resolve_stack(r: &mut GameRunner) {
    drive(r, |r| {
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

fn attack(r: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    drive(r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    r.act(GameAction::DeclareAttackers {
        attacks: attacks.to_vec(),
        bands: vec![],
    })
    .expect("declare attackers");
}

fn block(r: &mut GameRunner, assignments: &[(ObjectId, ObjectId)]) {
    drive(r, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    r.act(GameAction::DeclareBlockers {
        assignments: assignments.to_vec(),
    })
    .expect("declare blockers");
    to_priority(r);
}

/// The stored trigger events of `source`'s triggered abilities on the stack.
fn triggers_from(r: &GameRunner, source: ObjectId) -> Vec<Option<GameEvent>> {
    r.state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } if *source_id == source => Some(trigger_event.clone()),
            _ => None,
        })
        .collect()
}

fn has_deathtouch(r: &GameRunner, id: ObjectId) -> bool {
    r.state()
        .objects
        .get(&id)
        .is_some_and(|obj| obj.has_keyword(&Keyword::Deathtouch))
}

/// No transient continuous effect is bound to `id` (CR 400.7: nothing was
/// installed on that storage id).
fn no_transient_on(r: &GameRunner, id: ObjectId) -> bool {
    !r.state().transient_continuous_effects.iter().any(|effect| {
        effect.affected == TargetFilter::SpecificObject { id }
            || effect
                .affected_recipient
                .is_some_and(|recipient| recipient.object_id == id)
    })
}

fn pay(r: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    for kind in mana {
        r.state_mut()
            .add_mana_to_pool(player, ManaUnit::new(*kind, ObjectId(0), false, vec![]));
    }
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn attach(r: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = r.state_mut();
    if let Some(AttachTarget::Object(old)) = state.objects[&equipment].attached_to {
        state
            .objects
            .get_mut(&old)
            .unwrap()
            .attachments
            .retain(|id| *id != equipment);
    }
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    let host_obj = state.objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&equipment) {
        host_obj.attachments.push(equipment);
    }
    state.layers_dirty.mark_full();
}

fn seifer(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature_from_oracle(player, "Seifer, Balamb Rival", 4, 3, SEIFER)
        .with_color(vec![ManaColor::Black, ManaColor::Red])
        .id()
}

fn bears(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature(player, "Grizzly Bears", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id()
}

fn myr_enforcer(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature(player, "Myr Enforcer", 4, 4)
        .as_artifact_creature()
        .id()
}

fn hedron_blade(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_artifact_from_oracle(player, "Hedron Blade", HEDRON_BLADE)
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn mirror_match(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_spell_to_hand_from_oracle(player, "Mirror Match", true, MIRROR_MATCH)
        .with_mana_cost(cost(4, vec![ManaCostShard::Blue, ManaCostShard::Blue]))
        .id()
}

/// P1 casts Mirror Match during the declare blockers step and it resolves.
fn cast_mirror_match(r: &mut GameRunner, spell: ObjectId) {
    priority_for(r, P1);
    pay(
        r,
        P1,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    r.cast(spell).commit();
    // Resolve Mirror Match only; whatever it triggers waits in the window.
    drive(r, |r| {
        r.state().objects[&spell].zone != Zone::Stack
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

fn becomes_blocked_definition(r: &GameRunner, source: ObjectId) -> TriggerDefinition {
    r.state().objects[&source]
        .trigger_definitions
        .iter_all()
        .map(|entry| &entry.definition)
        .find(|t| t.mode == TriggerMode::BecomesBlocked)
        .cloned()
        .expect("a BecomesBlocked trigger")
}

fn trigger_of<'a>(defs: &'a [TriggerDefinition], mode: &TriggerMode) -> &'a TriggerDefinition {
    defs.iter()
        .find(|t| &t.mode == mode)
        .unwrap_or_else(|| panic!("no {mode:?} trigger in {defs:?}"))
}

fn parse_triggers(text: &str) -> Vec<TriggerDefinition> {
    parse_oracle_text(text, "Probe", &[], &["Creature".to_string()], &[]).triggers
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// CR 509.3e + CR 508.1b: Seifer's third ability: a creature attacking one of
/// your opponents (a player), a two-creature threshold with no blocker filter.
#[test]
fn seifer_parses_threshold_subject_and_attack_target() {
    let parsed = parse_oracle_text(
        SEIFER,
        "Seifer, Balamb Rival",
        &["First strike".to_string()],
        &["Creature".to_string()],
        &["Human".to_string(), "Mercenary".to_string()],
    );
    let trigger = trigger_of(&parsed.triggers, &TriggerMode::BecomesBlocked);
    assert_eq!(
        trigger.blocker_count,
        Some(BlockerCountThreshold {
            minimum: 2,
            blocker: None
        })
    );
    assert_eq!(
        trigger.attack_target_filter,
        Some(AttackTargetFilter::Player)
    );
    assert!(!trigger.batched);
    assert_eq!(trigger.valid_target, None);
    let TargetFilter::Typed(subject) = trigger.valid_card.as_ref().expect("subject") else {
        panic!("typed subject expected: {:?}", trigger.valid_card);
    };
    assert!(subject.properties.contains(&FilterProp::Attacking {
        defender: Some(ControllerRef::Opponent)
    }));
}

/// CR 509.3e: the #9637 self form keeps its count instead of dropping it.
#[test]
fn self_form_threshold_is_not_dropped() {
    let triggers = parse_triggers(
        "Whenever this creature becomes blocked by two or more creatures, it gains deathtouch until end of turn.",
    );
    let trigger = trigger_of(&triggers, &TriggerMode::BecomesBlocked);
    assert_eq!(
        trigger.blocker_count,
        Some(BlockerCountThreshold {
            minimum: 2,
            blocker: None
        })
    );
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
}

/// CR 509.3e + CR 603.2c: Hedron Blade's "one or more" is the blocker
/// quantifier, not a batched subject.
#[test]
fn hedron_blade_parses_colorless_threshold_unbatched() {
    let parsed = parse_oracle_text(
        HEDRON_BLADE,
        "Hedron Blade",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let trigger = trigger_of(&parsed.triggers, &TriggerMode::BecomesBlocked);
    assert!(
        !trigger.batched,
        "the blocker quantifier must not batch the trigger"
    );
    let threshold = trigger.blocker_count.as_ref().expect("threshold");
    assert_eq!(threshold.minimum, 1);
    let Some(TargetFilter::Typed(blocker)) = threshold.blocker.as_ref() else {
        panic!("colorless blocker filter expected: {threshold:?}");
    };
    // CR 105.2c: a colorless object has no color.
    assert_eq!(blocker.type_filters, vec![TypeFilter::Creature]);
    assert_eq!(
        blocker.properties,
        vec![FilterProp::ColorCount {
            comparator: Comparator::EQ,
            count: 0
        }]
    );
    assert_eq!(trigger.valid_card, Some(TargetFilter::AttachedTo));
}

/// Fail closed: every tail or subject this change does not model stays
/// unparsed. Each line keeps exactly one trigger, `Unknown`, carrying the
/// unmodelled condition (an empty result or a parsed mode both fail).
#[test]
fn unmodelled_threshold_shapes_stay_unparsed() {
    for (line, condition_tail) in [
        (
            "Whenever this creature becomes blocked by exactly two creatures, it gains deathtouch until end of turn.",
            "becomes blocked by exactly two creatures",
        ),
        (
            "Whenever this creature becomes blocked by two or fewer creatures, it gains deathtouch until end of turn.",
            "becomes blocked by two or fewer creatures",
        ),
        (
            "Whenever this creature becomes blocked by two or more creatures this turn, it gains deathtouch until end of turn.",
            "becomes blocked by two or more creatures this turn",
        ),
        (
            "Whenever one or more creatures you control become blocked by two or more creatures, draw a card.",
            "one or more creatures you control become blocked by two or more creatures",
        ),
        (
            "Whenever a creature attacking one of your opponents dies, draw a card.",
            "a creature attacking one of your opponents dies",
        ),
        (
            "Whenever a creature attacking your opponents and/or planeswalkers they control becomes blocked, draw a card.",
            "a creature attacking your opponents and/or planeswalkers they control becomes blocked",
        ),
    ] {
        let triggers = parse_triggers(line);
        assert_eq!(triggers.len(), 1, "{line} => {triggers:?}");
        let TriggerMode::Unknown(fragment) = &triggers[0].mode else {
            panic!("{line} parsed as {:?}", triggers[0].mode);
        };
        assert!(
            fragment.ends_with(condition_tail),
            "{line}: unparsed fragment {fragment:?}"
        );
    }
}

/// CR 509.3d: No Quarter's "by a creature with lesser power" needs a
/// blocker-vs-attacker comparison; it is unparsed rather than firing on any
/// blocker. Its second line is a `Blocks` trigger and is untouched.
#[test]
fn no_quarter_first_line_is_unparsed_second_untouched() {
    let parsed = parse_oracle_text(
        NO_QUARTER,
        "No Quarter",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 2, "{:?}", parsed.triggers);
    assert!(matches!(parsed.triggers[0].mode, TriggerMode::Unknown(_)));
    assert_eq!(parsed.triggers[1].mode, TriggerMode::Blocks);
}

// ---------------------------------------------------------------------------
// Seifer
// ---------------------------------------------------------------------------

/// Seifer and a 0/4 attacker (0 power: no damage-assignment prompt) for P0,
/// two 1/1 blockers for P1.
fn seifer_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let attacker = s.add_vanilla(P0, 0, 4);
    let a = s.add_vanilla(P1, 1, 1);
    let b = s.add_vanilla(P1, 1, 1);
    (s.build(), seifer, attacker, a, b)
}

/// CR 509.3e: two blockers fire Seifer exactly once; the stored event names the
/// attacker and its incarnation; the attacker, not a blocker, gains deathtouch.
#[test]
fn seifer_double_block_grants_attacker_deathtouch() {
    let (mut r, seifer, attacker, a, b) = seifer_board();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    let fired = triggers_from(&r, seifer);
    assert_eq!(fired.len(), 1, "exactly one Seifer trigger");
    let incarnation = r.state().objects[&attacker].incarnation;
    assert_eq!(
        fired[0],
        Some(GameEvent::AttackerBecameBlocked {
            attacker,
            attacker_incarnation: Some(incarnation),
        })
    );
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker));
    assert!(!has_deathtouch(&r, a) && !has_deathtouch(&r, b));
}

/// Control: one blocker is below the threshold.
#[test]
fn seifer_single_block_does_not_fire() {
    let (mut r, seifer, attacker, a, _) = seifer_board();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker)]);
    let blockers = &r.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
    assert_eq!(blockers.len(), 1, "reach: blocked by one creature");
    assert!(triggers_from(&r, seifer).is_empty());
    resolve_stack(&mut r);
    assert!(!has_deathtouch(&r, attacker));
}

/// Ruling (2025-10-02): "Seifer's last ability triggers regardless of which
/// player controls the attacking creatures." A creature attacking Seifer's
/// controller is not attacking one of their opponents (CR 508.1b).
#[test]
fn seifer_any_controller_attacking_an_opponent_multiplayer() {
    for (defender, expected) in [(P2, 1), (P0, 0)] {
        let mut s = GameScenario::new_n_player(3, 7);
        s.at_phase(Phase::PreCombatMain);
        let seifer = seifer(&mut s, P0);
        let attacker = s.add_vanilla(P1, 0, 4);
        let a = s.add_vanilla(defender, 1, 1);
        let b = s.add_vanilla(defender, 1, 1);
        let mut r = s.build();
        r.state_mut().active_player = P1;
        r.state_mut().priority_player = P1;
        r.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        attack(&mut r, &[(attacker, AttackTarget::Player(defender))]);
        block(&mut r, &[(a, attacker), (b, attacker)]);
        let blockers = &r.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
        assert_eq!(blockers.len(), 2, "reach: double block");
        assert_eq!(
            triggers_from(&r, seifer).len(),
            expected,
            "defender {defender:?}"
        );
        resolve_stack(&mut r);
        assert_eq!(has_deathtouch(&r, attacker), expected == 1);
    }
}

/// CR 508.1b: attacking an opponent's planeswalker is not attacking the player.
#[test]
fn seifer_ignores_attacks_on_planeswalkers() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let attacker = s.add_vanilla(P0, 0, 4);
    let walker = s
        .add_planeswalker_from_oracle(P1, "Test Walker", "Jace", 5, "+1: Draw a card.")
        .id();
    let a = s.add_vanilla(P1, 1, 1);
    let b = s.add_vanilla(P1, 1, 1);
    let mut r = s.build();
    attack(&mut r, &[(attacker, AttackTarget::Planeswalker(walker))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    let combat = r.state().combat.as_ref().unwrap();
    assert!(combat
        .attackers
        .iter()
        .any(|info| info.object_id == attacker
            && info.attack_target == AttackTarget::Planeswalker(walker)));
    assert_eq!(combat.blocker_assignments[&attacker].len(), 2, "reach");
    assert!(triggers_from(&r, seifer).is_empty());
}

/// CR 509.3e: two attackers each blocked by two fire twice, once per attacker.
#[test]
fn seifer_distinct_attackers_fire_once_each() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let first = s.add_vanilla(P0, 0, 4);
    let second = s.add_vanilla(P0, 0, 4);
    let blockers: Vec<ObjectId> = (0..4).map(|_| s.add_vanilla(P1, 1, 1)).collect();
    let mut r = s.build();
    attack(
        &mut r,
        &[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ],
    );
    block(
        &mut r,
        &[
            (blockers[0], first),
            (blockers[1], second),
            (blockers[2], first),
            (blockers[3], second),
        ],
    );
    let fired = triggers_from(&r, seifer);
    assert_eq!(fired.len(), 2);
    for attacker in [first, second] {
        assert!(fired.iter().any(|event| matches!(
            event,
            Some(GameEvent::AttackerBecameBlocked { attacker: a, .. }) if *a == attacker
        )));
    }
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, first) && has_deathtouch(&r, second));
}

/// Grant expiry: "until end of turn" ends in the cleanup step (CR 514.2).
#[test]
fn seifer_grant_expires_at_end_of_turn() {
    let (mut r, _, attacker, a, b) = seifer_board();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker), "reach: granted");
    let turn = r.state().turn_number;
    drive(&mut r, |r| r.state().turn_number > turn);
    assert!(
        !has_deathtouch(&r, attacker),
        "the grant ended with the turn"
    );
}

// ---------------------------------------------------------------------------
// Entering blockers (CR 509.3e: effects that add blockers)
// ---------------------------------------------------------------------------

/// Parallel Lives doubles Mirror Match's copy: two simultaneous entrants block
/// the one attacker. 0 -> 2 is a single crossing: exactly one trigger.
#[test]
fn simultaneous_entrants_fire_once() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let attacker = bears(&mut s, P0);
    s.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    let spell = mirror_match(&mut s, P1);
    let mut r = s.build();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    drive(&mut r, |r| r.state().phase == Phase::DeclareBlockers);
    if matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. }) {
        r.act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("no blocks");
    }
    cast_mirror_match(&mut r, spell);
    to_priority(&mut r);
    let blockers = &r.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
    assert_eq!(blockers.len(), 2, "reach: two copies entered blocking");
    assert_eq!(
        triggers_from(&r, seifer).len(),
        1,
        "exactly one Seifer trigger"
    );
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker));
}

/// 1 -> 2 -> 3: one declared blocker, then Mirror Match twice. The first copy
/// crosses the threshold; the second adds nothing.
#[test]
fn sequential_entrants_fire_only_on_the_crossing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let attacker = bears(&mut s, P0);
    let declared = s.add_vanilla(P1, 0, 1);
    let first = mirror_match(&mut s, P1);
    let second = mirror_match(&mut s, P1);
    let mut r = s.build();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(declared, attacker)]);
    assert!(
        triggers_from(&r, seifer).is_empty(),
        "1 blocker: no trigger"
    );
    cast_mirror_match(&mut r, first);
    to_priority(&mut r);
    assert_eq!(triggers_from(&r, seifer).len(), 1, "1 -> 2 crosses once");
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker));
    cast_mirror_match(&mut r, second);
    to_priority(&mut r);
    let blockers = &r.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
    assert_eq!(blockers.len(), 3, "reach: three blockers");
    assert!(
        triggers_from(&r, seifer).is_empty(),
        "2 -> 3: no new trigger"
    );
}

// ---------------------------------------------------------------------------
// Hedron Blade
// ---------------------------------------------------------------------------

/// R-batch: Hedron Blade on Myr Enforcer A; Grizzly Bears B also attacks. P1
/// declares B's blocker first, then two Myr Enforcers on A. One Hedron trigger,
/// its stored event names A at A's incarnation, and only A gets deathtouch.
#[test]
fn hedron_blade_two_colorless_blockers_fire_once_for_the_equipped_attacker() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let blade = hedron_blade(&mut s, P0);
    let a = myr_enforcer(&mut s, P0);
    let b = bears(&mut s, P0);
    let b_blocker = bears(&mut s, P1);
    let e1 = myr_enforcer(&mut s, P1);
    let e2 = myr_enforcer(&mut s, P1);
    let mut r = s.build();
    attach(&mut r, blade, a);
    let trigger = becomes_blocked_definition(&r, blade);
    assert!(!trigger.batched);
    assert_eq!(trigger.blocker_count.as_ref().map(|t| t.minimum), Some(1));
    attack(
        &mut r,
        &[(a, AttackTarget::Player(P1)), (b, AttackTarget::Player(P1))],
    );
    block(&mut r, &[(b_blocker, b), (e1, a), (e2, a)]);
    let fired = triggers_from(&r, blade);
    assert_eq!(fired.len(), 1, "exactly one Hedron trigger");
    let incarnation = r.state().objects[&a].incarnation;
    assert_eq!(
        fired[0],
        Some(GameEvent::AttackerBecameBlocked {
            attacker: a,
            attacker_incarnation: Some(incarnation),
        })
    );
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, a));
    for other in [b, b_blocker, e1, e2] {
        assert!(!has_deathtouch(&r, other));
    }
}

/// CR 509.3f: a colored blocker that later becomes colorless (Moonlace) was not
/// colorless when it blocked, so Hedron Blade does not fire. Control: a
/// colorless blocker at declaration fires once.
#[test]
fn hedron_blade_blocker_made_colorless_later_does_not_fire() {
    for colorless_blocker in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let blade = hedron_blade(&mut s, P0);
        let attacker = bears(&mut s, P0);
        let blocker = if colorless_blocker {
            myr_enforcer(&mut s, P1)
        } else {
            bears(&mut s, P1)
        };
        let moonlace = s
            .add_spell_to_hand_from_oracle(P0, "Moonlace", true, MOONLACE)
            .with_mana_cost(cost(0, vec![ManaCostShard::Blue]))
            .id();
        let mut r = s.build();
        attach(&mut r, blade, attacker);
        attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
        block(&mut r, &[(blocker, attacker)]);
        if colorless_blocker {
            assert_eq!(triggers_from(&r, blade).len(), 1, "control fires once");
            resolve_stack(&mut r);
            assert!(has_deathtouch(&r, attacker));
            continue;
        }
        assert!(
            triggers_from(&r, blade).is_empty(),
            "green blocker: no trigger"
        );
        priority_for(&mut r, P0);
        pay(&mut r, P0, &[ManaType::Blue]);
        r.cast(moonlace).target_object(blocker).commit();
        resolve_stack(&mut r);
        assert!(
            r.state().objects[&blocker].color.is_empty(),
            "reach: the blocker is colorless now"
        );
        assert!(triggers_from(&r, blade).is_empty());
        assert!(!has_deathtouch(&r, attacker));
    }
}

/// CR 400.7 + CR 608.2k: the trigger names the creature that became blocked.
/// Moving the Blade to another creature (Magnetic Theft) before it resolves
/// does not move the grant.
#[test]
fn hedron_blade_grant_follows_the_blocked_attacker_not_the_new_host() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let blade = hedron_blade(&mut s, P0);
    let attacker = bears(&mut s, P0);
    let other = bears(&mut s, P0);
    let blocker = myr_enforcer(&mut s, P1);
    let theft = s
        .add_spell_to_hand_from_oracle(P0, "Magnetic Theft", true, MAGNETIC_THEFT)
        .with_mana_cost(cost(0, vec![ManaCostShard::Red]))
        .id();
    let mut r = s.build();
    attach(&mut r, blade, attacker);
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(blocker, attacker)]);
    assert_eq!(triggers_from(&r, blade).len(), 1, "reach: Hedron trigger");
    priority_for(&mut r, P0);
    pay(&mut r, P0, &[ManaType::Red]);
    r.cast(theft).target_objects(&[blade, other]).commit();
    drive(&mut r, |r| {
        r.state().objects[&theft].zone != Zone::Stack
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        r.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(other)),
        "reach: the Blade moved"
    );
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker));
    assert!(!has_deathtouch(&r, other));
}

/// D6 fence (CR 509.3e + CR 509.3f): a blocker-filtered threshold fires only
/// when blockers are declared. Reach leg: a declared colorless blocker fires
/// once. Fence leg: a colorless copy entering blocking (Mirror Match on a
/// colorless attacker) does not fire.
#[test]
fn hedron_blade_fires_at_declaration_but_not_on_an_entering_blocker() {
    for entering in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let blade = hedron_blade(&mut s, P0);
        let attacker = myr_enforcer(&mut s, P0);
        let declared = myr_enforcer(&mut s, P1);
        let spell = mirror_match(&mut s, P1);
        let mut r = s.build();
        attach(&mut r, blade, attacker);
        attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
        if !entering {
            block(&mut r, &[(declared, attacker)]);
            assert_eq!(triggers_from(&r, blade).len(), 1, "declaration fires");
            continue;
        }
        block(&mut r, &[]);
        cast_mirror_match(&mut r, spell);
        to_priority(&mut r);
        let blockers = &r.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
        assert_eq!(
            blockers.len(),
            1,
            "reach: the colorless copy entered blocking"
        );
        assert!(
            r.state().objects[&blockers[0]].color.is_empty(),
            "reach: the entrant is colorless"
        );
        assert!(triggers_from(&r, blade).is_empty(), "fenced: no trigger");
        resolve_stack(&mut r);
        assert!(!has_deathtouch(&r, attacker));
    }
}

// ---------------------------------------------------------------------------
// Identity (CR 400.7 + CR 608.2k + CR 702.26b)
// ---------------------------------------------------------------------------

struct SnapBoard {
    r: GameRunner,
    seifer: ObjectId,
    snapcaster: ObjectId,
    unsummon: ObjectId,
}

/// P0: Seifer and an established Snapcaster Mage, Unsummon in hand. P1: two
/// Grizzly Bears double-block the attacking Snapcaster.
fn snapcaster_double_blocked() -> SnapBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let snapcaster = s
        .add_creature_from_oracle(P0, "Snapcaster Mage", 2, 1, SNAPCASTER)
        .with_mana_cost(cost(1, vec![ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    let unsummon = s
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(cost(0, vec![ManaCostShard::Blue]))
        .id();
    let a = bears(&mut s, P1);
    let b = bears(&mut s, P1);
    let mut r = s.build();
    attack(&mut r, &[(snapcaster, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, snapcaster), (b, snapcaster)]);
    assert_eq!(
        triggers_from(&r, seifer).len(),
        1,
        "exactly one Seifer trigger"
    );
    SnapBoard {
        r,
        seifer,
        snapcaster,
        unsummon,
    }
}

fn unsummon(board: &mut SnapBoard) {
    let r = &mut board.r;
    priority_for(r, P0);
    pay(r, P0, &[ManaType::Blue]);
    let (spell, target) = (board.unsummon, board.snapcaster);
    r.cast(spell).target_object(target).commit();
    drive(r, |r| {
        r.state().objects[&spell].zone != Zone::Stack
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        r.state().objects[&target].zone,
        Zone::Hand,
        "reach: bounced"
    );
}

fn recast_snapcaster(board: &mut SnapBoard) {
    let r = &mut board.r;
    priority_for(r, P0);
    pay(r, P0, &[ManaType::Blue, ManaType::Colorless]);
    let snapcaster = board.snapcaster;
    r.cast(snapcaster).commit();
    drive(r, |r| {
        r.state().objects[&snapcaster].zone == Zone::Battlefield
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

/// Control: without the round trip, the attacker gets deathtouch.
#[test]
fn snapcaster_control_gets_deathtouch() {
    let mut board = snapcaster_double_blocked();
    resolve_stack(&mut board.r);
    assert!(has_deathtouch(&board.r, board.snapcaster));
}

/// Unsummon and recast Snapcaster before Seifer's trigger resolves: the new
/// object is not the creature that became blocked (CR 400.7).
#[test]
fn snapcaster_recast_before_resolution_gets_no_deathtouch() {
    let mut board = snapcaster_double_blocked();
    unsummon(&mut board);
    recast_snapcaster(&mut board);
    assert_eq!(
        triggers_from(&board.r, board.seifer).len(),
        1,
        "reach: Seifer's trigger still waits"
    );
    resolve_stack(&mut board.r);
    assert_eq!(
        board.r.state().objects[&board.snapcaster].zone,
        Zone::Battlefield
    );
    assert!(!has_deathtouch(&board.r, board.snapcaster));
    assert!(no_transient_on(&board.r, board.snapcaster));
}

/// Unsummon, let Seifer's trigger resolve while Snapcaster is in hand, then
/// recast it: nothing is installed on the hand object, and the recast
/// permanent has no deathtouch.
#[test]
fn snapcaster_trigger_resolving_in_hand_installs_nothing() {
    let mut board = snapcaster_double_blocked();
    unsummon(&mut board);
    resolve_stack(&mut board.r);
    assert_eq!(board.r.state().objects[&board.snapcaster].zone, Zone::Hand);
    assert!(
        triggers_from(&board.r, board.seifer).is_empty(),
        "reach: Seifer's trigger resolved"
    );
    assert!(
        no_transient_on(&board.r, board.snapcaster),
        "no attacker-bound grant was installed while it was in hand"
    );
    assert_eq!(board.r.state().phase, Phase::DeclareBlockers);
    recast_snapcaster(&mut board);
    resolve_stack(&mut board.r);
    assert!(!has_deathtouch(&board.r, board.snapcaster));
}

/// CR 702.26b: a phased-out permanent can't be affected. Slip Out the Back
/// phases the attacker out before Seifer's trigger resolves.
#[test]
fn phased_out_attacker_is_not_granted_deathtouch() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let seifer = seifer(&mut s, P0);
    let attacker = s.add_vanilla(P0, 0, 4);
    let slip = s
        .add_spell_to_hand_from_oracle(P0, "Slip Out the Back", true, SLIP_OUT_THE_BACK)
        .with_mana_cost(cost(0, vec![ManaCostShard::Blue]))
        .id();
    let a = s.add_vanilla(P1, 1, 1);
    let b = s.add_vanilla(P1, 1, 1);
    let mut r = s.build();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    assert_eq!(triggers_from(&r, seifer).len(), 1, "reach: Seifer trigger");
    priority_for(&mut r, P0);
    pay(&mut r, P0, &[ManaType::Blue]);
    r.cast(slip).target_object(attacker).commit();
    drive(&mut r, |r| {
        r.state().objects[&slip].zone != Zone::Stack
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert!(
        !r.state().objects[&attacker].is_phased_in(),
        "reach: the attacker phased out"
    );
    resolve_stack(&mut r);
    assert!(
        no_transient_on(&r, attacker),
        "no grant on a phased-out permanent"
    );
}

/// Regression guard, base behaviour preserved verbatim (Main's SB7-1 ruling):
/// Dread Slaver returns the creature it killed, and it is a black Zombie.
#[test]
fn dread_slaver_return_and_zombie_grant_unchanged() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let slaver = s
        .add_creature_from_oracle(P0, "Dread Slaver", 3, 5, DREAD_SLAVER)
        .with_color(vec![ManaColor::Black])
        .id();
    let victim = bears(&mut s, P1);
    let mut r = s.build();
    attack(&mut r, &[(slaver, AttackTarget::Player(P1))]);
    block(&mut r, &[(victim, slaver)]);
    drive(&mut r, |r| {
        r.state().phase == Phase::EndCombat
            || (r.state().stack.is_empty()
                && r.state()
                    .objects
                    .get(&victim)
                    .is_some_and(|o| o.zone == Zone::Battlefield && o.controller == P0))
    });
    let returned = &r.state().objects[&victim];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "returned to the battlefield"
    );
    assert_eq!(returned.controller, P0, "under Dread Slaver's controller");
    assert!(returned.card_types.subtypes.iter().any(|t| t == "Zombie"));
    assert!(returned.color.contains(&ManaColor::Black));
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Legacy load: the pre-rename event tag and a definition without the field.
#[test]
fn legacy_event_and_definition_load() {
    let mut legacy = serde_json::to_value(GameEvent::AttackerBecameBlocked {
        attacker: ObjectId(7),
        attacker_incarnation: None,
    })
    .unwrap();
    legacy["type"] = serde_json::json!("AttackerBecameBlockedByEffect");
    let loaded: GameEvent = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        loaded,
        GameEvent::AttackerBecameBlocked {
            attacker: ObjectId(7),
            attacker_incarnation: None,
        }
    );
    let definition =
        serde_json::to_value(TriggerDefinition::new(TriggerMode::BecomesBlocked)).unwrap();
    assert!(
        definition.get("blocker_count").is_none(),
        "skipped when absent"
    );
    let loaded: TriggerDefinition = serde_json::from_value(definition).unwrap();
    assert_eq!(loaded.blocker_count, None);
}

/// New state round trip: a game paused with Seifer's trigger on the stack
/// survives save/load and resolves the same way.
#[test]
fn paused_trigger_round_trips_through_serialization() {
    let (mut r, seifer, attacker, a, b) = seifer_board();
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    let before = triggers_from(&r, seifer);
    assert_eq!(before.len(), 1, "reach");
    let json = serde_json::to_string(r.state()).unwrap();
    let loaded: GameState = serde_json::from_str(&json).unwrap();
    let mut reloaded = GameRunner::from_state(loaded);
    assert_eq!(triggers_from(&reloaded, seifer), before);
    let definition = becomes_blocked_definition(&reloaded, seifer);
    assert_eq!(definition.blocker_count.map(|t| t.minimum), Some(2));
    resolve_stack(&mut reloaded);
    assert!(has_deathtouch(&reloaded, attacker));
}

// ---------------------------------------------------------------------------
// Internal contract test (NOT production coverage)
// ---------------------------------------------------------------------------

/// Internal contract test for the shared delayed collector. The spell text is
/// FABRICATED: no printed card has a delayed blocker-count trigger. Seifer,
/// Balamb Rival is the nearest printed card (a printed, non-delayed trigger of
/// the same threshold). Its only purpose is to show that the delayed collector
/// (`WheneverEvent`, through `trigger_firing_groups`) reaches the narrowed
/// matcher and fires once per crossing with the attacker-subject event. It
/// claims no card's support.
#[test]
fn delayed_collector_narrows_threshold_contract() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Contract Probe",
            true,
            "Until end of turn, whenever a creature becomes blocked by two or more creatures, it gains deathtouch until end of turn.",
        )
        .with_mana_cost(cost(0, vec![ManaCostShard::Blue]))
        .id();
    let attacker = s.add_vanilla(P0, 0, 4);
    let a = s.add_vanilla(P1, 1, 1);
    let b = s.add_vanilla(P1, 1, 1);
    let mut r = s.build();
    pay(&mut r, P0, &[ManaType::Blue]);
    r.cast(spell).commit();
    resolve_stack(&mut r);
    attack(&mut r, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut r, &[(a, attacker), (b, attacker)]);
    let fired: Vec<GameEvent> = r
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { trigger_event, .. } => trigger_event.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "one delayed firing: {fired:?}");
    assert!(matches!(
        fired[0],
        GameEvent::AttackerBecameBlocked { attacker: a, attacker_incarnation: Some(_) } if a == attacker
    ));
    resolve_stack(&mut r);
    assert!(has_deathtouch(&r, attacker));
}
