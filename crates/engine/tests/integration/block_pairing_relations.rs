//! CR 509.1g + CR 509.1h + CR 506.4 + CR 608.2h: current-block-pairing relations
//! ("creatures blocking or blocked by it", "creatures blocked by this creature")
//! touch only the creatures in that block relationship with the subject — read
//! from live combat while the subject is on the battlefield, and from the
//! pairings it had immediately before it left (its last known information) once
//! it has left, for candidates still in that combat role.
//!
//! Every card is built from its verbatim Oracle text (MTGJSON AtomicCards) and
//! every step runs through the public pipeline: real `DeclareAttackers` /
//! `DeclareBlockers` / `PassPriority` / `CastSpell` / `ActivateAbility` actions.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ABU_JAFAR: &str = "When this creature dies, destroy all creatures blocking or blocked by it. They can't be regenerated.";
const WALL_OF_NETS: &str = "Defender (This creature can't attack.)\nAt end of combat, exile all creatures blocked by this creature.\nWhen this creature leaves the battlefield, return all cards exiled with it to the battlefield under their owners' control.";
const WHIP_VINE: &str = "Defender; reach (This creature can block creatures with flying.)\nYou may choose not to untap this creature during your untap step.\n{T}: Tap target creature with flying blocked by this creature. That creature doesn't untap during its controller's untap step for as long as this creature remains tapped.";
const KJELDORAN_FROSTBEAST: &str =
    "At end of combat, destroy all creatures blocking or blocked by this creature.";
const PLAGUE_WIGHT: &str =
    "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.";
const BANECLAW_MARAUDER: &str = "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.\nNightbound (If a player casts at least two spells during their own turn, it becomes day next turn.)";
const CROMAT: &str = "{W}{B}: Destroy target creature blocking or blocked by Cromat.\n{U}{R}: Cromat gains flying until end of turn.\n{B}{G}: Regenerate Cromat.\n{R}{W}: Cromat gets +1/+1 until end of turn.\n{G}{U}: Put Cromat on top of its owner's library.";
const ALMS_BEAST: &str = "Creatures blocking or blocked by this creature have lifelink.";
const LABYRINTH_OF_SKOPHOS: &str =
    "{T}: Add {C}.\n{4}, {T}: Remove target attacking or blocking creature from combat.";
const RECONNAISSANCE: &str = "{0}: Remove target attacking creature you control from combat and untap it. (If you activate during end of combat, the creature will untap after it deals combat damage.)";
const ROLLING_STONES: &str = "Wall creatures can attack as though they didn't have defender.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const MURDER: &str = "Destroy target creature.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const REGENERATE: &str = "Regenerate target creature. (The next time that creature would be destroyed this turn, instead tap it, remove it from combat, and heal all damage on it.)";

// ── Drivers ─────────────────────────────────────────────────────────────

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn assert_zone(runner: &GameRunner, ids: &[ObjectId], expected: Zone, why: &str) {
    for id in ids {
        assert_eq!(
            zone(runner, *id),
            expected,
            "{why}: {} ({id:?})",
            runner.state().objects[id].name
        );
    }
}

/// CR 500.5: mana empties between steps, so each payment is funded right
/// before the spell or ability that spends it.
fn seed(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    pool.clear();
    for color in mana {
        pool.add(ManaUnit::new(*color, ObjectId(0), false, vec![]));
    }
}

/// Pass priority (ordering simultaneous triggers in identity order) until
/// `done` holds.
fn pass_until(runner: &mut GameRunner, label: &str, done: impl Fn(&GameState) -> bool) {
    for _ in 0..96 {
        if done(runner.state()) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("{label}: unexpected prompt {other:?}"),
        }
    }
    panic!("{label}: condition never reached");
}

/// Resolve everything on the stack, stopping at the next empty-stack priority
/// window of the current step.
fn drain_stack(runner: &mut GameRunner, label: &str) {
    pass_until(runner, label, |state| {
        state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
}

/// P0 attacks P1 with `attackers`; P1 declares `blocks` (blocker, attacker).
/// Returns at P0's priority in the declare blockers step.
fn attack_and_block(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
) {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    pass_until(runner, "to declare blockers", |state| {
        matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    runner.declare_blockers(blocks).expect("declare blockers");
    pass_until(runner, "to blockers priority", |state| {
        state.phase == Phase::DeclareBlockers
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    let combat = runner.state().combat.as_ref().expect("combat is live");
    for (blocker, attacker) in blocks {
        assert!(
            combat.blocker_to_attacker[blocker].contains(attacker),
            "reach guard: {blocker:?} blocks {attacker:?}"
        );
    }
}

/// The objects the top stack entry targets, across its sub-ability chain.
fn top_targets(runner: &GameRunner) -> Vec<TargetRef> {
    let entry = runner.state().stack.back().expect("a stacked ability");
    let ability = match &entry.kind {
        StackEntryKind::ActivatedAbility { ability, .. } => ability,
        other => panic!("expected an activated ability, got {other:?}"),
    };
    std::iter::successors(Some(&**ability), |link| link.sub_ability.as_deref())
        .flat_map(|link| link.targets.iter().cloned())
        .collect()
}

/// Activate `source`'s ability `index`, choosing `target` at the target prompt
/// when one opens, and assert the stacked ability targets exactly `target`.
fn activate_targeting(runner: &mut GameRunner, source: ObjectId, index: usize, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation accepted");
    for _ in 0..4 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("target accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected activation prompt {other:?}"),
        }
    }
    assert_eq!(top_targets(runner), vec![TargetRef::Object(target)]);
}

/// Cast an instant from P0's hand at `target` (pool funded by `mana`) and
/// return once it is on the stack.
fn cast_at(runner: &mut GameRunner, spell: ObjectId, target: ObjectId, mana: &[ManaType]) {
    seed(runner, P0, mana);
    let _ = runner.cast(spell).target_object(target).commit();
    assert_eq!(
        zone(runner, spell),
        Zone::Stack,
        "reach guard: the spell is cast"
    );
}

fn labyrinth(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Labyrinth of Skophos", LABYRINTH_OF_SKOPHOS)
        .id()
}

/// Labyrinth's removal ability is its second line.
const LABYRINTH_REMOVE: usize = 1;

fn remove_from_combat_with_labyrinth(runner: &mut GameRunner, land: ObjectId, target: ObjectId) {
    seed(runner, P0, &[ManaType::Colorless; 4]);
    activate_targeting(runner, land, LABYRINTH_REMOVE, target);
    pass_until(runner, "labyrinth resolves", |state| {
        state.combat.as_ref().is_some_and(|combat| {
            !combat.blocker_to_attacker.contains_key(&target)
                && combat.attackers.iter().all(|a| a.object_id != target)
        })
    });
}

// ── Abu Ja'far ──────────────────────────────────────────────────────────

/// A1: Abu Ja'far blocks A and dies in combat; D blocks E elsewhere. Only A —
/// the creature Abu was blocking when it died — is destroyed (CR 608.2h: its
/// last known pairing). Base: every creature was destroyed.
#[test]
fn abu_jafar_dying_blocker_destroys_only_the_creature_it_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let e = scenario.add_creature(P0, "Attacker E", 2, 2).id();
    let g = scenario.add_creature(P0, "Bystander G", 1, 1).id();
    let abu = scenario
        .add_creature_from_oracle(P1, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let d = scenario.add_creature(P1, "Wall D", 0, 4).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[a, e], &[(abu, a), (d, e)]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(&runner, &[a], Zone::Graveyard, "Abu was blocking it");
    assert_zone(
        &runner,
        &[e, d, g, h],
        Zone::Battlefield,
        "not paired with Abu",
    );
}

/// A2: Abu Ja'far attacks and is blocked by B1 and B2; it dies to their
/// damage and both blockers are destroyed. Bystanders survive.
#[test]
fn abu_jafar_dying_attacker_destroys_every_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abu = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let g = scenario.add_creature(P0, "Bystander G", 1, 1).id();
    let b1 = scenario.add_creature(P1, "Blocker B1", 1, 1).id();
    let b2 = scenario.add_creature(P1, "Blocker B2", 1, 1).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[abu], &[(b1, abu), (b2, abu)]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(&runner, &[b1, b2], Zone::Graveyard, "blocking Abu");
    assert_zone(&runner, &[g, h], Zone::Battlefield, "not paired with Abu");
}

/// A3: "They can't be regenerated." — a regeneration shield on the blocked
/// creature does not save it (CR 701.19c).
#[test]
fn abu_jafar_destroyed_partner_cannot_be_regenerated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let g = scenario.add_creature(P0, "Bystander G", 1, 1).id();
    let regenerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Regenerate", true, REGENERATE)
        .id();
    let abu = scenario
        .add_creature_from_oracle(P1, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();

    seed(&mut runner, P0, &[ManaType::Green, ManaType::Green]);
    let _ = runner.cast(regenerate).target_object(a).resolve();
    assert_zone(
        &runner,
        &[regenerate],
        Zone::Graveyard,
        "reach guard: Regenerate resolved",
    );

    attack_and_block(&mut runner, &[a], &[(abu, a)]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(&runner, &[a], Zone::Graveyard, "can't be regenerated");
    assert_zone(&runner, &[g, h], Zone::Battlefield, "not paired with Abu");
}

/// A4: Abu Ja'far dies outside combat — it blocks nothing and is blocked by
/// nothing, so nothing else is destroyed. Base: every creature was destroyed.
#[test]
fn abu_jafar_dying_outside_combat_destroys_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Creature A", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let abu = scenario
        .add_creature_from_oracle(P1, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let h = scenario.add_creature(P1, "Creature H", 1, 1).id();
    let mut runner = scenario.build();

    cast_at(&mut runner, shock, abu, &[ManaType::Red]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.source_id),
        Some(abu),
        "reach guard: Abu's dies trigger is on the stack"
    );
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(&runner, &[a, h], Zone::Battlefield, "no block pairing");
}

/// A5: Abu Ja'far was blocked by B but was removed from combat before it died
/// (CR 506.4): B is not blocking it any more and survives. A block-history
/// reading would destroy B; base destroyed every creature.
#[test]
fn abu_jafar_removed_from_combat_before_dying_destroys_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abu = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let reconnaissance = scenario
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let b = scenario.add_creature(P1, "Blocker B", 1, 1).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[abu], &[(b, abu)]);
    activate_targeting(&mut runner, reconnaissance, 0, abu);
    pass_until(&mut runner, "Reconnaissance resolves", |state| {
        state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().all(|a| a.object_id != abu))
    });
    drain_stack(&mut runner, "Reconnaissance settled");

    cast_at(&mut runner, shock, abu, &[ManaType::Red]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(&runner, &[b, h], Zone::Battlefield, "Abu was out of combat");
}

/// A6: Abu Ja'far, blocked by B1 and B2, dies to Shock; in response to its
/// dies trigger Labyrinth of Skophos removes B1 from combat. B1 is no longer
/// blocking anything (CR 506.4) and survives; B2 is destroyed. Base destroyed
/// every creature.
#[test]
fn abu_jafar_partner_removed_from_combat_in_response_survives() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abu = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let land = labyrinth(&mut scenario);
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let b1 = scenario.add_creature(P1, "Blocker B1", 1, 1).id();
    let b2 = scenario.add_creature(P1, "Blocker B2", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[abu], &[(b1, abu), (b2, abu)]);
    cast_at(&mut runner, shock, abu, &[ManaType::Red]);
    pass_until(&mut runner, "Abu's dies trigger", |state| {
        state.objects[&abu].zone == Zone::Graveyard && !state.stack.is_empty()
    });
    remove_from_combat_with_labyrinth(&mut runner, land, b1);
    drain_stack(&mut runner, "resolve Abu's trigger");

    assert_zone(
        &runner,
        &[b1],
        Zone::Battlefield,
        "removed from combat first",
    );
    assert_zone(&runner, &[b2], Zone::Graveyard, "still blocking Abu's LKI");
}

// ── Wall of Nets ────────────────────────────────────────────────────────

struct WallBoard {
    runner: GameRunner,
    wall: ObjectId,
    a: ObjectId,
    e: ObjectId,
    d: ObjectId,
    g: ObjectId,
    h: ObjectId,
    land: ObjectId,
    unsummon: ObjectId,
    murder: ObjectId,
}

/// Wall of Nets blocks A; D blocks E; G and H stay home. Stops at P0's
/// priority in the declare blockers step.
fn wall_board() -> WallBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let e = scenario.add_creature(P0, "Attacker E", 2, 2).id();
    let g = scenario.add_creature(P0, "Bystander G", 1, 1).id();
    let land = labyrinth(&mut scenario);
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let wall = scenario
        .add_creature(P1, "Wall of Nets", 0, 7)
        .from_oracle_text_with_keywords(&["Defender"], WALL_OF_NETS)
        .id();
    let d = scenario.add_creature(P1, "Wall D", 0, 4).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();
    attack_and_block(&mut runner, &[a, e], &[(wall, a), (d, e)]);
    WallBoard {
        runner,
        wall,
        a,
        e,
        d,
        g,
        h,
        land,
        unsummon,
        murder,
    }
}

/// Advance to the end of combat step with the Wall's trigger on the stack.
fn to_wall_trigger(runner: &mut GameRunner, wall: ObjectId) {
    pass_until(runner, "Wall of Nets' end-of-combat trigger", |state| {
        state.phase == Phase::EndCombat && !state.stack.is_empty()
    });
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.source_id),
        Some(wall),
        "reach guard: the Wall's end-of-combat trigger is on the stack"
    );
}

/// W1 + W3: at end of combat Wall of Nets exiles only A, the creature it
/// blocked (CR 509.1g); E, D, the bystanders and the Wall stay. Base exiled
/// every creature. Then (W3, CR 607.2a) the Wall leaving returns A.
#[test]
fn wall_of_nets_exiles_only_the_creature_it_blocked_and_returns_it() {
    let WallBoard {
        mut runner,
        wall,
        a,
        e,
        d,
        g,
        h,
        murder,
        ..
    } = wall_board();
    to_wall_trigger(&mut runner, wall);
    drain_stack(&mut runner, "resolve the Wall's trigger");

    assert_zone(&runner, &[a], Zone::Exile, "blocked by the Wall");
    assert_zone(
        &runner,
        &[e, d, g, h, wall],
        Zone::Battlefield,
        "not blocked by the Wall",
    );

    pass_until(&mut runner, "postcombat main", |state| {
        state.phase == Phase::PostCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    cast_at(
        &mut runner,
        murder,
        wall,
        &[ManaType::Black, ManaType::Black, ManaType::Black],
    );
    drain_stack(&mut runner, "Murder and the Wall's leave trigger");
    assert_zone(
        &runner,
        &[wall],
        Zone::Graveyard,
        "reach guard: Murder resolved",
    );
    assert_zone(
        &runner,
        &[a],
        Zone::Battlefield,
        "returned by the Wall's leave trigger",
    );
}

/// W2: A is removed from combat before end of combat (CR 506.4), so it is no
/// longer blocked by the Wall and stays. Base exiled every creature.
#[test]
fn wall_of_nets_spares_a_creature_removed_from_combat() {
    let WallBoard {
        mut runner,
        wall,
        a,
        e,
        d,
        g,
        h,
        land,
        ..
    } = wall_board();
    remove_from_combat_with_labyrinth(&mut runner, land, a);
    drain_stack(&mut runner, "Labyrinth settled");
    to_wall_trigger(&mut runner, wall);
    drain_stack(&mut runner, "resolve the Wall's trigger");

    assert_zone(
        &runner,
        &[a, e, d, g, h, wall],
        Zone::Battlefield,
        "nothing blocked by the Wall",
    );
}

/// W4: the Wall is bounced in response to its end-of-combat trigger. CR 608.2h:
/// the trigger uses the Wall's last known information — A, which it was
/// blocking — and A stays exiled (its leave trigger already resolved). E, D and
/// the bystanders are untouched. Base: the live map no longer named the Wall,
/// but the unfiltered parse exiled every creature anyway.
#[test]
fn wall_of_nets_bounced_in_response_still_exiles_its_blocked_creature() {
    let WallBoard {
        mut runner,
        wall,
        a,
        e,
        d,
        g,
        h,
        unsummon,
        ..
    } = wall_board();
    to_wall_trigger(&mut runner, wall);
    cast_at(&mut runner, unsummon, wall, &[ManaType::Blue]);
    drain_stack(
        &mut runner,
        "Unsummon, the leave trigger, then the Wall's trigger",
    );

    assert_zone(
        &runner,
        &[wall],
        Zone::Hand,
        "reach guard: Unsummon resolved",
    );
    assert_zone(&runner, &[a], Zone::Exile, "last known pairing");
    assert_zone(
        &runner,
        &[e, d, g, h],
        Zone::Battlefield,
        "not blocked by the Wall",
    );
}

/// W7: the Wall is bounced in response and Labyrinth of Skophos then removes A
/// from combat. A is no longer blocked by anything (CR 506.4), so the Wall's
/// last known pairing does not reach it. Base exiled every creature.
#[test]
fn wall_of_nets_departed_pairing_requires_the_partner_still_attacking() {
    let WallBoard {
        mut runner,
        wall,
        a,
        e,
        d,
        g,
        h,
        land,
        unsummon,
        ..
    } = wall_board();
    to_wall_trigger(&mut runner, wall);
    cast_at(&mut runner, unsummon, wall, &[ManaType::Blue]);
    pass_until(&mut runner, "Unsummon resolves", |state| {
        state.objects[&wall].zone == Zone::Hand
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    remove_from_combat_with_labyrinth(&mut runner, land, a);
    drain_stack(&mut runner, "resolve the rest of the stack");

    assert_zone(
        &runner,
        &[a, e, d, g, h],
        Zone::Battlefield,
        "A left combat before resolution",
    );
}

/// W5 (ruling 2004-10-04): a Wall of Nets that dies in combat does not
/// trigger at end of combat, so the creature it blocked is not exiled.
#[test]
fn wall_of_nets_that_died_in_combat_exiles_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Huge Attacker", 8, 8).id();
    let wall = scenario
        .add_creature(P1, "Wall of Nets", 0, 7)
        .from_oracle_text_with_keywords(&["Defender"], WALL_OF_NETS)
        .id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[a], &[(wall, a)]);
    pass_until(&mut runner, "postcombat main", |state| {
        state.phase == Phase::PostCombatMain
    });

    assert_zone(
        &runner,
        &[wall],
        Zone::Graveyard,
        "reach guard: the Wall died in combat",
    );
    assert_zone(&runner, &[a], Zone::Battlefield, "no end-of-combat trigger");
}

/// W6: with Rolling Stones, Wall of Nets attacks and is blocked by B. B is
/// blocking the Wall, not blocked by it, so nothing is exiled — the passive
/// relation is one-directional. Base exiled every creature.
#[test]
fn wall_of_nets_attacking_does_not_exile_its_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Rolling Stones", ROLLING_STONES)
        .id();
    let wall = scenario
        .add_creature(P0, "Wall of Nets", 0, 7)
        .from_oracle_text_with_keywords(&["Defender"], WALL_OF_NETS)
        .with_subtypes(vec!["Wall"])
        .id();
    let b = scenario.add_creature(P1, "Blocker B", 1, 1).id();
    let g = scenario.add_creature(P1, "Bystander G", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[wall], &[(b, wall)]);
    to_wall_trigger(&mut runner, wall);
    drain_stack(&mut runner, "resolve the Wall's trigger");

    assert_zone(
        &runner,
        &[b, g, wall],
        Zone::Battlefield,
        "nothing blocked by the Wall",
    );
}

// ── Whip Vine ───────────────────────────────────────────────────────────

/// V1: "target creature with flying blocked by this creature" — the only
/// legal target is the flyer Whip Vine blocks, not the flyer another creature
/// blocks. Base offered every flyer.
#[test]
fn whip_vine_targets_only_the_flyer_it_blocks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let f1 = scenario
        .add_creature(P0, "Flyer F1", 1, 1)
        .flying()
        .vigilance()
        .id();
    let f2 = scenario
        .add_creature(P0, "Flyer F2", 1, 1)
        .flying()
        .vigilance()
        .id();
    let vine = scenario
        .add_creature(P1, "Whip Vine", 1, 4)
        .from_oracle_text_with_keywords(&["Defender", "Reach"], WHIP_VINE)
        .id();
    let reacher = scenario.add_creature(P1, "Reacher", 1, 4).reach().id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[f1, f2], &[(vine, f1), (reacher, f2)]);
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority in the declare blockers step"
    );
    let index = runner.state().objects[&vine]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Whip Vine has its tap ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: vine,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Whip Vine activates");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the single legal target is auto-selected, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(top_targets(&runner), vec![TargetRef::Object(f1)]);
    drain_stack(&mut runner, "resolve Whip Vine");
    assert!(runner.state().objects[&f1].tapped, "F1 is tapped");
    assert!(!runner.state().objects[&f2].tapped, "F2 is untouched");
}

// ── Already-parsed current-pairing consumers ────────────────────────────

fn frostbeast_board() -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Attacker A", 1, 5).id();
    let e = scenario.add_creature(P0, "Attacker E", 1, 1).id();
    let g = scenario.add_creature(P0, "Bystander G", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let beast = scenario
        .add_creature_from_oracle(P1, "Kjeldoran Frostbeast", 2, 4, KJELDORAN_FROSTBEAST)
        .id();
    let d = scenario.add_creature(P1, "Wall D", 0, 4).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();
    attack_and_block(&mut runner, &[a, e], &[(beast, a), (d, e)]);
    (runner, beast, a, e, d, g, h, murder)
}

/// K0 (preservation): at end of combat Kjeldoran Frostbeast destroys the
/// creature it blocked and nothing else.
#[test]
fn kjeldoran_frostbeast_destroys_only_its_partner() {
    let (mut runner, beast, a, e, d, g, h, _) = frostbeast_board();
    to_wall_trigger(&mut runner, beast);
    drain_stack(&mut runner, "resolve the Frostbeast's trigger");

    assert_zone(&runner, &[a], Zone::Graveyard, "blocked by the Frostbeast");
    assert_zone(
        &runner,
        &[beast, e, d, g, h],
        Zone::Battlefield,
        "not paired",
    );
}

/// K1: the Frostbeast is destroyed in response to its end-of-combat trigger.
/// CR 608.2h: the trigger reads its last known pairing and still destroys A.
/// Base read the pruned live map and destroyed nothing.
#[test]
fn kjeldoran_frostbeast_killed_in_response_still_destroys_its_partner() {
    let (mut runner, beast, a, e, d, g, h, murder) = frostbeast_board();
    to_wall_trigger(&mut runner, beast);
    cast_at(
        &mut runner,
        murder,
        beast,
        &[ManaType::Black, ManaType::Black, ManaType::Black],
    );
    drain_stack(&mut runner, "Murder, then the Frostbeast's trigger");

    assert_zone(
        &runner,
        &[beast],
        Zone::Graveyard,
        "reach guard: Murder resolved",
    );
    assert_zone(&runner, &[a], Zone::Graveyard, "last known pairing");
    assert_zone(&runner, &[e, d, g, h], Zone::Battlefield, "not paired");
}

/// S1: Cromat targets B, the creature blocking it, then puts itself on top of
/// its owner's library in response. CR 608.2b: the target check uses Cromat's
/// last known information, so B is still a legal target and is destroyed.
/// Base fizzled the ability.
#[test]
fn cromat_on_library_still_destroys_its_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cromat = scenario
        .add_creature(P0, "Cromat", 5, 5)
        .from_oracle_text_with_keywords(&["Regenerate"], CROMAT)
        .id();
    let b = scenario.add_creature(P1, "Blocker B", 1, 1).id();
    let h = scenario.add_creature(P1, "Bystander H", 1, 1).id();
    let mut runner = scenario.build();
    let abilities = runner.state().objects[&cromat].abilities.len();
    assert_eq!(abilities, 5, "reach guard: one ability per activated line");

    attack_and_block(&mut runner, &[cromat], &[(b, cromat)]);
    seed(&mut runner, P0, &[ManaType::White, ManaType::Black]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cromat,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Cromat's destroy ability activates");
    assert_eq!(top_targets(&runner), vec![TargetRef::Object(b)]);

    seed(&mut runner, P0, &[ManaType::Green, ManaType::Blue]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cromat,
            ability_index: 4,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Cromat's library ability activates");
    drain_stack(&mut runner, "Cromat leaves, then its destroy ability");

    assert_zone(
        &runner,
        &[cromat],
        Zone::Library,
        "reach guard: Cromat left first",
    );
    assert_zone(&runner, &[b], Zone::Graveyard, "blocking Cromat's LKI");
    assert_zone(&runner, &[h], Zone::Battlefield, "never targeted");
}

/// AB0 (preservation): Alms Beast's static grants lifelink only to the creature
/// it is blocking — A's damage gains P0 life, E's does not.
#[test]
fn alms_beast_grants_lifelink_only_to_its_partner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let e = scenario.add_creature(P0, "Attacker E", 3, 3).id();
    let beast = scenario
        .add_creature_from_oracle(P1, "Alms Beast", 6, 6, ALMS_BEAST)
        .id();
    let d = scenario.add_creature(P1, "Wall D", 0, 4).id();
    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    attack_and_block(&mut runner, &[a, e], &[(beast, a), (d, e)]);
    pass_until(&mut runner, "end of combat", |state| {
        state.phase == Phase::EndCombat
    });

    assert_eq!(
        runner.life(P0) - p0_before,
        2,
        "A (lifelink) dealt 2; E did not gain"
    );
    assert_eq!(runner.life(P1), p1_before, "every attacker was blocked");
}

// ── BlockingSource: Plague Wight and Baneclaw Marauder ──────────────────

fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

fn murder_in_hand(scenario: &mut GameScenario) -> ObjectId {
    let mut spell = scenario.add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER);
    spell.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    });
    spell.id()
}

/// P1: Plague Wight is removed in response to its becomes-blocked trigger, to a
/// public or a hidden zone. Ruling (2019-01-25) + CR 608.2h: "the creatures that
/// were blocking it before it left the battlefield get -1/-1." The other
/// attacker's blocker and the bystander are untouched. Base read the pruned
/// live map and shrank nobody.
#[test]
fn plague_wight_removed_in_response_still_shrinks_its_blockers() {
    for (name, oracle, mana, expected) in [
        ("Murder", MURDER, &[ManaType::Black; 3][..], Zone::Graveyard),
        ("Unsummon", UNSUMMON, &[ManaType::Blue][..], Zone::Hand),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wight = scenario
            .add_creature_from_oracle(P0, "Plague Wight", 2, 2, PLAGUE_WIGHT)
            .id();
        let other = scenario.add_creature(P0, "Other Attacker", 2, 2).id();
        let removal = if name == "Murder" {
            murder_in_hand(&mut scenario)
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P0, name, true, oracle)
                .id()
        };
        let b1 = scenario.add_creature(P1, "Blocker B1", 2, 4).id();
        let b2 = scenario.add_creature(P1, "Blocker B2", 3, 5).id();
        let d = scenario.add_creature(P1, "Other Blocker", 4, 6).id();
        let h = scenario.add_creature(P1, "Bystander H", 4, 6).id();
        let mut runner = scenario.build();

        attack_and_block(
            &mut runner,
            &[wight, other],
            &[(b1, wight), (b2, wight), (d, other)],
        );
        assert!(
            runner.state().stack.iter().any(|e| e.source_id == wight),
            "reach guard: the trigger is on the stack"
        );
        cast_at(&mut runner, removal, wight, mana);
        drain_stack(&mut runner, "the removal, then the Wight's trigger");

        assert_zone(&runner, &[wight], expected, "reach guard: removal resolved");
        assert_eq!(pt(&runner, b1), (Some(1), Some(3)), "{name}: B1 blocked it");
        assert_eq!(pt(&runner, b2), (Some(2), Some(4)), "{name}: B2 blocked it");
        assert_eq!(pt(&runner, d), (Some(4), Some(6)), "{name}: blocks another");
        assert_eq!(pt(&runner, h), (Some(4), Some(6)), "{name}: not blocking");
    }
}

/// P1b: Plague Wight is blinked in response, so its source is back on the
/// battlefield as a new object (CR 400.7) that nothing is blocking. Ruling
/// (2019-01-25) + CR 608.2h: the trigger still answers from the pairings the
/// departed incarnation had, not the live Wight's empty ones.
#[test]
fn plague_wight_blinked_in_response_still_shrinks_its_blockers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wight = scenario
        .add_creature_from_oracle(P0, "Plague Wight", 2, 2, PLAGUE_WIGHT)
        .id();
    let cloudshift = {
        let mut spell = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT);
        spell.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        spell.id()
    };
    let b1 = scenario.add_creature(P1, "Blocker B1", 2, 4).id();
    let h = scenario.add_creature(P1, "Bystander H", 4, 6).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[wight], &[(b1, wight)]);
    let incarnation_before = runner.state().objects[&wight].incarnation;
    cast_at(&mut runner, cloudshift, wight, &[ManaType::White]);
    drain_stack(&mut runner, "Cloudshift, then the Wight's trigger");

    assert_zone(
        &runner,
        &[cloudshift],
        Zone::Graveyard,
        "reach guard: Cloudshift resolved",
    );
    let returned = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Plague Wight")
        .expect("reach guard: the Wight returned");
    assert!(
        runner.state().objects[&returned].incarnation > incarnation_before,
        "CR 400.7: the returned Wight is a new object"
    );
    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == returned)),
        "reach guard: the new object is not attacking"
    );
    assert_eq!(pt(&runner, b1), (Some(1), Some(3)), "B1 blocked it");
    assert_eq!(pt(&runner, h), (Some(4), Some(6)), "not blocking");
}

/// P2: B1 is removed from combat and then the Wight is destroyed, both in
/// response. CR 506.4: B1 is no longer blocking anything, so last known
/// pairings answer only for B2.
#[test]
fn plague_wight_blocker_removed_from_combat_in_response_is_spared() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wight = scenario
        .add_creature_from_oracle(P0, "Plague Wight", 2, 2, PLAGUE_WIGHT)
        .id();
    let land = labyrinth(&mut scenario);
    let murder = murder_in_hand(&mut scenario);
    let b1 = scenario.add_creature(P1, "Blocker B1", 2, 4).id();
    let b2 = scenario.add_creature(P1, "Blocker B2", 3, 5).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[wight], &[(b1, wight), (b2, wight)]);
    remove_from_combat_with_labyrinth(&mut runner, land, b1);
    cast_at(&mut runner, murder, wight, &[ManaType::Black; 3]);
    drain_stack(&mut runner, "Murder, then the Wight's trigger");

    assert_zone(&runner, &[wight], Zone::Graveyard, "reach guard: Murder");
    assert_eq!(pt(&runner, b1), (Some(2), Some(4)), "out of combat");
    assert_eq!(pt(&runner, b2), (Some(2), Some(4)), "still blocking");
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// B1: a creature blocking the Marauder dies in combat, and so does one
/// blocking a different attacker. CR 603.10a: the dying creature's own recorded
/// pairing fires the trigger once, and its controller loses 1 life. Base never
/// fired it (the snapshot arm always rejected the subject).
#[test]
fn baneclaw_marauder_trigger_reads_only_its_own_blockers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marauder = scenario
        .add_creature_from_oracle(P0, "Baneclaw Marauder", 4, 4, BANECLAW_MARAUDER)
        .id();
    let other = scenario.add_creature(P0, "Other Attacker", 4, 4).id();
    let b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let d = scenario.add_creature(P1, "Other Blocker", 1, 1).id();
    let mut runner = scenario.build();

    attack_and_block(
        &mut runner,
        &[marauder, other],
        &[(b, marauder), (d, other)],
    );
    // The becomes-blocked trigger shrinks only the Marauder's blocker.
    drain_stack(&mut runner, "resolve the becomes-blocked trigger");
    assert_eq!(pt(&runner, b), (Some(1), Some(1)), "B blocks the Marauder");
    assert_eq!(pt(&runner, d), (Some(1), Some(1)), "D blocks another");
    assert_zone(&runner, &[b], Zone::Battlefield, "B survives the -1/-1");
    pass_until(&mut runner, "combat damage", |state| {
        state.objects[&b].zone == Zone::Graveyard
    });
    assert_eq!(
        runner.state().phase,
        Phase::CombatDamage,
        "B died to combat damage"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|e| e.source_id == marauder)
            .count(),
        1,
        "exactly one of the two deaths triggers"
    );
    drain_stack(&mut runner, "resolve the Marauder's trigger");

    assert_zone(&runner, &[b, d], Zone::Graveyard, "reach guard: both died");
    assert_eq!(life(&runner, P1), 19, "the blocker's controller loses 1");
    assert_eq!(life(&runner, P0), 20, "not the Marauder's controller");
}

/// B2: the Marauder and its blocker die to the same combat damage. CR 603.10a:
/// the Marauder looks back and sees the creature blocking it die, whichever of
/// the two the batch moves first, so both creation orders are exercised.
#[test]
fn baneclaw_marauder_dying_with_its_blocker_still_triggers() {
    for marauder_first in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let early = marauder_first.then(|| {
            scenario
                .add_creature_from_oracle(P0, "Baneclaw Marauder", 2, 2, BANECLAW_MARAUDER)
                .id()
        });
        let b = scenario.add_creature(P1, "Blocker B", 3, 3).id();
        let marauder = early.unwrap_or_else(|| {
            scenario
                .add_creature_from_oracle(P0, "Baneclaw Marauder", 2, 2, BANECLAW_MARAUDER)
                .id()
        });
        let mut runner = scenario.build();

        attack_and_block(&mut runner, &[marauder], &[(b, marauder)]);
        drain_stack(&mut runner, "resolve the becomes-blocked trigger");
        assert_eq!(pt(&runner, b), (Some(2), Some(2)), "B is 2/2 after -1/-1");
        pass_until(&mut runner, "combat damage", |state| {
            state.objects[&b].zone == Zone::Graveyard
        });
        drain_stack(&mut runner, "resolve the Marauder's trigger");

        assert_zone(&runner, &[b, marauder], Zone::Graveyard, "both died");
        assert_eq!(life(&runner, P1), 19, "marauder_first={marauder_first}");
    }
}

/// B3: the blocker is removed from combat before it dies. CR 506.4: it is no
/// longer blocking the Marauder when it dies, so nothing triggers.
#[test]
fn baneclaw_marauder_ignores_a_former_blocker_dying() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marauder = scenario
        .add_creature_from_oracle(P0, "Baneclaw Marauder", 4, 4, BANECLAW_MARAUDER)
        .id();
    let land = labyrinth(&mut scenario);
    let murder = murder_in_hand(&mut scenario);
    let b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[marauder], &[(b, marauder)]);
    drain_stack(&mut runner, "resolve the becomes-blocked trigger");
    assert_eq!(pt(&runner, b), (Some(1), Some(1)), "B is 1/1 after -1/-1");
    remove_from_combat_with_labyrinth(&mut runner, land, b);
    drain_stack(&mut runner, "Labyrinth settled");
    cast_at(&mut runner, murder, b, &[ManaType::Black; 3]);
    drain_stack(&mut runner, "Murder resolves");

    assert_zone(&runner, &[b], Zone::Graveyard, "reach guard: Murder");
    assert_eq!(life(&runner, P1), 20, "it was not blocking when it died");
}

// ── Trigger ordering (CR 603.3b) ────────────────────────────────────────

const REVEILLARK: &str = "Flying\nWhen this creature leaves the battlefield, return up to two target creature cards with power 2 or less from your graveyard to the battlefield.";
const DRIVER_OF_THE_DEAD: &str = "When this creature dies, return target creature card with mana value 2 or less from your graveyard to the battlefield.";
const CATHEDRAL_MEMBRANE: &str = "({W/P} can be paid with either {W} or 2 life.)\nDefender\nWhen this creature dies during combat, it deals 6 damage to each creature it blocked this combat.";

/// CR 603.3b: the two dies triggers of `sources` must reach their
/// `controller` as an ordering choice. Orders them so `first` resolves first
/// (CR 405.3: index 0 is placed first and resolves last).
fn order_so_first_resolves(
    runner: &mut GameRunner,
    controller: PlayerId,
    sources: [ObjectId; 2],
    first: ObjectId,
) {
    pass_until(runner, "both sources die", |state| {
        matches!(state.waiting_for, WaitingFor::OrderTriggers { .. })
            || (sources
                .iter()
                .all(|id| state.objects[id].zone == Zone::Graveyard)
                && state.stack.len() >= 2)
    });
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "CR 603.3b: the two dies triggers must be ordered by their controller, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, controller,
        "the sources' controller orders the triggers"
    );
    let mut ordered: Vec<_> = triggers.iter().map(|t| t.source_id).collect();
    ordered.sort();
    let mut expected = sources.to_vec();
    expected.sort();
    assert_eq!(ordered, expected, "one trigger from each source");

    let last = triggers
        .iter()
        .position(|t| t.source_id != first)
        .expect("the other trigger");
    runner
        .act(GameAction::OrderTriggers {
            order: vec![last, 1 - last],
        })
        .expect("order the dies triggers");
}

/// Resolve the stack. A returning trigger's controller returns `card`
/// whenever it is a legal target, and otherwise makes the least choice the
/// trigger allows.
fn resolve_returning(runner: &mut GameRunner, card: ObjectId) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let card_target = TargetRef::Object(card);
                let targets = if target_slots
                    .iter()
                    .any(|slot| slot.legal_targets.contains(&card_target))
                {
                    vec![card_target]
                } else if target_slots.iter().all(|slot| slot.optional) {
                    vec![]
                } else {
                    target_slots
                        .iter()
                        .find_map(|slot| slot.legal_targets.first().cloned())
                        .into_iter()
                        .collect()
                };
                runner
                    .act(GameAction::SelectTargets { targets })
                    .expect("choose the returning trigger's targets");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("resolving the dies triggers: unexpected prompt {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// O1: two Abu Ja'fars attack; Reveillark blocks one and Grizzly Bears the
/// other, and both Abu Ja'fars die together. Each dies trigger destroys a
/// different creature (its own last known pairing), so the order is
/// observable: if the Bears trigger resolves first, Reveillark's leaves
/// trigger (CR 603.6c) can return the Bears; if Reveillark's resolves first,
/// the Bears aren't in the graveyard yet. Returns the Bears' zone.
fn two_abu_jafars_dying_together(bears_first: bool) -> Zone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abu_r = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let abu_b = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let reveillark = scenario
        .add_creature_from_oracle(P1, "Reveillark", 4, 3, REVEILLARK)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    attack_and_block(
        &mut runner,
        &[abu_r, abu_b],
        &[(reveillark, abu_r), (bears, abu_b)],
    );
    let first = if bears_first { abu_b } else { abu_r };
    order_so_first_resolves(&mut runner, P0, [abu_r, abu_b], first);
    resolve_returning(&mut runner, bears);

    assert_zone(
        &runner,
        &[reveillark],
        Zone::Graveyard,
        "blocking an Abu Ja'far",
    );
    zone(&runner, bears)
}

#[test]
fn two_abu_jafars_dying_together_let_their_controller_order_the_triggers() {
    assert_eq!(
        two_abu_jafars_dying_together(true),
        Zone::Battlefield,
        "Bears destroyed first, then Reveillark's leaves trigger returns them"
    );
    assert_eq!(
        two_abu_jafars_dying_together(false),
        Zone::Graveyard,
        "Reveillark's trigger resolved before the Bears were destroyed"
    );
}

/// O2: Driver of the Dead and a 1/1 deathtouch creature attack; a Cathedral
/// Membrane blocks each, and both Membranes die in combat. Each dies trigger
/// deals 6 damage to the creature that Membrane blocked, so the same order
/// question arises as in O1, through damage and state-based actions
/// (CR 704.5g): only if the 1/1 dies first can Driver of the Dead's dies
/// trigger return it. Returns the deathtouch creature's zone.
fn two_cathedral_membranes_dying_together(stinger_first: bool) -> Zone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let driver = scenario
        .add_creature_from_oracle(P0, "Driver of the Dead", 3, 2, DRIVER_OF_THE_DEAD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id();
    let stinger = scenario
        .add_creature_from_oracle(P0, "Deathtouch Stinger", 1, 1, "Deathtouch")
        .id();
    let membrane_d = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let membrane_s = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .id();
    let mut runner = scenario.build();

    attack_and_block(
        &mut runner,
        &[driver, stinger],
        &[(membrane_d, driver), (membrane_s, stinger)],
    );
    let first = if stinger_first {
        membrane_s
    } else {
        membrane_d
    };
    order_so_first_resolves(&mut runner, P1, [membrane_d, membrane_s], first);
    resolve_returning(&mut runner, stinger);
    zone(&runner, stinger)
}

#[test]
fn two_cathedral_membranes_dying_together_let_their_controller_order_the_triggers() {
    assert_eq!(
        two_cathedral_membranes_dying_together(true),
        Zone::Battlefield,
        "the 1/1 died first, then Driver of the Dead's dies trigger returned it"
    );
    assert_eq!(
        two_cathedral_membranes_dying_together(false),
        Zone::Graveyard,
        "Driver of the Dead's trigger resolved before the 1/1 died"
    );
}
