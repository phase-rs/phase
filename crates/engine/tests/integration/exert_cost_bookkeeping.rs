//! Exert paid as part of an activated ability's cost records the exertion, and
//! "whenever you exert a creature" observers see it.
//!
//! Before the fix the `AbilityCost::Exert` arm only installed the "doesn't
//! untap" effect: it never recorded the permanent in `exerted_this_turn` and
//! emitted no `CreatureExerted`. Separately `match_exerted` ignored the
//! trigger's `valid_card` / `valid_target`, so an observer could only ever fire
//! for its own source.
//!
//! Rules: CR 701.43a (exert = choose to have a permanent not untap during your
//! next untap step; an event each time), CR 701.43b (an already-exerted
//! permanent can be exerted again), CR 701.43d (the attack cost and its linked
//! trigger), CR 602.2b + CR 605.3b (exert paid while activating an ability,
//! mana abilities included), CR 601.2h (a mana ability activated while paying a
//! spell).
//!
//! No printed card is both a "whenever you exert" observer and an exert-cost
//! creature, so the observer tests pair Resolute Survivors (observer) with a
//! cost-exerter (Arena of Glory, Fervent Paincaster, Oasis Ritualist).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, ChosenAttribute, ContinuousModification, TargetFilter,
    TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
};
use engine::types::actions::GameAction;
use engine::types::events::{ExertOrigin, GameEvent};
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, ManaChoice, PersistedGameState,
    PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{ResolvedObjectStatus, ResolvedRulesCommand};
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const OASIS_RITUALIST: &str = "{T}: Add one mana of any color.\n\
{T}, Exert this creature: Add two mana of any one color. \
(An exerted creature won't untap during your next untap step.)";
const ARENA_OF_GLORY: &str = "This land enters tapped unless you control a Mountain.\n\
{T}: Add {R}.\n\
{R}, {T}, Exert this land: Add {R}{R}. If any of that mana is spent on a creature spell, \
it gains haste until end of turn. (An exerted permanent won't untap during your next untap step.)";
const FERVENT_PAINCASTER: &str = "{T}: This creature deals 1 damage to target player or \
planeswalker.\n{T}, Exert this creature: It deals 1 damage to target creature. \
(An exerted creature won't untap during your next untap step.)";
const RESOLUTE_SURVIVORS: &str = "You may exert this creature as it attacks. (It won't untap \
during your next untap step.)\nWhenever you exert a creature, this creature deals 1 damage to \
each opponent and you gain 1 life.";
const BITTERBLADE_WARRIOR: &str = "You may exert this creature as it attacks. When you do, it \
gets +1/+0 and gains deathtouch until end of turn. (An exerted creature won't untap during your \
next untap step.)";
const STANDALONE_EXERT_PERMISSION: &str =
    "You may exert this creature as it attacks. (It won't untap during your next untap step.)";
const GLORY_BOUND_INITIATE: &str = "You may exert this creature as it attacks. When you do, it gets +1/+3 and gains lifelink until end of turn. (An exerted creature won't untap during your next untap step.)";
const KOH: &str = "When Koh enters, exile up to one other target creature.\nWhenever another nontoken creature dies, you may exile it.\nPay 1 life: Choose a creature card exiled with Koh.\nKoh has all activated and triggered abilities of the last chosen card.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const FERVOR: &str = "Creatures you control have haste. (They can attack and {T} as soon as they come under your control.)";

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn exerted_events(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CreatureExerted {
                object_id,
                player: Some(player),
                ..
            } => Some((*object_id, *player)),
            _ => None,
        })
        .collect()
}

/// Transient `CantUntap` effects aimed at `id` (the shape an exert installs).
fn cant_untap_markers(runner: &GameRunner, id: ObjectId) -> usize {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|e| e.affected == TargetFilter::SpecificObject { id })
        .filter(|e| {
            e.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap
                    }
                )
            })
        })
        .count()
}

fn pool_units(runner: &GameRunner, color: ManaType) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .mana_pool
        .units()
        .filter(|unit| unit.color == color)
        .count()
}

/// Index of the ability whose cost is a `Composite` (the `..., Exert` ability),
/// so a test can never activate the plain `{T}:` ability by accident.
fn composite_index(runner: &GameRunner, source: ObjectId) -> usize {
    let abilities = &runner.state().objects[&source].abilities;
    let indices: Vec<usize> = abilities
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a.cost, Some(AbilityCost::Composite { .. })))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(indices.len(), 1, "exactly one Composite-cost ability");
    indices[0]
}

/// Index of the ability whose cost is exactly `{T}`.
fn tap_only_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Tap)))
        .expect("a Tap-only ability")
}

fn untap(runner: &mut GameRunner, ids: &[ObjectId]) {
    for id in ids {
        runner.state_mut().objects.get_mut(id).unwrap().tapped = false;
    }
}

/// Run `action`, appending its events to `acc`.
fn act_collect(runner: &mut GameRunner, action: GameAction, acc: &mut Vec<GameEvent>) {
    let result = runner.act(action).expect("action should succeed");
    acc.extend(result.events);
}

/// Activate Oasis Ritualist's ability `index` and answer the color prompt with
/// Green. (`AbilityActivation::resolve` does not drive `ChooseManaColor`.)
fn activate_ritualist(
    runner: &mut GameRunner,
    ritualist: ObjectId,
    index: usize,
    acc: &mut Vec<GameEvent>,
) {
    act_collect(
        runner,
        GameAction::ActivateAbility {
            source_id: ritualist,
            ability_index: index,
        },
        acc,
    );
    assert_eq!(runner.waiting_for_kind(), "ChooseManaColor");
    act_collect(
        runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        },
        acc,
    );
}

/// Row 1: a creature mana ability whose cost is `{T}, Exert` records exertion.
#[test]
fn exert_cost_on_a_creature_mana_ability_records_exertion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
        .id();
    let mut runner = scenario.build();
    untap(&mut runner, &[ritualist]);

    let idx = composite_index(&runner, ritualist);
    let mut events = Vec::new();
    activate_ritualist(&mut runner, ritualist, idx, &mut events);

    // Reach guard: the exert ability ran (two Green, tapped, one CantUntap).
    assert_eq!(pool_units(&runner, ManaType::Green), 2);
    assert!(runner.state().objects[&ritualist].tapped);
    assert_eq!(cant_untap_markers(&runner, ritualist), 1);
    // The fix.
    assert!(runner.state().exerted_this_turn.contains(&ritualist));
    assert_eq!(exerted_events(&events), vec![(ritualist, P0)]);
}

/// Row 1 control: the plain `{T}: Add one mana` ability exerts nothing.
#[test]
fn plain_tap_ability_does_not_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
        .id();
    let mut runner = scenario.build();
    untap(&mut runner, &[ritualist]);

    let idx = tap_only_index(&runner, ritualist);
    let mut events = Vec::new();
    activate_ritualist(&mut runner, ritualist, idx, &mut events);

    assert_eq!(pool_units(&runner, ManaType::Green), 1, "reach guard");
    assert!(runner.state().objects[&ritualist].tapped);
    assert!(runner.state().exerted_this_turn.is_empty());
    assert_eq!(cant_untap_markers(&runner, ritualist), 0);
    assert!(exerted_events(&events).is_empty());
}

fn arena_scenario(with_survivors: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    if with_survivors {
        scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    }
    let mut runner = scenario.build();
    untap(&mut runner, &[arena, mountain]);
    (runner, arena, mountain)
}

/// Taps the Mountain for the `{R}` sub-cost, then exerts Arena of Glory.
fn exert_arena(runner: &mut GameRunner, arena: ObjectId, mountain: ObjectId) -> Vec<GameEvent> {
    let mut events = runner.activate(mountain, 0).resolve().events().to_vec();
    let idx = composite_index(runner, arena);
    events.extend(runner.activate(arena, idx).resolve().events().to_vec());
    events
}

/// Row 2: a land mana ability with a mana sub-cost (`{R}, {T}, Exert`).
#[test]
fn exert_cost_on_a_land_mana_ability_records_exertion() {
    let (mut runner, arena, mountain) = arena_scenario(false);
    let events = exert_arena(&mut runner, arena, mountain);

    // Reach guard: the exert ability ran (two Red in the pool, Arena tapped).
    assert_eq!(pool_units(&runner, ManaType::Red), 2);
    assert!(runner.state().objects[&arena].tapped);
    assert_eq!(cant_untap_markers(&runner, arena), 1);
    assert!(runner.state().exerted_this_turn.contains(&arena));
    assert_eq!(exerted_events(&events), vec![(arena, P0)]);
}

/// Row 3: a stack ability paid with `{T}, Exert` (Fervent Paincaster).
#[test]
fn exert_cost_on_a_stack_ability_records_exertion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paincaster = scenario
        .add_creature_from_oracle(P0, "Fervent Paincaster", 2, 2, FERVENT_PAINCASTER)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    untap(&mut runner, &[paincaster]);

    let idx = composite_index(&runner, paincaster);
    let events = runner
        .activate(paincaster, idx)
        .target_object(bear)
        .resolve()
        .events()
        .to_vec();

    // Reach guard: the ability resolved.
    assert_eq!(runner.state().objects[&bear].damage_marked, 1);
    assert!(runner.state().objects[&paincaster].tapped);
    assert_eq!(cant_untap_markers(&runner, paincaster), 1);
    assert!(runner.state().exerted_this_turn.contains(&paincaster));
    assert_eq!(exerted_events(&events), vec![(paincaster, P0)]);
}

/// Row 3 control: the `{T}:` player-damage ability exerts nothing.
#[test]
fn paincasters_tap_only_ability_does_not_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paincaster = scenario
        .add_creature_from_oracle(P0, "Fervent Paincaster", 2, 2, FERVENT_PAINCASTER)
        .id();
    let mut runner = scenario.build();
    untap(&mut runner, &[paincaster]);

    let idx = tap_only_index(&runner, paincaster);
    let events = runner
        .activate(paincaster, idx)
        .target_player(P1)
        .resolve()
        .events()
        .to_vec();

    assert_eq!(life(&runner, P1), 19, "reach guard: the ability resolved");
    assert!(runner.state().exerted_this_turn.is_empty());
    assert!(exerted_events(&events).is_empty());
}

/// Row 4a: the observer fires for a stack-ability exert.
#[test]
fn survivors_fires_for_a_stack_ability_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paincaster = scenario
        .add_creature_from_oracle(P0, "Fervent Paincaster", 2, 2, FERVENT_PAINCASTER)
        .id();
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    untap(&mut runner, &[paincaster]);

    let idx = composite_index(&runner, paincaster);
    let events = runner
        .activate(paincaster, idx)
        .target_object(bear)
        .resolve()
        .events()
        .to_vec();

    assert_eq!(exerted_events(&events).len(), 1, "reach guard: exert event");
    assert_eq!(life(&runner, P1), 19, "Survivors drained exactly once");
    assert_eq!(life(&runner, P0), 21);
}

/// Row 4b: the observer fires for a creature mana-ability exert at priority
/// (Arena of Glory is a land, so "exert a creature" does not cover it).
#[test]
fn survivors_fires_for_a_mana_ability_exert() {
    let (mut runner, ritualist) = ritualist_with_survivors();
    let idx = composite_index(&runner, ritualist);
    let mut events = Vec::new();
    activate_ritualist(&mut runner, ritualist, idx, &mut events);

    assert_eq!(exerted_events(&events).len(), 1, "reach guard: exert event");
    assert_eq!(runner.state().stack.len(), 1, "reach guard: trigger queued");
    runner.advance_until_stack_empty();
    assert_eq!(life(&runner, P1), 19, "Survivors drained exactly once");
    assert_eq!(life(&runner, P0), 21);
}

/// Row 4c: a land exerted as a cost is not "a creature", so the observer
/// stays quiet (negative pair of row 4b; the exert itself is recorded).
#[test]
fn survivors_ignores_a_land_exerted_as_a_cost() {
    let (mut runner, arena, mountain) = arena_scenario(true);
    let events = exert_arena(&mut runner, arena, mountain);

    assert_eq!(exerted_events(&events), vec![(arena, P0)], "reach guard");
    assert!(runner.state().stack.is_empty());
    runner.advance_until_stack_empty();
    assert_eq!(life(&runner, P1), 20);
    assert_eq!(life(&runner, P0), 20);
}

fn ritualist_with_survivors() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
        .id();
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let mut runner = scenario.build();
    untap(&mut runner, &[ritualist]);
    (runner, ritualist)
}

/// Attack route helper: `attacker` (controlled by `controller`) attacks the
/// other player and the exert is answered with `exert`. Returns all events.
fn attack_with_exert(
    runner: &mut GameRunner,
    controller: PlayerId,
    attacker: ObjectId,
    exert: bool,
) -> Vec<GameEvent> {
    let defender = if controller == P0 { P1 } else { P0 };
    if controller != P0 {
        runner.state_mut().active_player = controller;
    }
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    let mut events = Vec::new();
    let result = runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("declare attackers");
    events.extend(result.events);
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");
    act_collect(runner, GameAction::ChooseExert { exert }, &mut events);
    runner.advance_until_stack_empty();
    events
}

/// Row 5: the observer does not fire for an opponent's attack exert.
#[test]
fn survivors_does_not_fire_for_an_opponents_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let warrior = scenario
        .add_creature_from_oracle(P1, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
        .id();
    let mut runner = scenario.build();

    let events = attack_with_exert(&mut runner, P1, warrior, true);

    // Reach guards: the opponent exerted, and its own linked trigger resolved.
    assert_eq!(exerted_events(&events), vec![(warrior, P1)]);
    assert!(runner.state().exerted_this_turn.contains(&warrior));
    assert_eq!(runner.state().objects[&warrior].power, Some(3));
    // The observer stayed quiet.
    assert_eq!(life(&runner, P0), 20);
    assert_eq!(life(&runner, P1), 20);
}

/// Row 6: the observer fires for another creature's attack exert, and the
/// self-linked trigger still resolves. Pair of row 5.
#[test]
fn survivors_fires_for_another_creatures_attack_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let warrior = scenario
        .add_creature_from_oracle(P0, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
        .id();
    let mut runner = scenario.build();

    let events = attack_with_exert(&mut runner, P0, warrior, true);

    assert_eq!(exerted_events(&events), vec![(warrior, P0)]);
    assert_eq!(runner.state().objects[&warrior].power, Some(3));
    assert_eq!(life(&runner, P1), 19);
    assert_eq!(life(&runner, P0), 21);
}

/// Row 6 control: declining the exert does nothing.
#[test]
fn declining_the_exert_fires_no_observer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let warrior = scenario
        .add_creature_from_oracle(P0, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
        .id();
    let mut runner = scenario.build();

    let events = attack_with_exert(&mut runner, P0, warrior, false);

    assert!(exerted_events(&events).is_empty());
    assert!(runner.state().exerted_this_turn.is_empty());
    assert_eq!(runner.state().objects[&warrior].power, Some(2));
    assert_eq!(life(&runner, P1), 20);
    assert_eq!(life(&runner, P0), 20);
}

/// Row 7: no double fire - Survivors exerting itself drains exactly once.
#[test]
fn survivors_exerting_itself_drains_once() {
    for exert in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let survivors = scenario
            .add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS)
            .id();
        let mut runner = scenario.build();
        let object = &runner.state().objects[&survivors];
        let observer = object.trigger_definition_ref(
            object
                .trigger_definitions
                .iter_unchecked()
                .find(|entry| {
                    entry.definition.mode == TriggerMode::Exerted
                        && entry.definition.valid_target == Some(TargetFilter::Controller)
                })
                .expect("the real observer exists separately from its permission"),
        );
        declare_exert_attacker(&mut runner, survivors);
        let permission = current_permission(&runner);
        let events = runner
            .act(GameAction::ChooseExert { exert })
            .unwrap()
            .events;
        // CR 701.43d + CR 603.1: The static permission creates no stack item
        // and cannot manufacture an OrderTriggers prompt alongside the observer.
        assert_exert_priority(&runner, usize::from(exert));
        if exert {
            assert_eq!(exerted_events(&events), vec![(survivors, P0)]);
            assert_eq!(attack_receipts(&events), vec![permission]);
            assert_eq!(runner.state().stack[0].source_id, survivors);
            assert_eq!(
                runner.state().stack[0]
                    .ability()
                    .unwrap()
                    .trigger_definition_ref,
                Some(observer)
            );
            assert!(runner.state().exerted_this_turn.contains(&survivors));
        } else {
            assert!(exerted_events(&events).is_empty());
            assert!(!runner.state().exerted_this_turn.contains(&survivors));
        }
        assert_eq!(life(&runner, P0), 20);
        assert_eq!(life(&runner, P1), 20);
        runner.advance_until_stack_empty();
        assert_exert_priority(&runner, 0);
        assert_eq!(life(&runner, P1), if exert { 19 } else { 20 });
        assert_eq!(life(&runner, P0), if exert { 21 } else { 20 });
    }
}

/// CR 701.43d + CR 603.1: The exact first Survivors paragraph is an isolated
/// compositional static permission fixture, without a triggered effect.
#[test]
fn standalone_attack_permission_has_no_triggered_stack_entry() {
    for exert in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let attacker = scenario
            .add_creature_from_oracle(
                P0,
                "Survivors permission component",
                3,
                3,
                STANDALONE_EXERT_PERMISSION,
            )
            .id();
        let mut runner = scenario.build();
        declare_exert_attacker(&mut runner, attacker);
        let permission = current_permission(&runner);
        let events = runner
            .act(GameAction::ChooseExert { exert })
            .unwrap()
            .events;
        assert_exert_priority(&runner, 0);
        assert_eq!(cant_untap_markers(&runner, attacker), usize::from(exert));
        assert_eq!(runner.state().exerted_this_turn.contains(&attacker), exert);
        if exert {
            assert_eq!(exerted_events(&events), vec![(attacker, P0)]);
            assert_eq!(attack_receipts(&events), vec![permission]);
        } else {
            assert!(exerted_events(&events).is_empty());
        }
    }
}

/// CR 701.43d + CR 607.2h: A real linked effect remains a triggered ability.
#[test]
fn linked_attack_permission_still_publishes_its_selected_trigger() {
    for exert in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let initiate = scenario
            .add_creature_from_oracle(P0, "Glory-Bound Initiate", 3, 1, GLORY_BOUND_INITIATE)
            .id();
        let mut runner = scenario.build();
        declare_exert_attacker(&mut runner, initiate);
        let permission = current_permission(&runner);
        let events = runner
            .act(GameAction::ChooseExert { exert })
            .unwrap()
            .events;
        assert_exert_priority(&runner, usize::from(exert));
        if exert {
            assert_eq!(attack_receipts(&events), vec![permission.clone()]);
            assert_eq!(
                runner.state().stack[0]
                    .ability()
                    .unwrap()
                    .trigger_definition_ref,
                Some(permission)
            );
        } else {
            assert!(exerted_events(&events).is_empty());
        }
        runner.advance_until_stack_empty();
        assert_exert_priority(&runner, 0);
        let object = &runner.state().objects[&initiate];
        assert_eq!(object.power, Some(if exert { 4 } else { 3 }));
        assert_eq!(object.toughness, Some(if exert { 4 } else { 1 }));
        assert_eq!(object.has_keyword(&Keyword::Lifelink), exert);
    }
}

/// CR 701.43d + CR 603.1: Koh copies Survivors' observer, but its triggered-only
/// grant cannot give Koh the standalone static attack permission.
#[test]
fn triggered_only_grant_omits_standalone_exert_permission() {
    for exert_warrior in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let koh = scenario
            .add_creature_from_oracle(P0, "Koh, the Face Stealer", 6, 6, KOH)
            .id();
        let mut provider_builder = scenario.add_creature_to_exile(P0, "Resolute Survivors", 3, 3);
        provider_builder.from_oracle_text(RESOLUTE_SURVIVORS);
        let provider = provider_builder.id();
        let warrior = scenario
            .add_creature_from_oracle(P0, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
            .id();
        let mut runner = scenario.build();
        assert!(!runner.state().objects[&koh]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| entry.definition.mode == TriggerMode::Exerted));
        let provider_object = &runner.state().objects[&provider];
        assert_eq!(provider_object.trigger_definitions.len(), 2);
        let observer = provider_object.trigger_definition_ref(
            provider_object
                .trigger_definitions
                .iter_unchecked()
                .find(|entry| entry.definition.valid_target == Some(TargetFilter::Controller))
                .unwrap(),
        );
        let pin = ObjectIncarnationRef::from_object(provider_object);
        runner
            .state_mut()
            .objects
            .get_mut(&koh)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Card(pin));
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: provider,
            source_id: koh,
            kind: ExileLinkKind::TrackedBySource,
        });
        runner.state_mut().layers_dirty.mark_full();
        engine::game::layers::evaluate_layers(runner.state_mut());
        let copied: Vec<_> = runner.state().objects[&koh]
            .trigger_definitions
            .iter_unchecked()
            .filter(|entry| entry.definition.mode == TriggerMode::Exerted)
            .collect();
        assert_eq!(copied.len(), 1, "only the genuine observer is granted");
        assert!(!copied[0].definition.is_attack_exert_permission());
        match &copied[0].occurrence {
            TriggerDefinitionOccurrenceRef::ExpandedGrant { provider, .. } => {
                assert_eq!(provider.as_ref(), &observer)
            }
            occurrence => panic!("expected the original provider identity, got {occurrence:?}"),
        }
        let copied_ref = runner.state().objects[&koh].trigger_definition_ref(copied[0]);
        runner.advance_to_combat();
        assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
        let mut attacks = vec![(koh, AttackTarget::Player(P1))];
        if exert_warrior {
            attacks.push((warrior, AttackTarget::Player(P1)));
        }
        runner
            .declare_attackers(&attacks)
            .expect("Koh attacks legally");
        assert!(runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|entry| entry.object_id == koh));
        if exert_warrior {
            let permission = current_permission(&runner);
            assert_eq!(permission.source.object_id, warrior);
            let events = runner
                .act(GameAction::ChooseExert { exert: true })
                .unwrap()
                .events;
            assert_eq!(attack_receipts(&events), vec![permission]);
            match &runner.state().waiting_for {
                WaitingFor::OrderTriggers { player, triggers } => {
                    assert_eq!(*player, P0);
                    assert_eq!(
                        triggers.len(),
                        2,
                        "one Warrior payoff and one copied observer"
                    );
                }
                waiting => panic!("expected ordering of the two genuine triggers, got {waiting:?}"),
            }
            runner
                .act(GameAction::OrderTriggers { order: vec![1, 0] })
                .unwrap();
            assert_exert_priority(&runner, 2);
            assert_eq!(
                runner
                    .state()
                    .stack
                    .iter()
                    .filter(
                        |entry| entry.ability().unwrap().trigger_definition_ref.as_ref()
                            == Some(&copied_ref)
                    )
                    .count(),
                1
            );
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P0), 21);
            assert_eq!(life(&runner, P1), 19);
            assert_eq!(runner.state().objects[&warrior].power, Some(3));
        } else {
            assert_exert_priority(&runner, 0);
            assert_eq!(life(&runner, P0), 20);
            assert_eq!(life(&runner, P1), 20);
        }
        assert_exert_priority(&runner, 0);
    }
}

/// Row 8, CR 701.43b: an already-exerted permanent can be exerted again; the
/// event, effect and observer trigger happen again.
#[test]
fn exerting_an_already_exerted_permanent_exerts_again() {
    let (mut runner, ritualist) = ritualist_with_survivors();
    let idx = composite_index(&runner, ritualist);
    let mut first = Vec::new();
    activate_ritualist(&mut runner, ritualist, idx, &mut first);
    runner.advance_until_stack_empty();
    untap(&mut runner, &[ritualist]);
    let mut second = Vec::new();
    activate_ritualist(&mut runner, ritualist, idx, &mut second);
    runner.advance_until_stack_empty();

    assert_eq!(exerted_events(&first).len(), 1);
    assert_eq!(exerted_events(&second), vec![(ritualist, P0)]);
    assert!(runner.state().exerted_this_turn.contains(&ritualist));
    assert_eq!(cant_untap_markers(&runner, ritualist), 2);
    assert_eq!(life(&runner, P1), 18, "one drain per exert");
    assert_eq!(life(&runner, P0), 22);
}

fn witness(scenario: &mut GameScenario, shards: &[ManaCostShard]) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Exert Witness", false, "You gain 1 life.")
        .with_mana_cost(ManaCost::Cost {
            shards: shards.to_vec(),
            generic: 0,
        })
        .id()
}

/// Row 10: a mana ability with an exert cost activated inside a cast's payment
/// (CR 601.2h + CR 605.3b) records exertion once, and the observer fires once.
#[test]
fn exert_cost_mana_ability_inside_a_spells_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
        .id();
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let spell = witness(&mut scenario, &[ManaCostShard::Green, ManaCostShard::Green]);
    let mut runner = scenario.build();
    untap(&mut runner, &[ritualist]);

    let mut events = Vec::new();
    let card_id = runner.state().objects[&spell].card_id;
    act_collect(
        &mut runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
        &mut events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let idx = composite_index(&runner, ritualist);
    activate_ritualist(&mut runner, ritualist, idx, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    act_collect(&mut runner, GameAction::PassPriority, &mut events);
    runner.advance_until_stack_empty();

    // Reach guards: the spell was paid with the exert ability and resolved.
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(cant_untap_markers(&runner, ritualist), 1);
    // The fix.
    assert!(runner.state().exerted_this_turn.contains(&ritualist));
    assert_eq!(exerted_events(&events), vec![(ritualist, P0)]);
    assert_eq!(life(&runner, P1), 19, "exactly one observer trigger");
    assert_eq!(life(&runner, P0), 22, "witness +1, drain +1");
}

/// Row 11(a): auto-pay of `{G}` uses the plain row, so nothing is exerted.
#[test]
fn autopay_plain_row_does_not_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
        .id();
    scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    let spell = witness(&mut scenario, &[ManaCostShard::Green]);
    let mut runner = scenario.build();
    untap(&mut runner, &[ritualist]);

    let outcome: CastOutcome = runner.cast(spell).resolve();

    // Reach guard: the witness resolved and the Ritualist paid for it.
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    assert!(outcome.state().objects[&ritualist].tapped);
    assert!(outcome.events().iter().any(|e| matches!(
        e,
        GameEvent::ManaAdded { source_id, .. } if *source_id == ritualist
    )));
    assert!(outcome.state().exerted_this_turn.is_empty());
    assert!(exerted_events(outcome.events()).is_empty());
    assert_eq!(outcome.life_delta(P0), 1);
    assert_eq!(outcome.life_delta(P1), 0);
}

const AGATHAS_SOUL_CAULDRON: &str = "You may spend mana as though it were mana of any color to activate abilities of creatures you control.\n\
Creatures you control with +1/+1 counters on them have all activated abilities of all creature cards exiled with Agatha's Soul Cauldron.\n\
{T}: Exile target card from a graveyard. When a creature card is exiled this way, put a +1/+1 counter on target creature you control.";
const COMBAT_CELEBRANT: &str = "If this creature hasn't been exerted this turn, you may exert it as it attacks. When you do, untap all other creatures you control and after this phase, there is an additional combat phase. (An exerted creature won't untap during your next untap step.)";

/// CR 701.43d + CR 607.2h: Bitterblade Warrior (a +1/+1 counter on it) holds
/// Fervent Paincaster's `{T}, Exert` activation through Agatha's Soul Cauldron.
/// Returns the runner, the Warrior and an opposing creature to aim it at.
fn warrior_with_granted_exert_activation(with_survivors: bool) -> (GameRunner, ObjectId, ObjectId) {
    let (runner, warrior, bear, _) =
        granted_exert_scenario("Bitterblade Warrior", BITTERBLADE_WARRIOR, with_survivors);
    (runner, warrior, bear)
}

fn granted_exert_scenario(
    name: &str,
    oracle: &str,
    with_survivors: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    granted_exert_scenario_with(name, oracle, with_survivors, |_| {})
}

fn granted_exert_scenario_with(
    name: &str,
    oracle: &str,
    with_survivors: bool,
    configure: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario
        .add_artifact_from_oracle(P0, "Agatha's Soul Cauldron", AGATHAS_SOUL_CAULDRON)
        .id();
    let mut paincaster = scenario.add_creature_to_exile(P0, "Fervent Paincaster", 2, 2);
    paincaster.from_oracle_text(FERVENT_PAINCASTER);
    let paincaster_card = paincaster.id();
    let mut warrior_builder = scenario.add_creature_from_oracle(P0, name, 2, 2, oracle);
    warrior_builder.with_plus_counters(1);
    let warrior = warrior_builder.id();
    if with_survivors {
        scenario.add_creature_from_oracle(P0, "Resolute Survivors", 3, 3, RESOLUTE_SURVIVORS);
    }
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let vitalize = scenario
        .add_spell_to_hand_from_oracle(P0, "Vitalize", false, "Untap all creatures you control.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    configure(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: paincaster_card,
        source_id: cauldron,
        kind: ExileLinkKind::TrackedBySource,
    });
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    untap(&mut runner, &[warrior]);
    (runner, warrior, bear, vitalize)
}

/// Activate the granted `{T}, Exert` ability on the Warrior, aimed at `bear`.
fn activate_granted_exert(
    runner: &mut GameRunner,
    warrior: ObjectId,
    bear: ObjectId,
) -> Vec<GameEvent> {
    let idx = composite_index(runner, warrior);
    let events = runner
        .activate(warrior, idx)
        .target_object(bear)
        .resolve()
        .events()
        .to_vec();
    // Reach guard: the Cauldron grant put an exert-cost activation on the
    // Warrior and it resolved and exerted.
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        1,
        "ability resolved"
    );
    assert!(runner.state().exerted_this_turn.contains(&warrior));
    events
}

fn first_exert_origin(events: &[GameEvent]) -> Option<ExertOrigin> {
    events.iter().find_map(|event| match event {
        GameEvent::CreatureExerted { origin, .. } => Some(*origin),
        _ => None,
    })
}

/// An exert paid as the cost of an activated ability the Warrior was granted is
/// not the attack cost its "when you do" trigger is linked to (CR 701.43d,
/// CR 607.2h): no +1/+0, no deathtouch.
#[test]
fn granted_exert_cost_activation_does_not_fire_the_attack_linked_trigger() {
    let (mut runner, warrior, bear) = warrior_with_granted_exert_activation(false);
    let power_before = runner.state().objects[&warrior].power;

    let events = activate_granted_exert(&mut runner, warrior, bear);

    assert_eq!(first_exert_origin(&events), Some(ExertOrigin::AbilityCost));
    assert_eq!(runner.state().objects[&warrior].power, power_before);
    assert!(runner.state().stack.is_empty());
    assert!(!runner.state().objects[&warrior]
        .has_keyword(&engine::types::keywords::Keyword::Deathtouch));
}

/// Generic-observer control: the same granted activation does fire a "whenever
/// you exert a creature" observer, so the refusal above is the linked trigger's
/// origin, not a dead event.
#[test]
fn granted_exert_cost_activation_still_fires_a_generic_observer() {
    let (mut runner, warrior, bear) = warrior_with_granted_exert_activation(true);
    let power_before = runner.state().objects[&warrior].power;

    activate_granted_exert(&mut runner, warrior, bear);
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P1), 19);
    assert_eq!(life(&runner, P0), 21);
    assert_eq!(runner.state().objects[&warrior].power, power_before);
}

/// Positive control: with the grant in play, the Warrior's own attack exert
/// still fires its linked trigger.
#[test]
fn attack_exert_still_fires_the_linked_trigger_with_the_grant_in_play() {
    let (mut runner, warrior, _bear) = warrior_with_granted_exert_activation(false);
    let power_before = runner.state().objects[&warrior].power.unwrap();

    let events = attack_with_exert(&mut runner, P0, warrior, true);

    assert_eq!(first_exert_origin(&events), Some(ExertOrigin::AttackCost));
    assert_eq!(
        runner.state().objects[&warrior].power,
        Some(power_before + 1)
    );
}

/// Restore the state `runner` holds through the production persisted path after
/// `edit` has rewritten its serialized JSON.
fn restore_after(
    runner: &GameRunner,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<engine::types::game_state::GameState, String> {
    let mut json = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("serialize state");
    edit(&mut json);
    serde_json::from_value::<PersistedGameState>(json)
        .map_err(|e| e.to_string())?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|e| e.to_string())?
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|e| e.to_string())
}

fn push_pending_attack_event(json: &mut serde_json::Value, event: serde_json::Value) {
    let state = if json.get("state").is_some() {
        &mut json["state"]
    } else {
        json
    };
    // An empty buffer is left out of the serialized state, so create it.
    state
        .as_object_mut()
        .expect("state serializes as an object")
        .entry("pending_attack_trigger_events")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .expect("pending_attack_trigger_events is an array")
        .push(event);
}

/// A save written before `CreatureExerted` carried an actor and an origin holds
/// only `object_id`. It restores: the actor stays unknown (none is invented) and
/// the origin is the attack cost, the only exert the old engine emitted.
#[test]
fn legacy_exert_event_shape_restores_without_inventing_an_actor() {
    let (runner, warrior, _bear) = warrior_with_granted_exert_activation(false);
    let legacy = serde_json::json!({
        "type": "CreatureExerted",
        "data": { "object_id": warrior }
    });

    let restored = restore_after(&runner, |json| push_pending_attack_event(json, legacy))
        .expect("a legacy exert event must restore");

    assert_eq!(
        restored.pending_attack_trigger_events,
        vec![GameEvent::CreatureExerted {
            object_id: warrior,
            player: None,
            origin: ExertOrigin::AttackCost,
            attack_permission: None,
        }]
    );
}

/// Current-shape control: an event that records its actor and origin round-trips
/// through the same restore path unchanged.
#[test]
fn current_exert_event_shape_restores_with_its_actor_and_origin() {
    let (runner, warrior, _bear) = warrior_with_granted_exert_activation(false);
    let current = GameEvent::CreatureExerted {
        object_id: warrior,
        player: Some(P0),
        origin: ExertOrigin::AbilityCost,
        attack_permission: None,
    };
    let value = serde_json::to_value(&current).unwrap();

    let restored = restore_after(&runner, |json| push_pending_attack_event(json, value))
        .expect("a current exert event must restore");

    assert_eq!(restored.pending_attack_trigger_events, vec![current]);
}

fn cast_vitalize_after_activation(runner: &mut GameRunner, source: ObjectId, vitalize: ObjectId) {
    assert!(runner.state().objects[&source].tapped);
    assert!(runner.state().exerted_this_turn.contains(&source));
    runner.cast(vitalize).resolve();
    assert_eq!(runner.state().objects[&vitalize].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&source].tapped);
    // CR 701.43b: An ordinary untap does not erase exertion history.
    assert!(runner.state().exerted_this_turn.contains(&source));
    assert!(runner.state().stack.is_empty());
}

fn declare_exert_attacker(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("legal attack");
    assert!(runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|entry| entry.object_id == attacker));
}

fn current_permission(runner: &GameRunner) -> TriggerDefinitionRef {
    match &runner.state().waiting_for {
        WaitingFor::ExertChoice {
            player,
            attacker,
            permission,
            ..
        } => {
            assert_eq!(*attacker, permission.source.object_id);
            assert_eq!(*player, runner.state().objects[attacker].controller);
            permission.clone()
        }
        waiting => panic!("expected attack exert permission, got {waiting:?}"),
    }
}

fn assert_exert_priority(runner: &GameRunner, stack_len: usize) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), stack_len);
    assert!(runner.state().pending_trigger.is_none());
    assert!(runner.state().pending_trigger_entry.is_none());
    assert!(runner.state().pending_trigger_order.is_none());
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().pending_attack_trigger_events.is_empty());
}

fn attack_receipts(events: &[GameEvent]) -> Vec<TriggerDefinitionRef> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CreatureExerted {
                origin: ExertOrigin::AttackCost,
                attack_permission: Some(permission),
                ..
            } => Some(permission.clone()),
            _ => None,
        })
        .collect()
}

/// CR 701.43b + CR 508.1g: An unconditional permission remains available after
/// an exert activation followed by a real untap spell in the same game.
#[test]
fn granted_exert_then_vitalize_preserves_unconditional_attack_permission() {
    for exert in [false, true] {
        let (mut runner, warrior, bear, vitalize) =
            granted_exert_scenario("Bitterblade Warrior", BITTERBLADE_WARRIOR, false);
        let power = runner.state().objects[&warrior].power.unwrap();
        let ability_events = activate_granted_exert(&mut runner, warrior, bear);
        assert_eq!(
            first_exert_origin(&ability_events),
            Some(ExertOrigin::AbilityCost)
        );
        assert!(attack_receipts(&ability_events).is_empty());
        assert_eq!(runner.state().objects[&warrior].power, Some(power));
        assert!(!runner.state().objects[&warrior].has_keyword(&Keyword::Deathtouch));
        cast_vitalize_after_activation(&mut runner, warrior, vitalize);
        declare_exert_attacker(&mut runner, warrior);
        let permission = current_permission(&runner);
        let events = runner
            .act(GameAction::ChooseExert { exert })
            .expect("answer permission")
            .events;
        assert_exert_priority(&runner, usize::from(exert));
        runner.advance_until_stack_empty();
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::ExertChoice { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert_exert_priority(&runner, 0);
        if exert {
            assert_eq!(attack_receipts(&events), vec![permission]);
            assert_eq!(runner.state().objects[&warrior].power, Some(power + 1));
            assert!(runner.state().objects[&warrior].has_keyword(&Keyword::Deathtouch));
        } else {
            assert!(exerted_events(&events).is_empty());
            assert_eq!(runner.state().objects[&warrior].power, Some(power));
            assert!(!runner.state().objects[&warrior].has_keyword(&Keyword::Deathtouch));
        }
    }
}

/// CR 400.7 + CR 702.10b: A real blink ends the old exerted object's history;
/// Fervor permits the returned Celebrant to attack during the same turn.
#[test]
fn granted_exert_then_cloudshift_allows_fresh_celebrant_attack() {
    let mut added = None;
    let (mut runner, celebrant, bear, _) =
        granted_exert_scenario_with("Combat Celebrant", COMBAT_CELEBRANT, false, |scenario| {
            scenario.add_enchantment_from_oracle(P0, "Fervor", FERVOR);
            let cloudshift = scenario
                .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 0,
                })
                .id();
            scenario.add_basic_land(P0, ManaColor::White);
            let other = scenario
                .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST)
                .id();
            added = Some((cloudshift, other));
        });
    let (cloudshift, other) = added.unwrap();
    let mut other_events = Vec::new();
    let other_index = composite_index(&runner, other);
    activate_ritualist(&mut runner, other, other_index, &mut other_events);
    assert_eq!(exerted_events(&other_events), vec![(other, P0)]);
    let ability_events = activate_granted_exert(&mut runner, celebrant, bear);
    assert_eq!(
        first_exert_origin(&ability_events),
        Some(ExertOrigin::AbilityCost)
    );
    let old = ObjectIncarnationRef::from_object(&runner.state().objects[&celebrant]);
    assert!(!runner.state().objects[&celebrant].counters.is_empty());
    assert_exert_priority(&runner, 0);
    let outcome = runner.cast(cloudshift).target_object(celebrant).resolve();
    let moves: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                ..
            } if *object_id == celebrant => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        moves,
        vec![
            (Some(Zone::Battlefield), Zone::Exile),
            (Some(Zone::Exile), Zone::Battlefield)
        ]
    );
    outcome.assert_zone(&[cloudshift], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let returned = &runner.state().objects[&celebrant];
    let fresh = ObjectIncarnationRef::from_object(returned);
    assert_eq!(fresh.object_id, old.object_id);
    assert_ne!(fresh, old);
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.counters.is_empty());
    assert!(!returned
        .abilities
        .iter()
        .any(|ability| matches!(ability.cost, Some(AbilityCost::Composite { .. }))));
    assert!(returned.has_keyword(&Keyword::Haste));
    assert!(!runner.state().exerted_this_turn.contains(&celebrant));
    assert!(runner.state().exerted_this_turn.contains(&other));
    assert_exert_priority(&runner, 0);
    declare_exert_attacker(&mut runner, celebrant);
    let permission = current_permission(&runner);
    assert_eq!(permission.source, fresh);
    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .unwrap()
        .events;
    assert_eq!(attack_receipts(&events), vec![permission.clone()]);
    assert_exert_priority(&runner, 1);
    assert_eq!(
        runner.state().stack[0]
            .ability()
            .unwrap()
            .trigger_definition_ref,
        Some(permission)
    );
    runner.advance_until_stack_empty();
    assert_exert_priority(&runner, 0);
    assert_eq!(runner.state().extra_phases.len(), 1);
    assert_eq!(
        runner.state().extra_phases[0].segment,
        TurnSegment::Phase(PhaseGroup::Combat)
    );
    assert!(runner.state().exerted_this_turn.contains(&celebrant));
    assert!(runner.state().exerted_this_turn.contains(&other));
}

/// CR 400.7: Replay of actual recorded transitions ends the same object's
/// exert history as live delivery, without adding replay journal authority.
#[test]
fn exert_history_zone_change_live_replay_agree() {
    let (mut runner, celebrant, bear, _) =
        granted_exert_scenario("Combat Celebrant", COMBAT_CELEBRANT, false);
    activate_granted_exert(&mut runner, celebrant, bear);
    // A second real permanent is independently exerted through the existing
    // status authority, so clearing the entire set fails the preservation guard.
    engine::game::object_state::resolve_and_apply_object_edit(
        runner.state_mut(),
        bear,
        ResolvedObjectStatus::Exerted,
        true,
    )
    .unwrap();
    let old_status = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ObjectStatus(command)
                if command.object.object_id == celebrant
                    && command.status == ResolvedObjectStatus::Exerted =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .expect("activation recorded its exert status authority");
    let before_noop = ObjectIncarnationRef::from_object(&runner.state().objects[&celebrant]);
    let journal_before_noop = runner.state().resolved_rules_journal.clone();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        celebrant,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&celebrant]),
        before_noop
    );
    assert!(runner.state().exerted_this_turn.contains(&celebrant));
    assert_eq!(runner.state().resolved_rules_journal, journal_before_noop);
    let mut replayed = runner.state().clone();
    let replay_journal = replayed.resolved_rules_journal.clone();
    for destination in [Zone::Exile, Zone::Battlefield] {
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            celebrant,
            destination,
            &mut Vec::new(),
        );
        let command = runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .rev()
            .filter_map(|entry| entry.command.as_ref())
            .find_map(|command| match command {
                ResolvedRulesCommand::ZoneChange(command)
                    if command.object.object_id == celebrant =>
                {
                    Some(command.as_ref().clone())
                }
                _ => None,
            })
            .expect("the live move records its actual zone command");
        if destination == Zone::Exile {
            for stale_occurrence in [false, true] {
                let mut rejected = command.clone();
                if stale_occurrence {
                    rejected.object.incarnation += 1;
                } else {
                    rejected.from = Zone::Graveyard;
                }
                assert!(
                    engine::game::zones::apply_resolved_zone_change(&mut replayed, &rejected)
                        .is_err()
                );
                assert!(replayed.exerted_this_turn.contains(&celebrant));
                assert_eq!(
                    ObjectIncarnationRef::from_object(&replayed.objects[&celebrant]),
                    before_noop
                );
                assert_eq!(replayed.resolved_rules_journal, replay_journal);
            }
        }
        engine::game::zones::apply_resolved_zone_change(&mut replayed, &command)
            .expect("valid recorded transition replays");
        let live = &runner.state().objects[&celebrant];
        let replay = &replayed.objects[&celebrant];
        assert_eq!(live.zone, destination);
        assert_eq!(replay.zone, destination);
        assert_eq!(
            ObjectIncarnationRef::from_object(live),
            ObjectIncarnationRef::from_object(replay)
        );
        assert_eq!(live.timestamp, replay.timestamp);
        assert_eq!(replay.incarnation, command.resulting_incarnation);
        if let Some(timestamp) = command.entry_timestamp {
            assert_eq!(replay.timestamp, timestamp);
        }
        assert!(!runner.state().exerted_this_turn.contains(&celebrant));
        assert!(!replayed.exerted_this_turn.contains(&celebrant));
        assert!(runner.state().exerted_this_turn.contains(&bear));
        assert!(replayed.exerted_this_turn.contains(&bear));
        assert_eq!(replayed.resolved_rules_journal, replay_journal);
    }
    assert!(engine::game::object_state::apply_resolved_object_edit(
        runner.state_mut(),
        &old_status
    )
    .is_err());
    assert!(
        engine::game::object_state::apply_resolved_object_edit(&mut replayed, &old_status).is_err()
    );
    assert!(!runner.state().exerted_this_turn.contains(&celebrant));
    assert!(!replayed.exerted_this_turn.contains(&celebrant));
}

/// CR 400.7: A live Command departure bypasses the ordinary resolved-zone
/// applier, so it independently exercises the live cleanup authority.
#[test]
fn exert_history_clears_on_live_commander_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // This compositional fixture marks the real exert-cost creature as a
    // commander in place, isolating Command's special live-container branch.
    let paincaster = scenario
        .add_creature_from_oracle(P0, "Fervent Paincaster", 2, 2, FERVENT_PAINCASTER)
        .commander()
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let index = composite_index(&runner, paincaster);
    let events = runner
        .activate(paincaster, index)
        .target_object(bear)
        .resolve()
        .events()
        .to_vec();
    assert_eq!(runner.state().objects[&bear].damage_marked, 1);
    assert_eq!(exerted_events(&events), vec![(paincaster, P0)]);
    assert!(runner.state().exerted_this_turn.contains(&paincaster));
    assert_exert_priority(&runner, 0);
    let old = ObjectIncarnationRef::from_object(&runner.state().objects[&paincaster]);
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        paincaster,
        Zone::Command,
        &mut Vec::new(),
    );
    let departed = &runner.state().objects[&paincaster];
    assert_eq!(departed.zone, Zone::Command);
    assert!(departed.is_commander);
    assert_ne!(ObjectIncarnationRef::from_object(departed), old);
    assert!(!runner.state().exerted_this_turn.contains(&paincaster));
}

/// CR 603.4 + CR 701.43b: Celebrant's explicit gate is a permission restriction,
/// not an intervening-if that would counter its own payoff after payment.
#[test]
fn celebrant_gate_restricts_payment_but_not_the_paid_trigger() {
    for prior_exert in [false, true] {
        let (mut runner, celebrant, bear, vitalize) =
            granted_exert_scenario("Combat Celebrant", COMBAT_CELEBRANT, false);
        if prior_exert {
            activate_granted_exert(&mut runner, celebrant, bear);
            cast_vitalize_after_activation(&mut runner, celebrant, vitalize);
        }
        declare_exert_attacker(&mut runner, celebrant);
        if prior_exert {
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::ExertChoice { .. }
            ));
            assert!(runner.state().extra_phases.is_empty());
            assert_eq!(cant_untap_markers(&runner, celebrant), 1);
        } else {
            let permission = current_permission(&runner);
            let events = runner
                .act(GameAction::ChooseExert { exert: true })
                .unwrap()
                .events;
            assert_eq!(attack_receipts(&events), vec![permission]);
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().extra_phases.len(), 1);
            assert!(runner.state().exerted_this_turn.contains(&celebrant));
        }
        assert!(runner.state().stack.is_empty());
        assert_exert_priority(&runner, 0);
    }
}

/// CR 607.2h: A conditional and an unconditional permission on one recipient
/// retain independent identities. The generic observer sees each actual exert.
#[test]
fn mixed_attack_permissions_pay_only_the_selected_linked_occurrence() {
    let mixed = format!("{COMBAT_CELEBRANT}\n{BITTERBLADE_WARRIOR}");
    for prior_exert in [false, true] {
        let (mut runner, source, bear, vitalize) =
            granted_exert_scenario("Mixed permissions", &mixed, true);
        let power = runner.state().objects[&source].power.unwrap();
        if prior_exert {
            activate_granted_exert(&mut runner, source, bear);
            runner.advance_until_stack_empty();
            assert_eq!(life(&runner, P1), 19);
            cast_vitalize_after_activation(&mut runner, source, vitalize);
        }
        declare_exert_attacker(&mut runner, source);
        if !prior_exert {
            let declined = current_permission(&runner);
            let events = runner
                .act(GameAction::ChooseExert { exert: false })
                .unwrap()
                .events;
            assert!(exerted_events(&events).is_empty());
            assert_ne!(current_permission(&runner), declined);
        }
        let permission = current_permission(&runner);
        if let WaitingFor::ExertChoice { remaining, .. } = &runner.state().waiting_for {
            assert!(
                remaining.is_empty(),
                "the restricted permission was never queued"
            );
        }
        let events = runner
            .act(GameAction::ChooseExert { exert: true })
            .unwrap()
            .events;
        assert_eq!(attack_receipts(&events), vec![permission]);
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::ExertChoice { .. }
        ));
        assert_eq!(runner.state().objects[&source].power, Some(power + 1));
        assert!(runner.state().objects[&source].has_keyword(&Keyword::Deathtouch));
        assert!(
            runner.state().extra_phases.is_empty(),
            "an unselected linked payoff must not fire"
        );
        assert_eq!(life(&runner, P1), if prior_exert { 18 } else { 19 });
        assert_eq!(life(&runner, P0), if prior_exert { 22 } else { 21 });
    }
}

/// CR 508.1g–j + CR 607.2h: Both initially eligible costs remain eligible after
/// the first payment, even when their definitions have identical bodies.
#[test]
fn duplicate_conditional_permissions_latch_before_payment_and_restore_exactly() {
    let duplicate = format!("{COMBAT_CELEBRANT}\n{COMBAT_CELEBRANT}");
    let (mut runner, source, _, _) =
        granted_exert_scenario("Two Celebrant permissions", &duplicate, false);
    declare_exert_attacker(&mut runner, source);
    let first = current_permission(&runner);
    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .unwrap()
        .events;
    assert_eq!(attack_receipts(&events), vec![first.clone()]);
    let second = current_permission(&runner);
    assert_ne!(
        first, second,
        "identical payloads still have different occurrence authority"
    );
    assert!(runner.state().exerted_this_turn.contains(&source));
    let restored = restore_after(&runner, |_| {}).expect("current bound queue restores");
    assert_eq!(
        attack_receipts(&restored.pending_attack_trigger_events),
        vec![first]
    );
    let mut runner = GameRunner::from_state(restored);
    assert_eq!(current_permission(&runner), second);
    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .unwrap()
        .events;
    assert_eq!(attack_receipts(&events), vec![second]);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "one linked payoff per payment, never broadcast"
    );
    assert!(runner.state().stack.is_empty());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ExertChoice { .. }
    ));
}

/// Restore-only hostile fixtures: ordinary gameplay gives no priority during
/// optional attack-cost selection. A stale prompt cannot become a fresh grant.
#[test]
fn stale_attack_permission_never_rebinds_to_a_new_source_or_slot() {
    for change_source in [false, true] {
        let duplicate = format!("{BITTERBLADE_WARRIOR}\n{BITTERBLADE_WARRIOR}");
        let (mut runner, source, _, _) =
            granted_exert_scenario("Duplicate Warrior", &duplicate, false);
        declare_exert_attacker(&mut runner, source);
        let original = current_permission(&runner);
        let restored = restore_after(&runner, |_| {}).expect("current prompt restores");
        let mut runner = GameRunner::from_state(restored);
        let object = runner.state_mut().objects.get_mut(&source).unwrap();
        if change_source {
            // CR 400.7: Same storage ID, different all-zone incarnation.
            object.incarnation += 1;
        } else {
            // Removing the selected entry shifts the identical surviving payload
            // into the same display slot; its occurrence remains distinct.
            object
                .trigger_definitions
                .retain(|entry| entry.occurrence != original.occurrence);
        }
        assert!(runner.act(GameAction::ChooseExert { exert: true }).is_err());
        assert!(runner.state().exerted_this_turn.is_empty());
        assert_eq!(current_permission(&runner), original);
    }
}

/// CR 508.1g: Neither an empty declaration nor a generic exert observer grants
/// a vanilla attacker an optional cost it does not actually have.
#[test]
fn generic_observer_does_not_manufacture_an_attack_permission() {
    for mode in 0..3 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let observer_only = "Whenever you exert a creature, this creature deals 1 damage to each opponent and you gain 1 life.";
        let source = scenario
            .add_creature_from_oracle(P0, "Observer only", 3, 3, observer_only)
            .id();
        let vanilla = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let attacker = if mode == 0 { source } else { vanilla };
        let mut runner = scenario.build();
        runner.advance_to_combat();
        let attacks = if mode == 2 {
            vec![]
        } else {
            vec![(attacker, AttackTarget::Player(P1))]
        };
        let events = runner
            .declare_attackers(&attacks)
            .expect("legal declaration")
            .events;
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::ExertChoice { .. }
        ));
        assert!(exerted_events(&events).is_empty());
        assert_eq!(life(&runner, P1), 20);
        if mode != 2 {
            assert!(runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|entry| entry.object_id == attacker));
        }
    }
}

#[test]
fn legacy_unbound_exert_prompts_are_rejected_in_raw_and_trusted_saves() {
    let duplicate = format!("{BITTERBLADE_WARRIOR}\n{BITTERBLADE_WARRIOR}");
    for oracle in [BITTERBLADE_WARRIOR, duplicate.as_str()] {
        let (mut runner, source, _, _) =
            granted_exert_scenario("Permission restore", oracle, false);
        declare_exert_attacker(&mut runner, source);
        let current = restore_after(&runner, |_| {}).expect("positive bound prompt restore");
        let mut restored = GameRunner::from_state(current);
        restored
            .act(GameAction::ChooseExert { exert: false })
            .unwrap();
        if matches!(restored.state().waiting_for, WaitingFor::ExertChoice { .. }) {
            restored
                .act(GameAction::ChooseExert { exert: false })
                .unwrap();
        }
        assert!(!matches!(
            restored.state().waiting_for,
            WaitingFor::ExertChoice { .. }
        ));
        for trusted in [false, true] {
            for corruption in 0..5 {
                let mut json = if trusted {
                    serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
                        .unwrap()
                } else {
                    serde_json::to_value(runner.state()).unwrap()
                };
                let state = if trusted {
                    &mut json["state"]
                } else {
                    &mut json
                };
                let prompt = &mut state["waiting_for"]["data"];
                match corruption {
                    0 => {
                        prompt.as_object_mut().unwrap().remove("permission");
                    }
                    1 => prompt["permission"] = serde_json::Value::Null,
                    2 => prompt["remaining"] = serde_json::json!([source]),
                    3 => prompt["remaining"] = serde_json::json!([{ "attacker": source }]),
                    4 => {
                        prompt["remaining"] =
                            serde_json::json!([{ "attacker": source, "permission": null }])
                    }
                    _ => unreachable!(),
                }
                assert!(
                    serde_json::from_value::<WaitingFor>(state["waiting_for"].clone()).is_err()
                );
                let error = serde_json::from_value::<PersistedGameState>(json)
                    .expect_err("unbound pending cost must not publish")
                    .to_string();
                assert!(
                    error.contains("exact attack permission authority"),
                    "{error}"
                );
            }
        }
    }
}

/// CR 607.2h: Paying one attacker's permission cannot pay the other attacker's
/// identical-looking permission, even though both trigger on exert events.
#[test]
fn separate_attackers_keep_their_own_permission_receipts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_from_oracle(P0, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, "Bitterblade Warrior", 2, 2, BITTERBLADE_WARRIOR)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .unwrap();
    let first_permission = current_permission(&runner);
    let restored = restore_after(&runner, |_| {}).expect("bound remaining queue survives restore");
    let mut runner = GameRunner::from_state(restored);
    assert_eq!(current_permission(&runner), first_permission);
    let events = runner
        .act(GameAction::ChooseExert { exert: true })
        .unwrap()
        .events;
    assert_eq!(attack_receipts(&events), vec![first_permission]);
    let second_permission = current_permission(&runner);
    assert_ne!(second_permission.source.object_id, first);
    assert_eq!(second_permission.source.object_id, second);
    let events = runner
        .act(GameAction::ChooseExert { exert: false })
        .unwrap()
        .events;
    assert!(exerted_events(&events).is_empty());
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&first].power, Some(3));
    assert_eq!(runner.state().objects[&second].power, Some(2));
    assert!(!runner.state().objects[&second].has_keyword(&Keyword::Deathtouch));
    assert!(runner.state().stack.is_empty());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ExertChoice { .. }
    ));
}

/// Historical completed events retain their old linked-trigger meaning. An
/// absent actor still cannot satisfy an observer's "you exert" player filter.
#[test]
fn historical_exert_events_preserve_linked_matching_without_inventing_an_actor() {
    for actor_present in [false, true] {
        let (mut runner, source, _, _) =
            granted_exert_scenario("Bitterblade Warrior", BITTERBLADE_WARRIOR, true);
        let power = runner.state().objects[&source].power.unwrap();
        declare_exert_attacker(&mut runner, source);
        let mut event =
            serde_json::json!({"type": "CreatureExerted", "data": {"object_id": source}});
        if actor_present {
            event["data"]["player"] = serde_json::to_value(P0).unwrap();
        }
        let restored = restore_after(&runner, |json| push_pending_attack_event(json, event))
            .expect("completed historical event restores");
        let mut runner = GameRunner::from_state(restored);
        let events = runner
            .act(GameAction::ChooseExert { exert: false })
            .unwrap()
            .events;
        assert!(
            exerted_events(&events).is_empty(),
            "no current payment was made"
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&source].power,
            Some(power + 1),
            "historical receipt-less attack event still matches its linked trigger"
        );
        assert_eq!(life(&runner, P1), if actor_present { 19 } else { 20 });
        assert_eq!(life(&runner, P0), if actor_present { 21 } else { 20 });
        assert!(runner.state().stack.is_empty());
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::ExertChoice { .. }
        ));
    }
}
