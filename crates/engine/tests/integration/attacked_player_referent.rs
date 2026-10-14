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
    sc.add_planeswalker_from_oracle(P2, "Karn Stand-in", "Karn", loyalty, "+1: You gain 1 life.")
        .id()
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
    // Synthetic: the attacker is removed from combat by a retain-filter while staying on the
    // battlefield (CR 506.4).
    r.state_mut()
        .combat
        .as_mut()
        .unwrap()
        .attackers
        .retain(|a| a.object_id != id);
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
