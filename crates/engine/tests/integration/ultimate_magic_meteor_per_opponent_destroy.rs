//! Per-opponent battlefield choice acted on as a set — Ultimate Magic: Meteor.
//!
//! Oracle text: "Ultimate Magic: Meteor deals 7 damage to each creature. If this
//! spell was cast from exile, for each opponent, choose an artifact or land that
//! player controls. Destroy the chosen permanents." Foretell {5}{R}.
//!
//! The spell's controller makes every choice (CR 608.2c + CR 608.2d), one per
//! opponent (CR 102.2 + CR 102.3), and none of them is a target (CR 115.10a).
//! Each choice draws only from permanents THAT opponent controls, so the pools
//! are disjoint and no earlier pick changes a later pool; the chosen permanents
//! are then destroyed together (CR 701.8a). The walk order over opponents is an
//! implementation detail (CR 101.4c: one player orders their own choices), so
//! the tests identify each prompt by the controller of its candidates.
//!
//! When the spell was not cast from exile nothing is chosen and nothing is
//! destroyed — including the creatures its own damage step published as a
//! tracked set (CR 608.2c + CR 609.3).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ContinuousModification, Duration, Effect, PerPlayerScope,
    SubAbilityLink, TargetFilter, ZoneOwner,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const METEOR: &str = "Ultimate Magic: Meteor deals 7 damage to each creature. If this spell was cast from exile, for each opponent, choose an artifact or land that player controls. Destroy the chosen permanents.\nForetell {5}{R} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn parse(text: &str, name: &str, keywords: &[&str], types: &[&str]) -> ParsedAbilities {
    let keywords: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(text, name, &keywords, &types, &[])
}

/// Every definition reachable from the parsed abilities and trigger bodies,
/// following `sub_ability` / `else_ability`.
fn all_defs(parsed: &ParsedAbilities) -> Vec<&AbilityDefinition> {
    fn push<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
        out.push(def);
        if let Some(sub) = def.sub_ability.as_deref() {
            push(sub, out);
        }
        if let Some(other) = def.else_ability.as_deref() {
            push(other, out);
        }
    }
    let mut out = Vec::new();
    for def in &parsed.abilities {
        push(def, &mut out);
    }
    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            push(execute, &mut out);
        }
    }
    out
}

fn has_unimplemented(parsed: &ParsedAbilities) -> bool {
    all_defs(parsed)
        .iter()
        .any(|d| matches!(&*d.effect, Effect::Unimplemented { .. }))
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|o| o.zone)
}

fn on_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().battlefield.contains(&id)
}

fn add_mana(runner: &mut GameRunner, colorless: usize, red: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .mana_pool;
    for _ in 0..colorless {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..red {
        pool.add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
}

fn meteor_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 5,
    }
}

fn add_meteor(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand(P0, "Ultimate Magic: Meteor", false);
    builder
        .from_oracle_text_with_keywords(&["Foretell"], METEOR)
        .with_mana_cost(meteor_cost());
    builder.id()
}

fn add_artifact(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario.add_artifact_from_oracle(player, name, "").id()
}

fn add_land(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario.add_land_from_oracle(player, name, "").id()
}

/// Make an existing battlefield creature an artifact creature too.
fn make_artifact_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).expect("object");
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types.core_types.push(CoreType::Artifact);
}

/// Foretell Meteor (CR 702.143a), move to a later turn, and cast it from exile
/// for its foretell cost.
fn cast_from_exile(runner: &mut GameRunner, meteor: ObjectId) {
    add_mana(runner, 2, 0);
    let card_id = runner.state().objects[&meteor].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: meteor,
            card_id,
        })
        .expect("foretell special action");
    assert_eq!(zone_of(runner, meteor), Some(Zone::Exile));
    let turn = runner.state().turn_number;
    runner.state_mut().turn_number = turn + 1;
    add_mana(runner, 5, 1);
    runner
        .act(GameAction::CastSpell {
            object_id: meteor,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from foretell exile");
    let entry = runner.state().stack.last().expect("Meteor on the stack");
    assert_eq!(entry.id, meteor, "reach: Meteor is the cast spell");
}

fn cast_from_hand(runner: &mut GameRunner, meteor: ObjectId) {
    add_mana(runner, 5, 1);
    let card_id = runner.state().objects[&meteor].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: meteor,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
    assert_eq!(zone_of(runner, meteor), Some(Zone::Stack));
}

#[derive(Debug, Clone)]
struct Prompt {
    chooser: PlayerId,
    cards: Vec<ObjectId>,
}

/// Resolve the stack, answering each per-opponent prompt with `pick`, and
/// return every prompt seen. `between` runs after each answered prompt.
fn resolve_with(
    runner: &mut GameRunner,
    mut pick: impl FnMut(&GameRunner, &[ObjectId]) -> Vec<ObjectId>,
    mut between: impl FnMut(&mut GameRunner, usize),
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                prompts.push(Prompt {
                    chooser: player,
                    cards: cards.clone(),
                });
                let chosen = pick(runner, &cards);
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("a legal pick is accepted");
                between(runner, prompts.len());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }
    prompts
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

/// The single controller of every candidate in a prompt — which opponent the
/// prompt is for.
fn pool_owner(runner: &GameRunner, prompt: &Prompt) -> PlayerId {
    let owner = controller(runner, prompt.cards[0]);
    assert!(
        prompt
            .cards
            .iter()
            .all(|id| controller(runner, *id) == owner),
        "a per-opponent pool holds only one opponent's permanents: {prompt:?}"
    );
    owner
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------
// T1: parse
// ---------------------------------------------------------------------------

/// The full chain: damage, then a cast-from-exile-gated per-opponent choice of an
/// artifact or land, then a set-wide destroy of the chosen permanents that is a
/// continuation of the gated choice (so it is skipped with it).
#[test]
fn meteor_parses_to_gated_per_opponent_choice_and_set_destroy() {
    let parsed = parse(
        METEOR,
        "Ultimate Magic: Meteor",
        &["Foretell"],
        &["Sorcery"],
    );
    let dbg = format!("{:#?}", parsed.abilities);
    assert!(!has_unimplemented(&parsed), "no gaps expected:\n{dbg}");

    let root = &parsed.abilities[0];
    assert!(matches!(&*root.effect, Effect::DamageAll { .. }), "{dbg}");
    let choose = root.sub_ability.as_deref().expect("choose clause");
    match &*choose.effect {
        Effect::ChooseFromZone {
            count,
            zone,
            zone_owner,
            filter,
            up_to,
            ..
        } => {
            assert_eq!(*count, 1);
            assert_eq!(*zone, Zone::Battlefield);
            assert_eq!(*zone_owner, ZoneOwner::Each(PerPlayerScope::Opponents));
            assert!(!*up_to, "exactly one per opponent");
            let filter = format!("{filter:?}");
            assert!(
                filter.contains("Artifact") && filter.contains("Land"),
                "artifact-or-land filter: {filter}"
            );
        }
        other => panic!("expected ChooseFromZone, got {other:?}"),
    }
    assert!(
        matches!(
            choose.condition,
            Some(AbilityCondition::WasCast {
                zone: Some(Zone::Exile)
            })
        ),
        "the choice is gated on cast-from-exile: {:?}",
        choose.condition
    );
    assert!(choose.repeat_for.is_none(), "no bare repeat count: {dbg}");
    let destroy = choose.sub_ability.as_deref().expect("destroy clause");
    assert!(
        matches!(
            &*destroy.effect,
            Effect::DestroyAll {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "set-wide destroy over the chosen set: {dbg}"
    );
    assert_eq!(
        destroy.sub_link,
        SubAbilityLink::ContinuationStep,
        "the destroy is skipped together with the gated choice"
    );
}

// ---------------------------------------------------------------------------
// T2 / T5: cast from exile, three players
// ---------------------------------------------------------------------------

#[test]
fn foretold_meteor_destroys_one_chosen_artifact_or_land_per_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 101);
    scenario.at_phase(Phase::PreCombatMain);
    let p0_art = add_artifact(&mut scenario, P0, "P0 Relic");
    let p0_land = add_land(&mut scenario, P0, "P0 Land");
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p1_land = add_land(&mut scenario, P1, "P1 Land");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let p2_giant = scenario.add_creature(P2, "P2 Giant", 8, 8).id();
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    let prompts = resolve_with(
        &mut runner,
        |r, cards| {
            // P1's artifact, P2's land.
            let owner = controller(r, cards[0]);
            let want = if owner == P1 { p1_art } else { p2_land };
            vec![want]
        },
        |_, _| {},
    );

    assert_eq!(prompts.len(), 2, "one prompt per opponent: {prompts:?}");
    assert!(
        prompts.iter().all(|p| p.chooser == P0),
        "the spell's controller makes every choice: {prompts:?}"
    );
    // Identify each prompt by its pool, not by position.
    let mut owners: Vec<PlayerId> = prompts.iter().map(|p| pool_owner(&runner, p)).collect();
    owners.sort();
    assert_eq!(
        owners,
        vec![P1, P2],
        "each opponent is offered exactly once"
    );
    for prompt in &prompts {
        let owner = pool_owner(&runner, prompt);
        let expected = if owner == P1 {
            vec![p1_art, p1_land]
        } else {
            vec![p2_art, p2_land]
        };
        assert_eq!(
            sorted(prompt.cards.clone()),
            sorted(expected),
            "the full, unreduced artifact-or-land pool of {owner:?} — no creature, nothing of P0's"
        );
    }

    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    for id in [p1_land, p2_art, p0_art, p0_land, p2_giant] {
        assert!(on_battlefield(&runner, id), "{id:?} was not chosen");
    }
    assert_eq!(runner.state().objects[&p2_giant].damage_marked, 7);
    assert!(
        !on_battlefield(&runner, p1_bear),
        "the 2/2 died to 7 damage"
    );
}

// ---------------------------------------------------------------------------
// T3: cast from hand — nothing chosen, nothing destroyed
// ---------------------------------------------------------------------------

/// Cast from hand, the choice is skipped. The damage step still publishes the
/// creatures it damaged as the chain's tracked set (because the gated choice
/// below it reads the tracked set), so a destroy that ran anyway would destroy
/// the surviving 2/8. A set from an earlier resolution holding P2's artifact is
/// a shadowing control: it must be untouched either way.
#[test]
fn meteor_cast_from_hand_chooses_and_destroys_nothing() {
    let mut scenario = GameScenario::new_n_player(3, 102);
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario.add_creature(P1, "P1 Wall", 2, 8).id();
    let p1_land = add_land(&mut scenario, P1, "P1 Land");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, wall);

    // Shadowing control: an earlier resolution's non-empty set.
    let earlier = TrackedSetId(runner.state().next_tracked_set_id);
    runner.state_mut().next_tracked_set_id += 1;
    runner
        .state_mut()
        .tracked_object_sets
        .insert(earlier, vec![p2_art]);
    let first_id_this_cast = runner.state().next_tracked_set_id;

    cast_from_hand(&mut runner, meteor);
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    assert!(prompts.is_empty(), "no choice without cast-from-exile");
    // Reach guard: this resolution published a current-chain set that holds
    // the damaged 2/8 — the set a destroy running past the gate would read.
    let published_with_wall = runner
        .state()
        .tracked_object_sets
        .iter()
        .any(|(id, members)| id.0 >= first_id_this_cast && members.contains(&wall));
    assert!(
        published_with_wall,
        "reach: the damage step published a set containing the 2/8: {:?}",
        runner.state().tracked_object_sets
    );
    assert_eq!(runner.state().objects[&wall].damage_marked, 7);
    assert!(
        on_battlefield(&runner, wall),
        "the 2/8 survives: nothing was chosen"
    );
    assert!(on_battlefield(&runner, p1_land));
    assert!(
        on_battlefield(&runner, p2_art),
        "the earlier set is untouched"
    );
}

// ---------------------------------------------------------------------------
// T4: an opponent with nothing to choose
// ---------------------------------------------------------------------------

#[test]
fn opponent_with_no_artifact_or_land_is_skipped() {
    let mut scenario = GameScenario::new_n_player(3, 103);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_giant = scenario.add_creature(P2, "P2 Giant", 9, 9).id();
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    assert_eq!(prompts.len(), 1, "only P1 has a candidate: {prompts:?}");
    assert_eq!(pool_owner(&runner, &prompts[0]), P1);
    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p2_giant));
}

// ---------------------------------------------------------------------------
// T6: teams — a teammate is not an opponent
// ---------------------------------------------------------------------------

#[test]
fn two_headed_giant_teammate_is_never_offered() {
    // 2HG seats: {P0, P1} vs {P2, P3}.
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 104);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "Teammate Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let p3_land = add_land(&mut scenario, P3, "P3 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    let mut owners: Vec<PlayerId> = prompts.iter().map(|p| pool_owner(&runner, p)).collect();
    owners.sort();
    assert_eq!(owners, vec![P2, P3], "opponents only: {prompts:?}");
    assert!(prompts.iter().all(|p| p.chooser == P0));
    assert!(
        on_battlefield(&runner, p1_art),
        "the teammate's artifact survives"
    );
    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p3_land), Some(Zone::Graveyard));
}

// ---------------------------------------------------------------------------
// T7 / T8 / T8b: players leaving the game
// ---------------------------------------------------------------------------

#[test]
fn opponent_who_left_before_resolution_is_not_iterated() {
    let mut scenario = GameScenario::new_n_player(3, 105);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    engine::game::elimination::eliminate_player(runner.state_mut(), P1, &mut Vec::new());
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    assert_eq!(prompts.len(), 1, "only the live opponent: {prompts:?}");
    assert_eq!(pool_owner(&runner, &prompts[0]), P2);
    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
}

/// CR 104.3a + CR 800.4a: an opponent concedes between the two choices. The
/// remaining opponent is still offered, their pick is destroyed, and nothing
/// else is.
#[test]
fn opponent_conceding_mid_resolution_leaves_the_other_choice_intact() {
    let mut scenario = GameScenario::new_n_player(3, 106);
    scenario.at_phase(Phase::PreCombatMain);
    let p0_art = add_artifact(&mut scenario, P0, "P0 Relic");
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    let mut conceded = false;
    let prompts = resolve_with(
        &mut runner,
        |r, cards| {
            let owner = controller(r, cards[0]);
            vec![if owner == P2 { p2_land } else { cards[0] }]
        },
        |r, answered| {
            // After the first pick, P1 concedes while the other prompt is
            // still pending.
            if answered == 1
                && !conceded
                && matches!(
                    r.state().waiting_for,
                    WaitingFor::ChooseFromZoneChoice { .. }
                )
            {
                r.act(GameAction::Concede { player_id: P1 })
                    .expect("a player may concede at any time");
                conceded = true;
            }
        },
    );

    assert!(conceded, "reach: P1 conceded between the choices");
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p2_art));
    assert!(on_battlefield(&runner, p0_art));
    assert!(
        !on_battlefield(&runner, p1_art),
        "P1's objects left the game"
    );
}

/// P1 controls P2's artifact through a real control-changing effect, and P0
/// chooses it for P1. P1 then concedes: the effect ends and control returns to
/// P2 — a control change, not a zone change, so it is still the same object and
/// still the chosen permanent (CR 800.4a).
#[test]
fn chosen_permanent_whose_controller_concedes_is_still_destroyed() {
    let mut scenario = GameScenario::new_n_player(3, 107);
    scenario.at_phase(Phase::PreCombatMain);
    let stolen = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.add_transient_continuous_effect(
            stolen,
            P1,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: stolen },
            vec![ContinuousModification::ChangeController],
            None,
        );
        engine::game::layers::mark_layers_full(state);
        engine::game::layers::evaluate_layers(state);
    }
    assert_eq!(controller(&runner, stolen), P1, "reach: P1 controls it");

    cast_from_exile(&mut runner, meteor);
    let mut conceded = false;
    let prompts = resolve_with(
        &mut runner,
        |r, cards| {
            let owner = controller(r, cards[0]);
            vec![if owner == P1 { stolen } else { p2_land }]
        },
        |r, answered| {
            if answered == 1
                && !conceded
                && matches!(
                    r.state().waiting_for,
                    WaitingFor::ChooseFromZoneChoice { .. }
                )
            {
                r.act(GameAction::Concede { player_id: P1 })
                    .expect("a player may concede at any time");
                conceded = true;
            }
        },
    );

    assert!(conceded, "reach: P1 conceded between the choices");
    assert_eq!(
        pool_owner(&runner, &prompts[1]),
        P2,
        "the second prompt is P2's: {prompts:?}"
    );
    assert!(
        prompts[0].cards.contains(&stolen),
        "the stolen artifact was offered in P1's pool: {prompts:?}"
    );
    assert_eq!(zone_of(&runner, stolen), Some(Zone::Graveyard));
    assert_eq!(runner.state().objects[&stolen].owner, P2);
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
}

// ---------------------------------------------------------------------------
// T9 / T11: what happens to a chosen permanent
// ---------------------------------------------------------------------------

#[test]
fn chosen_indestructible_permanent_survives() {
    let mut scenario = GameScenario::new_n_player(3, 108);
    scenario.at_phase(Phase::PreCombatMain);
    let sturdy = {
        let mut b = scenario.add_artifact_from_oracle(P1, "P1 Sturdy Relic", "");
        b.indestructible();
        b.id()
    };
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    assert_eq!(prompts.len(), 2);
    assert!(
        prompts.iter().any(|p| p.cards == vec![sturdy]),
        "reach: offered"
    );
    assert!(
        on_battlefield(&runner, sturdy),
        "CR 702.12b: indestructible"
    );
    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
}

/// CR 704.3: no state-based actions are checked between Meteor's instructions,
/// so a 2/2 artifact creature dealt 7 damage is still on the battlefield and
/// can be chosen — and is then destroyed by the instruction, exactly once.
#[test]
fn damaged_artifact_creature_is_offered_and_destroyed_once() {
    let mut scenario = GameScenario::new_n_player(3, 109);
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario.add_creature(P1, "P1 Golem", 2, 2).id();
    let p1_land = add_land(&mut scenario, P1, "P1 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, golem);

    cast_from_exile(&mut runner, meteor);
    let mut offered_while_damaged = false;
    let prompts = resolve_with(
        &mut runner,
        |r, cards| {
            if cards.contains(&golem) {
                let obj = &r.state().objects[&golem];
                offered_while_damaged = obj.damage_marked == 7 && obj.zone == Zone::Battlefield;
                vec![golem]
            } else {
                vec![cards[0]]
            }
        },
        |_, _| {},
    );

    assert_eq!(prompts.len(), 1);
    assert_eq!(
        sorted(prompts[0].cards.clone()),
        sorted(vec![golem, p1_land])
    );
    assert!(
        offered_while_damaged,
        "the damaged 2/2 is still on the battlefield and offered"
    );
    assert_eq!(zone_of(&runner, golem), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p1_land));
    let graveyard = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .graveyard;
    assert_eq!(
        graveyard.iter().filter(|id| **id == golem).count(),
        1,
        "moved to the graveyard exactly once"
    );
}

// ---------------------------------------------------------------------------
// T12 / T13: the wider class through the same dispatch
// ---------------------------------------------------------------------------

#[test]
fn up_to_one_per_opponent_allows_declining() {
    let text = "For each opponent, choose up to one creature that player controls. Destroy the chosen permanents.";
    let mut scenario = GameScenario::new_n_player(3, 110);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let p2_bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe", false, text)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    let prompts = resolve_with(
        &mut runner,
        |r, cards| {
            if controller(r, cards[0]) == P1 {
                vec![]
            } else {
                vec![p2_bear]
            }
        },
        |_, _| {},
    );

    assert_eq!(prompts.len(), 2);
    assert!(on_battlefield(&runner, p1_bear), "P1's pick was declined");
    assert_eq!(zone_of(&runner, p2_bear), Some(Zone::Graveyard));
}

#[test]
fn per_opponent_choice_in_a_trigger_destroys_the_picks() {
    let text = "When this creature enters, for each opponent, choose a nonland permanent that player controls. Destroy the chosen permanents.";
    let parsed = parse(text, "Probe Beast", &[], &["Creature"]);
    assert!(!has_unimplemented(&parsed), "{:#?}", parsed.triggers);
    let execute = parsed.triggers[0].execute.as_deref().expect("trigger body");
    assert!(matches!(
        &*execute.effect,
        Effect::ChooseFromZone {
            zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
            ..
        }
    ));

    let mut scenario = GameScenario::new_n_player(3, 111);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p1_land = add_land(&mut scenario, P1, "P1 Land");
    let p2_bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let beast = scenario
        .add_creature_to_hand_from_oracle(P0, "Probe Beast", 3, 3, text)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&beast].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: beast,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast creature");
    let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});

    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts.iter().all(|p| !p.cards.contains(&p1_land)),
        "nonland only"
    );
    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_bear), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p1_land));
    assert!(on_battlefield(&runner, beast));
}

// ---------------------------------------------------------------------------
// T14 / T15: cards that must not change meaning
// ---------------------------------------------------------------------------

/// Chaos Defiler's consumer ("Destroy one of them chosen at random") is not
/// built; it must stay an honest gap rather than a destroy with no producer.
#[test]
fn chaos_defiler_random_consumer_stays_unsupported() {
    let text = "Trample\nBattle Cannon — When this creature enters or dies, for each opponent, choose a nonland permanent that player controls. Destroy one of them chosen at random.";
    let parsed = parse(
        text,
        "Chaos Defiler",
        &["Trample"],
        &["Artifact", "Creature"],
    );
    let defs = all_defs(&parsed);
    assert!(
        defs.iter().any(|d| matches!(
            &*d.effect,
            Effect::Unimplemented { name, .. } if name == "per_player_choice_parent_target"
        )),
        "{:#?}",
        parsed.triggers
    );
    assert!(
        !defs.iter().any(|d| matches!(
            &*d.effect,
            Effect::Destroy {
                target: TargetFilter::ParentTarget,
                ..
            }
        )),
        "no destroy reading a target the choice never sets"
    );
}

/// Highcliff Felidar's "greatest power among creatures that player controls" is
/// a per-opponent comparison the search filter cannot express; the choice stays
/// unparsed rather than comparing across every creature.
#[test]
fn highcliff_felidar_relative_superlative_stays_unsupported() {
    let text = "Vigilance\nWhen this creature enters, for each opponent, choose a creature with the greatest power among creatures that player controls. Destroy those creatures.";
    let parsed = parse(text, "Highcliff Felidar", &["Vigilance"], &["Creature"]);
    let defs = all_defs(&parsed);
    assert!(
        !defs
            .iter()
            .any(|d| matches!(&*d.effect, Effect::ChooseFromZone { .. })),
        "{:#?}",
        parsed.triggers
    );
    assert!(has_unimplemented(&parsed));
}

#[test]
fn benthic_anomaly_copy_consumer_stays_unsupported() {
    let text = "Devoid (This card has no color.)\nWhen you cast this spell, for each opponent, choose a creature that player controls. Create a token that's a copy of one of those creatures, except its power is equal to the total power of those creatures, its toughness is equal to the total toughness of those creatures, and it's a colorless Eldrazi creature.";
    let parsed = parse(text, "Benthic Anomaly", &["Devoid"], &["Creature"]);
    assert!(has_unimplemented(&parsed), "{:#?}", parsed.triggers);
}

/// The printed-`target` per-opponent form and a targeted "the chosen creatures"
/// antecedent keep their target readings.
#[test]
fn targeted_forms_keep_their_parent_target_reading() {
    let mega_flare = "Kicker {3}{R}{R}\nIf this spell was kicked, create a 6/6 red Dragon creature token with flying.\nFor each opponent, choose up to one target creature that player controls. Mega Flare deals damage equal to the greatest power among creatures you control to each of the chosen creatures.";
    let parsed = parse(mega_flare, "Mega Flare", &["Kicker"], &["Sorcery"]);
    let defs = all_defs(&parsed);
    assert!(
        defs.iter()
            .any(|d| matches!(&*d.effect, Effect::TargetOnly { .. })),
        "reach: the printed-target clause still lowers to its target declaration"
    );
    assert!(
        !defs
            .iter()
            .any(|d| matches!(&*d.effect, Effect::ChooseFromZone { .. })),
        "the printed-target form is not a resolution choice"
    );

    let vats = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nChoose any number of target creatures with equal toughness. Destroy the chosen creatures.";
    let parsed = parse(vats, "V.A.T.S.", &["Split second"], &["Instant"]);
    let defs = all_defs(&parsed);
    assert!(
        defs.iter().any(|d| matches!(
            &*d.effect,
            Effect::Destroy {
                target: TargetFilter::ParentTarget,
                ..
            }
        )),
        "{:#?}",
        parsed.abilities
    );

    // Kaya's exile arm keeps its own two prefixes.
    let parsed = parse(
        "For each opponent, exile up to one creature that player controls.",
        "Probe",
        &[],
        &["Sorcery"],
    );
    assert!(
        !all_defs(&parsed).iter().any(|d| matches!(
            &*d.effect,
            Effect::ChooseFromZone {
                zone_owner: ZoneOwner::Each(_),
                ..
            }
        )),
        "{:#?}",
        parsed.abilities
    );
}

// ---------------------------------------------------------------------------
// T15b: clause boundary / T17: serialized state
// ---------------------------------------------------------------------------

/// Text trailing "that player controls" on the same clause must not be dropped:
/// the arm only claims a clause that ends there.
#[test]
fn per_opponent_choice_with_a_trailing_instruction_is_not_claimed() {
    let parsed = parse(
        "For each opponent, choose a creature that player controls at random.",
        "Probe",
        &[],
        &["Sorcery"],
    );
    assert!(
        !all_defs(&parsed).iter().any(|d| matches!(
            &*d.effect,
            Effect::ChooseFromZone {
                zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
                ..
            }
        )),
        "{:#?}",
        parsed.abilities
    );
    assert!(has_unimplemented(&parsed), "{:#?}", parsed.abilities);
}

/// A game paused on Meteor's per-opponent choice serializes the new
/// population and loads back.
#[test]
fn game_state_paused_on_the_per_opponent_choice_round_trips() {
    let mut scenario = GameScenario::new_n_player(3, 112);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ));

    let json = serde_json::to_string(runner.state()).expect("serialize");
    assert!(
        json.contains("\"Opponents\""),
        "the new population is written"
    );
    let back: GameState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        serde_json::to_string(&back).expect("re-serialize"),
        json,
        "round trip is lossless"
    );
}
