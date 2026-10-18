//! Steward of the Harvest — "Creatures you control have all activated abilities of
//! all land cards exiled with this creature." (CR 607.2a linked exile ability;
//! CR 613.1f layer-6 ability grant; CR 205.2a land card type qualifier;
//! CR 400.7 zone-change invalidation; CR 302.6 summoning sickness.)
//!
//! The whole pipeline is exercised: the verbatim Oracle text is parsed, Steward
//! is cast through the real stack, its ETB trigger exiles the chosen graveyard
//! lands (recording the CR 607.2a link — same ETB-link path as
//! `hazel_of_the_rootbloom.rs`'s trigger-driven resolution), and the layer system
//! grants the exiled lands' activated abilities to Steward's controller's
//! creatures.
//!
//! REVERT DISCRIMINATORS:
//!   * Removing the `land ` arm of `grant_exiled_card_type_qualifier`
//!     (parser/oracle_static/keyword_grant.rs) leaves the static line
//!     `Unimplemented`, so `steward_grants_exiled_land_abilities_to_creatures`
//!     fails on its first grant assertion (the creature gains no abilities).
//!   * Widening the qualifier to a bare `ExiledBySource` makes
//!     `non_land_card_exiled_with_steward_donates_nothing` fail (the exiled
//!     creature card's ability would be granted).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaPipId, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Steward of the Harvest, `{3}{G}` Creature — Human Druid 3/3. Verbatim from
/// Scryfall / `client/public/card-data.json`.
const STEWARD: &str = "When this creature enters, exile up to three target land cards from your graveyard.\nCreatures you control have all activated abilities of all land cards exiled with this creature.";

/// Strip Mine — verbatim: one mana ability and one non-mana activated ability.
const STRIP_MINE: &str = "{T}: Add {C}.\n{T}, Sacrifice this land: Destroy target land.";

/// Breeding Pool — verbatim (a shock land: two intrinsic mana abilities from its
/// Forest Island types plus an enters-tapped/pay-life replacement).
const BREEDING_POOL: &str = "({T}: Add {G} or {U}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";

/// Maze of Ith — verbatim: a non-mana `{T}` activated ability only.
const MAZE_OF_ITH: &str = "{T}: Untap target attacking creature. Prevent all combat damage that would be dealt to and dealt by that creature this turn.";

fn mana_ability(color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
}

fn fund_green(state: &mut GameState, count: u32) {
    let pool = &mut state.players[P0.0 as usize].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(0),
            pip_id: ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

struct Board {
    runner: GameRunner,
    steward: ObjectId,
    forest: ObjectId,
    strip_mine: ObjectId,
    breeding_pool: ObjectId,
    /// A fourth land card, left in the graveyard when three are chosen.
    maze: ObjectId,
    own_creature: ObjectId,
    opp_creature: ObjectId,
}

/// P0 holds Steward in hand with {3}{G} in pool; four land cards sit in P0's
/// graveyard; P0 and P1 each control a non-summoning-sick vanilla creature.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let steward = scenario
        .add_creature_to_hand_from_oracle(P0, "Steward of the Harvest", 3, 3, STEWARD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .id();

    let forest = {
        let mut b = scenario.add_land_to_graveyard(P0, "Forest");
        b.with_subtypes(vec!["Forest"])
            .with_ability_definition(mana_ability(ManaColor::Green));
        b.id()
    };
    let strip_mine = {
        let mut b = scenario.add_land_to_graveyard(P0, "Strip Mine");
        b.from_oracle_text(STRIP_MINE);
        b.id()
    };
    let breeding_pool = {
        let mut b = scenario.add_land_to_graveyard(P0, "Breeding Pool");
        b.with_subtypes(vec!["Forest", "Island"])
            .from_oracle_text(BREEDING_POOL);
        // The export synthesizes the two intrinsic mana abilities (CR 305.6)
        // from the basic land types; the scenario Oracle parse does not.
        b.with_ability_definition(mana_ability(ManaColor::Blue))
            .with_ability_definition(mana_ability(ManaColor::Green));
        b.id()
    };
    let maze = {
        let mut b = scenario.add_land_to_graveyard(P0, "Maze of Ith");
        b.from_oracle_text(MAZE_OF_ITH);
        b.id()
    };

    let own_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let opp_creature = scenario.add_creature(P1, "Opposing Bears", 2, 2).id();
    // A battlefield land so Strip Mine's granted "destroy target land" has a
    // legal target (the refusal under test must come from CR 302.6, not from a
    // missing target).
    scenario.add_basic_land(P1, ManaColor::Red);

    let mut runner = scenario.build();
    fund_green(runner.state_mut(), 4);
    Board {
        runner,
        steward,
        forest,
        strip_mine,
        breeding_pool,
        maze,
        own_creature,
        opp_creature,
    }
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn steward_links(runner: &GameRunner, steward: ObjectId) -> Vec<ObjectId> {
    runner
        .state()
        .exile_links
        .iter()
        .filter(|l| l.source_id == steward && l.kind == ExileLinkKind::TrackedBySource)
        .map(|l| l.exiled_id)
        .collect()
}

fn activated_count(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .filter(|a| matches!(a.kind, AbilityKind::Activated))
        .count()
}

fn mana_ability_index(runner: &GameRunner, id: ObjectId, color: ManaColor) -> Option<usize> {
    runner.state().objects[&id].abilities.iter().position(|a| {
        matches!(
            a.effect.as_ref(),
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } if colors == &vec![color]
        )
    })
}

fn destroy_ability_index(runner: &GameRunner, id: ObjectId) -> Option<usize> {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::Destroy { .. }))
}

fn has_destroy_ability(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .abilities
        .iter()
        .any(|a| matches!(a.effect.as_ref(), Effect::Destroy { .. }))
}

fn green_in_pool(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(ManaType::Green)
}

/// Cast Steward choosing `targets` for the ETB, run it to an empty stack, and
/// recompute layers.
fn cast_steward(b: &mut Board, targets: &[ObjectId]) {
    b.runner.cast(b.steward).target_objects(targets).resolve();
    evaluate_layers(b.runner.state_mut());
}

/// Primary: the real ETB exiles the chosen lands, records the CR 607.2a links,
/// and every creature P0 controls gains exactly those lands' activated
/// abilities; the shock land's replacement is not granted.
#[test]
fn steward_grants_exiled_land_abilities_to_creatures() {
    let mut b = board();
    let picks = [b.forest, b.strip_mine, b.breeding_pool];
    cast_steward(&mut b, &picks);

    // Reach-guard (a): the ETB exiled the three chosen lands and recorded a
    // TrackedBySource link to Steward for each; the unchosen fourth stayed.
    for land in [b.forest, b.strip_mine, b.breeding_pool] {
        assert_eq!(zone_of(&b.runner, land), Zone::Exile, "land {land:?}");
    }
    assert_eq!(zone_of(&b.runner, b.maze), Zone::Graveyard);
    let mut links = steward_links(&b.runner, b.steward);
    links.sort();
    let mut expected = vec![b.forest, b.strip_mine, b.breeding_pool];
    expected.sort();
    assert_eq!(
        links, expected,
        "ETB must link exactly the three exiled lands"
    );
    assert_eq!(zone_of(&b.runner, b.steward), Zone::Battlefield);

    // (b) Forest {T}: Add {G}, Strip Mine {T}: Add {C} and its destroy ability,
    // Breeding Pool {T}: Add {U} and {T}: Add {G}. The layer-6 grant dedups
    // structurally identical abilities (Forest's and Breeding Pool's green
    // abilities), leaving 4 distinct activated abilities on a creature with no
    // printed abilities.
    assert_eq!(activated_count(&b.runner, b.own_creature), 4);
    assert!(mana_ability_index(&b.runner, b.own_creature, ManaColor::Green).is_some());
    assert!(mana_ability_index(&b.runner, b.own_creature, ManaColor::Blue).is_some());
    assert!(has_destroy_ability(&b.runner, b.own_creature));
    // Steward is itself a creature you control.
    assert_eq!(activated_count(&b.runner, b.steward), 4);

    // The Maze of Ith stayed in the graveyard: its ability is not granted.
    assert!(!b.runner.state().objects[&b.own_creature]
        .abilities
        .iter()
        .any(|a| a
            .description
            .as_deref()
            .is_some_and(|d| d.contains("Untap"))));

    // Only ACTIVATED abilities (and not the shock land's replacement) are gained.
    assert!(b.runner.state().objects[&b.own_creature]
        .replacement_definitions
        .is_empty());
    assert!(b.runner.state().objects[&b.own_creature]
        .trigger_definitions
        .is_empty());

    // (c) An opponent's creature gets nothing (paired with (b) as reach-guard).
    assert_eq!(activated_count(&b.runner, b.opp_creature), 0);
}

/// (f) The granted mana ability really works for a creature that is not
/// summoning-sick: tapping it for {G} through `GameAction` adds green mana.
#[test]
fn granted_mana_ability_activates_on_a_ready_creature() {
    let mut b = board();
    let picks = [b.forest];
    cast_steward(&mut b, &picks);
    // The cast spent the funded pool; reach-guard that the grant exists.
    assert_eq!(green_in_pool(&b.runner), 0);
    let idx = mana_ability_index(&b.runner, b.own_creature, ManaColor::Green)
        .expect("creature must gain Forest's mana ability");

    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.own_creature,
            ability_index: idx,
        })
        .expect("a ready creature may tap for the granted mana ability");
    assert_eq!(green_in_pool(&b.runner), 1);
    assert!(b.runner.state().objects[&b.own_creature].tapped);
}

/// CR 302.6: direct mana activation must reject the newly cast Steward before
/// tapping it or producing mana. The ready recipient uses the identical grant.
#[test]
fn summoning_sick_steward_cannot_use_granted_mana_ability() {
    let mut b = board();
    let picks = [b.forest];
    cast_steward(&mut b, &picks);
    let idx = mana_ability_index(&b.runner, b.steward, ManaColor::Green)
        .expect("Steward must gain Forest's mana ability");
    assert!(b.runner.state().objects[&b.steward].summoning_sick);
    assert_eq!(green_in_pool(&b.runner), 0);

    let err = b
        .runner
        .act(GameAction::ActivateAbility {
            source_id: b.steward,
            ability_index: idx,
        })
        .expect_err("summoning-sick Steward must not activate a granted mana ability");
    assert!(
        format!("{err:?}").contains("summoning sickness"),
        "refusal must be the CR 302.6 gate, got {err:?}"
    );
    assert!(!b.runner.state().objects[&b.steward].tapped);
    assert_eq!(green_in_pool(&b.runner), 0);

    let ready_idx = mana_ability_index(&b.runner, b.own_creature, ManaColor::Green)
        .expect("ready creature must gain the same Forest ability");
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.own_creature,
            ability_index: ready_idx,
        })
        .expect("ready recipient may use the same granted mana ability");
    assert!(b.runner.state().objects[&b.own_creature].tapped);
    assert_eq!(green_in_pool(&b.runner), 1);
}

/// CR 702.10c: haste permits a newly cast creature's {T} mana ability.
#[test]
fn newly_cast_hasty_creature_can_activate_mana_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Hasty mana creature", 1, 1, "{T}: Add {G}.")
        .haste()
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(creature)
        .resolve()
        .assert_zone(&[creature], Zone::Battlefield);
    assert!(runner.state().objects[&creature].summoning_sick);
    let idx = mana_ability_index(&runner, creature, ManaColor::Green)
        .expect("creature has its printed mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: creature,
            ability_index: idx,
        })
        .expect("haste permits tapping the newly cast mana creature");
    assert!(runner.state().objects[&creature].tapped);
    assert_eq!(green_in_pool(&runner), 1);
}

/// CR 302.6: the {T}/{Q} control-duration restriction applies only to creatures.
#[test]
fn noncreature_land_mana_activation_ignores_summoning_sickness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_from_oracle(P0, "Mana land", "{T}: Add {G}.")
        .with_summoning_sickness()
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().objects[&land].summoning_sick);
    let idx =
        mana_ability_index(&runner, land, ManaColor::Green).expect("Forest has its mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: idx,
        })
        .expect("a noncreature land may tap immediately");
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(green_in_pool(&runner), 1);
}

/// CR 302.6: a creature that came under its controller's control this turn
/// cannot activate a granted `{T}` ability. Steward itself is summoning-sick, so
/// Strip Mine's granted `{T}, Sacrifice` destroy ability is refused for it but
/// accepted for the ready creature.
#[test]
fn summoning_sick_steward_cannot_use_granted_tap_ability() {
    let mut b = board();
    let picks = [b.strip_mine];
    cast_steward(&mut b, &picks);
    let sick_idx = destroy_ability_index(&b.runner, b.steward)
        .expect("Steward must gain Strip Mine's destroy ability");
    let ready_idx = destroy_ability_index(&b.runner, b.own_creature)
        .expect("ready creature must gain Strip Mine's destroy ability");
    assert!(b.runner.state().objects[&b.steward].summoning_sick);
    assert!(!b.runner.state().objects[&b.own_creature].summoning_sick);

    let result = b.runner.act(GameAction::ActivateAbility {
        source_id: b.steward,
        ability_index: sick_idx,
    });
    let err = result.expect_err("summoning-sick creature must be refused");
    assert!(
        format!("{err:?}").contains("summoning sickness"),
        "refusal must be the CR 302.6 gate, got {err:?}"
    );
    assert!(!b.runner.state().objects[&b.steward].tapped);

    // Positive control: the ready creature is accepted (reaches target choice).
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.own_creature,
            ability_index: ready_idx,
        })
        .expect("a ready creature may activate the granted {T} ability");
}

/// CR 607.2a: only cards exiled with THIS Steward donate. A land exiled by a
/// different source donates nothing; neither does a non-land card exiled with
/// Steward (this is what the `land` qualifier discriminates from a bare
/// `ExiledBySource`). Each negative sits beside the positive grant.
#[test]
fn non_land_card_exiled_with_steward_donates_nothing() {
    let mut b = board();
    let picks = [b.forest];
    cast_steward(&mut b, &picks);
    // Positive reach-guard: exactly Forest's one ability is granted.
    assert_eq!(activated_count(&b.runner, b.own_creature), 1);

    // (d) A land exiled by some other object, with a distinctive ability.
    let other_exiler = b.runner.state().battlefield[0];
    assert_ne!(other_exiler, b.steward);
    let foreign_land = {
        let state = b.runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(9001),
            P0,
            "Foreign Land".to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
        let ability = mana_ability(ManaColor::Red);
        std::sync::Arc::make_mut(&mut obj.abilities).push(ability.clone());
        std::sync::Arc::make_mut(&mut obj.base_abilities).push(ability);
        state.exile_links.push(ExileLink {
            exiled_id: id,
            source_id: other_exiler,
            kind: ExileLinkKind::TrackedBySource,
        });
        id
    };
    let _ = foreign_land;

    // (e) A non-land (creature) card exiled with Steward, with a mana ability.
    {
        let state = b.runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(9002),
            P0,
            "Exiled Elf".to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        let ability = mana_ability(ManaColor::White);
        std::sync::Arc::make_mut(&mut obj.abilities).push(ability.clone());
        std::sync::Arc::make_mut(&mut obj.base_abilities).push(ability);
        state.exile_links.push(ExileLink {
            exiled_id: id,
            source_id: b.steward,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    evaluate_layers(b.runner.state_mut());

    assert_eq!(activated_count(&b.runner, b.own_creature), 1);
    assert!(mana_ability_index(&b.runner, b.own_creature, ManaColor::Green).is_some());
    assert!(mana_ability_index(&b.runner, b.own_creature, ManaColor::Red).is_none());
    assert!(mana_ability_index(&b.runner, b.own_creature, ManaColor::White).is_none());
}

/// CR 400.7: when Steward leaves the battlefield the static ends and the grants
/// disappear. Positive reach-guard first.
#[test]
fn grants_end_when_steward_leaves_the_battlefield() {
    let mut b = board();
    let picks = [b.forest, b.strip_mine];
    cast_steward(&mut b, &picks);
    assert_eq!(activated_count(&b.runner, b.own_creature), 3);

    let mut events = Vec::new();
    assert!(!move_object_for_test(
        b.runner.state_mut(),
        ZoneMoveRequest::effect(b.steward, Zone::Graveyard, b.steward),
        &mut events,
    ));
    evaluate_layers(b.runner.state_mut());
    assert_eq!(zone_of(&b.runner, b.steward), Zone::Graveyard);
    assert_eq!(activated_count(&b.runner, b.own_creature), 0);
}

/// "Up to three": choosing zero targets exiles nothing and grants nothing; four
/// declared lands exile exactly three.
#[test]
fn up_to_three_target_bounds() {
    // Empty selection.
    let mut b = board();
    let picks = [];
    cast_steward(&mut b, &picks);
    assert_eq!(zone_of(&b.runner, b.steward), Zone::Battlefield);
    for land in [b.forest, b.strip_mine, b.breeding_pool, b.maze] {
        assert_eq!(zone_of(&b.runner, land), Zone::Graveyard);
    }
    assert!(steward_links(&b.runner, b.steward).is_empty());
    assert_eq!(activated_count(&b.runner, b.own_creature), 0);

    // Four offered, three taken.
    let mut b = board();
    let picks = [b.forest, b.strip_mine, b.breeding_pool, b.maze];
    cast_steward(&mut b, &picks);
    let exiled = [b.forest, b.strip_mine, b.breeding_pool, b.maze]
        .iter()
        .filter(|id| zone_of(&b.runner, **id) == Zone::Exile)
        .count();
    assert_eq!(exiled, 3);
    assert_eq!(steward_links(&b.runner, b.steward).len(), 3);
}
