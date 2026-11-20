//! CR 115.9b / CR 115.9c: "counter target spell or ability that targets …"
//! keeps its controller and "that targets" qualifiers (#11211), and a target
//! that left its zone no longer satisfies "targets X" (#11218).

use std::collections::BTreeSet;

use engine::ai_support::legal_actions_full;
use engine::game::ability_utils::declared_targets_in_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NOT_OF_THIS_WORLD: &str = "Counter target spell or ability that targets a permanent you control.\nThis spell costs {7} less to cast if it targets a spell or ability that targets a creature you control with power 7 or greater.";
const TEFERIS_RESPONSE: &str = "Counter target spell or ability an opponent controls that targets a land you control. If a permanent's ability is countered this way, destroy that permanent.\nDraw two cards.";
const SIREN_STORMTAMER: &str = "Flying\n{U}, Sacrifice this creature: Counter target spell or ability that targets you or a creature you control.";
const DIPLOMATIC_ESCORT: &str =
    "{U}, {T}, Discard a card: Counter target spell or ability that targets a creature.";
const NIMBLE_OBSTRUCTIONIST: &str = "Flash\nFlying\nCycling {2}{U} ({2}{U}, Discard this card: Draw a card.)\nWhen you cycle this card, counter target activated or triggered ability you don't control.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const KEEP_SAFE: &str = "Counter target spell that targets a permanent you control.\nDraw a card.";
const MURDER: &str = "Destroy target creature.";
const STONE_RAIN: &str = "Destroy target land.";
const ARC_TRAIL: &str = "Arc Trail deals 2 damage to any target and 1 damage to any other target.";
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
const CUOMBAJJ_WITCHES: &str = "{T}: This creature deals 1 damage to any target and 1 damage to any target of an opponent's choice.";
const RISHADAN_PORT: &str = "{T}: Add {C}.\n{1}, {T}: Tap target land.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";
const SKY_SKIFF: &str = "Flying\nCrew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)";

fn mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn bears(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature(player, "Grizzly Bears", 2, 2).id()
}

fn pyromancer(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_creature_from_oracle(player, "Prodigal Pyromancer", 1, 1, PRODIGAL_PYROMANCER)
        .id()
}

fn free_spell(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    instant: bool,
    text: &str,
) -> ObjectId {
    s.add_spell_to_hand_from_oracle(player, name, instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Hands priority to `player`, making them the active player on their turn.
fn give_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn pass(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass priority");
}

fn kill(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Graveyard, &mut events);
}

fn on_stack(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().stack.iter().any(|e| e.id == id)
}

fn top_id(runner: &GameRunner) -> ObjectId {
    runner.state().stack.back().expect("stack entry").id
}

/// Pushes a new entry and returns its id.
fn pushed(runner: &GameRunner, before: usize, label: &str) -> ObjectId {
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "{label} was not put on the stack: {:?}",
        runner.state().waiting_for
    );
    top_id(runner)
}

fn cast(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId]) -> ObjectId {
    let before = runner.state().stack.len();
    runner.cast(spell).target_objects(targets).commit();
    pushed(runner, before, "spell")
}

fn ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&source)
        .into_iter()
        .flatten()
        .find_map(|a| match a {
            GameAction::ActivateAbility { ability_index, .. } => Some(*ability_index),
            _ => None,
        })
        .expect("activatable ability")
}

fn start_activation(runner: &mut GameRunner, source: ObjectId) {
    let ability_index = ability_index(runner, source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate");
}

fn start_cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
}

/// Answers every target prompt the engine raises, in order.
fn choose_targets(runner: &mut GameRunner, targets: &[TargetRef]) {
    for target in targets {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
            ),
            "expected a target prompt, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target.clone()),
            })
            .expect("choose target");
    }
}

fn activate(runner: &mut GameRunner, source: ObjectId, targets: &[TargetRef]) -> ObjectId {
    let before = runner.state().stack.len();
    start_activation(runner, source);
    choose_targets(runner, targets);
    pushed(runner, before, "ability")
}

fn offered(runner: &GameRunner) -> BTreeSet<ObjectId> {
    let slots = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. }
        | WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots,
        other => panic!("expected a target prompt, got {other:?}"),
    };
    slots
        .iter()
        .flat_map(|slot| slot.legal_targets.iter())
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            TargetRef::Player(pid) => panic!("a stack-object filter offered player {pid:?}"),
        })
        .collect()
}

fn entry_targets(runner: &GameRunner, id: ObjectId) -> Vec<TargetRef> {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == id)
        .expect("stack entry");
    declared_targets_in_chain(entry.ability().expect("entry ability"))
}

fn pass_until_gone(runner: &mut GameRunner, id: ObjectId) {
    for _ in 0..6 {
        if !on_stack(runner, id) {
            return;
        }
        pass(runner);
    }
    panic!("{id:?} never left the stack");
}

fn set(ids: &[ObjectId]) -> BTreeSet<ObjectId> {
    ids.iter().copied().collect()
}

/// Not of This World offered only entries with a target that is a permanent P0
/// controls; a player-only or opponent-permanent victim is refused.
#[test]
fn not_of_this_world_legal_set_reads_that_targets_clause() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mine = bears(&mut s, P0);
    let theirs = bears(&mut s, P1);
    let pyro = pyromancer(&mut s, P1);
    let pyro2 = pyromancer(&mut s, P1);
    let witches = s
        .add_creature_from_oracle(P1, "Cuombajj Witches", 1, 3, CUOMBAJJ_WITCHES)
        .id();
    let arc = free_spell(&mut s, P1, "Arc Trail", false, ARC_TRAIL);
    let murder_mine = free_spell(&mut s, P1, "Murder", true, MURDER);
    let murder_theirs = free_spell(&mut s, P1, "Murder", true, MURDER);
    let notw = free_spell(&mut s, P0, "Not of This World", true, NOT_OF_THIS_WORLD);
    let mut r = s.build();
    give_turn(&mut r, P1);

    let s_arc = cast(&mut r, arc, &[theirs, mine]);
    let s_mine = cast(&mut r, murder_mine, &[mine]);
    let s_theirs = cast(&mut r, murder_theirs, &[theirs]);
    let a_mine = activate(&mut r, pyro, &[TargetRef::Object(mine)]);
    let a_p0 = activate(&mut r, pyro2, &[TargetRef::Player(P0)]);
    let a_multi = activate(
        &mut r,
        witches,
        &[TargetRef::Object(theirs), TargetRef::Object(mine)],
    );
    assert!(entry_targets(&r, a_multi).contains(&TargetRef::Object(mine)));
    assert!(entry_targets(&r, s_arc).contains(&TargetRef::Object(mine)));
    pass(&mut r);

    start_cast(&mut r, notw);
    let legal = offered(&r);
    assert_eq!(legal, set(&[s_arc, s_mine, a_mine, a_multi]));
    assert!(!legal.contains(&s_theirs) && !legal.contains(&a_p0));
}

/// CR 608.2b: Not of This World counters a matching ability, and does not
/// resolve once the targeted permanent is no longer controlled by P0.
#[test]
fn not_of_this_world_rechecks_qualifier_at_resolution() {
    for control_changes in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mine = bears(&mut s, P0);
        let mine2 = bears(&mut s, P0);
        let pyro = pyromancer(&mut s, P1);
        let pyro2 = pyromancer(&mut s, P1);
        let notw = free_spell(&mut s, P0, "Not of This World", true, NOT_OF_THIS_WORLD);
        let mut r = s.build();
        give_turn(&mut r, P1);
        let victim = activate(&mut r, pyro, &[TargetRef::Object(mine)]);
        activate(&mut r, pyro2, &[TargetRef::Object(mine2)]);
        pass(&mut r);

        start_cast(&mut r, notw);
        choose_targets(&mut r, &[TargetRef::Object(victim)]);
        if control_changes {
            let bears = r.state_mut().objects.get_mut(&mine).unwrap();
            bears.base_controller = Some(P1);
            bears.controller = P1;
        }
        pass_until_gone(&mut r, notw);

        assert_eq!(r.state().objects[&notw].zone, Zone::Graveyard);
        assert_eq!(
            on_stack(&r, victim),
            control_changes,
            "control_changes={control_changes}"
        );
        if !control_changes {
            assert_eq!(
                r.state().objects[&mine].damage_marked,
                0,
                "countered, not resolved"
            );
        }
    }
}

/// Teferi's Response: an opponent's entry that targets a land P0 controls; the
/// rider destroys the countered ability's source.
#[test]
fn teferis_response_requires_opponent_entry_targeting_my_land() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let my_bears = bears(&mut s, P0);
    let port = s
        .add_land_from_oracle(P1, "Rishadan Port", RISHADAN_PORT)
        .id();
    let my_port = s
        .add_land_from_oracle(P0, "Rishadan Port", RISHADAN_PORT)
        .id();
    let pyro = pyromancer(&mut s, P1);
    let rain = free_spell(&mut s, P1, "Stone Rain", false, STONE_RAIN);
    let teferi = free_spell(&mut s, P0, "Teferi's Response", true, TEFERIS_RESPONSE);
    s.with_library_top(P0, &["Grizzly Bears", "Grizzly Bears"]);
    s.with_mana_pool(P1, mana(1, ManaType::Colorless));
    s.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut r = s.build();
    give_turn(&mut r, P1);

    let s_rain = cast(&mut r, rain, &[island]);
    let p_port = activate(&mut r, port, &[TargetRef::Object(island)]);
    let a_bears = activate(&mut r, pyro, &[TargetRef::Object(my_bears)]);
    pass(&mut r);
    let own_port = activate(&mut r, my_port, &[TargetRef::Object(island)]);

    start_cast(&mut r, teferi);
    let legal = offered(&r);
    assert_eq!(legal, set(&[s_rain, p_port]));
    assert!(!legal.contains(&own_port) && !legal.contains(&a_bears));

    choose_targets(&mut r, &[TargetRef::Object(p_port)]);
    pass_until_gone(&mut r, teferi);
    assert!(!on_stack(&r, p_port));
    assert_eq!(r.state().objects[&port].zone, Zone::Graveyard);
}

/// Siren Stormtamer: entries targeting P0 or a creature P0 controls.
#[test]
fn siren_stormtamer_reads_player_or_creature_qualifier() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let island = s.add_basic_land(P0, ManaColor::Blue);
    let mine = bears(&mut s, P0);
    let theirs = bears(&mut s, P1);
    let pyros: Vec<_> = (0..4).map(|_| pyromancer(&mut s, P1)).collect();
    let port = s
        .add_land_from_oracle(P1, "Rishadan Port", RISHADAN_PORT)
        .id();
    let siren = s
        .add_creature_from_oracle(P0, "Siren Stormtamer", 1, 1, SIREN_STORMTAMER)
        .id();
    s.with_mana_pool(P1, mana(1, ManaType::Colorless));
    s.with_mana_pool(P0, mana(1, ManaType::Blue));
    let mut r = s.build();
    give_turn(&mut r, P1);

    let a_p0 = activate(&mut r, pyros[0], &[TargetRef::Player(P0)]);
    let a_mine = activate(&mut r, pyros[1], &[TargetRef::Object(mine)]);
    let a_p1 = activate(&mut r, pyros[2], &[TargetRef::Player(P1)]);
    let a_theirs = activate(&mut r, pyros[3], &[TargetRef::Object(theirs)]);
    let p_land = activate(&mut r, port, &[TargetRef::Object(island)]);
    pass(&mut r);

    start_activation(&mut r, siren);
    let legal = offered(&r);
    assert_eq!(legal, set(&[a_p0, a_mine]));
    assert!(![a_p1, a_theirs, p_land].iter().any(|id| legal.contains(id)));
}

/// CR 109.5 + CR 608.2b: a stolen Siren Stormtamer sacrificed to its own cost
/// reads "you" as its activator, not its owner, when the Bolt it counters is
/// rechecked at resolution; a Bolt targeting only the owner is never offered.
#[test]
fn stolen_siren_stormtamer_reads_you_as_activator() {
    for victim_is_player in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mine = bears(&mut s, P0);
        let siren = s
            .add_creature_from_oracle(P1, "Siren Stormtamer", 1, 1, SIREN_STORMTAMER)
            .id();
        let treason = free_spell(&mut s, P0, "Act of Treason", false, ACT_OF_TREASON);
        let bolts: Vec<_> = (0..3)
            .map(|_| free_spell(&mut s, P1, "Lightning Bolt", true, LIGHTNING_BOLT))
            .collect();
        s.with_mana_pool(P0, mana(1, ManaType::Blue));
        let mut r = s.build();
        give_turn(&mut r, P0);
        let treason_entry = cast(&mut r, treason, &[siren]);
        pass_until_gone(&mut r, treason_entry);
        assert_eq!(r.state().objects[&siren].controller, P0);

        give_turn(&mut r, P1);
        let before = r.state().stack.len();
        r.cast(bolts[0]).target_player(P0).commit();
        let at_p0 = pushed(&r, before, "bolt at P0");
        let at_mine = cast(&mut r, bolts[1], &[mine]);
        let before = r.state().stack.len();
        r.cast(bolts[2]).target_player(P1).commit();
        let at_owner = pushed(&r, before, "bolt at P1");
        pass(&mut r);

        start_activation(&mut r, siren);
        assert_eq!(offered(&r), set(&[at_p0, at_mine]));
        let (victim, survivor) = if victim_is_player {
            (at_p0, at_mine)
        } else {
            (at_mine, at_p0)
        };
        choose_targets(&mut r, &[TargetRef::Object(victim)]);
        let counter = top_id(&r);
        assert_eq!(r.state().objects[&siren].zone, Zone::Graveyard);
        assert_eq!(r.state().objects[&siren].controller, P1);
        pass_until_gone(&mut r, counter);

        assert_eq!(
            r.state().objects[&victim].zone,
            Zone::Graveyard,
            "victim_is_player={victim_is_player}"
        );
        assert!(on_stack(&r, survivor) && on_stack(&r, at_owner));
    }
}

/// Diplomatic Escort: any creature target, including an Equip's; Crew has no
/// target.
#[test]
fn diplomatic_escort_counts_equip_target_not_crew() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let theirs = bears(&mut s, P1);
    let crewer = bears(&mut s, P1);
    let pyro = pyromancer(&mut s, P1);
    let pyro2 = pyromancer(&mut s, P1);
    let my_pyro = pyromancer(&mut s, P0);
    let sword = s
        .add_artifact_from_oracle(P1, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let skiff = s
        .add_creature(P1, "Sky Skiff", 2, 3)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(SKY_SKIFF)
        .id();
    let escort = s
        .add_creature_from_oracle(P0, "Diplomatic Escort", 1, 1, DIPLOMATIC_ESCORT)
        .id();
    s.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    s.with_mana_pool(P1, mana(1, ManaType::Colorless));
    s.with_mana_pool(P0, mana(1, ManaType::Blue));
    let mut r = s.build();
    give_turn(&mut r, P1);

    let k_equip = equip(&mut r, sword, theirs);
    let k_crew = crew(&mut r, skiff, crewer);
    let a_p0 = activate(&mut r, pyro, &[TargetRef::Player(P0)]);
    let a_theirs = activate(&mut r, pyro2, &[TargetRef::Object(theirs)]);
    pass(&mut r);
    let a_own = activate(&mut r, my_pyro, &[TargetRef::Object(theirs)]);

    start_activation(&mut r, escort);
    let legal = offered(&r);
    assert_eq!(legal, set(&[k_equip, a_theirs, a_own]));
    assert!(!legal.contains(&k_crew) && !legal.contains(&a_p0));
}

fn equip(runner: &mut GameRunner, equipment: ObjectId, target: ObjectId) -> ObjectId {
    let before = runner.state().stack.len();
    let action = GameAction::Equip {
        equipment_id: equipment,
        target_id: target,
    };
    runner.act(action.clone()).expect("announce equip");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::EquipTarget { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    runner.act(action).expect("choose equip target");
    pushed(runner, before, "equip")
}

fn crew(runner: &mut GameRunner, vehicle: ObjectId, creature: ObjectId) -> ObjectId {
    let before = runner.state().stack.len();
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("announce crew");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![creature],
        })
        .expect("pay crew");
    pushed(runner, before, "crew")
}

/// CR 115.9b + Siren Stormtamer ruling: a victim whose targeted creature left
/// the battlefield no longer targets it, so only the live sibling is legal and
/// is auto-assigned.
#[test]
fn zone_left_target_is_ignored_at_announcement() {
    for keep_safe in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mine = bears(&mut s, P0);
        let mine2 = bears(&mut s, P0);
        let pyro = pyromancer(&mut s, P1);
        let pyro2 = pyromancer(&mut s, P1);
        let murder = free_spell(&mut s, P1, "Murder", true, MURDER);
        let murder2 = free_spell(&mut s, P1, "Murder", true, MURDER);
        let siren = s
            .add_creature_from_oracle(P0, "Siren Stormtamer", 1, 1, SIREN_STORMTAMER)
            .id();
        let keep = free_spell(&mut s, P0, "Keep Safe", true, KEEP_SAFE);
        s.with_mana_pool(P0, mana(1, ManaType::Blue));
        let mut r = s.build();
        give_turn(&mut r, P1);
        let (dead_victim, live_victim) = if keep_safe {
            (
                cast(&mut r, murder, &[mine]),
                cast(&mut r, murder2, &[mine2]),
            )
        } else {
            (
                activate(&mut r, pyro, &[TargetRef::Object(mine)]),
                activate(&mut r, pyro2, &[TargetRef::Object(mine2)]),
            )
        };
        pass(&mut r);
        kill(&mut r, mine);

        let before = r.state().stack.len();
        if keep_safe {
            start_cast(&mut r, keep);
        } else {
            start_activation(&mut r, siren);
        }
        let responder = pushed(&r, before, "responder");
        assert_eq!(
            entry_targets(&r, responder),
            vec![TargetRef::Object(live_victim)],
            "keep_safe={keep_safe}"
        );
        assert_ne!(live_victim, dead_victim);
    }
}

/// CR 115.9b + CR 115.1e: an Equip whose creature left the battlefield no
/// longer targets a creature.
#[test]
fn zone_left_equip_target_is_ignored() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let theirs = bears(&mut s, P1);
    let other = bears(&mut s, P1);
    let pyro = pyromancer(&mut s, P1);
    let pyro2 = pyromancer(&mut s, P1);
    let sword = s
        .add_artifact_from_oracle(P1, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let escort = s
        .add_creature_from_oracle(P0, "Diplomatic Escort", 1, 1, DIPLOMATIC_ESCORT)
        .id();
    s.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    s.with_mana_pool(P1, mana(1, ManaType::Colorless));
    s.with_mana_pool(P0, mana(1, ManaType::Blue));
    let mut r = s.build();
    give_turn(&mut r, P1);

    let k_equip = equip(&mut r, sword, theirs);
    let live = activate(&mut r, pyro, &[TargetRef::Object(other)]);
    let live2 = activate(&mut r, pyro2, &[TargetRef::Object(other)]);
    pass(&mut r);
    kill(&mut r, theirs);

    start_activation(&mut r, escort);
    let legal = offered(&r);
    assert_eq!(legal, set(&[live, live2]));
    assert!(!legal.contains(&k_equip));
}

/// CR 608.2b + CR 115.9b: Not of This World does not resolve once its
/// target's targeted creature has left the battlefield.
#[test]
fn zone_left_target_is_ignored_at_resolution() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mine = bears(&mut s, P0);
    let mine2 = bears(&mut s, P0);
    let pyro = pyromancer(&mut s, P1);
    let pyro2 = pyromancer(&mut s, P1);
    let notw = free_spell(&mut s, P0, "Not of This World", true, NOT_OF_THIS_WORLD);
    let mut r = s.build();
    give_turn(&mut r, P1);
    let victim = activate(&mut r, pyro, &[TargetRef::Object(mine)]);
    activate(&mut r, pyro2, &[TargetRef::Object(mine2)]);
    pass(&mut r);

    start_cast(&mut r, notw);
    choose_targets(&mut r, &[TargetRef::Object(victim)]);
    kill(&mut r, mine);
    pass_until_gone(&mut r, notw);

    assert_eq!(r.state().objects[&notw].zone, Zone::Graveyard);
    assert!(on_stack(&r, victim));
}

/// Nimble Obstructionist: activated or triggered abilities P0 doesn't control,
/// keyword actions included.
#[test]
fn nimble_obstructionist_refuses_own_ability() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let theirs = bears(&mut s, P1);
    let crewer = bears(&mut s, P1);
    let pyro = pyromancer(&mut s, P1);
    let my_pyro = pyromancer(&mut s, P0);
    let sword = s
        .add_artifact_from_oracle(P1, "Bonesplitter", BONESPLITTER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let skiff = s
        .add_creature(P1, "Sky Skiff", 2, 3)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(SKY_SKIFF)
        .id();
    let nimble = s
        .add_creature_to_hand_from_oracle(P0, "Nimble Obstructionist", 3, 1, NIMBLE_OBSTRUCTIONIST)
        .id();
    s.with_mana_pool(P1, mana(1, ManaType::Colorless));
    s.with_mana_pool(P0, mana(3, ManaType::Blue));
    let mut r = s.build();
    give_turn(&mut r, P1);

    let k_equip = equip(&mut r, sword, theirs);
    let k_crew = crew(&mut r, skiff, crewer);
    let a_theirs = activate(&mut r, pyro, &[TargetRef::Object(theirs)]);
    pass(&mut r);
    let a_own = activate(&mut r, my_pyro, &[TargetRef::Object(theirs)]);

    start_activation(&mut r, nimble);
    let legal = offered(&r);
    assert_eq!(legal, set(&[k_equip, k_crew, a_theirs]));
    assert!(!legal.contains(&a_own));
}
