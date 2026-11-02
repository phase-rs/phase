//! Per-opponent battlefield choice acted on as a set — Ultimate Magic: Meteor.
//!
//! Oracle text: "Ultimate Magic: Meteor deals 7 damage to each creature. If this
//! spell was cast from exile, for each opponent, choose an artifact or land that
//! player controls. Destroy the chosen permanents." Foretell {5}{R}.
//!
//! The spell's controller makes every choice (CR 608.2c + CR 608.2d), one per
//! opponent (CR 102.2 + CR 102.3), and none of them is a target (CR 115.10a).
//! Each choice draws only from permanents THAT opponent controls, and the
//! chosen permanents are then destroyed together (CR 701.8a). The caster makes
//! every choice, so the caster chooses their order (CR 101.4c): with two or
//! more opponents to choose for, an order prompt asks whose selection to make
//! next. Tests that do not exercise ordering identify each pool prompt by the
//! controller of its candidates, never by position.
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
use engine::types::game_state::{
    CastPaymentMode, GameState, WaitingFor, ZoneOpponentChooserPurpose,
};
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
        for mode in &def.mode_abilities {
            push(mode, out);
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

/// Resolve the stack, answering each per-opponent pool prompt with `pick`, and
/// return every pool prompt seen. `between` runs after each answered pool
/// prompt. The caster's order prompts (CR 101.4c) are answered with the first
/// offered candidate; use [`resolve_with_order`] to choose the order.
fn resolve_with(
    runner: &mut GameRunner,
    pick: impl FnMut(&GameRunner, &[ObjectId]) -> Vec<ObjectId>,
    between: impl FnMut(&mut GameRunner, usize),
) -> Vec<Prompt> {
    resolve_with_order(runner, |_, candidates| candidates[0], pick, between)
}

/// [`resolve_with`], answering each order prompt (CR 101.4c: the caster
/// chooses whose selection to make next) with `order`.
fn resolve_with_order(
    runner: &mut GameRunner,
    mut order: impl FnMut(&GameRunner, &[PlayerId]) -> PlayerId,
    mut pick: impl FnMut(&GameRunner, &[ObjectId]) -> Vec<ObjectId>,
    mut between: impl FnMut(&mut GameRunner, usize),
) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    for _ in 0..300 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneOpponentChooser {
                candidates,
                purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
                ..
            } => {
                let next = order(runner, &candidates);
                runner
                    .act(GameAction::ChooseZoneOpponentChooser { opponent: next })
                    .expect("a legal order pick is accepted");
            }
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

/// Advance until the first per-player prompt (order or pool) is outstanding.
fn advance_to_per_player_prompt(runner: &mut GameRunner) {
    for _ in 0..50 {
        match runner.state().waiting_for {
            WaitingFor::ChooseFromZoneOpponentChooser { .. }
            | WaitingFor::ChooseFromZoneChoice { .. } => return,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            ref other => panic!("no per-player prompt was reached: {other:?}"),
        }
    }
    panic!("no per-player prompt was reached");
}

/// The candidates of the outstanding order prompt.
fn order_candidates(runner: &GameRunner) -> Vec<PlayerId> {
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            player,
            ..
        } => {
            assert_eq!(*player, P0, "the caster orders their own choices");
            candidates.clone()
        }
        other => panic!("expected the caster's order prompt, got {other:?}"),
    }
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
    // b3: the caster orders P1 first (CR 101.4c), explicitly.
    let prompts = resolve_with_order(
        &mut runner,
        |_, candidates| {
            assert!(candidates.contains(&P1), "reach: P1 is orderable");
            P1
        },
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
        defs.iter().any(|d| is_trailing_gap(d)),
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
    assert!(
        all_defs(&parsed).iter().any(|d| is_trailing_gap(d)),
        "{:#?}",
        parsed.triggers
    );
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

    // Kaya's exile arm keeps its own prefix ("for each other player"; a bare
    // "for each player" exile is taken earlier by the player-scope fan-out),
    // and an opponent-prefixed exile is not claimed by it: it keeps its
    // existing single-exile fallback shape.
    let text = "For each other player, exile up to one creature that player controls.";
    let parsed = parse(text, "Probe", &[], &["Sorcery"]);
    assert!(
        all_defs(&parsed).iter().any(|d| matches!(
            &*d.effect,
            Effect::ChooseFromZone {
                zone_owner: ZoneOwner::Each(PerPlayerScope::OtherPlayers),
                zone: Zone::Battlefield,
                up_to: true,
                ..
            }
        )),
        "positive control: {text}\n{:#?}",
        parsed.abilities
    );
    let parsed = parse(
        "For each opponent, exile up to one creature that player controls.",
        "Probe",
        &[],
        &["Sorcery"],
    );
    let defs = all_defs(&parsed);
    assert!(
        !defs.iter().any(|d| matches!(
            &*d.effect,
            Effect::ChooseFromZone {
                zone_owner: ZoneOwner::Each(_),
                ..
            }
        )),
        "{:#?}",
        parsed.abilities
    );
    assert_eq!(defs.len(), 1, "{:#?}", parsed.abilities);
    assert!(
        matches!(
            &*defs[0].effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::Typed(filter),
                ..
            } if filter.type_filters == vec![engine::types::ability::TypeFilter::Creature]
                && filter.controller == Some(engine::types::ability::ControllerRef::TargetPlayer)
        ),
        "the opponent-prefixed exile keeps its single-exile fallback shape: {:#?}",
        parsed.abilities
    );
}

// ---------------------------------------------------------------------------
// T18+: the positional reader rule after a per-opponent choice
// ---------------------------------------------------------------------------

fn is_trailing_gap(def: &AbilityDefinition) -> bool {
    matches!(
        &*def.effect,
        Effect::Unimplemented { name, .. } if name == "per_opponent_choice_trailing_clause"
    )
}

fn has_per_opponent_choice(parsed: &ParsedAbilities) -> bool {
    all_defs(parsed).iter().any(|d| {
        matches!(
            &*d.effect,
            Effect::ChooseFromZone {
                zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
                zone: Zone::Battlefield,
                ..
            }
        )
    })
}

fn has_set_destroy(parsed: &ParsedAbilities) -> bool {
    all_defs(parsed).iter().any(|d| {
        matches!(
            &*d.effect,
            Effect::DestroyAll {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        )
    })
}

/// Parse `text` as a sorcery and assert the per-opponent choice parsed (reach)
/// and every clause after it is an honest gap with no set destroy.
fn assert_strict(text: &str) -> ParsedAbilities {
    let parsed = parse(text, "Probe", &[], &["Sorcery"]);
    assert!(
        has_per_opponent_choice(&parsed),
        "reach: the per-opponent choice parses: {text}\n{:#?}",
        parsed.abilities
    );
    assert!(
        all_defs(&parsed).iter().any(|d| is_trailing_gap(d)),
        "the trailing clause is strict: {text}\n{:#?}",
        parsed.abilities
    );
    assert!(
        !has_set_destroy(&parsed),
        "no supported destroy of the chosen set: {text}"
    );
    parsed
}

/// Parse `text` as a sorcery and assert the supported shape: the per-opponent
/// choice followed by one set destroy, no gap.
fn assert_supported(text: &str) {
    let parsed = parse(text, "Probe", &[], &["Sorcery"]);
    assert!(has_per_opponent_choice(&parsed), "{text}");
    assert!(has_set_destroy(&parsed), "{text}\n{:#?}", parsed.abilities);
    assert!(
        !has_unimplemented(&parsed),
        "{text}\n{:#?}",
        parsed.abilities
    );
}

/// Cast a zero-cost sorcery with `text` in a 3-player game where each opponent
/// controls one artifact and one land, pick with `pick`, and return
/// `(runner, prompts, [p1_art, p1_land, p2_art, p2_land])`.
fn run_three_player(
    text: &str,
    seed: u64,
    pick: impl FnMut(&GameRunner, &[ObjectId]) -> Vec<ObjectId>,
) -> (GameRunner, Vec<Prompt>, [ObjectId; 4]) {
    let mut scenario = GameScenario::new_n_player(3, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p1_land = add_land(&mut scenario, P1, "P1 Land");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
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
    let prompts = resolve_with(&mut runner, pick, |_, _| {});
    (runner, prompts, [p1_art, p1_land, p2_art, p2_land])
}

/// A narrower noun than the choice ("the chosen artifacts", "those creatures",
/// "each artifact chosen this way" after "choose an artifact or land") names
/// only part of the chosen set, which the bare set cannot express: strict, and
/// at runtime a chosen land is never destroyed (CR 608.2c + CR 608.2d).
#[test]
fn narrower_noun_than_the_choice_is_strict_and_spares_a_chosen_land() {
    for text in [
        "For each opponent, choose an artifact or land that player controls. Destroy the chosen artifacts.",
        "For each opponent, choose a creature or land that player controls. Destroy the chosen creatures.",
        "For each opponent, choose a creature or land that player controls. Destroy those creatures.",
        "For each opponent, choose an artifact or land that player controls. Destroy each artifact chosen this way.",
    ] {
        assert_strict(text);
    }

    for text in [
        "For each opponent, choose an artifact or land that player controls. Destroy the chosen artifacts.",
        "For each opponent, choose an artifact or land that player controls. Destroy each artifact chosen this way.",
    ] {
        let mut land = None;
        let (runner, prompts, [_, p1_land, _, _]) = run_three_player(text, 301, |r, cards| {
            let pick = if controller(r, cards[0]) == P1 {
                *cards
                    .iter()
                    .find(|id| r.state().objects[id].card_types.core_types.contains(&CoreType::Land))
                    .expect("P1's land is offered")
            } else {
                cards[0]
            };
            if controller(r, cards[0]) == P1 {
                land = Some(pick);
            }
            vec![pick]
        });
        assert_eq!(prompts.len(), 2, "reach: both opponents prompted");
        assert_eq!(land, Some(p1_land), "reach: the land was picked");
        assert!(
            on_battlefield(&runner, p1_land),
            "a chosen land is not one of the named artifacts: {text}"
        );
    }
}

/// Implied nouns and Druid of Purification's distributive wording are the
/// supported shape, and destroy every pick.
#[test]
fn implied_noun_and_distributive_forms_destroy_every_pick() {
    for text in [
        "For each opponent, choose a creature that player controls. Destroy the chosen creatures.",
        "For each opponent, choose a creature that player controls. Destroy those creatures.",
        "For each opponent, choose an artifact that player controls. Destroy each artifact chosen this way.",
        "For each opponent, choose an artifact or land that player controls. Destroy each permanent chosen this way.",
    ] {
        assert_supported(text);
    }
    let (runner, prompts, [p1_art, p1_land, p2_art, p2_land]) = run_three_player(
        "For each opponent, choose an artifact or land that player controls. Destroy each permanent chosen this way.",
        302,
        |r, cards| {
            let want_land = controller(r, cards[0]) == P2;
            vec![*cards
                .iter()
                .find(|id| {
                    r.state().objects[id]
                        .card_types
                        .core_types
                        .contains(&CoreType::Land)
                        == want_land
                })
                .expect("candidate")]
        },
    );
    assert_eq!(prompts.len(), 2);
    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p1_land));
    assert!(on_battlefield(&runner, p2_art));

    let (runner, _, [p1_art, _, p2_art, _]) = run_three_player(
        "For each opponent, choose an artifact that player controls. Destroy each artifact chosen this way.",
        303,
        |_, cards| vec![cards[0]],
    );
    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
}

/// A singular definite ("the chosen artifact", "that artifact", "the artifact
/// chosen this way") names one object; a per-opponent choice supplies one per
/// opponent. Strict.
#[test]
fn singular_definite_reference_is_strict() {
    for text in [
        "For each opponent, choose an artifact that player controls. Destroy the chosen artifact.",
        "For each opponent, choose an artifact that player controls. Destroy that artifact.",
        "For each opponent, choose an artifact that player controls. Destroy the artifact chosen this way.",
    ] {
        assert_strict(text);
    }
}

/// Any clause after the choice other than the single final destroy is strict,
/// whatever it says or reads.
#[test]
fn every_other_trailing_clause_is_strict() {
    for text in [
        // R2b / CH1 / M2-R2: an instruction between the choice and the destroy.
        "For each opponent, choose an artifact or land that player controls. You gain 1 life. Destroy the chosen permanents.",
        "For each opponent, choose an artifact or land that player controls. Target player gains 1 life. Destroy the chosen permanents.",
        "For each opponent, choose an artifact or land that player controls. Tap target creature. Destroy the chosen permanents.",
        // Singular anaphor after an intervening clause.
        "For each opponent, choose an artifact that player controls. You gain 1 life. Destroy that artifact.",
        // Pronouns, adjacent and not.
        "For each opponent, choose an artifact that player controls. Destroy them.",
        "For each opponent, choose an artifact that player controls. You gain 1 life. Destroy them.",
        "For each opponent, choose a creature that player controls. Destroy it.",
        // A quantity reader.
        "For each opponent, choose a creature that player controls. You gain 1 life. Create a 1/1 white Soldier creature token for each creature chosen this way.",
        // A correct destroy that is not the last clause.
        "For each opponent, choose an artifact or land that player controls. Destroy the chosen permanents. You gain 1 life.",
        // An in-chain reflexive and an outer delayed trigger.
        "For each opponent, choose an artifact that player controls. When you do, you gain 1 life.",
        "For each opponent, choose an artifact that player controls. At the beginning of the next end step, destroy the chosen permanents.",
    ] {
        assert_strict(text);
    }

    // CH2: honesty over coverage — strict even though "it" could name the
    // Equipment.
    let ch2 = "When this creature enters, for each opponent, choose a creature that player controls. Attach up to one target Equipment you control to this creature. Destroy it.";
    let parsed = parse(ch2, "Probe", &[], &["Creature"]);
    assert!(has_per_opponent_choice(&parsed), "{:#?}", parsed.triggers);
    assert!(
        all_defs(&parsed).iter().any(|d| is_trailing_gap(d)),
        "{:#?}",
        parsed.triggers
    );
}

/// A correct destroy followed by another clause does not bind, and the chosen
/// permanents are not destroyed at runtime.
#[test]
fn destroy_that_is_not_last_is_not_bound_at_runtime() {
    let (runner, prompts, [p1_art, _, p2_art, _]) = run_three_player(
        "For each opponent, choose an artifact that player controls. Destroy the chosen permanents. You gain 1 life.",
        304,
        |_, cards| vec![cards[0]],
    );
    assert_eq!(prompts.len(), 2, "reach: the choice ran");
    assert!(on_battlefield(&runner, p1_art));
    assert!(on_battlefield(&runner, p2_art));
}

/// Two per-opponent choices: the first one governs, so the second choice and
/// the destroy are both strict. The destroy would otherwise act on only the
/// second group (each choice starts a fresh tracked set).
#[test]
fn second_per_opponent_choice_is_strict() {
    let text = "For each opponent, choose an artifact that player controls. For each opponent, choose a land that player controls. Destroy the chosen permanents.";
    let parsed = assert_strict(text);
    let gaps = all_defs(&parsed)
        .iter()
        .filter(|d| is_trailing_gap(d))
        .count();
    assert_eq!(
        gaps, 2,
        "the second choice and the destroy: {:#?}",
        parsed.abilities
    );

    let (runner, prompts, [p1_art, p1_land, p2_art, p2_land]) =
        run_three_player(text, 305, |_, cards| vec![cards[0]]);
    assert_eq!(prompts.len(), 2, "only the first choice prompts");
    for id in [p1_art, p1_land, p2_art, p2_land] {
        assert!(
            on_battlefield(&runner, id),
            "nothing is claimed as destroyed"
        );
    }
}

/// Conditional bodies that the chunker splits into several clauses are parsed
/// as their own chain and absorbed into the enclosing one. With a per-opponent
/// choice inside such a body the splitter separates "for each opponent" from
/// "choose …", so the choice is never produced there and nothing is claimed as
/// supported: the body stays an honest gap (Meteor's own conditional body is a
/// single chunk — `meteor_conditional_body_is_a_single_chunk`).
#[test]
fn split_conditional_bodies_never_claim_a_set_destroy() {
    for text in [
        "If you control an artifact, for each opponent, choose a creature that player controls, then you gain 1 life. Destroy the chosen permanents.",
        "If you control an artifact, you gain 1 life, then for each opponent, choose a creature that player controls. Destroy the chosen permanents.",
    ] {
        let parsed = parse(text, "Probe", &[], &["Sorcery"]);
        assert!(!has_set_destroy(&parsed), "{text}\n{:#?}", parsed.abilities);
        assert!(has_unimplemented(&parsed), "{text}\n{:#?}", parsed.abilities);
    }
}

/// Separate abilities and modes are separate chains: the rule never reaches
/// them.
#[test]
fn independent_abilities_and_modes_reset() {
    let parsed = parse(
        "When this creature enters, for each opponent, choose an artifact that player controls. Destroy the chosen permanents.\n{T}: You gain 1 life.",
        "Probe",
        &[],
        &["Creature"],
    );
    assert!(has_set_destroy(&parsed), "reach: the trigger binds");
    assert!(
        !all_defs(&parsed).iter().any(|d| is_trailing_gap(d)),
        "the activated ability is not governed: {:#?}",
        parsed.abilities
    );
    assert!(
        parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::GainLife { .. })),
        "reach: the second ability parsed"
    );

    let modal = parse(
        "Choose one —\n• For each opponent, choose an artifact that player controls. Destroy the chosen permanents.\n• You gain 1 life.",
        "Probe",
        &[],
        &["Sorcery"],
    );
    let dbg = format!("{modal:#?}");
    assert!(
        !dbg.contains("per_opponent_choice_trailing_clause"),
        "the second mode is not governed:\n{dbg}"
    );
    assert!(dbg.contains("GainLife"), "reach: the second mode parsed");
    // Reach, mode 1: its own chain holds the supported shape — the choice
    // followed by the set destroy, and no gap.
    let modal_defs = all_defs(&modal);
    let mode_one = modal_defs
        .iter()
        .find(|d| {
            matches!(
                &*d.effect,
                Effect::ChooseFromZone {
                    zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("mode 1's choice is reachable:\n{dbg}"));
    let destroy = mode_one
        .sub_ability
        .as_deref()
        .unwrap_or_else(|| panic!("mode 1 continues:\n{dbg}"));
    assert!(
        matches!(
            &*destroy.effect,
            Effect::DestroyAll {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "mode 1 binds its set destroy:\n{dbg}"
    );
    let mut node = Some(*mode_one);
    while let Some(def) = node {
        assert!(
            !matches!(&*def.effect, Effect::Unimplemented { .. }),
            "mode 1 has no gap:\n{dbg}"
        );
        node = def.sub_ability.as_deref();
    }
}

/// A rejected trailing clause keeps no executable metadata: an optional "you
/// may" or an unless-payment on it would otherwise still prompt at runtime.
#[test]
fn rejected_trailing_clause_prompts_no_one() {
    for (text, seed) in [
        (
            "For each opponent, choose an artifact that player controls. You may draw a card.",
            306,
        ),
        (
            "For each opponent, choose an artifact that player controls. You gain 1 life unless you pay {2}.",
            307,
        ),
    ] {
        assert_strict(text);
        let mut scenario = GameScenario::new_n_player(3, seed);
        scenario.at_phase(Phase::PreCombatMain);
        add_artifact(&mut scenario, P1, "P1 Relic");
        add_artifact(&mut scenario, P2, "P2 Relic");
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Probe", false, text)
            .id();
        let mut runner = scenario.build();
        let hand_before = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .len();
        let life_before = runner.state().players.iter().find(|p| p.id == P0).unwrap().life;
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast");
        let prompts = resolve_with(&mut runner, |_, cards| vec![cards[0]], |_, _| {});
        assert_eq!(prompts.len(), 2, "reach: the choice ran: {text}");
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. } | WaitingFor::UnlessPayment { .. }
            ),
            "no prompt for the rejected clause: {text}: {:?}",
            runner.state().waiting_for
        );
        assert!(runner.state().stack.is_empty(), "resolved: {text}");
        let p0 = runner.state().players.iter().find(|p| p.id == P0).unwrap();
        assert_eq!(p0.hand.len(), hand_before - 1, "no draw: {text}");
        assert_eq!(p0.life, life_before, "no life change: {text}");
    }
}

/// Merge-base parity: other per-player populations and zones are untouched.
#[test]
fn other_per_player_populations_are_untouched() {
    let breach = parse(
        "Each player mills ten cards. For each player, choose a creature or planeswalker card in that player's graveyard. Put those cards onto the battlefield under your control. Then each creature you control becomes a Phyrexian in addition to its other types.",
        "Breach the Multiverse",
        &[],
        &["Sorcery"],
    );
    let dbg = format!("{breach:#?}");
    assert!(
        dbg.contains("AllPlayers"),
        "reach: Breach's choice parses:\n{dbg}"
    );
    assert!(
        !dbg.contains("per_opponent_choice_trailing_clause"),
        "Breach is not governed:\n{dbg}"
    );

    let druid = parse(
        "When this creature enters, starting with you, each player may choose an artifact or enchantment you don't control. Destroy each permanent chosen this way.",
        "Druid of Purification",
        &[],
        &["Creature"],
    );
    let dbg = format!("{druid:#?}");
    assert!(
        !dbg.contains("per_opponent_choice_trailing_clause") && dbg.contains("DestroyAll"),
        "Druid keeps its merge-base destroy:\n{dbg}"
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
/// population, the order prompt's purpose and the frame's `current` field, and
/// loads back through the persisted-state restore — parked on the caster's
/// order prompt and parked on one opponent's pool.
#[test]
fn game_state_paused_on_the_per_opponent_choice_round_trips() {
    let mut scenario = GameScenario::new_n_player(3, 112);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);

    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "reach: order prompt"
    );
    round_trip(&runner, &["\"Opponents\"", "\"PerPlayerChoiceOrder\""]);

    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("order pick");
    let frame = runner
        .state()
        .active_per_player_zone_choice()
        .expect("reach: the pool prompt's frame is parked");
    assert_eq!(frame.current, Some(P2), "reach: P2's pool is current");
    round_trip(&runner, &["\"current\""]);
}

/// Serialize `runner`'s state, check each needle is written, check the plain
/// serde round trip is lossless, then restore it through the persisted-state
/// pipeline.
fn round_trip(runner: &GameRunner, must_contain: &[&str]) {
    let json = serde_json::to_string(runner.state()).expect("serialize");
    for needle in must_contain {
        assert!(json.contains(needle), "{needle} is written");
    }
    let back: GameState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        serde_json::to_string(&back).expect("re-serialize"),
        json,
        "round trip is lossless"
    );
    restore(runner).expect("the parked per-player choice restores");
}

/// The engine's own save/restore pipeline.
fn restore(
    runner: &GameRunner,
) -> Result<GameState, engine::types::game_state::PersistedRestoreError> {
    let persisted = serde_json::to_string(&engine::types::game_state::PersistedGameState::capture(
        runner.state().clone(),
    ))
    .expect("persisted state serializes");
    serde_json::from_str::<engine::types::game_state::PersistedGameState>(&persisted)
        .expect("persisted state decodes")
        .prepare_for_restore(
            engine::types::game_state::PersistedRestoreFinalization::DeferUntilRehydrated,
        )?
        .finalize_after_rehydration(|_| Ok(()))
}

/// Reshape the live parked frame into its legacy (v97) form: no `current`.
fn make_frame_legacy(runner: &mut GameRunner) {
    let mut frame = runner
        .state_mut()
        .take_active_per_player_zone_choice()
        .expect("the per-player frame is the top")
        .expect("reach: a parked frame");
    frame.current = None;
    frame.nominee = None;
    runner.state_mut().push_per_player_zone_choice(frame);
}

/// A per-player frame persisted before `current` and `nominee` existed decodes
/// with both absent.
#[test]
fn legacy_per_player_frame_without_current_decodes() {
    let mut scenario = GameScenario::new_n_player(3, 113);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P1);
    let frame = runner
        .state()
        .active_per_player_zone_choice()
        .expect("reach: a parked frame")
        .clone();
    let mut value = serde_json::to_value(&frame).expect("serialize frame");
    let object = value.as_object_mut().expect("frame object");
    assert!(
        object.remove("current").is_some(),
        "reach: current is written"
    );
    object.remove("nominee");
    let legacy: engine::types::game_state::PendingPerPlayerZoneChoice =
        serde_json::from_value(value).expect("a legacy frame decodes");
    assert_eq!(legacy.current, None);
    assert_eq!(legacy.nominee, None);
}

/// R6-R3 / S3: a legacy (v97) snapshot parked on P1's pool restores with the
/// pool's owner re-established from the queue's provenance — v97 parked the
/// population suffix after the owner — and play continues: P1 concedes and
/// P2's pool is presented.
///
/// REVERT PROBE: drop the migration and the restored frame keeps
/// `current: None`; P1 conceding then leaves P1's stale pool prompt pending.
#[test]
fn legacy_pool_prompt_restores_its_owner_from_the_queue() {
    let mut scenario = GameScenario::new_n_player(3, 114);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P1);
    assert_eq!(pool(&runner), vec![p1_art], "reach: P1's pool is pending");
    make_frame_legacy(&mut runner);
    assert_eq!(
        runner
            .state()
            .active_per_player_zone_choice()
            .map(|frame| frame.remaining_players.clone()),
        Some(vec![P2]),
        "reach: the v97 queue is the suffix after P1"
    );

    let restored = restore(&runner).expect("a provenance-established legacy prompt restores");
    assert_eq!(
        restored
            .active_per_player_zone_choice()
            .and_then(|frame| frame.current),
        Some(P1),
        "the owner is re-established from the queue"
    );
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");
    assert_eq!(pool(&runner), vec![p2_art], "P2's pool is presented next");
}

/// S3 (hostile): P2 is the active player, so the static opponent population
/// is [P2, P1]. P2's pick is made first; P1's pool — offering the artifact P1
/// stole from P2 — is pending with an empty queue. P1 leaves (in the legacy
/// shape, so nothing reconciles), control returns to P2, and P2 now uniquely
/// holds that artifact. Then the game is saved. The owner the queue names (P1)
/// has left, so the snapshot is rejected rather than silently rebound to P2.
///
/// REVERT PROBE: infer the owner by live containment and the restore adopts
/// P2.
#[test]
fn hostile_legacy_pool_prompt_is_rejected_not_rebound() {
    const CHOICE: &str =
        "For each opponent, choose an artifact or land that player controls. Destroy the chosen permanents.";
    let mut scenario = GameScenario::new_n_player(3, 115);
    scenario.at_phase(Phase::PreCombatMain);
    let stolen = add_artifact(&mut scenario, P2, "P2 Relic Held By P1");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Instant Probe", true, CHOICE)
        .id();
    let mut runner = scenario.build();
    steal(&mut runner, stolen, P1);
    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    assert_eq!(runner.state().seat_order, vec![P0, P1, P2]);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the instant on P2's turn");
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P2, P1],
        "reach: the static opponent population is [P2, P1]"
    );
    order(&mut runner, P2);
    pick(&mut runner, p2_land);
    assert_eq!(
        pool(&runner),
        vec![stolen],
        "reach: P1's pool offers the stolen artifact"
    );
    assert_eq!(
        runner
            .state()
            .active_per_player_zone_choice()
            .map(|frame| frame.remaining_players.clone()),
        Some(vec![]),
        "reach: the queue after P1 is empty"
    );
    make_frame_legacy(&mut runner);
    engine::game::elimination::eliminate_player(runner.state_mut(), P1, &mut Vec::new());
    assert_eq!(
        controller(&runner, stolen),
        P2,
        "reach: control returned to P2"
    );

    let restored = restore(&runner);
    assert!(
        matches!(
            restored,
            Err(engine::types::game_state::PersistedRestoreError::InvalidPerPlayerChoice(_))
        ),
        "the stale snapshot is rejected, never rebound to P2: {:?}",
        restored.map(|state| state.active_per_player_zone_choice().cloned())
    );
}

/// S3: a queue that is not the static population's suffix (here P0 ordered
/// P2 before P1, a shape v97 could not produce) has no established owner.
#[test]
fn legacy_pool_prompt_with_a_non_suffix_queue_is_rejected() {
    let mut scenario = GameScenario::new_n_player(3, 116);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P2);
    assert_eq!(pool(&runner), vec![p2_art], "reach: P2's pool is pending");
    make_frame_legacy(&mut runner);
    assert!(matches!(
        restore(&runner),
        Err(engine::types::game_state::PersistedRestoreError::InvalidPerPlayerChoice(_))
    ));
}

// ---------------------------------------------------------------------------
// CR 101.4c: the caster orders their own per-opponent choices
// ---------------------------------------------------------------------------

/// Give `taker` control of `id` through a transient control-changing effect
/// that `taker` controls (so it ends when `taker` leaves, CR 800.4a).
fn steal(runner: &mut GameRunner, id: ObjectId, taker: PlayerId) {
    let state = runner.state_mut();
    state.add_transient_continuous_effect(
        id,
        taker,
        Duration::Permanent,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::mark_layers_full(state);
    engine::game::layers::evaluate_layers(state);
    assert_eq!(
        controller(runner, id),
        taker,
        "reach: {taker:?} controls {id:?}"
    );
}

fn order(runner: &mut GameRunner, next: PlayerId) {
    assert!(
        order_candidates(runner).contains(&next),
        "{next:?} is orderable"
    );
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: next })
        .expect("a legal order pick is accepted");
}

/// The outstanding pool prompt's candidates.
fn pool(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the caster makes every choice");
            sorted(cards.clone())
        }
        other => panic!("expected a pool prompt, got {other:?}"),
    }
}

fn pick(runner: &mut GameRunner, id: ObjectId) {
    runner
        .act(GameAction::SelectCards { cards: vec![id] })
        .expect("a legal pick is accepted");
}

fn no_per_player_prompt(runner: &GameRunner) -> bool {
    !matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneOpponentChooser { .. } | WaitingFor::ChooseFromZoneChoice { .. }
    )
}

/// (a) CR 101.4c: the caster chooses a non-APNAP order — P2's pool first —
/// and the prompts follow the chosen order.
///
/// REVERT PROBE: ignore the order answer (always take the first candidate)
/// and P1's pool is presented first.
#[test]
fn caster_chooses_a_non_apnap_order() {
    let mut scenario = GameScenario::new_n_player(3, 201);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "both opponents offered"
    );
    order(&mut runner, P2);
    assert_eq!(
        pool(&runner),
        vec![p2_art],
        "P2's pool comes first, as ordered"
    );
    pick(&mut runner, p2_art);
    assert_eq!(
        pool(&runner),
        vec![p1_art],
        "with one opponent left there is nothing to order"
    );
    pick(&mut runner, p1_art);
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
}

/// (b1) Under the chosen order (P2 first), P1 concedes while P1's pool —
/// which offers the artifact P1 stole from P2 — is outstanding. P1's control
/// effect ends and the artifact returns to P2 (CR 800.4a), whose choice was
/// already made: it is not selected, and survives. P1's stale prompt is
/// discarded and the iteration finishes.
///
/// REVERT PROBE: drop the departure reconciliation and P1's stale pool prompt
/// (still offering the returned artifact) stays outstanding.
#[test]
fn concession_after_the_ordered_pick_leaves_the_returned_artifact_unselected() {
    let mut scenario = GameScenario::new_n_player(3, 202);
    scenario.at_phase(Phase::PreCombatMain);
    let stolen = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    steal(&mut runner, stolen, P1);

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P2);
    assert_eq!(
        pool(&runner),
        vec![p2_land],
        "P2's pool excludes what P1 controls"
    );
    pick(&mut runner, p2_land);
    assert_eq!(
        pool(&runner),
        vec![stolen],
        "reach: P1's pool offers the stolen artifact"
    );

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");
    assert!(
        no_per_player_prompt(&runner),
        "P1's stale pool prompt is discarded: {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    assert!(
        on_battlefield(&runner, stolen),
        "the returned artifact was not selected"
    );
    assert_eq!(controller(&runner, stolen), P2);
}

/// (b2) P1 concedes at the order prompt. The artifact P1 stole returns to P2
/// (CR 800.4a), and P2's pool — now the only one — is presented directly and
/// offers it.
#[test]
fn concession_at_the_order_prompt_returns_the_artifact_to_the_remaining_pool() {
    let mut scenario = GameScenario::new_n_player(3, 203);
    scenario.at_phase(Phase::PreCombatMain);
    let stolen = add_artifact(&mut scenario, P2, "P2 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    steal(&mut runner, stolen, P1);

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "reach: order prompt"
    );
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");

    assert_eq!(
        pool(&runner),
        sorted(vec![stolen, p2_land]),
        "P2's pool is presented directly and includes the returned artifact"
    );
    pick(&mut runner, stolen);
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, stolen), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p2_land));
}

/// (c) With nothing changing between the choices, every order destroys the
/// same permanents.
#[test]
fn fixed_pools_give_the_same_result_in_every_order() {
    let mut destroyed = Vec::new();
    for first in [P1, P2] {
        let mut scenario = GameScenario::new_n_player(3, 204);
        scenario.at_phase(Phase::PreCombatMain);
        let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
        let p1_land = add_land(&mut scenario, P1, "P1 Land");
        let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
        let p2_land = add_land(&mut scenario, P2, "P2 Land");
        let meteor = add_meteor(&mut scenario);
        let mut runner = scenario.build();

        cast_from_exile(&mut runner, meteor);
        let prompts = resolve_with_order(
            &mut runner,
            |_, _| first,
            |r, cards| {
                vec![if controller(r, cards[0]) == P1 {
                    p1_art
                } else {
                    p2_land
                }]
            },
            |_, _| {},
        );
        assert_eq!(
            pool_owner(&runner, &prompts[0]),
            first,
            "reach: ordered {first:?} first"
        );
        let gone: Vec<ObjectId> = [p1_art, p1_land, p2_art, p2_land]
            .into_iter()
            .filter(|id| zone_of(&runner, *id) == Some(Zone::Graveyard))
            .collect();
        destroyed.push(gone.len());
        assert_eq!(gone, vec![p1_art, p2_land]);
    }
    assert_eq!(destroyed, vec![2, 2]);
}

/// With a single eligible opponent there is nothing to order: the pool is
/// presented directly.
#[test]
fn a_single_eligible_opponent_gets_no_order_prompt() {
    let mut scenario = GameScenario::new_n_player(3, 205);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(pool(&runner), vec![p1_art], "presented directly");
}

/// The AI's legal actions at the order prompt are exactly one order pick per
/// eligible opponent.
#[test]
fn ai_legal_actions_at_the_order_prompt_are_the_eligible_opponents() {
    let mut scenario = GameScenario::new_n_player(3, 206);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    let mut offered: Vec<PlayerId> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseZoneOpponentChooser { opponent } => Some(opponent),
            _ => None,
        })
        .collect();
    offered.sort();
    assert_eq!(offered, vec![P1, P2]);
}

// ---------------------------------------------------------------------------
// CR 800.4a: departures while the per-opponent choice is outstanding
// ---------------------------------------------------------------------------

/// E1 (owner leaves, controller stays): P1 controls an artifact P2 owns. P0
/// is choosing from P1's pool when P2 concedes: P2's artifact leaves the game,
/// so P1's refreshed pool holds only P1's own artifact, and the departed one
/// cannot be submitted.
///
/// REVERT PROBE: reconcile only when `current` or a candidate leaves, and the
/// departed artifact stays offered in P1's pool.
#[test]
fn an_offered_permanents_owner_leaving_refreshes_the_current_pool() {
    let mut scenario = GameScenario::new_n_player(4, 211);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_own = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_owned = add_artifact(&mut scenario, P2, "P2 Relic Held By P1");
    let p2_own = add_artifact(&mut scenario, P2, "P2 Relic");
    let p3_art = add_artifact(&mut scenario, P3, "P3 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    steal(&mut runner, p2_owned, P1);

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(order_candidates(&runner), vec![P1, P2, P3]);
    order(&mut runner, P1);
    assert_eq!(
        pool(&runner),
        sorted(vec![p1_own, p2_owned]),
        "reach: P1's pool offers P2's artifact"
    );

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("a player may concede at any time");
    assert_eq!(
        pool(&runner),
        vec![p1_own],
        "the departed owner's artifact is gone"
    );
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![p2_owned]
            })
            .is_err(),
        "the departed artifact cannot be submitted"
    );
    pick(&mut runner, p1_own);
    assert_eq!(
        pool(&runner),
        vec![p3_art],
        "only P3 remains, so P3's pool is presented directly"
    );
    pick(&mut runner, p3_art);
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p1_own), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p3_art), Some(Zone::Graveyard));
    assert!(
        !on_battlefield(&runner, p2_own),
        "P2's objects left the game"
    );
}

/// E1 mirror: a non-current player's control effect ends when they leave, so
/// the current pool GROWS: P1's artifact that P2 held returns to P1 and is
/// offered.
#[test]
fn a_control_effect_ending_adds_to_the_current_pool() {
    let mut scenario = GameScenario::new_n_player(3, 212);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_first = add_artifact(&mut scenario, P1, "P1 Relic");
    let p1_second = add_artifact(&mut scenario, P1, "P1 Relic Held By P2");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    steal(&mut runner, p1_second, P2);

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P1);
    assert_eq!(
        pool(&runner),
        vec![p1_first],
        "reach: P2 holds P1's second artifact"
    );

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("a player may concede at any time");
    assert_eq!(
        pool(&runner),
        sorted(vec![p1_first, p1_second]),
        "the returned artifact joins P1's pool"
    );
    pick(&mut runner, p1_second);
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, p1_second), Some(Zone::Graveyard));
    assert!(on_battlefield(&runner, p1_first));
}

/// E1: a departure that empties a candidate's pool collapses the order prompt
/// — with one eligible opponent left, that pool is presented directly.
#[test]
fn a_departure_that_empties_a_candidate_collapses_the_order_prompt() {
    let mut scenario = GameScenario::new_n_player(4, 213);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_owned = add_artifact(&mut scenario, P2, "P2 Relic Held By P3");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    steal(&mut runner, p2_owned, P3);

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P3],
        "reach: P2 controls nothing"
    );
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("a player may concede at any time");
    assert_eq!(
        pool(&runner),
        vec![p1_art],
        "P3's pool emptied; P1's is presented"
    );
}

/// A departure that changes no pool leaves the outstanding prompt unchanged.
#[test]
fn an_unrelated_departure_leaves_the_prompt_unchanged() {
    let mut scenario = GameScenario::new_n_player(4, 214);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "reach: P3 has nothing"
    );
    let prompt = runner.state().waiting_for.clone();
    let frame = runner.state().active_per_player_zone_choice().cloned();
    runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("a player may concede at any time");
    assert_eq!(runner.state().waiting_for, prompt);
    let mut expected = frame.expect("reach: a parked frame");
    expected.remaining_players.retain(|&player| player != P3);
    assert_eq!(
        runner.state().active_per_player_zone_choice().cloned(),
        Some(expected),
        "the frame is unchanged except that the departed player is no longer iterated"
    );
}

/// The outstanding election prompt (CR 800.4g): who elects, and the
/// candidates.
fn election(runner: &GameRunner) -> (PlayerId, Vec<PlayerId>) {
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates,
            purpose: ZoneOpponentChooserPurpose::SubstituteChooser,
            ..
        } => (*player, candidates.clone()),
        other => panic!("expected an election prompt, got {other:?}"),
    }
}

/// The outstanding pool prompt: who chooses, and the candidates.
fn pool_prompt(runner: &GameRunner) -> (PlayerId, Vec<ObjectId>) {
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => (*player, sorted(cards.clone())),
        other => panic!("expected a pool prompt, got {other:?}"),
    }
}

fn act_as(runner: &mut GameRunner, action: GameAction) {
    runner.act(action).expect("a legal answer is accepted");
}

fn elect(runner: &mut GameRunner, nominee: PlayerId) {
    act_as(
        runner,
        GameAction::ChooseZoneOpponentChooser { opponent: nominee },
    );
}

/// E2 / S1 (CR 608.2m + CR 800.4h + CR 800.4g): the caster concedes at the
/// order prompt. The spell keeps resolving. The order is a rule-required
/// choice, so it goes straight to the next player in turn order (P1) — no
/// election. Each pool pick is an object-required choice, elected separately
/// by P1: P2 for P1's pool, then P1 for P2's pool. Both picks are destroyed.
///
/// REVERT PROBE: keep the first nominee for the rest of the iteration and the
/// second pool goes to P2 with no second election.
#[test]
fn caster_leaving_hands_the_order_on_and_elects_each_pick_separately() {
    let mut scenario = GameScenario::new_n_player(3, 221);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "reach: order prompt"
    );
    assert!(
        runner.state().resolving_stack_entry.is_some(),
        "reach: Meteor is resolving"
    );
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates,
            purpose: ZoneOpponentChooserPurpose::PerPlayerChoiceOrder,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the next player in turn order makes the order choice"
            );
            assert_eq!(
                candidates,
                &vec![P1, P2],
                "P0's opponents, by last-known information"
            );
        }
        other => panic!("expected P1's ORDER prompt, not an election: {other:?}"),
    }
    act_as(
        &mut runner,
        GameAction::ChooseZoneOpponentChooser { opponent: P1 },
    );

    assert_eq!(
        election(&runner),
        (P1, vec![P1, P2]),
        "P1 elects for P1's pool"
    );
    elect(&mut runner, P2);
    assert_eq!(
        pool_prompt(&runner),
        (P2, vec![p1_art]),
        "P2 makes P1's pick"
    );
    act_as(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![p1_art],
        },
    );

    assert_eq!(
        election(&runner),
        (P1, vec![P1, P2]),
        "a fresh election for P2's pool, though P2 is still in the game"
    );
    elect(&mut runner, P1);
    assert_eq!(
        pool_prompt(&runner),
        (P1, vec![p2_land]),
        "P1 makes P2's pick"
    );
    act_as(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![p2_land],
        },
    );
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "CR 800.4j: with the active player gone, the next player in turn order"
    );
}

/// E2 / S2: the caster concedes while P2's pool is pending, after a pick was
/// already published. The pending pool survives its election: P1 elects
/// themself, picks from P2's pool, and both the earlier pick and this one are
/// destroyed.
///
/// REVERT PROBE: clear `current` at the election and P2's pool is never
/// presented again.
#[test]
fn caster_leaving_at_a_pool_keeps_that_pool_through_its_election() {
    let mut scenario = GameScenario::new_n_player(3, 222);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_art = add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_land = add_land(&mut scenario, P2, "P2 Land");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P1);
    pick(&mut runner, p1_art);
    let chosen = runner
        .state()
        .chain_tracked_set_id
        .and_then(|id| runner.state().tracked_object_sets.get(&id).cloned())
        .unwrap_or_default();
    assert!(
        chosen.contains(&p1_art),
        "reach: the first pick was published"
    );
    let frame = runner
        .state()
        .active_per_player_zone_choice()
        .expect("reach: a parked frame");
    assert_eq!(frame.current, Some(P2), "reach: P2's pool is pending");
    assert!(
        !frame.remaining_players.contains(&P2),
        "reach: the pending pool is held only by `current`"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");
    assert_eq!(election(&runner), (P1, vec![P1, P2]));
    elect(&mut runner, P1);
    assert_eq!(
        pool_prompt(&runner),
        (P1, vec![p2_land]),
        "the same pending pool"
    );
    act_as(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![p2_land],
        },
    );
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p1_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p2_land), Some(Zone::Graveyard));
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
}

/// S2: a nominee who leaves while choosing is replaced for the SAME pending
/// pool by a fresh election (4 players).
#[test]
fn a_departing_nominee_is_replaced_for_the_same_pool() {
    let mut scenario = GameScenario::new_n_player(4, 223);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    let p2_art = add_artifact(&mut scenario, P2, "P2 Relic");
    let p3_art = add_artifact(&mut scenario, P3, "P3 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");
    act_as(
        &mut runner,
        GameAction::ChooseZoneOpponentChooser { opponent: P2 },
    );
    assert_eq!(election(&runner).0, P1, "P1 elects for P2's pool");
    elect(&mut runner, P1);
    assert_eq!(pool_prompt(&runner), (P1, vec![p2_art]));
    let frame = runner
        .state()
        .active_per_player_zone_choice()
        .expect("reach: a parked frame");
    assert_eq!(frame.current, Some(P2), "reach: P2's pool is pending");
    assert!(
        !frame.remaining_players.contains(&P2),
        "reach: `current` is not in `remaining`"
    );

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");
    let (elector, candidates) = election(&runner);
    assert_eq!(
        elector, P2,
        "the next player in turn order after P0 still in the game"
    );
    assert_eq!(candidates, vec![P2, P3]);
    elect(&mut runner, P3);
    assert_eq!(
        pool_prompt(&runner),
        (P3, vec![p2_art]),
        "the same pool, a new nominee"
    );
    act_as(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![p2_art],
        },
    );
    assert_eq!(election(&runner).0, P2, "P3's pool is elected afresh");
    elect(&mut runner, P2);
    act_as(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![p3_art],
        },
    );
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, p2_art), Some(Zone::Graveyard));
    assert_eq!(zone_of(&runner, p3_art), Some(Zone::Graveyard));
}

/// The AI's legal actions at an election prompt are exactly the surviving
/// players.
#[test]
fn ai_legal_actions_at_an_election_are_the_surviving_players() {
    let mut scenario = GameScenario::new_n_player(3, 224);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    order(&mut runner, P1);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");
    assert_eq!(election(&runner).0, P1, "reach: an election is pending");
    round_trip(&runner, &["\"SubstituteChooser\""]);
    let mut offered: Vec<PlayerId> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseZoneOpponentChooser { opponent } => Some(opponent),
            _ => None,
        })
        .collect();
    offered.sort();
    assert_eq!(offered, vec![P1, P2]);
    elect(&mut runner, P2);
    round_trip(&runner, &["\"nominee\""]);
}

/// E2 terminal: the last opponent concedes at their pool prompt — the game is
/// over, and no prompt replaces `GameOver`.
///
/// REVERT PROBE: run the reconciliation before the game-over decision and a
/// per-player prompt replaces `GameOver`.
#[test]
fn last_opponent_conceding_ends_the_game_with_no_prompt() {
    let mut scenario = GameScenario::new_n_player(2, 223);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ));
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
}

/// E2 terminal, batched: both opponents leave in one simultaneous elimination
/// at the order prompt. The game is over and no prompt replaces `GameOver`.
#[test]
fn simultaneous_departure_of_every_opponent_ends_the_game_with_no_prompt() {
    let mut scenario = GameScenario::new_n_player(3, 224);
    scenario.at_phase(Phase::PreCombatMain);
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();

    cast_from_exile(&mut runner, meteor);
    advance_to_per_player_prompt(&mut runner);
    assert_eq!(
        order_candidates(&runner),
        vec![P1, P2],
        "reach: order prompt"
    );
    engine::game::elimination::eliminate_players_simultaneously(
        runner.state_mut(),
        &[P1, P2],
        &mut Vec::new(),
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
}

/// E2 control: a departure while no per-player choice is outstanding leaves
/// the prompt to the generic elimination path.
#[test]
fn a_departure_without_a_per_player_choice_is_untouched() {
    let mut scenario = GameScenario::new_n_player(3, 225);
    scenario.at_phase(Phase::PreCombatMain);
    let meteor = add_meteor(&mut scenario);
    let mut runner = scenario.build();
    cast_from_hand(&mut runner, meteor);
    let before = runner.state().waiting_for.clone();
    assert!(matches!(before, WaitingFor::Priority { player } if player == P0));
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("a player may concede at any time");
    assert_eq!(runner.state().waiting_for, before);
}

/// E2 under `repeat_for` (CR 608.2m + CR 800.4g/800.4h), production path:
/// "For each land you control, for each opponent, choose an artifact that
/// player controls." parses to `repeat_for` over the per-opponent choice. When
/// the caster concedes at the first prompt the resolution continues: every
/// later choice is made by a player still in the game, each pick through its
/// own election, and no prompt is ever addressed to the departed caster.
#[test]
fn caster_leaving_under_a_repeated_choice_keeps_resolving() {
    const TEXT: &str =
        "For each land you control, for each opponent, choose an artifact that player controls.";
    let parsed = parse(TEXT, "Repeated Probe", &[], &["Sorcery"]);
    assert!(
        all_defs(&parsed).iter().any(|def| def.repeat_for.is_some()
            && matches!(
                &*def.effect,
                Effect::ChooseFromZone {
                    zone_owner: ZoneOwner::Each(PerPlayerScope::Opponents),
                    ..
                }
            )),
        "reach: the phrase parses to a repeated per-opponent choice"
    );

    let mut scenario = GameScenario::new_n_player(4, 231);
    scenario.at_phase(Phase::PreCombatMain);
    add_land(&mut scenario, P0, "P0 Land A");
    add_land(&mut scenario, P0, "P0 Land B");
    add_artifact(&mut scenario, P1, "P1 Relic");
    add_artifact(&mut scenario, P2, "P2 Relic");
    add_artifact(&mut scenario, P3, "P3 Relic");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Repeated Probe", false, TEXT)
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
    advance_to_per_player_prompt(&mut runner);
    let kinds: Vec<_> = runner
        .state()
        .resolution_stack
        .iter()
        .map(engine::types::resolution::ResolutionFrame::kind)
        .collect();
    assert!(
        kinds.contains(&engine::types::resolution::FrameKind::RepeatFor),
        "reach: a repeat driver is parked beneath the choice: {kinds:?}"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");

    let mut elections = 0;
    let mut picks = 0;
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneOpponentChooser {
                player,
                candidates,
                purpose,
                ..
            } => {
                assert_ne!(player, P0, "no prompt for the departed caster");
                assert!(!candidates.contains(&P0));
                if purpose == ZoneOpponentChooserPurpose::SubstituteChooser {
                    elections += 1;
                }
                act_as(
                    &mut runner,
                    GameAction::ChooseZoneOpponentChooser {
                        opponent: candidates[0],
                    },
                );
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert_ne!(player, P0, "no prompt for the departed caster");
                picks += 1;
                act_as(
                    &mut runner,
                    GameAction::SelectCards {
                        cards: vec![cards[0]],
                    },
                );
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    // Two P0 lands x three opponents with an artifact each: six pool picks,
    // each needing its own election (CR 800.4g).
    assert_eq!(picks, 6, "every repeated pool pick was made");
    assert_eq!(elections, 6, "every pick was elected separately");
    assert!(
        runner.state().resolution_stack.is_empty(),
        "nothing left parked"
    );
    assert!(runner.state().stack.is_empty());
}
