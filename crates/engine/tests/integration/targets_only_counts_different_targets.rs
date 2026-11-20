//! CR 115.9c: "a spell that targets only [X]" counts the different objects or
//! players chosen as its targets; one object chosen several times counts once,
//! and a chosen target that has since left its zone still counts (#11221).

use std::collections::BTreeSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const REBOUND: &str =
    "Change the target of target spell that targets only a player. The new target must be a player.";
const ARC_TRAIL: &str = "Arc Trail deals 2 damage to any target and 1 damage to any other target.";
const LAVA_SPIKE: &str = "Lava Spike deals 3 damage to target player or planeswalker.";
const TORCHLING: &str = "{R}: Untap this creature.\n{R}: Target creature blocks this creature this turn if able.\n{R}: Change the target of target spell that targets only this creature.\n{1}: This creature gets +1/-1 until end of turn.\n{1}: This creature gets -1/+1 until end of turn.";
const SEEDS_OF_STRENGTH: &str = "Target creature gets +1/+1 until end of turn.\nTarget creature gets +1/+1 until end of turn.\nTarget creature gets +1/+1 until end of turn.";
const MURDER: &str = "Destroy target creature.";
const INVOKE_CALAMITY: &str = "You may cast up to two instant and/or sorcery spells with total mana value 6 or less from your graveyard and/or hand without paying their mana costs. If those spells would be put into your graveyard, exile them instead. Exile Invoke Calamity.";
const ZADA: &str = "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control that the spell could target. Each copy targets a different one of those creatures.";

fn free_spell(s: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    s.add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

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

fn choose_targets(runner: &mut GameRunner, targets: &[TargetRef]) {
    for target in targets {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
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

fn cast(runner: &mut GameRunner, spell: ObjectId, targets: &[TargetRef]) -> ObjectId {
    let before = runner.state().stack.len();
    start_cast(runner, spell);
    choose_targets(runner, targets);
    assert_eq!(
        runner.state().stack.len(),
        before + 1,
        "spell was not put on the stack: {:?}",
        runner.state().waiting_for
    );
    runner.state().stack.back().expect("stack entry").id
}

fn offered(runner: &GameRunner) -> BTreeSet<ObjectId> {
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    target_slots
        .iter()
        .flat_map(|slot| slot.legal_targets.iter())
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            TargetRef::Player(pid) => panic!("a spell filter offered player {pid:?}"),
        })
        .collect()
}

fn set(ids: &[ObjectId]) -> BTreeSet<ObjectId> {
    ids.iter().copied().collect()
}

/// P1 casts two Lava Spikes (one at each player), so Rebound always has two
/// matching spells and prompts instead of auto-choosing.
fn rebound_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spike_a = free_spell(&mut s, P1, "Lava Spike", LAVA_SPIKE);
    let spike_b = free_spell(&mut s, P1, "Lava Spike", LAVA_SPIKE);
    let rebound = free_spell(&mut s, P0, "Rebound", REBOUND);
    (s, spike_a, spike_b, rebound)
}

#[test]
fn rebound_refuses_spell_targeting_two_different_players() {
    let (mut s, spike_a, spike_b, rebound) = rebound_scenario();
    let arc = free_spell(&mut s, P1, "Arc Trail", ARC_TRAIL);
    let mut r = s.build();
    give_turn(&mut r, P1);
    let spike_p0 = cast(&mut r, spike_a, &[TargetRef::Player(P0)]);
    let spike_p1 = cast(&mut r, spike_b, &[TargetRef::Player(P1)]);
    let arc_two_players = cast(&mut r, arc, &[TargetRef::Player(P0), TargetRef::Player(P1)]);
    pass(&mut r);
    start_cast(&mut r, rebound);

    let legal = offered(&r);
    assert!(!legal.contains(&arc_two_players));
    assert_eq!(legal, set(&[spike_p0, spike_p1]));
}

#[test]
fn rebound_counts_a_departed_second_target() {
    let (mut s, spike_a, spike_b, rebound) = rebound_scenario();
    let bears = s.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let arc = free_spell(&mut s, P1, "Arc Trail", ARC_TRAIL);
    let mut r = s.build();
    give_turn(&mut r, P1);
    let spike_p0 = cast(&mut r, spike_a, &[TargetRef::Player(P0)]);
    let spike_p1 = cast(&mut r, spike_b, &[TargetRef::Player(P1)]);
    let arc_departed = cast(
        &mut r,
        arc,
        &[TargetRef::Player(P0), TargetRef::Object(bears)],
    );
    kill(&mut r, bears);
    pass(&mut r);
    start_cast(&mut r, rebound);

    let legal = offered(&r);
    assert!(!legal.contains(&arc_departed));
    assert_eq!(legal, set(&[spike_p0, spike_p1]));
}

/// CR 608.2g: the resolving Invoke Calamity is still a targetable spell, but it
/// chose no targets, so it is not a spell that targets only a player.
#[test]
fn rebound_refuses_resolving_spell_with_no_targets() {
    let (mut s, spike_a, spike_b, rebound) = rebound_scenario();
    let invoke = free_spell(&mut s, P0, "Invoke Calamity", INVOKE_CALAMITY);
    let mut r = s.build();
    give_turn(&mut r, P1);
    let spike_p0 = cast(&mut r, spike_a, &[TargetRef::Player(P0)]);
    let spike_p1 = cast(&mut r, spike_b, &[TargetRef::Player(P1)]);
    pass(&mut r);
    cast(&mut r, invoke, &[]);
    pass(&mut r);
    pass(&mut r);
    r.act(GameAction::FreeCastWindowChoice {
        selection: Some(rebound),
    })
    .expect("free-cast Rebound");

    let legal = offered(&r);
    assert!(!legal.contains(&invoke));
    assert_eq!(legal, set(&[spike_p0, spike_p1]));
}

#[test]
fn torchling_counts_one_object_chosen_three_times_once() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let torch = s
        .add_creature_from_oracle(P0, "Torchling", 3, 3, TORCHLING)
        .id();
    let bears = s.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let seeds_a = free_spell(&mut s, P1, "Seeds of Strength", SEEDS_OF_STRENGTH);
    let seeds_b = free_spell(&mut s, P1, "Seeds of Strength", SEEDS_OF_STRENGTH);
    let murder = free_spell(&mut s, P1, "Murder", MURDER);
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut r = s.build();
    give_turn(&mut r, P1);
    let t = TargetRef::Object(torch);
    let seeds_x3 = cast(&mut r, seeds_a, &[t.clone(), t.clone(), t.clone()]);
    let murder_torch = cast(&mut r, murder, std::slice::from_ref(&t));
    let seeds_mixed = cast(
        &mut r,
        seeds_b,
        &[t.clone(), t.clone(), TargetRef::Object(bears)],
    );
    pass(&mut r);
    let ability_index = r.state().objects[&torch]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::ChangeTargets { .. }))
        .expect("change-target ability");
    r.act(GameAction::ActivateAbility {
        source_id: torch,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");

    let legal = offered(&r);
    assert!(!legal.contains(&seeds_mixed));
    assert_eq!(legal, set(&[seeds_x3, murder_torch]));
}

/// Casts Seeds of Strength at Zada, Zada and `third`; returns how many Zada
/// triggers went on the stack.
fn zada_triggers(third_is_zada: bool) -> usize {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let zada = s
        .add_creature_from_oracle(P0, "Zada, Hedron Grinder", 3, 3, ZADA)
        .id();
    let bears = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let seeds = free_spell(&mut s, P0, "Seeds of Strength", SEEDS_OF_STRENGTH);
    let mut r = s.build();
    give_turn(&mut r, P0);
    let z = TargetRef::Object(zada);
    let third = if third_is_zada {
        z.clone()
    } else {
        TargetRef::Object(bears)
    };
    start_cast(&mut r, seeds);
    choose_targets(&mut r, &[z.clone(), z, third]);
    let stack = &r.state().stack;
    assert!(
        stack
            .iter()
            .any(|e| e.id == seeds && matches!(e.kind, StackEntryKind::Spell { .. })),
        "Seeds of Strength was not cast: {:?}",
        r.state().waiting_for
    );
    stack
        .iter()
        .filter(|e| {
            e.source_id == zada && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

#[test]
fn zada_triggers_only_when_every_target_is_zada() {
    assert_eq!(zada_triggers(true), 1);
    assert_eq!(zada_triggers(false), 0);
}
