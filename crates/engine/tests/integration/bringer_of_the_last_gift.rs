use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Flying\nWhen this creature enters, if you cast it, each player sacrifices all other creatures they control. Then each player returns all creature cards from their graveyard that weren't put there this way to the battlefield.";
const MURDER_ORACLE: &str = "Destroy target creature.";

/// Printed cost {6}{B}{B}.
fn bringer_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 6,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    }
}

/// Printed cost {1}{B}{B}.
fn murder_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    }
}

fn black_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

struct Setup {
    runner: GameRunner,
    bringer: ObjectId,
    prior_graveyard_creatures: [ObjectId; 2],
    sacrificed_creatures: [ObjectId; 2],
}

fn setup() -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    scenario.with_mana_pool(P0, black_mana(8));
    let prior_graveyard_creatures = [
        scenario
            .add_creature_to_graveyard(P0, "Prior P0 Creature", 2, 2)
            .id(),
        scenario
            .add_creature_to_graveyard(P1, "Prior P1 Creature", 2, 2)
            .id(),
    ];
    let sacrificed_creatures = [
        scenario.add_creature(P0, "P0 Creature", 2, 2).id(),
        scenario.add_creature(P1, "P1 Creature", 2, 2).id(),
    ];
    Setup {
        runner: scenario.build(),
        bringer,
        prior_graveyard_creatures,
        sacrificed_creatures,
    }
}

fn resolve_cast_and_trigger(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Bringer's cast trigger should resolve without prompting"
    );
}

#[test]
fn returns_prior_graveyard_creatures_but_not_those_its_sacrifice_put_there() {
    let mut setup = setup();
    resolve_cast_and_trigger(&mut setup.runner, setup.bringer);

    // Reach guard: the trigger ran and returned the creatures that were
    // already in graveyards, so the negative assertion below is not vacuous.
    for creature in setup.prior_graveyard_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Battlefield,
            "creatures already in graveyards should return"
        );
    }
    for creature in setup.sacrificed_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Graveyard,
            "creatures sacrificed by this trigger must be excluded from its return"
        );
    }
}

/// An empty sacrifice publishes an empty chain set, so "this way" excludes
/// nothing on the second Bringer even though an earlier Bringer's sacrifice
/// published a non-empty set. Falling back to the stale earlier set would keep
/// the first sacrificed creature in the graveyard.
#[test]
fn empty_sacrifice_excludes_nothing_despite_an_earlier_bringers_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let bringer_b = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .with_mana_cost(murder_cost())
        .id();
    scenario.with_mana_pool(P0, black_mana(8 + 3 + 8));
    let creature_x = scenario.add_creature(P0, "Creature X", 2, 2).id();
    let mut runner = scenario.build();

    // First Bringer sacrifices X, publishing a non-empty tracked set.
    resolve_cast_and_trigger(&mut runner, bringer_a);
    assert_eq!(
        runner.state().objects[&bringer_a].zone,
        Zone::Battlefield,
        "Bringer A stays (it sacrifices only *other* creatures)"
    );
    assert_eq!(
        runner.state().objects[&creature_x].zone,
        Zone::Graveyard,
        "X was sacrificed by Bringer A and excluded from its return"
    );

    // Murder destroys Bringer A; it publishes no tracked set.
    runner.cast(murder).target_object(bringer_a).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&bringer_a].zone,
        Zone::Graveyard,
        "Murder should destroy Bringer A"
    );

    // Second Bringer finds no other creatures: nothing is put into a graveyard
    // "this way", so both X and Bringer A return.
    resolve_cast_and_trigger(&mut runner, bringer_b);
    assert_eq!(runner.state().objects[&bringer_b].zone, Zone::Battlefield);
    for (name, id) in [("X", creature_x), ("Bringer A", bringer_a)] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "{name} was not put into a graveyard by the second sacrifice and must return"
        );
    }
}

const GEMRAZER_ORACLE: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";
const RAISE_THE_ALARM_ORACLE: &str = "Create two 1/1 white Soldier creature tokens.";
const ZOMBIFY_ORACLE: &str = "Return target creature card from your graveyard to the battlefield.";

/// Printed cost {1}{W}.
fn raise_the_alarm_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::White],
    }
}

/// Printed cost {3}{B}.
fn zombify_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 3,
        shards: vec![ManaCostShard::Black],
    }
}

/// SYNTHETIC STEP: merge `rider` under or over `host` directly with
/// `merge_object_onto` instead of a real mutate cast (precedent:
/// `exile_attribution_10414.rs`). The rider leaves hand and battlefield first,
/// as a resolving mutate spell would.
fn stage_merge(runner: &mut GameRunner, rider: ObjectId, host: ObjectId, side: MergeSide) {
    let state = runner.state_mut();
    for player in state.players.iter_mut() {
        player.hand.retain(|&id| id != rider);
    }
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();
    merge_object_onto(state, rider, host, side, &mut events);
    state.battlefield.retain(|&id| id != rider);
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|object| object.zone)
}

fn soldier_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    let mut soldiers: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.is_token && object.name == "Soldier")
        })
        .collect();
    soldiers.sort_by_key(|id| id.0);
    soldiers
}

/// CR 730.3c: "If an effect can find the new object that a merged permanent
/// becomes as it leaves the battlefield, it finds all of those objects."
/// Bringer of the Last Gift's "weren't put there this way" reads the set its
/// sacrifice published; the card-topped merged creature P0 controls is
/// sacrificed, and its component Gemrazer (put into the graveyard by the same
/// leave, CR 730.3) is found with it and excluded from the return.
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto` (real Bringer
/// and Gemrazer Oracle text; the host is a vanilla Beast, standing in for any
/// non-Human creature Gemrazer could have mutated onto).
///
/// Revert sensitivity: without the component finding at the `InTrackedSet`
/// predicate, Gemrazer is not "put there this way" and returns to the
/// battlefield.
#[test]
fn merged_component_of_a_sacrificed_creature_is_excluded_card_topped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let host = scenario
        .add_creature(P0, "Host Beast", 2, 2)
        .with_subtypes(vec!["Beast"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
        .with_subtypes(vec!["Beast"])
        .id();
    let prior = scenario
        .add_creature_to_graveyard(P1, "Prior P1 Creature", 2, 2)
        .id();
    scenario.with_mana_pool(P0, black_mana(8));
    let mut runner = scenario.build();
    stage_merge(&mut runner, gemrazer, host, MergeSide::Top);
    assert_eq!(
        runner.state().objects[&host].merged_components.len(),
        2,
        "reach guard: the host is a merged permanent of two cards"
    );

    resolve_cast_and_trigger(&mut runner, bringer);

    assert_eq!(
        zone_of(&runner, prior),
        Some(Zone::Battlefield),
        "positive control: a creature already in a graveyard returns"
    );
    assert_eq!(
        zone_of(&runner, host),
        Some(Zone::Graveyard),
        "the sacrificed survivor stays in the graveyard"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "CR 730.3c: the component put there by the same leave is found with it and excluded"
    );
}

/// CR 730.3c + CR 111.8 + CR 704.5d: the token-topped sibling. P0's Soldier
/// token is the survivor; the sacrifice moves it, it ceases to exist, and its
/// component Gemrazer is the one card left in the graveyard "this way".
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto` (Raise the
/// Alarm's real Oracle text makes the token host).
///
/// Revert sensitivity: as for the card-topped row.
#[test]
fn merged_component_of_a_sacrificed_creature_is_excluded_token_topped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let raise = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM_ORACLE)
        .with_mana_cost(raise_the_alarm_cost())
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
        .with_subtypes(vec!["Beast"])
        .id();
    let prior = scenario
        .add_creature_to_graveyard(P1, "Prior P1 Creature", 2, 2)
        .id();
    let mut pool = black_mana(8);
    pool.extend((0..2).map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.cast(raise).resolve();
    runner.advance_until_stack_empty();
    let soldiers = soldier_tokens(&runner);
    assert_eq!(
        soldiers.len(),
        2,
        "reach guard: Raise the Alarm made two tokens"
    );
    let survivor = soldiers[0];
    stage_merge(&mut runner, gemrazer, survivor, MergeSide::Bottom);
    assert_eq!(
        runner.state().objects[&survivor].merged_components.len(),
        2,
        "reach guard: the token is a merged permanent of two components"
    );

    resolve_cast_and_trigger(&mut runner, bringer);

    assert_eq!(
        zone_of(&runner, prior),
        Some(Zone::Battlefield),
        "positive control: a creature already in a graveyard returns"
    );
    assert_eq!(
        zone_of(&runner, survivor),
        None,
        "CR 111.8 + CR 704.5d: the sacrificed token ceased to exist"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "CR 730.3c: the component put there by the same leave is found with it and excluded"
    );
}

/// CR 730.3c + CR 400.7 (B2): a component split at an EARLIER departure of the
/// same host id is not "put there this way". Murder destroys the merged
/// creature (host and Gemrazer to the graveyard; Gemrazer pinned to the host's
/// first successor), Zombify returns only the host (a new object), and Bringer
/// then sacrifices that host. Gemrazer was already in the graveyard, so it
/// returns; the host, sacrificed by this trigger, does not.
///
/// SYNTHETIC STEP: the merge is staged with `merge_object_onto` (real Murder,
/// Zombify, Bringer and Gemrazer Oracle text).
///
/// Revert sensitivity: an id-only match of the pin (ignoring the incarnation)
/// finds Gemrazer through the host's later sacrifice and keeps it in the
/// graveyard.
#[test]
fn b2_zombify_returns_host_then_bringer_leaves_the_earlier_component_returning() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .with_mana_cost(murder_cost())
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY_ORACLE)
        .with_mana_cost(zombify_cost())
        .id();
    let host = scenario
        .add_creature(P0, "Host Beast", 2, 2)
        .with_subtypes(vec!["Beast"])
        .id();
    let gemrazer = scenario
        .add_creature_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER_ORACLE)
        .with_subtypes(vec!["Beast"])
        .id();
    scenario.with_mana_pool(P0, black_mana(8 + 3 + 4));
    let mut runner = scenario.build();
    stage_merge(&mut runner, gemrazer, host, MergeSide::Top);

    runner.cast(murder).target_object(host).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, host), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, gemrazer), Some(Zone::Graveyard));
    runner.cast(zombify).target_object(host).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        zone_of(&runner, host),
        Some(Zone::Battlefield),
        "reach guard: Zombify returned the host"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Graveyard),
        "reach guard: Zombify returned only the card it targeted"
    );

    resolve_cast_and_trigger(&mut runner, bringer);

    assert_eq!(
        zone_of(&runner, host),
        Some(Zone::Graveyard),
        "the host sacrificed by this trigger is excluded from its return"
    );
    assert_eq!(
        zone_of(&runner, gemrazer),
        Some(Zone::Battlefield),
        "CR 730.3c: Gemrazer was not put into the graveyard this way, so it returns"
    );
}
