//! CR 608.2c + CR 608.2f: "Each player creates … The tokens are …" — the
//! player-scoped creation is ONE action taken on several players, so the
//! following instruction's "the tokens" names every token that action created
//! for every seat (CR 111.1), including tokens a replacement effect makes in
//! their place or alongside them (CR 614.1a), whether the fan-out completes
//! synchronously or pauses mid-fan-out for a replacement-ordering choice
//! (CR 616.1). A token that left the battlefield before the instruction reads
//! the referent is a new object and is not included (CR 400.7, CR 609.3).

use std::collections::BTreeSet;

use engine::game::effects::resolve_ability_chain;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition, Duration,
    Effect, FilterProp, PlayerFilter, PtValue, QuantityExpr, QuantityRef, ResolvedAbility,
    StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const WAR_GAMES: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Each player creates three tapped 1/1 white Warrior creature tokens. The tokens are goaded for as long as this Saga remains on the battlefield.\nII, III — Put a +1/+1 counter on each Warrior creature.\nIV — You may exile a nontoken creature you control. When you do, exile all Warriors.";
const RENDMAW: &str = "Reach, menace\nWhen Rendmaw enters and whenever you play a card with two or more card types, each player creates a tapped 2/2 black Bird creature token with flying. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";
const LIFE_OF_THE_PARTY: &str = "First strike, trample, haste\nWhenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\nWhen this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";
const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
const TEMPT_WITH_VENGEANCE: &str = "Tempting offer — Create X 1/1 red Elemental creature tokens with haste. Each opponent may create X 1/1 red Elemental creature tokens with haste. For each opponent who does, create X 1/1 red Elemental creature tokens with haste.";

/// Which replacement a `ReplacementChoice` answer applies first (CR 616.1).
#[derive(Clone, Copy)]
enum First {
    ParallelLives,
    HalvingSeason,
}

fn goaded_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]))
}

fn is_goaded(state: &GameState, id: ObjectId) -> bool {
    matches_target_filter(state, id, &goaded_filter(), &FilterContext::neutral())
}

/// Tokens named `name` controlled by `controller`, on the battlefield.
fn tokens(state: &GameState, controller: PlayerId, name: &str) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.name == name && object.controller == controller
        })
        .collect()
}

fn all_tokens(state: &GameState, name: &str) -> Vec<ObjectId> {
    [P0, P1, P2]
        .into_iter()
        .flat_map(|player| tokens(state, player, name))
        .collect()
}

/// The exact objects `source` registered a goad grant for (one transient
/// continuous effect per created token).
fn goad_registrations(state: &GameState, source: ObjectId) -> BTreeSet<ObjectId> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.source_id == source
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    })
        })
        .filter_map(|effect| match &effect.affected {
            TargetFilter::SpecificObject { id } => Some(*id),
            _ => None,
        })
        .collect()
}

fn named_permanent(state: &GameState, name: &str) -> ObjectId {
    *state
        .battlefield
        .iter()
        .find(|id| state.objects[id].name == name && !state.objects[id].is_token)
        .unwrap_or_else(|| panic!("{name} must be on the battlefield"))
}

/// Drive the cast to an empty stack. `on_replacement` answers each
/// `ReplacementChoice`; `on_optional` answers each optional prompt.
fn drive(
    runner: &mut GameRunner,
    on_replacement: &mut dyn FnMut(&mut GameRunner, PlayerId) -> First,
    on_optional: &dyn Fn(PlayerId) -> bool,
) -> Vec<PlayerId> {
    let mut prompted = Vec::new();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompted,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger order accepted");
            }
            WaitingFor::OptionalEffectChoice { player, .. }
            | WaitingFor::OpponentMayChoice { player, .. } => {
                prompted.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: on_optional(player),
                    })
                    .expect("optional answer accepted");
            }
            WaitingFor::ReplacementChoice {
                player, candidates, ..
            } => {
                prompted.push(player);
                let first = on_replacement(runner, player);
                let needle = match first {
                    First::ParallelLives => "twice",
                    First::HalvingSeason => "half",
                };
                let index = candidates
                    .iter()
                    .position(|candidate| candidate.description.contains(needle))
                    .expect("the chosen replacement is a candidate");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("replacement choice accepted");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the cast did not settle");
}

fn no_replacement(_: &mut GameRunner, player: PlayerId) -> First {
    panic!("no replacement choice expected, got one for {player:?}")
}

fn no_optional(player: PlayerId) -> bool {
    panic!("no optional prompt expected, got one for {player:?}")
}

fn war_games_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let war = scenario
        .add_spell_to_hand(P0, "The War Games", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(WAR_GAMES)
        .id();
    (scenario, war)
}

/// Every token named `name` is goaded and carries exactly one goad registration
/// from `source`; returns how many there are.
fn assert_every_token_goaded(state: &GameState, source: ObjectId, name: &str) -> usize {
    let all = all_tokens(state, name);
    let registered = goad_registrations(state, source);
    for id in &all {
        assert!(
            is_goaded(state, *id),
            "{name} {id:?} (controller {:?}) must be goaded",
            state.objects[id].controller
        );
        assert!(
            registered.contains(id),
            "{name} {id:?} must carry the source's goad registration"
        );
    }
    all.len()
}

/// V1 — synchronous union: all nine Warriors are "the tokens".
#[test]
fn war_games_goads_every_players_warriors() {
    let (scenario, war) = war_games_scenario();
    let mut runner = scenario.build();
    runner.cast(war).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    assert!(state.stack.is_empty());
    let saga = named_permanent(state, "The War Games");
    for player in [P0, P1, P2] {
        let mine = tokens(state, player, "Warrior");
        assert_eq!(mine.len(), 3, "{player:?} creates three Warriors");
        // CR 111.2: the creating player owns its tokens.
        assert!(mine.iter().all(|id| state.objects[id].owner == player));
    }
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 9);
    assert_eq!(goad_registrations(state, saga).len(), 9);
}

/// V2 — Rendmaw's ETB: one Bird per seat, all goaded.
#[test]
fn rendmaw_goads_every_players_bird() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let rendmaw = scenario
        .add_creature_to_hand(P0, "Rendmaw, Creaking Nest", 5, 5)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Scarecrow"])
        .from_oracle_text_with_keywords(&["Reach", "Menace"], RENDMAW)
        .id();
    let mut runner = scenario.build();
    runner.cast(rendmaw).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    for player in [P0, P1, P2] {
        assert_eq!(tokens(state, player, "Bird").len(), 1, "{player:?}'s Bird");
    }
    assert_eq!(assert_every_token_goaded(state, rendmaw, "Bird"), 3);
}

/// V3 — `CopyTokenOf` member: both opponents' copies are goaded; the nontoken
/// original is not one of "the tokens".
#[test]
fn life_of_the_party_goads_every_opponents_copy_not_the_original() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let original = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(&["First strike", "Trample", "Haste"], LIFE_OF_THE_PARTY)
        .id();
    let mut runner = scenario.build();
    runner.cast(original).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    assert!(state.stack.is_empty(), "the ETB trigger resolved");
    assert!(tokens(state, P0, "Life of the Party").is_empty());
    for player in [P1, P2] {
        let copies = tokens(state, player, "Life of the Party");
        assert_eq!(copies.len(), 1, "{player:?} creates one copy");
        assert!(is_goaded(state, copies[0]), "{player:?}'s copy is goaded");
    }
    // Paired with the positive rows above: the original is on the battlefield
    // and is not a token the action created.
    assert!(state.battlefield.contains(&original));
    assert!(
        !is_goaded(state, original),
        "the nontoken original is not goaded"
    );
}

/// V4 — a multiplied batch (Parallel Lives, one replacement, no prompt).
#[test]
fn war_games_goads_parallel_lives_doubled_warriors() {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    let saga = named_permanent(state, "The War Games");
    assert_eq!(tokens(state, P1, "Warrior").len(), 6);
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 12);
}

/// Find a `player_scope_linked_exile` sidecar object carrying a
/// `created_tokens` channel anywhere in a serialized state.
fn serialized_created_channel_len(value: &serde_json::Value) -> Option<usize> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(len) = map
                .get("player_scope_linked_exile")
                .and_then(|sidecar| sidecar.get("created_tokens"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len)
            {
                return Some(len);
            }
            map.values().find_map(serialized_created_channel_len)
        }
        serde_json::Value::Array(items) => items.iter().find_map(serialized_created_channel_len),
        _ => None,
    }
}

/// V5 / V5b — the fan-out pauses on P1 (Halving Season and Parallel Lives
/// both apply to P1's creation). The tail waits on the sidecar, survives a
/// serde round trip, and goads every seat's Warriors once P1 answers.
fn war_games_paused_mid_fan_out(first: First) -> (usize, usize) {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    let mut at_prompt = |runner: &mut GameRunner, player: PlayerId| {
        // CR 616.1: the affected player orders the replacements.
        assert_eq!(player, P1);
        let state = runner.state();
        assert_eq!(tokens(state, P0, "Warrior").len(), 3, "P0's leg completed");
        assert!(
            all_tokens(state, "Warrior")
                .iter()
                .all(|id| !is_goaded(state, *id)),
            "the goad tail waits for every seat"
        );
        let wire = serde_json::to_value(state).expect("paused state serializes");
        assert_eq!(
            serialized_created_channel_len(&wire),
            Some(3),
            "the paused sidecar carries P0's three Warriors"
        );
        *runner.state_mut() = serde_json::from_value(wire).expect("paused state restores");
        first
    };
    let prompted = drive(&mut runner, &mut at_prompt, &no_optional);
    assert_eq!(prompted, vec![P1]);
    let state = runner.state();
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    let saga = named_permanent(state, "The War Games");
    assert_eq!(tokens(state, P0, "Warrior").len(), 3);
    // P0's Halving Season halves P2's three Warriors to one.
    assert_eq!(tokens(state, P2, "Warrior").len(), 1);
    (
        tokens(state, P1, "Warrior").len(),
        assert_every_token_goaded(state, saga, "Warrior"),
    )
}

#[test]
fn war_games_pause_mid_fan_out_goads_every_seat_parallel_first() {
    // Parallel Lives first: 3 → 6, then halved → 3.
    assert_eq!(war_games_paused_mid_fan_out(First::ParallelLives), (3, 7));
}

#[test]
fn war_games_pause_mid_fan_out_goads_every_seat_halving_first() {
    // Halving Season first: 3 → 1, then doubled → 2.
    assert_eq!(war_games_paused_mid_fan_out(First::HalvingSeason), (2, 6));
}

/// V6 / V6b — the fan-out pauses on the LAST seat (queue-end frame). In V6b a
/// completed seat's Warrior leaves the battlefield during the pause: it is a
/// new object (CR 400.7) and is not goaded (CR 609.3).
fn war_games_paused_on_last_seat(move_one_of_p0s: bool) -> (GameRunner, Option<ObjectId>) {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P2, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    let mut moved = None;
    let mut at_prompt = |runner: &mut GameRunner, player: PlayerId| {
        assert_eq!(player, P2, "CR 616.1: P2 orders its own replacements");
        if move_one_of_p0s {
            let victim = tokens(runner.state(), P0, "Warrior")[0];
            let before = runner.state().objects[&victim].incarnation;
            move_to_zone(runner.state_mut(), victim, Zone::Graveyard, &mut Vec::new());
            assert!(
                runner
                    .state()
                    .objects
                    .get(&victim)
                    .is_some_and(|object| object.incarnation != before),
                "reach guard: the moved Warrior is a new object"
            );
            moved = Some(victim);
        }
        First::ParallelLives
    };
    let prompted = drive(&mut runner, &mut at_prompt, &no_optional);
    assert_eq!(prompted, vec![P2]);
    (runner, moved)
}

#[test]
fn war_games_pause_on_last_seat_goads_every_seat() {
    let (runner, _) = war_games_paused_on_last_seat(false);
    let state = runner.state();
    assert!(state.stack.is_empty());
    let saga = named_permanent(state, "The War Games");
    assert_eq!(tokens(state, P0, "Warrior").len(), 3);
    assert_eq!(tokens(state, P1, "Warrior").len(), 1);
    assert_eq!(tokens(state, P2, "Warrior").len(), 3);
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 7);
}

#[test]
fn war_games_union_excludes_a_member_that_left_before_publication() {
    let (runner, moved) = war_games_paused_on_last_seat(true);
    let moved = moved.expect("one of P0's Warriors was moved");
    let state = runner.state();
    let saga = named_permanent(state, "The War Games");
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 6);
    assert!(
        !goad_registrations(state, saga).contains(&moved),
        "no goad grant names the departed Warrior's id"
    );
}

/// V7 — nested re-pause: P1 and P2 each pause; the sidecar is re-parked with
/// its accumulated union between the two prompts.
#[test]
fn war_games_two_pauses_goad_every_seat() {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P2, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    let prompted = drive(&mut runner, &mut |_, _| First::ParallelLives, &no_optional);
    // CR 101.4: APNAP order.
    assert_eq!(prompted, vec![P1, P2]);
    let state = runner.state();
    let saga = named_permanent(state, "The War Games");
    for player in [P0, P1, P2] {
        assert_eq!(tokens(state, player, "Warrior").len(), 3);
    }
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 9);
}

/// A tapped 1/1 white creature token effect named `name` (and of that subtype).
fn vanilla_token(name: &str, count: i32) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), name.to_string()],
        colors: vec![ManaColor::White],
        keywords: vec![],
        tapped: true,
        count: QuantityExpr::Fixed { value: count },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// V8 — attribution window: a token an EARLIER clause of the same source
/// created is not one of "the tokens" of the player-scoped clause.
#[test]
fn union_excludes_a_prior_clause_of_the_same_source() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Union source".to_string(),
        Zone::Battlefield,
    );
    let token = |name: &str| vanilla_token(name, 1);
    // The War Games tail shape, as the parser lowers "The tokens are goaded …".
    let goad = ResolvedAbility::new(
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::LastCreated)
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::Goaded,
                }])],
            duration: Some(Duration::WhileHostOnBattlefield),
            target: Some(TargetFilter::LastCreated),
            end_cost: None,
        },
        vec![],
        source,
        P0,
    );
    let mut warriors = ResolvedAbility::new(token("Warrior"), vec![], source, P0);
    warriors.player_scope = Some(PlayerFilter::All);
    warriors.sub_ability = Some(Box::new(goad));
    let mut chain = ResolvedAbility::new(token("Prior"), vec![], source, P0);
    chain.sub_ability = Some(Box::new(warriors));

    resolve_ability_chain(&mut state, &chain, &mut Vec::new(), 0).expect("chain resolves");
    evaluate_layers(&mut state);

    let warriors = all_tokens(&state, "Warrior");
    assert_eq!(warriors.len(), 3, "one Warrior per seat");
    assert!(
        warriors.iter().all(|id| is_goaded(&state, *id)),
        "every seat's Warrior is goaded"
    );
    let prior = tokens(&state, P0, "Prior");
    assert_eq!(prior.len(), 1, "the earlier clause made its token");
    assert!(
        !is_goaded(&state, prior[0]),
        "the earlier clause's token is not"
    );
}

/// V12 — substituted batch (Divine Visitation under P2): P2's Angels are the
/// tokens P2's leg of the action created, so they are goaded with the rest.
#[test]
fn war_games_goads_divine_visitation_angels_and_every_warrior() {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_enchantment_from_oracle(P2, "Divine Visitation", DIVINE_VISITATION);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    let saga = named_permanent(state, "The War Games");
    assert_eq!(
        tokens(state, P2, "Angel").len(),
        3,
        "the substitution applied"
    );
    assert!(tokens(state, P2, "Warrior").is_empty());
    assert_eq!(assert_every_token_goaded(state, saga, "Angel"), 3);
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 6);
    assert_eq!(goad_registrations(state, saga).len(), 9);
}

/// V13 — appended batch (Chatterfang under P2): P2's Squirrels are created by
/// the same action, so they are goaded too.
#[test]
fn war_games_goads_chatterfang_squirrels_and_every_warrior() {
    let (mut scenario, war) = war_games_scenario();
    scenario.add_creature_from_oracle(P2, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let mut runner = scenario.build();
    runner.cast(war).commit();
    drive(&mut runner, &mut no_replacement, &no_optional);
    let state = runner.state();
    let saga = named_permanent(state, "The War Games");
    assert_eq!(tokens(state, P2, "Squirrel").len(), 3, "the append applied");
    assert_eq!(tokens(state, P2, "Warrior").len(), 3);
    assert_eq!(assert_every_token_goaded(state, saga, "Squirrel"), 3);
    assert_eq!(assert_every_token_goaded(state, saga, "Warrior"), 9);
    assert_eq!(goad_registrations(state, saga).len(), 12);
}

/// V14 — a player-scoped token clause whose tail reads no created referent
/// keeps today's route: "For each opponent who does, create X …".
fn tempt_with_vengeance(accept: &dyn Fn(PlayerId) -> bool) -> [usize; 3] {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tempt with Vengeance", false, TEMPT_WITH_VENGEANCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Red, ManaType::Colorless, ManaType::Colorless]
            .iter()
            .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(spell).x(2).commit();
    let prompted = drive(&mut runner, &mut no_replacement, accept);
    assert_eq!(prompted, vec![P1, P2], "both opponents are offered");
    let state = runner.state();
    assert!(state.stack.is_empty());
    [P0, P1, P2].map(|player| tokens(state, player, "Elemental").len())
}

#[test]
fn tempt_with_vengeance_both_opponents_accept_keeps_its_count() {
    assert_eq!(tempt_with_vengeance(&|_| true), [6, 2, 2]);
}

#[test]
fn tempt_with_vengeance_one_opponent_accepts_keeps_its_count() {
    assert_eq!(tempt_with_vengeance(&|player| player == P1), [4, 2, 0]);
}

/// V16 / V17 / V18 — the paused route for a tail whose only reader is a
/// delayed trigger's payload, a counted effect quantity, or a counted
/// `repeat_for`. "Each player creates three … tokens"
/// (constructed: no printed card carries these tails) pauses on P1 (Halving
/// Season and Parallel Lives both apply, CR 616.1); `tail` must still read every
/// seat's tokens once P1 answers Parallel Lives first (P0 3, P1 3, P2 1).
fn paused_constructed_fan_out(
    tail: Effect,
    repeat_for: Option<QuantityExpr>,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
    scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = scenario.build();
    let source = create_object(
        runner.state_mut(),
        CardId(900),
        P0,
        "Union source".to_string(),
        Zone::Battlefield,
    );
    let mut warriors = ResolvedAbility::new(vanilla_token("Warrior", 3), vec![], source, P0);
    warriors.player_scope = Some(PlayerFilter::All);
    let mut tail = ResolvedAbility::new(tail, vec![], source, P0);
    tail.repeat_for = repeat_for;
    warriors.sub_ability = Some(Box::new(tail));

    resolve_ability_chain(runner.state_mut(), &warriors, &mut Vec::new(), 0)
        .expect("chain resolves");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P1
        ),
        "reach guard: the fan-out pauses on P1 (CR 616.1)"
    );
    assert_eq!(
        tokens(runner.state(), P0, "Warrior").len(),
        3,
        "reach guard: P0's leg completed before the pause"
    );
    let prompted = drive(&mut runner, &mut |_, _| First::ParallelLives, &no_optional);
    assert_eq!(prompted, vec![P1]);
    let state = runner.state();
    assert_eq!(tokens(state, P0, "Warrior").len(), 3);
    assert_eq!(tokens(state, P1, "Warrior").len(), 3);
    assert_eq!(tokens(state, P2, "Warrior").len(), 1);
    (runner, source)
}

/// V16 — "At the beginning of the next end step, sacrifice those tokens": the
/// delayed trigger snapshots "those tokens" when it is created (CR 603.7c), so
/// on the paused route it must snapshot every seat's Warriors, not P2's alone.
#[test]
fn paused_fan_out_delayed_trigger_snapshots_every_seats_tokens() {
    let tail = Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::LastCreated,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        )),
        uses_tracked_set: false,
    };
    let (runner, source) = paused_constructed_fan_out(tail, None);
    let state = runner.state();
    let delayed: Vec<_> = state
        .delayed_triggers
        .iter()
        .filter(|trigger| trigger.source_id == source)
        .collect();
    assert_eq!(delayed.len(), 1, "the delayed trigger was created");
    let snapshot: BTreeSet<ObjectId> = delayed[0]
        .ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    let every_warrior: BTreeSet<ObjectId> = all_tokens(state, "Warrior").into_iter().collect();
    assert_eq!(every_warrior.len(), 7);
    assert_eq!(snapshot, every_warrior, "\"those tokens\" are every seat's");
}

/// V17 — "You gain 1 life for each of those tokens": an effect quantity counting
/// the created referent's population reads every seat's tokens on the paused
/// route.
#[test]
fn paused_fan_out_counted_quantity_reads_every_seats_tokens() {
    let tail = Effect::GainLife {
        amount: count_warriors_created(),
        player: TargetFilter::Controller,
    };
    let starting = GameScenario::new_n_player(3, 42).build().state().players[0].life;
    let (runner, _) = paused_constructed_fan_out(tail, None);
    assert_eq!(runner.state().players[0].life, starting + 7);
}

/// V18 — "For each of those tokens, you gain 1 life": a `repeat_for` count over
/// the created referent's population reads every seat's tokens on the paused
/// route.
#[test]
fn paused_fan_out_repeat_count_reads_every_seats_tokens() {
    let tail = Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 1 },
        player: TargetFilter::Controller,
    };
    let starting = GameScenario::new_n_player(3, 42).build().state().players[0].life;
    let (runner, _) = paused_constructed_fan_out(tail, Some(count_warriors_created()));
    assert_eq!(runner.state().players[0].life, starting + 7);
}

/// The number of tokens in the created referent ("those tokens").
fn count_warriors_created() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::LastCreated,
        },
    }
}
