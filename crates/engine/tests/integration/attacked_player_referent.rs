//! "the player or planeswalker it's attacking" / "... that creature is attacking": the attacked
//! player-or-planeswalker referent (CR 506.3), resolved at resolution time, kind-preserving, with
//! no declared target (CR 115.10a).
//!
//! Every runtime row is a real card's verbatim Oracle text, declared as an attack through the real
//! pipeline in a three-player game (`GameScenario::new_n_player(3, 42)`). Expected values are
//! derived from the card text, its rulings and CR 506.3 / 506.4 / 510.1b, not from
//! `DefendingPlayer` (CR 508.5 collapses a planeswalker to its controller; this referent does not).
//!
//! Parse-shape assertions go through `serde_json`, reading the parsed ability shape without naming
//! the variant's Rust type. Each "nothing is damaged" assertion is paired with reach guards in
//! the same test: the trigger was on the stack with no declared target, and an
//! `EffectResolved { kind: DealDamage }` was emitted for it. The same chain damages in the sibling
//! rows whenever the referent is live.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::{ChosenAttribute, EffectKind, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const BATTLESPHERE: &str = "Whenever this creature attacks, you may tap X untapped Myr you control. If you do, this creature gets +X/+0 until end of turn and deals X damage to the player or planeswalker it's attacking.";
const HELLRIDER: &str = "Whenever a creature you control attacks, this creature deals 1 damage to the player or planeswalker it's attacking.";
const SWORDJACK: &str = "Whenever this creature attacks, it deals damage to the player or planeswalker it's attacking equal to the number of artifacts you control.";
const SPITTER: &str =
    "Whenever this creature attacks, it deals 1 damage to the player or planeswalker it's attacking.";
const ROUSTABOUT: &str = "Whenever this creature becomes blocked, it deals 1 damage to the player or planeswalker it's attacking.";
const MAGE_SLAYER: &str = "Whenever equipped creature attacks, it deals damage equal to its power to the player or planeswalker it's attacking.\nEquip {3}";
const CAVALCADE: &str = "Whenever a creature you control with power 1 or less attacks, this enchantment deals 1 damage to the player or planeswalker that creature is attacking.";
const RAID_BOMBARDMENT: &str = "Whenever a creature you control with power 2 or less attacks, this enchantment deals 1 damage to the player or planeswalker that creature is attacking.";
const BRUSHWAGG: &str = "Interplanar (When you roll the planar die and it lands on the planeswalker symbol, planeswalk.)\nVigilance, haste\nWhenever a player attacks with a creature with power 4 or greater, this creature also attacks the player or planeswalker that creature is attacking.";
const KITESAIL: &str = "Whenever this creature attacks, another target creature attacking the same player or planeswalker gains flying until end of turn.";
const GEODERM: &str = "Whenever you attack, target attacking creature gets +1/+1 until end of turn. If it's attacking a battle, put a +1/+1 counter on it instead.";

// ---------------------------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------------------------

fn scenario() -> GameScenario {
    let mut sc = GameScenario::new_n_player(3, 42);
    sc.at_phase(Phase::PreCombatMain);
    for _ in 0..20 {
        sc.add_card_to_library_top(P0, "Plains");
        sc.add_card_to_library_top(P1, "Plains");
        sc.add_card_to_library_top(P2, "Plains");
    }
    sc
}

/// A planeswalker owned by P2.
fn add_karn(sc: &mut GameScenario, loyalty: u32) -> ObjectId {
    let id = sc
        .add_planeswalker_from_oracle(P2, "Karn Stand-in", "Karn", loyalty, "+1: You gain 1 life.")
        .as_planeswalker_with_loyalty("Karn", loyalty)
        .id();
    id
}

/// The stand-in every attack test uses enters with its printed loyalty as both its
/// base value and its loyalty counters (CR 306.5b), so a later loyalty read is real.
#[test]
fn karn_stand_in_has_its_printed_loyalty() {
    for loyalty in [1, 5] {
        let mut sc = scenario();
        let id = add_karn(&mut sc, loyalty);
        let r = sc.build();
        let object = &r.state().objects[&id];
        assert_eq!(object.base_loyalty, Some(loyalty));
        assert_eq!(object.loyalty, Some(loyalty));
        assert_eq!(object.counters[&CounterType::Loyalty], loyalty);
    }
}

/// Owner P2, controller P1: the controller (the defending player CR 508.5 collapses to) differs
/// from the owner, so a mis-collapse and a wrong-player read cannot coincide.
fn control_by_p1(r: &mut GameRunner, karn: ObjectId) {
    let obj = r.state_mut().objects.get_mut(&karn).unwrap();
    obj.base_controller = Some(P1);
    obj.controller = P1;
}

/// Turn an existing battlefield creature into a Siege battle with the given protector (CR 310.9b,
/// CR 310.9d). Local copy of the helper in `defending_player_controls_combat_anchor.rs`.
fn make_into_siege(r: &mut GameRunner, id: ObjectId, protector: PlayerId, defense: u32) {
    let obj = r.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.clear();
    obj.card_types.core_types.push(CoreType::Battle);
    obj.card_types.subtypes = vec!["Siege".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.defense = Some(defense);
    obj.base_defense = Some(defense);
    obj.counters.insert(CounterType::Defense, defense);
    obj.chosen_attributes
        .push(ChosenAttribute::Player(protector));
}

fn loyalty(r: &GameRunner, karn: ObjectId) -> u32 {
    r.state().objects[&karn]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

fn life(r: &GameRunner, p: PlayerId) -> i32 {
    r.state().players[p.0 as usize].life
}

#[derive(Default)]
struct Seen {
    optional: bool,
    tap_prompt: bool,
    target_prompt: bool,
}

fn drain_orders(r: &mut GameRunner) {
    while let WaitingFor::OrderTriggers { triggers, .. } = &r.state().waiting_for {
        let order = (0..triggers.len()).collect();
        r.act(GameAction::OrderTriggers { order }).unwrap();
    }
}

fn attack(r: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    r.advance_to_combat();
    r.declare_attackers(attacks).expect("the attack is legal");
    drain_orders(r);
}

/// Abilities on the stack whose source is `source`.
fn stack_abilities(r: &GameRunner, source: ObjectId) -> Vec<&ResolvedAbility> {
    r.state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .filter_map(|e| match &e.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.as_ref()),
            _ => None,
        })
        .collect()
}

/// Reach guard: exactly `n` triggers from `source` are on the stack, none carries a declared
/// target, and no target prompt is pending. The referent is not a target (CR 115.10a).
fn assert_untargeted_triggers(r: &GameRunner, source: ObjectId, n: usize) {
    assert!(
        !matches!(
            r.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        "a target prompt is pending: {:?}",
        r.state().waiting_for
    );
    let triggers = stack_abilities(r, source);
    assert_eq!(triggers.len(), n, "triggers on the stack from the source");
    for t in triggers {
        assert!(t.targets.is_empty(), "declared targets: {:?}", t.targets);
        assert!(
            !serde_json::to_string(t).unwrap().contains("Unimplemented"),
            "real lowered trigger is functioning"
        );
    }
}

/// Pass priority and answer prompts until the stack is empty.
fn settle(r: &mut GameRunner, events: &mut Vec<GameEvent>, accept: bool, tap: usize) -> Seen {
    let mut seen = Seen::default();
    for _ in 0..80 {
        let result = match r.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => r.act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            }),
            WaitingFor::OptionalEffectChoice { .. } => {
                seen.optional = true;
                r.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::PayCost { choices, .. } => {
                seen.tap_prompt = true;
                let cards: Vec<ObjectId> = choices.iter().copied().take(tap).collect();
                r.act(GameAction::SelectCards { cards })
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                seen.target_prompt = true;
                r.choose_first_legal_target()
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                r.act(GameAction::PassPriority)
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        events.extend(result.expect("action applies").events);
    }
    seen
}

/// Damage the source dealt, in order.
fn damage_from(events: &[GameEvent], source: ObjectId) -> Vec<(TargetRef, u32)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                ..
            } if *source_id == source => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

/// How many DealDamage effects completed for `source` (the reach guard for "no damage").
fn deal_damage_resolutions(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(e, GameEvent::EffectResolved {
                kind: EffectKind::DealDamage,
                source_id,
                ..
            } if *source_id == source)
        })
        .count()
}

fn gone_from_combat(r: &mut GameRunner, id: ObjectId) {
    // Typed combat contract; printed action-path removal is covered below.
    engine::game::effects::remove_from_combat::remove_object_from_combat(r.state_mut(), id);
}

fn sphere_board(sc: &mut GameScenario) -> (ObjectId, Vec<ObjectId>) {
    let sphere = sc
        .add_creature_from_oracle(P0, "Myr Battlesphere", 4, 7, BATTLESPHERE)
        .with_subtypes(vec!["Artifact", "Myr", "Construct"])
        .id();
    let myr = (0..4)
        .map(|i| {
            sc.add_creature(P0, &format!("Myr {i}"), 1, 1)
                .with_subtypes(vec!["Myr"])
                .id()
        })
        .collect();
    (sphere, myr)
}

fn tapped(r: &GameRunner, ids: &[ObjectId]) -> usize {
    ids.iter().filter(|i| r.state().objects[*i].tapped).count()
}

// ---------------------------------------------------------------------------------------------
// Myr Battlesphere
// ---------------------------------------------------------------------------------------------

/// Attacks a player, no planeswalker anywhere. Taps 3, X = 3 for pump and damage.
#[test]
fn battlesphere_attacks_player_with_no_planeswalker_on_board() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 3);
    assert!(seen.optional && seen.tap_prompt && !seen.target_prompt);
    assert_eq!(tapped(&r, &myr), 3);
    assert_eq!(r.state().objects[&sphere].power, Some(7));
    assert_eq!(life(&r, P1), 17);
    assert_eq!(damage_from(&ev, sphere), vec![(TargetRef::Player(P1), 3)]);
}

/// Attacks a planeswalker (owner P2, controller P1). Damage goes to the planeswalker, not to
/// its controller or owner.
#[test]
fn battlesphere_attacks_planeswalker_damages_the_planeswalker_not_its_controller() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let karn = add_karn(&mut sc, 5);
    // A decoy planeswalker, so a target slot over "planeswalkers" has two legal choices.
    let decoy = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    attack(&mut r, &[(sphere, AttackTarget::Planeswalker(karn))]);
    assert_eq!(r.state().objects[&karn].controller, P1, "fixture guard");
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 2);
    assert!(seen.optional && seen.tap_prompt && !seen.target_prompt);
    assert_eq!(tapped(&r, &myr), 2);
    assert_eq!(r.state().objects[&sphere].power, Some(6));
    assert_eq!(loyalty(&r, karn), 3);
    assert_eq!(loyalty(&r, decoy), 5);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
    assert_eq!(damage_from(&ev, sphere), vec![(TargetRef::Object(karn), 2)]);
}

/// Attacks a battle. The pump still happens, the damage lands nowhere (not the battle, its
/// protector, its controller, or the bystander planeswalker).
#[test]
fn battlesphere_attacks_battle_pumps_but_damages_nothing() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let karn = add_karn(&mut sc, 5);
    let siege = sc.add_creature(P0, "Placeholder Siege", 0, 0).id();
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    make_into_siege(&mut r, siege, P1, 5);
    attack(&mut r, &[(sphere, AttackTarget::Battle(siege))]);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 3);
    assert!(seen.optional && seen.tap_prompt && !seen.target_prompt);
    assert_eq!(tapped(&r, &myr), 3);
    assert_eq!(r.state().objects[&sphere].power, Some(7), "pump happened");
    assert_eq!(deal_damage_resolutions(&ev, sphere), 1);
    assert_eq!(damage_from(&ev, sphere), vec![]);
    assert_eq!(r.state().objects[&siege].counters[&CounterType::Defense], 5);
    assert_eq!(loyalty(&r, karn), 5);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// The source leaves before the trigger resolves; it still taps and deals X (ruling
/// 2020-08-07), to what it was attacking.
#[test]
fn battlesphere_gone_before_resolution_still_deals_damage_to_the_player() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    zones::move_to_zone(r.state_mut(), sphere, Zone::Graveyard, &mut Vec::new());
    assert_eq!(r.state().objects[&sphere].zone, Zone::Graveyard);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 2);
    assert!(seen.optional && seen.tap_prompt);
    assert_eq!(tapped(&r, &myr), 2);
    assert_eq!(life(&r, P1), 18);
    assert_eq!(damage_from(&ev, sphere), vec![(TargetRef::Player(P1), 2)]);
}

/// Same, attacking a planeswalker.
#[test]
fn battlesphere_gone_before_resolution_still_deals_damage_to_the_planeswalker() {
    let mut sc = scenario();
    let (sphere, _myr) = sphere_board(&mut sc);
    let karn = add_karn(&mut sc, 5);
    let decoy = add_karn(&mut sc, 5);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Planeswalker(karn))]);
    zones::move_to_zone(r.state_mut(), sphere, Zone::Graveyard, &mut Vec::new());
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 2);
    assert!(!seen.target_prompt);
    assert_eq!(loyalty(&r, karn), 3);
    assert_eq!(loyalty(&r, decoy), 5);
    assert_eq!(life(&r, P1), 20);
}

/// A departed-and-returned source is a new object and gets no leaked +X.
#[test]
fn battlesphere_returned_as_a_new_object_gets_no_pump() {
    let mut sc = scenario();
    let (sphere, _myr) = sphere_board(&mut sc);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    zones::move_to_zone(r.state_mut(), sphere, Zone::Graveyard, &mut Vec::new());
    zones::move_to_zone(r.state_mut(), sphere, Zone::Battlefield, &mut Vec::new());
    assert_eq!(r.state().objects[&sphere].zone, Zone::Battlefield);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 2);
    assert!(seen.tap_prompt);
    // Positive control: the damage still reached the player the old object was attacking.
    assert_eq!(life(&r, P1), 18);
    r.state_mut().layers_dirty.mark_full();
    assert_eq!(r.state().objects[&sphere].power, Some(4), "no leaked +X");
}

/// The prompt is shown and answered with an empty selection. X = 0.
#[test]
fn battlesphere_tap_zero_pumps_and_damages_nothing() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 0);
    assert!(seen.optional && seen.tap_prompt);
    assert_eq!(tapped(&r, &myr), 0);
    assert_eq!(r.state().objects[&sphere].power, Some(4));
    assert_eq!(life(&r, P1), 20);
    assert_eq!(damage_from(&ev, sphere), vec![]);
}

/// A planeswalker is on the board but not attacked.
#[test]
fn battlesphere_attacking_the_player_leaves_a_bystander_planeswalker_alone() {
    let mut sc = scenario();
    let (sphere, _myr) = sphere_board(&mut sc);
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, true, 3);
    assert!(seen.tap_prompt && !seen.target_prompt);
    assert_eq!(life(&r, P1), 17);
    assert_eq!(loyalty(&r, karn), 5);
}

/// Declining the optional taps nothing.
#[test]
fn battlesphere_declined_optional_taps_nothing() {
    let mut sc = scenario();
    let (sphere, myr) = sphere_board(&mut sc);
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, sphere, 1);
    let mut ev = Vec::new();
    let seen = settle(&mut r, &mut ev, false, 3);
    assert!(seen.optional && !seen.tap_prompt);
    assert_eq!(tapped(&r, &myr), 0);
    assert_eq!(life(&r, P1), 20);
}

// ---------------------------------------------------------------------------------------------
// Hellrider (an observer: each creature's trigger answers for that creature)
// ---------------------------------------------------------------------------------------------

struct HellBoard {
    r: GameRunner,
    hellrider: ObjectId,
    goblin: ObjectId,
    karn: ObjectId,
}

fn hell_board() -> HellBoard {
    let mut sc = scenario();
    let hellrider = sc
        .add_creature_from_oracle(P0, "Hellrider", 3, 3, HELLRIDER)
        .id();
    let goblin = sc.add_creature(P0, "Goblin", 1, 1).id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    HellBoard {
        r,
        hellrider,
        goblin,
        karn,
    }
}

/// Hellrider attacks the player, the Goblin attacks the planeswalker. Each trigger damages
/// what ITS creature is attacking.
#[test]
fn hellrider_damages_what_each_attacking_creature_is_attacking() {
    let mut b = hell_board();
    attack(
        &mut b.r,
        &[
            (b.hellrider, AttackTarget::Player(P1)),
            (b.goblin, AttackTarget::Planeswalker(b.karn)),
        ],
    );
    assert_untargeted_triggers(&b.r, b.hellrider, 2);
    let mut ev = Vec::new();
    settle(&mut b.r, &mut ev, true, 0);
    assert_eq!(life(&b.r, P1), 19);
    assert_eq!(loyalty(&b.r, b.karn), 4);
    assert_eq!((life(&b.r, P0), life(&b.r, P2)), (20, 20));
    assert_eq!(damage_from(&ev, b.hellrider).len(), 2);
}

/// The Goblin that triggered it dies first. Its trigger still damages what it was attacking.
#[test]
fn hellrider_trigger_of_a_dead_attacker_damages_what_it_was_attacking() {
    let mut b = hell_board();
    attack(
        &mut b.r,
        &[
            (b.goblin, AttackTarget::Player(P1)),
            (b.hellrider, AttackTarget::Planeswalker(b.karn)),
        ],
    );
    assert_untargeted_triggers(&b.r, b.hellrider, 2);
    zones::move_to_zone(b.r.state_mut(), b.goblin, Zone::Graveyard, &mut Vec::new());
    assert_eq!(b.r.state().objects[&b.goblin].zone, Zone::Graveyard);
    let mut ev = Vec::new();
    settle(&mut b.r, &mut ev, true, 0);
    assert_eq!(life(&b.r, P1), 19, "the dead Goblin was attacking P1");
    assert_eq!(loyalty(&b.r, b.karn), 4, "Hellrider's own trigger");
    assert_eq!(damage_from(&ev, b.hellrider).len(), 2);
}

/// The Goblin is removed from combat but stays on the battlefield. It is attacking nothing
/// (CR 506.4), so its trigger deals no damage. Hellrider's own attack is the positive control.
#[test]
fn hellrider_trigger_of_an_attacker_removed_from_combat_deals_no_damage() {
    let mut b = hell_board();
    attack(
        &mut b.r,
        &[
            (b.goblin, AttackTarget::Player(P1)),
            (b.hellrider, AttackTarget::Planeswalker(b.karn)),
        ],
    );
    assert_untargeted_triggers(&b.r, b.hellrider, 2);
    gone_from_combat(&mut b.r, b.goblin);
    assert_eq!(b.r.state().objects[&b.goblin].zone, Zone::Battlefield);
    let mut ev = Vec::new();
    settle(&mut b.r, &mut ev, true, 0);
    assert_eq!(deal_damage_resolutions(&ev, b.hellrider), 2);
    assert_eq!(
        loyalty(&b.r, b.karn),
        4,
        "positive control: Hellrider's own trigger"
    );
    assert_eq!(life(&b.r, P1), 20, "the Goblin is attacking nothing");
    assert_eq!(
        (life(&b.r, P0), life(&b.r, P2)),
        (20, 20),
        "no controller fallback"
    );
    assert_eq!(
        damage_from(&ev, b.hellrider),
        vec![(TargetRef::Object(b.karn), 1)],
        "only Hellrider's own trigger deals damage"
    );
}

/// The Goblin is blinked in response. The new Goblin is a different object (CR 400.7); the
/// trigger damages what the old one was attacking.
#[test]
fn hellrider_trigger_of_a_blinked_attacker_uses_the_old_objects_attack() {
    let mut b = hell_board();
    attack(
        &mut b.r,
        &[
            (b.goblin, AttackTarget::Player(P1)),
            (b.hellrider, AttackTarget::Planeswalker(b.karn)),
        ],
    );
    assert_untargeted_triggers(&b.r, b.hellrider, 2);
    zones::move_to_zone(b.r.state_mut(), b.goblin, Zone::Exile, &mut Vec::new());
    zones::move_to_zone(
        b.r.state_mut(),
        b.goblin,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(b.r.state().objects[&b.goblin].zone, Zone::Battlefield);
    let mut ev = Vec::new();
    settle(&mut b.r, &mut ev, true, 0);
    assert_eq!(life(&b.r, P1), 19, "the old Goblin was attacking P1");
    assert_eq!(loyalty(&b.r, b.karn), 4);
    assert_eq!(damage_from(&ev, b.hellrider).len(), 2);
}

/// The live attack target is read at resolution, so a reselection is honored.
#[test]
fn hellrider_reads_the_live_attack_target() {
    let mut b = hell_board();
    attack(
        &mut b.r,
        &[
            (b.goblin, AttackTarget::Player(P1)),
            (b.hellrider, AttackTarget::Player(P1)),
        ],
    );
    assert_untargeted_triggers(&b.r, b.hellrider, 2);
    // Synthetic reselection: the Goblin now attacks the planeswalker.
    let karn = b.karn;
    let goblin = b.goblin;
    let info =
        b.r.state_mut()
            .combat
            .as_mut()
            .unwrap()
            .attackers
            .iter_mut()
            .find(|a| a.object_id == goblin)
            .unwrap();
    info.attack_target = AttackTarget::Planeswalker(karn);
    let pin =
        engine::game::combat::bind_attack_target(b.r.state(), AttackTarget::Planeswalker(karn))
            .unwrap();
    b.r.state_mut()
        .combat
        .as_mut()
        .unwrap()
        .attacked_permanents
        .insert(goblin, pin);
    let mut ev = Vec::new();
    settle(&mut b.r, &mut ev, true, 0);
    assert_eq!(
        life(&b.r, P1),
        19,
        "positive control: Hellrider's own attack"
    );
    assert_eq!(loyalty(&b.r, karn), 4, "the reselected Goblin attack");
}

/// Two Hellriders, one attacker. Both triggers answer for the attacker.
#[test]
fn two_hellriders_one_attacker_deal_two_damage_in_total() {
    let mut sc = scenario();
    let first = sc
        .add_creature_from_oracle(P0, "Hellrider", 3, 3, HELLRIDER)
        .id();
    let second = sc
        .add_creature_from_oracle(P0, "Hellrider", 3, 3, HELLRIDER)
        .id();
    let mut r = sc.build();
    attack(&mut r, &[(first, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, first, 1);
    assert_untargeted_triggers(&r, second, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(life(&r, P1), 18);
    assert_eq!(
        damage_from(&ev, first).len() + damage_from(&ev, second).len(),
        2
    );
}

// ---------------------------------------------------------------------------------------------
// Fathom Fleet Swordjack
// ---------------------------------------------------------------------------------------------

fn swordjack_board(artifacts: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = scenario();
    let jack = sc
        .add_creature_from_oracle(P0, "Fathom Fleet Swordjack", 2, 2, SWORDJACK)
        .id();
    for i in 0..artifacts {
        sc.add_artifact_from_oracle(P0, &format!("Trinket {i}"), "");
    }
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    (r, jack, karn)
}

/// Damage equal to the artifacts you control, to the attacked planeswalker.
#[test]
fn swordjack_deals_artifact_count_to_the_attacked_planeswalker() {
    let (mut r, jack, karn) = swordjack_board(3);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 2);
    assert_eq!(life(&r, P1), 20);
    assert_eq!(damage_from(&ev, jack), vec![(TargetRef::Object(karn), 3)]);
}

/// No artifacts, no damage. The chain resolved.
#[test]
fn swordjack_with_no_artifacts_deals_no_damage() {
    let (mut r, jack, karn) = swordjack_board(0);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(deal_damage_resolutions(&ev, jack), 1);
    assert_eq!(loyalty(&r, karn), 5);
}

/// The attacked planeswalker leaves in response. Nothing is dealt damage, and it is not
/// redirected to its controller (CR 506.4c, CR 510.1b).
#[test]
fn swordjack_attacked_planeswalker_gone_deals_no_damage_and_does_not_fizzle() {
    let (mut r, jack, karn) = swordjack_board(3);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    zones::move_to_zone(r.state_mut(), karn, Zone::Graveyard, &mut Vec::new());
    assert_eq!(r.state().objects[&karn].zone, Zone::Graveyard);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(deal_damage_resolutions(&ev, jack), 1);
    assert_eq!(damage_from(&ev, jack), vec![]);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// Alive but removed from combat (ruling 2020-11-10): no player or planeswalker is dealt
/// damage.
#[test]
fn swordjack_removed_from_combat_deals_no_damage() {
    let (mut r, jack, karn) = swordjack_board(3);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    gone_from_combat(&mut r, jack);
    assert_eq!(r.state().objects[&jack].zone, Zone::Battlefield);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(deal_damage_resolutions(&ev, jack), 1);
    assert_eq!(loyalty(&r, karn), 5);
    assert_eq!(damage_from(&ev, jack), vec![]);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

const FULL_SWORDJACK: &str = "Whenever this creature attacks, it deals damage to the player or planeswalker it's attacking equal to the number of artifacts you control.\nEncore {5}{R} ({5}{R}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";
const FULL_HELLRIDER: &str = "Haste\nWhenever a creature you control attacks, this creature deals 1 damage to the player or planeswalker it's attacking.";
const RECONNAISSANCE: &str = "{0}: Remove target attacking creature you control from combat and untap it. (If you activate during end of combat, the creature will untap after it deals combat damage.)";
const FLICKER: &str =
    "Exile target nontoken permanent, then return it to the battlefield under its owner's control.";
const WORD: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.";
const LEYLINE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const LIFELINK: &str = "Enchant creature\nEnchanted creature has lifelink. (Damage dealt by the creature also causes its controller to gain that much life.)";
const RANCOR: &str = "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen this Aura is put into a graveyard from the battlefield, return it to its owner's hand.";
const THRASTA: &str = "This spell costs {3} less to cast for each other spell cast this turn.\nTrample, haste\nTrample over planeswalkers (This creature can deal excess combat damage to the controller of the planeswalker it's attacking.)\nThrasta has hexproof as long as it entered this turn.";

struct AuthorityBoard {
    r: GameRunner,
    source: ObjectId,
    attacker: ObjectId,
    walker: ObjectId,
    recon: ObjectId,
    murder: ObjectId,
    flicker: ObjectId,
    word: ObjectId,
    blocker: ObjectId,
    rancor: ObjectId,
    unrelated: ObjectId,
}

fn authority_board(observer: bool, loyalty: u32) -> AuthorityBoard {
    damage_authority_board(observer, false, loyalty)
}

fn damage_authority_board(observer: bool, thrasta: bool, loyalty: u32) -> AuthorityBoard {
    let mut sc = scenario();
    let source = if thrasta {
        sc.add_creature(P0, "Thrasta, Tempest's Roar", 7, 7)
            .from_oracle_text_with_keywords(
                &["Trample", "Haste", "Trample over planeswalkers"],
                THRASTA,
            )
            .id()
    } else if observer {
        sc.add_creature(P0, "Hellrider", 3, 3)
            .from_oracle_text_with_keywords(&["Haste"], FULL_HELLRIDER)
            .id()
    } else {
        sc.add_creature_from_oracle(P0, "Fathom Fleet Swordjack", 4, 3, FULL_SWORDJACK)
            .id()
    };
    let attacker = if observer {
        sc.add_creature(P0, "Goblin", 1, 1).id()
    } else {
        source
    };
    for i in 0..3 {
        sc.add_artifact_from_oracle(P0, &format!("Artifact {i}"), "");
    }
    let walker = add_karn(&mut sc, loyalty);
    let recon = sc
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .id();
    sc.add_enchantment_from_oracle(P0, "Leyline of Anticipation", LEYLINE);
    let murder = sc
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();
    let flicker = sc
        .add_spell_to_hand_from_oracle(P0, "Flicker", false, FLICKER)
        .id();
    let word = sc
        .add_spell_to_hand_from_oracle(P0, "Word of Seizing", true, WORD)
        .from_oracle_text_with_keywords(&["Split second"], WORD)
        .id();
    let blocker = sc.add_creature(P1, "Blocking creature", 1, 4).id();
    let unrelated = sc.add_creature(P0, "Unrelated attacker", 1, 1).id();
    let rancor = sc
        .add_spell_to_hand(P0, "Rancor", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], RANCOR)
        .id();
    let mut r = sc.build();
    control_by_p1(&mut r, walker);
    AuthorityBoard {
        r,
        source,
        attacker,
        walker,
        recon,
        murder,
        flicker,
        word,
        blocker,
        rancor,
        unrelated,
    }
}

fn pass_to_p0(r: &mut GameRunner) {
    for _ in 0..3 {
        match r.state().waiting_for {
            WaitingFor::Priority { player: P0 } => return,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("expected response priority: {other:?}"),
        }
    }
    panic!("P0 did not get response priority");
}

/// Resolve just this response through CastCommit actions, preserving lower triggers.
fn resolve_response(r: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    pass_to_p0(r);
    let mut cast = r.cast(spell).target_object(target).commit();
    assert!(cast
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == spell));
    let mut events = Vec::new();
    for _ in 0..24 {
        if !cast
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            return events;
        }
        events.extend(
            cast.act(GameAction::PassPriority)
                .expect("response resolves")
                .events,
        );
    }
    panic!("response remained on stack");
}

fn remove_with_recon(b: &mut AuthorityBoard) {
    let predecessor = b.r.state().clone();
    pass_to_p0(&mut b.r);
    b.r.act(GameAction::ActivateAbility {
        source_id: b.recon,
        ability_index: 0,
        payment_mode: Default::default(),
    })
    .unwrap();
    if matches!(b.r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        b.r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b.attacker)),
        })
        .unwrap();
    }
    for _ in 0..24 {
        if !b
            .r
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == b.recon)
        {
            break;
        }
        b.r.act(GameAction::PassPriority).unwrap();
    }
    assert!(!b
        .r
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|info| info.object_id == b.attacker));
    assert_untargeted_triggers(&b.r, b.source, 1);
    let command = b
        .r
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry.command.as_ref() {
            Some(engine::types::resolved_commands::ResolvedRulesCommand::CombatMembership(
                command,
            )) if command.object.object_id == b.attacker
                && matches!(
                    command.edit,
                    engine::types::resolved_commands::ResolvedCombatMembershipEdit::Remove { .. }
                ) =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .expect("printed removal journals its complete combat receipt");
    let engine::types::resolved_commands::ResolvedCombatMembershipEdit::Remove {
        expected_participation,
    } = &command.edit
    else {
        unreachable!()
    };
    assert_eq!(
        *expected_participation,
        engine::game::combat::CombatParticipation::capture(&predecessor, b.attacker)
    );
    assert!(expected_participation
        .outgoing_attack_target_identity
        .flatten()
        .is_some());
    let mut replay = predecessor;
    engine::game::combat::apply_resolved_combat_membership(&mut replay, &command).unwrap();
    assert_eq!(
        replay.combat.as_ref().unwrap().attackers,
        b.r.state().combat.as_ref().unwrap().attackers
    );
    assert_eq!(
        replay.combat.as_ref().unwrap().attacked_permanents,
        b.r.state().combat.as_ref().unwrap().attacked_permanents
    );
}

/// CR 506.4 + CR 608.2h: newest exact departure, including no recipient, wins.
#[test]
fn exact_departure_after_printed_removal_overrides_self_and_observer_placement() {
    for observer in [false, true] {
        for removed in [false, true] {
            let mut b = authority_board(observer, 5);
            attack(
                &mut b.r,
                &[(b.attacker, AttackTarget::Planeswalker(b.walker))],
            );
            assert_untargeted_triggers(&b.r, b.source, 1);
            let pin = b.r.state().combat.as_ref().unwrap().attacked_permanents[&b.attacker];
            assert_eq!(pin.object_id, b.walker);
            if removed {
                remove_with_recon(&mut b);
            }
            resolve_response(&mut b.r, b.murder, b.attacker);
            assert_eq!(b.r.state().objects[&b.attacker].zone, Zone::Graveyard);
            assert_untargeted_triggers(&b.r, b.source, 1);
            let row = b
                .r
                .state()
                .zone_changes_this_turn
                .iter()
                .rev()
                .find(|row| row.object_id == b.attacker && row.from_zone == Some(Zone::Battlefield))
                .unwrap();
            assert_eq!(
                row.combat_status.attack_target_identity,
                (!removed).then_some(pin)
            );
            let mut events = Vec::new();
            settle(&mut b.r, &mut events, true, 0);
            let amount = if observer { 1 } else { 3 };
            assert_eq!(deal_damage_resolutions(&events, b.source), 1);
            assert_eq!(
                damage_from(&events, b.source),
                if removed {
                    vec![]
                } else {
                    vec![(TargetRef::Object(b.walker), amount)]
                }
            );
            assert_eq!(
                loyalty(&b.r, b.walker),
                5 - if removed { 0 } else { amount }
            );
            assert_eq!(
                (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
                (20, 20, 20)
            );
        }
    }
}

/// CR 400.7 + CR 506.4c + CR 608.2h: live edge lifetime and departure LKI differ.
#[test]
fn printed_recipient_mutations_respect_live_and_departure_timing() {
    for observer in [false, true] {
        for blink in [false, true] {
            for timing in 0..3 {
                let mut b = authority_board(observer, 5);
                attack(
                    &mut b.r,
                    &[(b.attacker, AttackTarget::Planeswalker(b.walker))],
                );
                assert_untargeted_triggers(&b.r, b.source, 1);
                let pin = b.r.state().combat.as_ref().unwrap().attacked_permanents[&b.attacker];
                if timing == 2 {
                    resolve_response(&mut b.r, b.murder, b.attacker);
                }
                resolve_response(&mut b.r, if blink { b.flicker } else { b.word }, b.walker);
                let object = &b.r.state().objects[&b.walker];
                assert_eq!(object.zone, Zone::Battlefield);
                assert_eq!(object.incarnation != pin.incarnation, blink);
                assert_eq!(object.controller, if blink { P2 } else { P0 });
                if blink {
                    // CR 306.5b + CR 306.5c: the returned incarnation has fresh printed loyalty.
                    assert_eq!(object.base_loyalty, Some(5));
                    assert_eq!(object.loyalty, Some(5));
                    assert_eq!(object.counters[&CounterType::Loyalty], 5);
                }
                assert!(!b
                    .r
                    .state()
                    .combat
                    .as_ref()
                    .unwrap()
                    .attacked_permanents
                    .contains_key(&b.attacker));
                if timing == 1 {
                    resolve_response(&mut b.r, b.murder, b.attacker);
                }
                if timing == 0 {
                    assert!(b
                        .r
                        .state()
                        .combat
                        .as_ref()
                        .unwrap()
                        .attackers
                        .iter()
                        .any(|info| info.object_id == b.attacker));
                }
                assert_untargeted_triggers(&b.r, b.source, 1);
                let mut events = Vec::new();
                settle(&mut b.r, &mut events, true, 0);
                let amount = if observer { 1 } else { 3 };
                let expected = if timing == 2 && !blink { amount } else { 0 };
                assert_eq!(deal_damage_resolutions(&events, b.source), 1);
                assert_eq!(loyalty(&b.r, b.walker), 5 - expected);
                assert_eq!(
                    damage_from(&events, b.source),
                    if expected == 0 {
                        vec![]
                    } else {
                        vec![(TargetRef::Object(b.walker), expected)]
                    }
                );
                assert_eq!(
                    (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
                    (20, 20, 20)
                );
            }
        }
    }
}

#[test]
fn recipient_retirement_ends_every_incoming_attack_but_keeps_a_player_attack() {
    for blink in [false, true] {
        let mut b = authority_board(true, 5);
        attack(
            &mut b.r,
            &[
                (b.source, AttackTarget::Planeswalker(b.walker)),
                (b.attacker, AttackTarget::Planeswalker(b.walker)),
                (b.unrelated, AttackTarget::Player(P1)),
            ],
        );
        assert_untargeted_triggers(&b.r, b.source, 3);
        assert_eq!(
            b.r.state()
                .combat
                .as_ref()
                .unwrap()
                .attacked_permanents
                .len(),
            2
        );
        let pin = b.r.state().combat.as_ref().unwrap().attacked_permanents[&b.attacker];
        resolve_response(&mut b.r, if blink { b.flicker } else { b.word }, b.walker);
        let object = &b.r.state().objects[&b.walker];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.incarnation != pin.incarnation, blink);
        if blink {
            assert_eq!(object.base_loyalty, Some(5));
            assert_eq!(object.loyalty, Some(5));
            assert_eq!(object.counters[&CounterType::Loyalty], 5);
        }
        assert!(b
            .r
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attacked_permanents
            .is_empty());
        assert_eq!(b.r.state().combat.as_ref().unwrap().attackers.len(), 3);
        let mut events = Vec::new();
        settle(&mut b.r, &mut events, true, 0);
        assert_eq!(deal_damage_resolutions(&events, b.source), 3);
        assert_eq!(
            damage_from(&events, b.source),
            vec![(TargetRef::Player(P1), 1)]
        );
        assert_eq!(loyalty(&b.r, b.walker), 5);
        assert_eq!(
            (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
            (20, 19, 20)
        );
    }
}

#[test]
fn source_return_and_new_departure_cannot_replace_the_original_exact_departure() {
    for observer in [false, true] {
        let mut b = authority_board(observer, 5);
        attack(
            &mut b.r,
            &[(b.attacker, AttackTarget::Planeswalker(b.walker))],
        );
        assert_untargeted_triggers(&b.r, b.source, 1);
        let original = b.r.state().objects[&b.attacker].incarnation;
        resolve_response(&mut b.r, b.flicker, b.attacker);
        assert_ne!(b.r.state().objects[&b.attacker].incarnation, original);
        resolve_response(&mut b.r, b.murder, b.attacker);
        let departures: Vec<_> = b
            .r
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|row| row.object_id == b.attacker && row.from_zone == Some(Zone::Battlefield))
            .collect();
        assert_eq!(departures.len(), 2);
        assert!(departures[0].combat_status.attack_target_identity.is_some());
        assert_eq!(departures[1].combat_status.attack_target, None);
        let mut events = Vec::new();
        settle(&mut b.r, &mut events, true, 0);
        let amount = if observer { 1 } else { 3 };
        assert_eq!(deal_damage_resolutions(&events, b.source), 1);
        assert_eq!(
            damage_from(&events, b.source),
            vec![(TargetRef::Object(b.walker), amount)]
        );
        assert_eq!(loyalty(&b.r, b.walker), 5 - amount);
        assert_eq!(
            (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
            (20, 20, 20)
        );
    }
}

#[test]
fn typed_reselection_then_printed_departure_reads_the_new_destination() {
    for observer in [false, true] {
        let mut b = authority_board(observer, 5);
        attack(&mut b.r, &[(b.attacker, AttackTarget::Player(P1))]);
        assert_untargeted_triggers(&b.r, b.source, 1);
        // Typed combat-contract setup, not a Portal Mage support assertion.
        let target = AttackTarget::Planeswalker(b.walker);
        let pin = engine::game::combat::bind_attack_target(b.r.state(), target).unwrap();
        let combat = b.r.state_mut().combat.as_mut().unwrap();
        combat
            .attackers
            .iter_mut()
            .find(|info| info.object_id == b.attacker)
            .unwrap()
            .attack_target = target;
        combat.attacked_permanents.insert(b.attacker, pin);
        resolve_response(&mut b.r, b.murder, b.attacker);
        let mut events = Vec::new();
        settle(&mut b.r, &mut events, true, 0);
        let amount = if observer { 1 } else { 3 };
        assert_eq!(deal_damage_resolutions(&events, b.source), 1);
        assert_eq!(
            damage_from(&events, b.source),
            vec![(TargetRef::Object(b.walker), amount)]
        );
        assert_eq!(loyalty(&b.r, b.walker), 5 - amount);
        assert_eq!(
            (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
            (20, 20, 20)
        );
    }
}

#[test]
fn printed_battlesphere_with_no_untapped_myr_reaches_optional_without_damage() {
    let mut sc = scenario();
    let sphere = sc.add_creature_from_oracle(P0, "Myr Battlesphere", 4, 7,
        "When this creature enters, create four 1/1 colorless Myr artifact creature tokens.\nWhenever this creature attacks, you may tap X untapped Myr you control. If you do, this creature gets +X/+0 until end of turn and deals X damage to the player or planeswalker it's attacking.")
        .with_subtypes(vec!["Myr", "Construct"]).id();
    let mut r = sc.build();
    attack(&mut r, &[(sphere, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, sphere, 1);
    assert!(r.state().objects[&sphere].tapped);
    let mut events = Vec::new();
    let seen = settle(&mut r, &mut events, true, 0);
    assert!(seen.optional && !seen.tap_prompt);
    assert_eq!(r.state().objects[&sphere].power, Some(4));
    assert_eq!(damage_from(&events, sphere), vec![]);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// CR 800.4 + CR 702.15b: no damage, hence no lifelink, to a departed seat.
#[test]
fn departed_player_is_no_recipient_and_cannot_supply_lifelink() {
    for concede in [false, true] {
        let mut sc = scenario();
        let rider = sc
            .add_creature(P0, "Hellrider", 3, 3)
            .from_oracle_text_with_keywords(&["Haste"], FULL_HELLRIDER)
            .id();
        let aura = sc
            .add_spell_to_hand(P0, "Lifelink", false)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], LIFELINK)
            .id();
        sc.add_enchantment_from_oracle(P1, "Leyline of Sanctity",
            "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof. (You can't be the target of spells or abilities your opponents control.)");
        let mut r = sc.build();
        let outcome = r.cast(aura).target_object(rider).resolve();
        outcome.assert_zone(&[aura], Zone::Battlefield);
        assert_eq!(
            outcome.state().objects[&aura].attached_to,
            Some(AttachTarget::Object(rider))
        );
        assert!(outcome.state().objects[&rider].attachments.contains(&aura));
        assert!(r.state().objects[&rider].has_keyword(&engine::types::keywords::Keyword::Lifelink));
        assert!(engine::game::static_abilities::player_has_hexproof(
            r.state(),
            P1
        ));
        assert!(
            !engine::game::targeting::player_is_legal_target(r.state(), P1, rider, P0),
            "positive control: P1 really cannot be targeted; the printed recipient is untargeted"
        );
        attack(&mut r, &[(rider, AttackTarget::Player(P1))]);
        assert_untargeted_triggers(&r, rider, 1);
        if concede {
            r.act(GameAction::Concede { player_id: P1 }).unwrap();
        }
        assert_eq!(engine::game::players::is_alive(r.state(), P1), !concede);
        assert_untargeted_triggers(&r, rider, 1);
        let mut events = Vec::new();
        settle(&mut r, &mut events, true, 0);
        assert_eq!(deal_damage_resolutions(&events, rider), 1);
        assert_eq!(
            damage_from(&events, rider),
            if concede {
                vec![]
            } else {
                vec![(TargetRef::Player(P1), 1)]
            }
        );
        assert_eq!(
            (life(&r, P0), life(&r, P1), life(&r, P2)),
            if concede { (20, 20, 20) } else { (21, 19, 20) }
        );
    }
}

/// CR 603.3b + CR 704.5i + CR 702.15b: the controller orders simultaneous
/// observers; only the first damages the 1-loyalty walker and can gain life.
#[test]
fn hellrider_observers_offer_both_orders_when_the_first_removes_the_recipient() {
    for lifelink_first in [false, true] {
        let mut sc = scenario();
        let riders = [0, 1].map(|_| {
            sc.add_creature(P0, "Hellrider", 3, 3)
                .from_oracle_text_with_keywords(&["Haste"], FULL_HELLRIDER)
                .id()
        });
        let goblin = sc.add_creature(P0, "Goblin", 1, 1).id();
        let walker = add_karn(&mut sc, 1);
        let aura = sc
            .add_spell_to_hand(P0, "Lifelink", false)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text_with_keywords(&["Enchant"], LIFELINK)
            .id();
        let mut r = sc.build();
        control_by_p1(&mut r, walker);
        let outcome = r.cast(aura).target_object(riders[0]).resolve();
        outcome.assert_zone(&[aura], Zone::Battlefield);
        assert_eq!(
            outcome.state().objects[&aura].attached_to,
            Some(AttachTarget::Object(riders[0]))
        );
        assert!(outcome.state().objects[&riders[0]]
            .attachments
            .contains(&aura));
        assert!(
            r.state().objects[&riders[0]].has_keyword(&engine::types::keywords::Keyword::Lifelink)
        );
        assert!(
            !r.state().objects[&riders[1]].has_keyword(&engine::types::keywords::Keyword::Lifelink)
        );
        r.advance_to_combat();
        r.declare_attackers(&[(goblin, AttackTarget::Planeswalker(walker))])
            .unwrap();
        let WaitingFor::OrderTriggers { player, triggers } = &r.state().waiting_for else {
            panic!("the live recipient read requires an ordering choice");
        };
        assert_eq!(*player, P0);
        assert_eq!(triggers.len(), 2);
        let first = if lifelink_first { riders[0] } else { riders[1] };
        let second = if lifelink_first { riders[1] } else { riders[0] };
        let first_index = triggers.iter().position(|t| t.source_id == first).unwrap();
        let second_index = triggers.iter().position(|t| t.source_id == second).unwrap();
        r.act(GameAction::OrderTriggers {
            order: vec![second_index, first_index],
        })
        .unwrap();
        assert_eq!(r.state().stack.back().unwrap().source_id, first);
        for source in riders {
            assert_untargeted_triggers(&r, source, 1);
        }
        // Both copies differ only in their own source identity: the trigger reference
        // and the per-source optional-trigger origin. Clear both before comparing.
        let mut normalized = stack_abilities(&r, riders[0])[0].clone();
        normalized.clear_trigger_identity_recursive();
        normalized.may_trigger_origin = None;
        let mut other = stack_abilities(&r, riders[1])[0].clone();
        other.clear_trigger_identity_recursive();
        other.may_trigger_origin = None;
        assert_eq!(
            normalized, other,
            "granted lifelink is outside the normalized AST"
        );

        let mut events = Vec::new();
        let seen = settle(&mut r, &mut events, true, 0);
        assert!(!seen.target_prompt);
        assert_eq!(deal_damage_resolutions(&events, first), 1);
        assert_eq!(deal_damage_resolutions(&events, second), 1);
        assert_eq!(
            damage_from(&events, first),
            vec![(TargetRef::Object(walker), 1)]
        );
        assert_eq!(damage_from(&events, second), vec![]);
        assert_eq!(r.state().objects[&walker].zone, Zone::Graveyard);
        assert!(!r
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attacked_permanents
            .contains_key(&goblin));
        assert_eq!(
            (life(&r, P0), life(&r, P1), life(&r, P2)),
            (if lifelink_first { 21 } else { 20 }, 20, 20)
        );
    }
}

fn reach_blocked_priority(b: &mut AuthorityBoard) {
    for _ in 0..32 {
        let blocked =
            b.r.state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|info| info.object_id == b.attacker && info.blocked);
        if blocked && matches!(b.r.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        match b.r.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { player, .. } => {
                let blockers = [(b.blocker, b.attacker)];
                b.r.declare_blockers(if player == P1 { &blockers } else { &[] })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                b.r.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("before legal blocking: {other:?}"),
        }
    }
    panic!("did not reach blocked priority");
}

fn reach_assignment(r: &mut GameRunner) {
    for _ in 0..24 {
        if matches!(r.state().waiting_for, WaitingFor::AssignCombatDamage { .. }) {
            return;
        }
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        r.act(GameAction::PassPriority).unwrap();
    }
    panic!("did not reach real damage assignment");
}

/// CR 702.19b/c/e + CR 506.4c: real prompt, shared candidate, validation and routing.
#[test]
fn interactive_excess_and_candidates_use_the_current_exact_recipient() {
    use engine::types::game_state::CombatDamageAssignmentMode as Mode;
    for over_pw in [false, true] {
        for retirement in 0..3 {
            for all_blocker in [false, true] {
                let mut b = damage_authority_board(false, over_pw, if over_pw { 1 } else { 10 });
                if !over_pw {
                    let outcome = b.r.cast(b.rancor).target_object(b.attacker).resolve();
                    outcome.assert_zone(&[b.rancor], Zone::Battlefield);
                    assert_eq!(
                        outcome.state().objects[&b.rancor].attached_to,
                        Some(AttachTarget::Object(b.attacker))
                    );
                    assert!(outcome.state().objects[&b.attacker]
                        .attachments
                        .contains(&b.rancor));
                    assert_eq!(b.r.state().objects[&b.attacker].power, Some(6));
                    assert!(b.r.state().objects[&b.attacker]
                        .has_keyword(&engine::types::keywords::Keyword::Trample));
                }
                attack(
                    &mut b.r,
                    &[(b.attacker, AttackTarget::Planeswalker(b.walker))],
                );
                let pin = b.r.state().combat.as_ref().unwrap().attacked_permanents[&b.attacker];
                if !over_pw {
                    assert_untargeted_triggers(&b.r, b.source, 1);
                    let mut trigger_events = Vec::new();
                    settle(&mut b.r, &mut trigger_events, true, 0);
                    assert_eq!(
                        damage_from(&trigger_events, b.source),
                        vec![(TargetRef::Object(b.walker), 3)]
                    );
                    assert_eq!(loyalty(&b.r, b.walker), 7);
                }
                reach_blocked_priority(&mut b);
                if retirement != 0 {
                    resolve_response(
                        &mut b.r,
                        if retirement == 1 { b.word } else { b.flicker },
                        b.walker,
                    );
                    assert!(!b
                        .r
                        .state()
                        .combat
                        .as_ref()
                        .unwrap()
                        .attacked_permanents
                        .contains_key(&b.attacker));
                    assert_eq!(
                        b.r.state().objects[&b.walker].incarnation != pin.incarnation,
                        retirement == 2
                    );
                    assert!(b.r.state().combat.as_ref().unwrap().attackers[0].blocked);
                }
                reach_assignment(&mut b.r);
                let power = if over_pw { 7 } else { 6 };
                let (threshold, controller) = match b.r.state().waiting_for {
                    WaitingFor::AssignCombatDamage {
                        total_damage,
                        pw_loyalty,
                        pw_controller,
                        ..
                    } => {
                        assert_eq!(total_damage, power);
                        (pw_loyalty, pw_controller)
                    }
                    _ => unreachable!(),
                };
                if over_pw {
                    assert_eq!(threshold, if retirement == 0 { Some(1) } else { None });
                    assert_eq!(controller, Some(P1));
                }
                if retirement != 0 && !over_pw {
                    let before = serde_json::to_value(b.r.state()).unwrap();
                    let dirty = format!("{:?}", b.r.state().layers_dirty);
                    let invalid = b.r.act(GameAction::AssignCombatDamage {
                        mode: Mode::Normal,
                        assignments: vec![(b.blocker, 4)],
                        trample_damage: 2,
                        controller_damage: 0,
                    });
                    assert!(matches!(
                        invalid,
                        Err(engine::game::engine::EngineError::InvalidAction(_))
                    ));
                    assert_eq!(
                        serde_json::to_value(b.r.state()).unwrap(),
                        before,
                        "rejected excess changes no graph, prompt, journal, cursor or damage"
                    );
                    assert_eq!(format!("{:?}", b.r.state().layers_dirty), dirty);
                }
                let generated = engine::ai_support::legal_actions(b.r.state())
                    .into_iter()
                    .find(|action| {
                        matches!(
                            action,
                            GameAction::AssignCombatDamage {
                                mode: Mode::Normal,
                                ..
                            }
                        )
                    })
                    .expect("normal legal response remains available");
                let expected_generated = if over_pw {
                    GameAction::AssignCombatDamage {
                        mode: Mode::Normal,
                        assignments: vec![(b.blocker, 4)],
                        trample_damage: if retirement == 0 { 1 } else { 0 },
                        controller_damage: if retirement == 0 { 2 } else { 3 },
                    }
                } else if retirement == 0 {
                    GameAction::AssignCombatDamage {
                        mode: Mode::Normal,
                        assignments: vec![(b.blocker, 4)],
                        trample_damage: 2,
                        controller_damage: 0,
                    }
                } else {
                    GameAction::AssignCombatDamage {
                        mode: Mode::Normal,
                        assignments: vec![(b.blocker, 6)],
                        trample_damage: 0,
                        controller_damage: 0,
                    }
                };
                assert_eq!(generated, expected_generated);
                let before_loyalty = loyalty(&b.r, b.walker);
                let result =
                    b.r.act(if all_blocker {
                        GameAction::AssignCombatDamage {
                            mode: Mode::Normal,
                            assignments: vec![(b.blocker, power)],
                            trample_damage: 0,
                            controller_damage: 0,
                        }
                    } else {
                        generated
                    })
                    .expect("legal assignment finishes");
                let expected = if all_blocker {
                    vec![(TargetRef::Object(b.blocker), power)]
                } else if !over_pw && retirement != 0 {
                    vec![(TargetRef::Object(b.blocker), 6)]
                } else if !over_pw {
                    vec![
                        (TargetRef::Object(b.blocker), 4),
                        (TargetRef::Object(b.walker), 2),
                    ]
                } else if retirement == 0 {
                    vec![
                        (TargetRef::Object(b.blocker), 4),
                        (TargetRef::Object(b.walker), 1),
                        (TargetRef::Player(P1), 2),
                    ]
                } else {
                    vec![
                        (TargetRef::Object(b.blocker), 4),
                        (TargetRef::Player(P1), 3),
                    ]
                };
                assert_eq!(damage_from(&result.events, b.attacker), expected);
                assert_eq!(
                    loyalty(&b.r, b.walker),
                    before_loyalty
                        - if all_blocker || retirement != 0 {
                            0
                        } else if over_pw {
                            1
                        } else {
                            2
                        }
                );
                assert_eq!(
                    (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
                    (
                        20,
                        if over_pw && !all_blocker {
                            if retirement == 0 {
                                18
                            } else {
                                17
                            }
                        } else {
                            20
                        },
                        20
                    )
                );
                assert!(!matches!(
                    b.r.state().waiting_for,
                    WaitingFor::AssignCombatDamage { .. }
                ));
            }
        }
    }
}

/// CR 510.1c + CR 702.19d + CR 702.19f + CR 506.4c: a blocked standard trampler whose only
/// blocker died and whose attacked planeswalker left combat assigns no combat damage. Trample
/// over an empty block assigns to the planeswalker it attacks, which is gone, and never to the
/// defending player, so no assignment prompt opens and combat finishes with no damage from it.
#[test]
fn blocked_trampler_with_no_blocker_and_no_attacked_planeswalker_assigns_nothing() {
    let mut b = damage_authority_board(false, false, 10);
    let outcome = b.r.cast(b.rancor).target_object(b.attacker).resolve();
    outcome.assert_zone(&[b.rancor], Zone::Battlefield);
    assert_eq!(b.r.state().objects[&b.attacker].power, Some(6));
    assert!(
        b.r.state().objects[&b.attacker].has_keyword(&engine::types::keywords::Keyword::Trample)
    );
    attack(
        &mut b.r,
        &[(b.attacker, AttackTarget::Planeswalker(b.walker))],
    );
    let mut trigger_events = Vec::new();
    settle(&mut b.r, &mut trigger_events, true, 0);
    assert_eq!(
        loyalty(&b.r, b.walker),
        7,
        "reach: the attack trigger hit Karn"
    );
    reach_blocked_priority(&mut b);
    resolve_response(&mut b.r, b.murder, b.blocker);
    assert_eq!(b.r.state().objects[&b.blocker].zone, Zone::Graveyard);
    resolve_response(&mut b.r, b.word, b.walker);
    {
        let combat = b.r.state().combat.as_ref().unwrap();
        assert!(!combat.attacked_permanents.contains_key(&b.attacker));
        let info = combat
            .attackers
            .iter()
            .find(|info| info.object_id == b.attacker)
            .expect("the trampler is still attacking");
        assert!(info.blocked, "CR 509.1h: it remains blocked");
        assert!(combat
            .blocker_assignments
            .get(&b.attacker)
            .is_none_or(Vec::is_empty));
    }
    let mut events = Vec::new();
    let mut saw_damage_step = false;
    for _ in 0..24 {
        if matches!(b.r.state().phase, Phase::EndCombat | Phase::PostCombatMain) {
            break;
        }
        saw_damage_step |= b.r.state().phase == Phase::CombatDamage;
        match b.r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                events.extend(b.r.act(GameAction::PassPriority).unwrap().events);
            }
            other => panic!("combat must finish without a damage prompt, got {other:?}"),
        }
    }
    assert!(saw_damage_step, "reach: the combat damage step was entered");
    assert!(
        matches!(b.r.state().phase, Phase::EndCombat | Phase::PostCombatMain),
        "combat finished"
    );
    assert_eq!(damage_from(&events, b.attacker), vec![]);
    assert_eq!(loyalty(&b.r, b.walker), 7);
    assert_eq!(
        (life(&b.r, P0), life(&b.r, P1), life(&b.r, P2)),
        (20, 20, 20)
    );
}

/// Killed in response, still damages what it was attacking (ruling 2020-11-10).
#[test]
fn swordjack_killed_in_response_still_damages_the_planeswalker_it_was_attacking() {
    let (mut r, jack, karn) = swordjack_board(3);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    zones::move_to_zone(r.state_mut(), jack, Zone::Graveyard, &mut Vec::new());
    assert_eq!(r.state().objects[&jack].zone, Zone::Graveyard);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 2);
}

// ---------------------------------------------------------------------------------------------
// Scorch Spitter, Rakdos Roustabout, Mage Slayer, Cavalcade of Calamity, Raid Bombardment
// ---------------------------------------------------------------------------------------------

/// Attacks a player.
#[test]
fn scorch_spitter_deals_one_to_the_attacked_player() {
    let mut sc = scenario();
    let spitter = sc
        .add_creature_from_oracle(P0, "Scorch Spitter", 3, 1, SPITTER)
        .id();
    let mut r = sc.build();
    attack(&mut r, &[(spitter, AttackTarget::Player(P1))]);
    assert_untargeted_triggers(&r, spitter, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(life(&r, P1), 19);
    assert_eq!(damage_from(&ev, spitter), vec![(TargetRef::Player(P1), 1)]);
}

/// Attacks a planeswalker at loyalty 1; it dies (CR 704.5i) and the Spitter keeps attacking
/// (ruling 2019-07-12).
#[test]
fn scorch_spitter_kills_a_one_loyalty_planeswalker_and_keeps_attacking() {
    let mut sc = scenario();
    let spitter = sc
        .add_creature_from_oracle(P0, "Scorch Spitter", 3, 1, SPITTER)
        .id();
    let karn = add_karn(&mut sc, 1);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    attack(&mut r, &[(spitter, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, spitter, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(
        damage_from(&ev, spitter),
        vec![(TargetRef::Object(karn), 1)]
    );
    assert_eq!(r.state().objects[&karn].zone, Zone::Graveyard);
    let combat = r.state().combat.as_ref().expect("combat continues");
    assert!(combat.attackers.iter().any(|a| a.object_id == spitter));
}

fn roustabout_board(at_karn: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut sc = scenario();
    let rb = sc
        .add_creature_from_oracle(P0, "Rakdos Roustabout", 2, 2, ROUSTABOUT)
        .id();
    let blocker = sc.add_creature(P1, "Blocker", 1, 4).id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    let target = if at_karn {
        AttackTarget::Planeswalker(karn)
    } else {
        AttackTarget::Player(P1)
    };
    attack(&mut r, &[(rb, target)]);
    for _ in 0..10 {
        if matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    r.declare_blockers(&[(blocker, rb)]).expect("block");
    drain_orders(&mut r);
    (r, rb, blocker, karn)
}

/// Blocked while attacking a player.
#[test]
fn roustabout_blocked_deals_one_to_the_player_it_is_attacking() {
    let (mut r, rb, _blocker, karn) = roustabout_board(false);
    assert_untargeted_triggers(&r, rb, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(life(&r, P1), 19);
    assert_eq!(loyalty(&r, karn), 5);
}

/// Dead before the trigger resolves (CR 113.7a, CR 608.2h).
#[test]
fn roustabout_dead_before_resolution_still_damages_the_player() {
    let (mut r, rb, _blocker, _karn) = roustabout_board(false);
    assert_untargeted_triggers(&r, rb, 1);
    zones::move_to_zone(r.state_mut(), rb, Zone::Graveyard, &mut Vec::new());
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(life(&r, P1), 19);
}

/// Blocked while attacking a planeswalker.
#[test]
fn roustabout_blocked_while_attacking_a_planeswalker_damages_the_planeswalker() {
    let (mut r, rb, _blocker, karn) = roustabout_board(true);
    assert_untargeted_triggers(&r, rb, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 4);
    assert_eq!(life(&r, P1), 20);
}

fn slayer_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut sc = scenario();
    let host = sc.add_creature(P0, "Host", 3, 3).id();
    let slayer = sc
        .add_artifact_from_oracle(P0, "Mage Slayer", MAGE_SLAYER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    r.state_mut().objects.get_mut(&slayer).unwrap().attached_to = Some(AttachTarget::Object(host));
    r.state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(slayer);
    r.state_mut().layers_dirty.mark_full();
    (r, host, slayer, karn)
}

/// The equipped creature deals damage equal to its power to the planeswalker it attacks.
#[test]
fn mage_slayer_equipped_creature_damages_the_attacked_planeswalker_by_its_power() {
    let (mut r, host, slayer, karn) = slayer_board();
    attack(&mut r, &[(host, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, slayer, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 2);
    assert_eq!(life(&r, P1), 20);
    assert_eq!(damage_from(&ev, host), vec![(TargetRef::Object(karn), 3)]);
}

/// The Equipment leaves in response; the creature still deals the damage (ruling 2009-05-01).
#[test]
fn mage_slayer_gone_in_response_still_damages_the_planeswalker() {
    let (mut r, host, slayer, karn) = slayer_board();
    attack(&mut r, &[(host, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, slayer, 1);
    zones::move_to_zone(r.state_mut(), slayer, Zone::Graveyard, &mut Vec::new());
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 2);
}

/// CR 603.3b + CR 506.4 + CR 704.5i: two Mage Slayers on two attackers of one 3-loyalty
/// planeswalker fire on distinct attack events. The 3-power creature's damage removes the
/// planeswalker, so whichever trigger resolves second may have no recipient: the controller must
/// be offered the order, and each order produces its own damage.
#[test]
fn mage_slayer_fanout_prompts_when_the_first_trigger_can_remove_the_planeswalker() {
    for small_first in [false, true] {
        let mut sc = scenario();
        let big = sc.add_creature(P0, "Three-Power", 3, 3).id();
        let small = sc.add_creature(P0, "One-Power", 1, 1).id();
        let slayers = [big, small].map(|_| {
            sc.add_artifact_from_oracle(P0, "Mage Slayer", MAGE_SLAYER)
                .with_subtypes(vec!["Equipment"])
                .id()
        });
        let karn = add_karn(&mut sc, 3);
        let mut r = sc.build();
        control_by_p1(&mut r, karn);
        for (slayer, host) in slayers.into_iter().zip([big, small]) {
            r.state_mut().objects.get_mut(&slayer).unwrap().attached_to =
                Some(AttachTarget::Object(host));
            r.state_mut()
                .objects
                .get_mut(&host)
                .unwrap()
                .attachments
                .push(slayer);
        }
        r.state_mut().layers_dirty.mark_full();
        r.advance_to_combat();
        r.declare_attackers(&[
            (big, AttackTarget::Planeswalker(karn)),
            (small, AttackTarget::Planeswalker(karn)),
        ])
        .unwrap();
        let WaitingFor::OrderTriggers { player, triggers } = r.state().waiting_for.clone() else {
            panic!(
                "the first trigger can remove the planeswalker: an ordering choice is required, \
                 got {:?}",
                r.state().waiting_for
            );
        };
        assert_eq!(player, P0);
        assert_eq!(triggers.len(), 2);
        let index_of = |source: ObjectId| triggers.iter().position(|t| t.source_id == source);
        let (big_index, small_index) =
            (index_of(slayers[0]).unwrap(), index_of(slayers[1]).unwrap());
        // The last entry is put on the stack last, so it resolves first.
        let order = if small_first {
            vec![big_index, small_index]
        } else {
            vec![small_index, big_index]
        };
        r.act(GameAction::OrderTriggers { order }).unwrap();
        for slayer in slayers {
            assert_untargeted_triggers(&r, slayer, 1);
        }
        let mut ev = Vec::new();
        settle(&mut r, &mut ev, true, 0);
        assert_eq!(
            deal_damage_resolutions(&ev, slayers[0]) + deal_damage_resolutions(&ev, slayers[1]),
            2,
            "both triggers resolved"
        );
        assert_eq!(
            damage_from(&ev, big),
            vec![(TargetRef::Object(karn), 3)],
            "the 3-power creature always deals its damage (small first: {small_first})"
        );
        assert_eq!(
            damage_from(&ev, small),
            if small_first {
                vec![(TargetRef::Object(karn), 1)]
            } else {
                vec![]
            },
            "the 1-power creature deals damage only if it resolves before Karn is gone"
        );
        assert_eq!(r.state().objects[&karn].zone, Zone::Graveyard);
        assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
    }
}

fn cavalcade_board(text: &str, name: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut sc = scenario();
    let ench = sc.add_enchantment_from_oracle(P0, name, text).id();
    let small = sc.add_creature(P0, "One-Power", 1, 1).id();
    let big = sc.add_creature(P0, "Three-Power", 3, 3).id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    (r, ench, small, big, karn)
}

/// Only the qualifying attacker triggers; it damages the planeswalker it attacks.
#[test]
fn cavalcade_damages_what_the_qualifying_attacker_is_attacking() {
    let (mut r, ench, small, big, karn) = cavalcade_board(CAVALCADE, "Cavalcade of Calamity");
    attack(
        &mut r,
        &[
            (small, AttackTarget::Planeswalker(karn)),
            (big, AttackTarget::Player(P1)),
        ],
    );
    assert_untargeted_triggers(&r, ench, 1);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 4);
    assert_eq!(life(&r, P1), 20);
    assert_eq!(damage_from(&ev, ench), vec![(TargetRef::Object(karn), 1)]);
}

/// The qualifying attacker dies before resolution; the damage still goes to what it was
/// attacking.
#[test]
fn cavalcade_trigger_of_a_dead_attacker_damages_what_it_was_attacking() {
    let (mut r, ench, small, _big, karn) = cavalcade_board(CAVALCADE, "Cavalcade of Calamity");
    attack(&mut r, &[(small, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, ench, 1);
    zones::move_to_zone(r.state_mut(), small, Zone::Graveyard, &mut Vec::new());
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(loyalty(&r, karn), 4);
}

/// One attacker at a battle, one at the player. The battle is no referent (ruling
/// 2023-09-01).
#[test]
fn raid_bombardment_deals_damage_only_for_the_creature_attacking_a_player() {
    let mut sc = scenario();
    let ench = sc
        .add_enchantment_from_oracle(P0, "Raid Bombardment", RAID_BOMBARDMENT)
        .id();
    let a = sc.add_creature(P0, "Two-Power A", 2, 2).id();
    let b = sc.add_creature(P0, "Two-Power B", 2, 2).id();
    let siege = sc.add_creature(P0, "Placeholder Siege", 0, 0).id();
    let mut r = sc.build();
    make_into_siege(&mut r, siege, P1, 5);
    attack(
        &mut r,
        &[
            (a, AttackTarget::Battle(siege)),
            (b, AttackTarget::Player(P1)),
        ],
    );
    assert_untargeted_triggers(&r, ench, 2);
    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(deal_damage_resolutions(&ev, ench), 2);
    assert_eq!(damage_from(&ev, ench), vec![(TargetRef::Player(P1), 1)]);
    assert_eq!(r.state().objects[&siege].counters[&CounterType::Defense], 5);
}

// ---------------------------------------------------------------------------------------------
// Parser rows
// ---------------------------------------------------------------------------------------------

fn parse(text: &str, name: &str, types: &[&str]) -> serde_json::Value {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let parsed = engine::parser::oracle::parse_oracle_text(text, name, &[], &types, &[]);
    serde_json::json!({ "triggers": parsed.triggers, "abilities": parsed.abilities })
}

/// The `target.type` of every DealDamage node anywhere in the value.
fn deal_damage_recipients(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(|t| t.as_str()) == Some("DealDamage") {
                if let Some(target) = map.get("target") {
                    out.push(target["type"].as_str().unwrap_or("?").to_string());
                }
            }
            map.values().for_each(|v| deal_damage_recipients(v, out));
        }
        serde_json::Value::Array(items) => {
            items.iter().for_each(|v| deal_damage_recipients(v, out))
        }
        _ => {}
    }
}

/// All eight printed cards parse their recipient to the referent.
#[test]
fn the_printed_class_parses_to_the_attacked_referent() {
    for (name, text, types) in [
        (
            "Myr Battlesphere",
            BATTLESPHERE,
            &["Artifact", "Creature"][..],
        ),
        ("Hellrider", HELLRIDER, &["Creature"][..]),
        ("Fathom Fleet Swordjack", SWORDJACK, &["Creature"][..]),
        ("Scorch Spitter", SPITTER, &["Creature"][..]),
        ("Rakdos Roustabout", ROUSTABOUT, &["Creature"][..]),
        ("Mage Slayer", MAGE_SLAYER, &["Artifact"][..]),
        ("Cavalcade of Calamity", CAVALCADE, &["Enchantment"][..]),
        ("Raid Bombardment", RAID_BOMBARDMENT, &["Enchantment"][..]),
    ] {
        let v = parse(text, name, types);
        let mut recipients = Vec::new();
        deal_damage_recipients(&v, &mut recipients);
        assert_eq!(
            recipients,
            vec!["AttackedPlayerOrPlaneswalker".to_string()],
            "{name}: {v}"
        );
    }
}

/// The targeted wording keeps its slot, with Hellrider's parse in the same test as
/// the positive control (CR 115.1: "target" declares a target).
#[test]
fn targeted_player_or_planeswalker_keeps_its_slot() {
    let control = parse(HELLRIDER, "Hellrider", &["Creature"]);
    assert!(control.to_string().contains("AttackedPlayerOrPlaneswalker"));
    let v = parse(
        "Mock Bolt deals 1 damage to target player or planeswalker.",
        "Mock Bolt",
        &["Instant"],
    );
    let mut recipients = Vec::new();
    deal_damage_recipients(&v, &mut recipients);
    assert_eq!(recipients, vec!["Or".to_string()], "{v}");
    assert!(!v.to_string().contains("AttackedPlayerOrPlaneswalker"));
}

/// Attack-target setters, filters and conditions are out of class. Each row carries a
/// reach marker: proof the clause parsed into its own shape (so the absence of the
/// referent is not a parse that gave up early).
#[test]
fn setters_filters_and_conditions_do_not_absorb_the_phrase() {
    let control = parse(HELLRIDER, "Hellrider", &["Creature"]);
    assert!(control.to_string().contains("AttackedPlayerOrPlaneswalker"));
    for (name, text, types, reach) in [
        (
            "Interplanar Brushwagg",
            BRUSHWAGG,
            &["Creature"][..],
            "\"Attacks\"",
        ),
        (
            "Kitesail Skirmisher",
            KITESAIL,
            &["Creature"][..],
            "\"keyword\":\"Flying\"",
        ),
        (
            "Rampaging Geoderm",
            GEODERM,
            &["Creature"][..],
            "\"type\":\"Pump\"",
        ),
    ] {
        let v = parse(text, name, types).to_string();
        assert!(v.contains(reach), "reach guard {name} ({reach}): {v}");
        assert!(!v.contains("AttackedPlayerOrPlaneswalker"), "{name}: {v}");
    }
    // Brushwagg's trigger body stays unimplemented (the attack-target setter is not this referent).
    let v = parse(BRUSHWAGG, "Interplanar Brushwagg", &["Creature"]).to_string();
    assert!(
        v.contains("\"Attacks\""),
        "reach guard: the trigger parsed: {v}"
    );
    assert!(v.contains("Unimplemented"), "{v}");
}

// ---------------------------------------------------------------------------------------------
// Player-scope tail of an observer trigger (CR 608.2c + CR 608.2f)
// ---------------------------------------------------------------------------------------------

/// Observer trigger whose "each player creates" clause leaves a detached tail ("The tokens
/// ...") that also deals damage to the attacked referent of the triggering attacker.
const MUSTER: &str = "Whenever a creature you control attacks, each player creates a 1/1 white Warrior creature token. The tokens are goaded for the rest of the game. This enchantment deals 1 damage to the player or planeswalker that creature is attacking.";
const TAIL_PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const TAIL_HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// CR 608.2c + CR 608.2f + CR 616.1: the fan-out pauses on P1's replacement choice
/// (Halving Season and Parallel Lives both apply), and the detached tail resumes after the
/// last seat. The tail is the rest of the same triggered ability, so it still reads that
/// trigger's event: the 1 damage goes to Karn, the planeswalker the Goblin is attacking.
#[test]
fn observer_player_scope_tail_resumed_after_a_pause_reads_the_trigger_event() {
    let mut sc = scenario();
    let muster = sc.add_enchantment_from_oracle(P0, "Muster", MUSTER).id();
    sc.add_enchantment_from_oracle(P0, "Halving Season", TAIL_HALVING_SEASON);
    sc.add_enchantment_from_oracle(P1, "Parallel Lives", TAIL_PARALLEL_LIVES);
    let goblin = sc.add_creature(P0, "Goblin", 1, 1).id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    attack(&mut r, &[(goblin, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, muster, 1);
    let mut events = Vec::new();
    let mut paused = false;
    for _ in 0..80 {
        let result = match r.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { player, .. } => {
                assert_eq!(player, P1, "CR 616.1: the affected player orders them");
                paused = true;
                r.act(GameAction::ChooseReplacement { index: 0 })
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                r.act(GameAction::PassPriority)
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        events.extend(result.expect("action applies").events);
    }
    assert!(
        paused,
        "reach: the fan-out paused mid-scope on P1's replacement choice"
    );
    assert!(
        r.state().stack.is_empty() && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    );
    assert_eq!(
        deal_damage_resolutions(&events, muster),
        1,
        "the tail resolved"
    );
    assert_eq!(
        damage_from(&events, muster),
        vec![(TargetRef::Object(karn), 1)]
    );
    assert_eq!(loyalty(&r, karn), 4);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

// ---------------------------------------------------------------------------------------------
// CR 506.4: a planeswalker that stops being one stops being attacked, for good
// ---------------------------------------------------------------------------------------------

const SONG_OF_THE_DRYADS: &str =
    "Enchant permanent\nEnchanted permanent is a colorless Forest land.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";

/// Swordjack (3 artifacts) attacks Karn (loyalty 5, controlled by P1) and its trigger
/// waits on the stack. With Leyline of Anticipation, P0 can answer with Song of the
/// Dryads on `song_target` and then Disenchant on the Song.
fn dryads_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut sc = scenario();
    let jack = sc
        .add_creature_from_oracle(P0, "Fathom Fleet Swordjack", 2, 2, SWORDJACK)
        .id();
    for i in 0..3 {
        sc.add_artifact_from_oracle(P0, &format!("Trinket {i}"), "");
    }
    let karn = add_karn(&mut sc, 5);
    let bystander = sc.add_creature(P2, "Bystander", 1, 1).id();
    sc.add_enchantment_from_oracle(P0, "Leyline of Anticipation", LEYLINE);
    let song = sc
        .add_spell_to_hand(P0, "Song of the Dryads", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], SONG_OF_THE_DRYADS)
        .id();
    let disenchant = sc
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
        .id();
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    (r, jack, karn, bystander, song, disenchant)
}

fn is_planeswalker(r: &GameRunner, id: ObjectId) -> bool {
    r.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Planeswalker)
}

/// CR 506.4 + CR 506.4c: Song of the Dryads makes the attacked Karn a land, so Karn is
/// removed from combat and stops being attacked; Swordjack stays an attacking creature,
/// attacking nothing. Disenchant then destroys the Song and Karn is a planeswalker
/// again, but restoring the type declares no new attack: the waiting trigger deals no
/// damage to Karn or to anyone else.
#[test]
fn swordjack_trigger_deals_nothing_after_its_planeswalker_lost_and_regained_the_type() {
    let (mut r, jack, karn, _, song, disenchant) = dryads_board();
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);

    resolve_response(&mut r, song, karn);
    assert_eq!(
        r.state().objects[&song].attached_to,
        Some(AttachTarget::Object(karn)),
        "reach: the Song enchants Karn"
    );
    assert!(
        !is_planeswalker(&r, karn),
        "Karn is a colorless Forest land"
    );
    {
        let combat = r.state().combat.as_ref().unwrap();
        assert!(
            !combat.attacked_permanents.contains_key(&jack),
            "Karn is no longer attacked"
        );
        assert!(
            combat.attackers.iter().any(|info| info.object_id == jack),
            "CR 506.4c: Swordjack is still attacking"
        );
    }

    resolve_response(&mut r, disenchant, song);
    assert_eq!(r.state().objects[&song].zone, Zone::Graveyard);
    assert!(
        is_planeswalker(&r, karn),
        "reach: Karn is a planeswalker again"
    );
    assert!(!r
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attacked_permanents
        .contains_key(&jack));
    assert_untargeted_triggers(&r, jack, 1);

    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(
        deal_damage_resolutions(&ev, jack),
        1,
        "the trigger resolved"
    );
    assert_eq!(damage_from(&ev, jack), vec![]);
    assert_eq!(loyalty(&r, karn), 5);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// Positive control for the same response sequence: the Song and the Disenchant go to
/// a bystander creature, Karn never stops being a planeswalker, and the trigger deals
/// its 3 damage to Karn.
#[test]
fn swordjack_trigger_still_damages_a_planeswalker_that_kept_its_type() {
    let (mut r, jack, karn, bystander, song, disenchant) = dryads_board();
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    resolve_response(&mut r, song, bystander);
    assert_eq!(
        r.state().objects[&song].attached_to,
        Some(AttachTarget::Object(bystander))
    );
    resolve_response(&mut r, disenchant, song);
    assert_eq!(r.state().objects[&song].zone, Zone::Graveyard);
    assert!(is_planeswalker(&r, karn));

    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(damage_from(&ev, jack), vec![(TargetRef::Object(karn), 3)]);
    assert_eq!(loyalty(&r, karn), 2);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

const MYCOSYNTH_LATTICE: &str = "All permanents are artifacts in addition to their other types.\nAll cards that aren't on the battlefield, spells, and permanents are colorless.\nPlayers may spend mana as though it were mana of any color.";
const MARCH_OF_THE_MACHINES: &str = "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)";
const CYBER_CONVERSION: &str =
    "Turn target creature face down. It's a 2/2 Cyberman artifact creature.";
const BREAK_OPEN: &str = "Turn target face-down creature an opponent controls face up.";
const LIQUIMETAL_COATING: &str =
    "{T}: Target permanent becomes an artifact in addition to its other types until end of turn.";

/// Swordjack attacks a mana value 4 Karn (loyalty 5, controlled by P1) under Mycosynth
/// Lattice and March of the Machines, so Karn is a 4/4 artifact creature planeswalker that
/// Cyber Conversion can target. P2's bystander is a creature too. Swordjack counts three
/// artifacts: itself, the Lattice and the March. P0 holds Cyber Conversion and Break Open.
fn face_down_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut sc = scenario();
    let jack = sc
        .add_creature_from_oracle(P0, "Fathom Fleet Swordjack", 2, 2, SWORDJACK)
        .id();
    sc.add_artifact_from_oracle(P0, "Mycosynth Lattice", MYCOSYNTH_LATTICE)
        .with_mana_cost(ManaCost::generic(6));
    sc.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        });
    let karn = sc
        .add_planeswalker_from_oracle(P2, "Karn Stand-in", "Karn", 5, "+1: You gain 1 life.")
        .as_planeswalker_with_loyalty("Karn", 5)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let bystander = sc.add_creature(P2, "Bystander", 1, 1).id();
    let conversion = sc
        .add_spell_to_hand_from_oracle(P0, "Cyber Conversion", true, CYBER_CONVERSION)
        .id();
    let break_open = sc
        .add_spell_to_hand_from_oracle(P0, "Break Open", true, BREAK_OPEN)
        .id();
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    (r, jack, karn, bystander, conversion, break_open)
}

fn attacked_by(r: &GameRunner, attacker: ObjectId) -> bool {
    r.state()
        .combat
        .as_ref()
        .unwrap()
        .attacked_permanents
        .contains_key(&attacker)
}

/// CR 506.4 + CR 506.4c + CR 708.2a: Cyber Conversion turns the attacked Karn face down,
/// so it is a 2/2 Cyberman artifact creature and no planeswalker; Karn is removed from
/// combat and stops being attacked, though the face change is written outside a layer pass.
/// Break Open turns Karn face up, a planeswalker again, but that declares no new attack:
/// the waiting trigger deals no damage to Karn or to anyone else.
#[test]
fn swordjack_trigger_deals_nothing_after_its_planeswalker_was_turned_face_down_and_up() {
    let (mut r, jack, karn, _, conversion, break_open) = face_down_board();
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    assert!(is_planeswalker(&r, karn));
    assert!(
        r.state().objects[&karn]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach: Karn is an artifact creature, so Cyber Conversion can target it"
    );
    assert!(attacked_by(&r, jack));

    resolve_response(&mut r, conversion, karn);
    assert!(
        r.state().objects[&karn].face_down,
        "reach: Karn is face down"
    );
    assert!(!is_planeswalker(&r, karn));
    assert!(!attacked_by(&r, jack), "Karn is no longer attacked");
    assert!(
        r.state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|info| info.object_id == jack),
        "CR 506.4c: Swordjack is still attacking"
    );

    resolve_response(&mut r, break_open, karn);
    assert!(
        !r.state().objects[&karn].face_down,
        "reach: Karn is face up"
    );
    assert!(
        is_planeswalker(&r, karn),
        "reach: Karn is a planeswalker again"
    );
    assert!(!attacked_by(&r, jack));
    assert_untargeted_triggers(&r, jack, 1);

    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(
        deal_damage_resolutions(&ev, jack),
        1,
        "the trigger resolved"
    );
    assert_eq!(damage_from(&ev, jack), vec![]);
    assert_eq!(loyalty(&r, karn), 5);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// Positive control for the same response sequence: Cyber Conversion and Break Open go to
/// the bystander, Karn never stops being a planeswalker, and the trigger deals its 3 damage
/// to Karn.
#[test]
fn swordjack_trigger_still_damages_a_planeswalker_another_creature_was_turned_face_down() {
    let (mut r, jack, karn, bystander, conversion, break_open) = face_down_board();
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);
    resolve_response(&mut r, conversion, bystander);
    assert!(r.state().objects[&bystander].face_down);
    resolve_response(&mut r, break_open, bystander);
    assert!(!r.state().objects[&bystander].face_down);
    assert!(is_planeswalker(&r, karn));
    assert!(attacked_by(&r, jack));

    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(damage_from(&ev, jack), vec![(TargetRef::Object(karn), 3)]);
    assert_eq!(loyalty(&r, karn), 2);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}

/// CR 506.4: a type change that leaves the attacked permanent a planeswalker does not end
/// the attack. Liquimetal Coating makes the attacked Karn an artifact in addition to its
/// other types, and Swordjack (two trinkets and the Coating: three artifacts) still deals
/// its 3 damage to Karn.
#[test]
fn swordjack_trigger_still_damages_a_planeswalker_that_became_an_artifact() {
    let mut sc = scenario();
    let jack = sc
        .add_creature_from_oracle(P0, "Fathom Fleet Swordjack", 2, 2, SWORDJACK)
        .id();
    for i in 0..2 {
        sc.add_artifact_from_oracle(P0, &format!("Trinket {i}"), "");
    }
    let coating = sc
        .add_artifact_from_oracle(P0, "Liquimetal Coating", LIQUIMETAL_COATING)
        .id();
    let karn = add_karn(&mut sc, 5);
    let mut r = sc.build();
    control_by_p1(&mut r, karn);
    attack(&mut r, &[(jack, AttackTarget::Planeswalker(karn))]);
    assert_untargeted_triggers(&r, jack, 1);

    pass_to_p0(&mut r);
    r.act(GameAction::ActivateAbility {
        source_id: coating,
        ability_index: 0,
        payment_mode: Default::default(),
    })
    .expect("Liquimetal Coating activates");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(karn)),
        })
        .expect("Karn is a legal target");
    }
    for _ in 0..24 {
        if !r
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == coating)
        {
            break;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        r.state().objects[&karn]
            .card_types
            .core_types
            .contains(&CoreType::Artifact),
        "reach: Karn is an artifact"
    );
    assert!(is_planeswalker(&r, karn));
    assert!(attacked_by(&r, jack), "Karn is still attacked");
    assert_untargeted_triggers(&r, jack, 1);

    let mut ev = Vec::new();
    settle(&mut r, &mut ev, true, 0);
    assert_eq!(damage_from(&ev, jack), vec![(TargetRef::Object(karn), 3)]);
    assert_eq!(loyalty(&r, karn), 2);
    assert_eq!((life(&r, P0), life(&r, P1), life(&r, P2)), (20, 20, 20));
}
