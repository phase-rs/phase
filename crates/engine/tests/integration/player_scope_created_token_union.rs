//! CR 608.2c + CR 608.2f: "Each player creates … The tokens are …" — the
//! player-scoped creation is ONE action taken on several players, so the
//! following instruction's "the tokens" names every token that action created
//! for every seat (CR 111.1), including tokens a replacement effect makes in
//! their place or alongside them (CR 614.1a), whether the fan-out completes
//! synchronously or pauses mid-fan-out for a replacement-ordering choice
//! (CR 616.1). A token that left the battlefield before the instruction reads
//! the referent is a new object and is not included (CR 400.7, CR 609.3).

use std::collections::BTreeSet;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, ContinuousModification, ControllerRef,
    DelayedTriggerCondition, DelayedTriggerLifetime, DieResultBranch, Duration, Effect,
    EffectScope, FilterProp, NumberQuality, PlayerFilter, PlayerRelation, PlayerScope, PtStat,
    PtValue, PtValueScope, QuantityExpr, QuantityRef, ResolvedAbility, StaticCondition,
    StaticDefinition, SubAbilityLink, TapStateChange, TargetFilter, TargetRef, TriggerDefinition,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
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
            // One pass at a time, so a resolution in a game of any size leaves
            // priority with the active player (CR 117.3b) when the stack empties.
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass accepted");
            }
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

/// Find the first `player_scope_linked_exile` sidecar object carrying a
/// `created_tokens` channel anywhere in a serialized state, and read it per
/// seat: each entry's seat and how many tokens that seat's instruction has
/// created so far, in channel order (APNAP; the last entry is the seat in
/// progress).
fn serialized_created_channel(value: &serde_json::Value) -> Option<Vec<(PlayerId, usize)>> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(entries) = map
                .get("player_scope_linked_exile")
                .and_then(|sidecar| sidecar.get("created_tokens"))
                .and_then(serde_json::Value::as_array)
            {
                return Some(
                    entries
                        .iter()
                        .map(|entry| {
                            let seat = entry["seat"].as_u64().expect("a seat entry names its seat");
                            let tokens = entry["tokens"]
                                .as_array()
                                .expect("a seat entry carries its tokens")
                                .len();
                            (PlayerId(u8::try_from(seat).expect("a seat id")), tokens)
                        })
                        .collect(),
                );
            }
            map.values().find_map(serialized_created_channel)
        }
        serde_json::Value::Array(items) => items.iter().find_map(serialized_created_channel),
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
            serialized_created_channel(&wire),
            Some(vec![(P0, 3), (P1, 0)]),
            "the paused sidecar carries P0's three Warriors and P1, the seat in progress"
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

/// The +1/+1 counters on `id`.
fn plus_one_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// "Put a +1/+1 counter on each of those tokens": the mass-population slot of
/// `PutCounterAll` (hidden from `target_filter()`).
fn put_counter_on_each_created() -> Effect {
    Effect::PutCounterAll {
        counter_type: CounterType::Plus1Plus1,
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::LastCreated,
    }
}

/// "Roll a die. 1-6 | You gain 1 life for each of those tokens.": an inline
/// results-table branch (CR 706.3a) reading the created referent.
fn roll_then_gain_per_created() -> Effect {
    Effect::RollDie {
        count: QuantityExpr::Fixed { value: 1 },
        sides: 6,
        results: vec![DieResultBranch {
            min: 1,
            max: 6,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: count_warriors_created(),
                    player: TargetFilter::Controller,
                },
            )),
        }],
        modifier: None,
    }
}

/// "You get an emblem with 'Those tokens get +1/+0.'": a registered static
/// payload whose `affected` reads the live created-token ledger (CR 611.3a).
fn emblem_pumping_created() -> Effect {
    Effect::CreateEmblem {
        statics: vec![StaticDefinition::continuous()
            .affected(TargetFilter::LastCreated)
            .modifications(vec![ContinuousModification::AddPower { value: 1 }])],
        triggers: vec![],
    }
}

/// The synchronous sibling of [`paused_constructed_fan_out`]: no replacement
/// effects, so every seat creates three Warriors and nothing pauses.
fn sync_constructed_fan_out(tail: Effect) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
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
    warriors.sub_ability = Some(Box::new(ResolvedAbility::new(tail, vec![], source, P0)));
    resolve_ability_chain(runner.state_mut(), &warriors, &mut Vec::new(), 0)
        .expect("chain resolves");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: the synchronous fan-out does not pause"
    );
    assert_eq!(all_tokens(runner.state(), "Warrior").len(), 9);
    runner
}

/// N1-P1 — "Put a +1/+1 counter on each of those tokens" after a paused fan-out:
/// the mass slot reads the completed clause union (CR 608.2e), not the last
/// seat's tokens.
#[test]
fn paused_fan_out_mass_counter_placement_reads_every_seats_tokens() {
    let (runner, _) = paused_constructed_fan_out(put_counter_on_each_created(), None);
    let state = runner.state();
    let warriors = all_tokens(state, "Warrior");
    assert_eq!(warriors.len(), 7);
    assert!(
        warriors.iter().all(|id| plus_one_counters(state, *id) == 1),
        "every seat's Warrior has exactly one counter"
    );
    assert!(
        state
            .battlefield
            .iter()
            .filter(|id| state.objects[id].name != "Warrior")
            .all(|id| plus_one_counters(state, *id) == 0),
        "no non-Warrior has a counter"
    );
}

/// N1-P2 — an inline die-result branch reading "those tokens" after a paused
/// fan-out counts every seat's tokens.
#[test]
fn paused_fan_out_inline_branch_reads_every_seats_tokens() {
    let starting = GameScenario::new_n_player(3, 42).build().state().players[0].life;
    let (runner, _) = paused_constructed_fan_out(roll_then_gain_per_created(), None);
    assert_eq!(runner.state().players[0].life, starting + 7);
}

/// N1-P3 — nearest publisher inside an inline branch: the branch's own Soldier
/// is its placement's referent, so no Warrior gets the counter.
#[test]
fn paused_fan_out_inline_branch_own_token_is_its_placements_referent() {
    let mut branch = AbilityDefinition::new(AbilityKind::Spell, vanilla_token("Soldier", 1));
    branch.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::LastCreated,
        },
    )));
    let tail = Effect::RollDie {
        count: QuantityExpr::Fixed { value: 1 },
        sides: 6,
        results: vec![DieResultBranch {
            min: 1,
            max: 6,
            effect: Box::new(branch),
        }],
        modifier: None,
    };
    let (runner, _) = paused_constructed_fan_out(tail, None);
    let state = runner.state();
    let soldiers = tokens(state, P0, "Soldier");
    assert_eq!(
        soldiers.len(),
        1,
        "reach guard: the branch created its Soldier"
    );
    assert_eq!(plus_one_counters(state, soldiers[0]), 1);
    assert!(all_tokens(state, "Warrior")
        .iter()
        .all(|id| plus_one_counters(state, *id) == 0));
}

/// N1-P4 — a registered static payload (an emblem's "those tokens get +1/+0")
/// reads the live ledger after the paused fan-out, so it must see the union.
#[test]
fn paused_fan_out_emblem_static_reads_every_seats_tokens() {
    let (mut runner, _) = paused_constructed_fan_out(emblem_pumping_created(), None);
    evaluate_layers(runner.state_mut());
    let state = runner.state();
    assert_eq!(
        state
            .objects
            .values()
            .filter(|object| object.zone == Zone::Command)
            .count(),
        1,
        "reach guard: one emblem in the command zone"
    );
    let warriors = all_tokens(state, "Warrior");
    assert_eq!(warriors.len(), 7);
    assert!(
        warriors.iter().all(|id| state.objects[id].power == Some(2)),
        "every seat's Warrior gets +1/+0"
    );
}

/// N1 synchronous siblings: the same tails without a pause read all nine.
#[test]
fn sync_fan_out_mass_counter_placement_reads_every_seats_tokens() {
    let runner = sync_constructed_fan_out(put_counter_on_each_created());
    let state = runner.state();
    assert!(all_tokens(state, "Warrior")
        .iter()
        .all(|id| plus_one_counters(state, *id) == 1));
}

#[test]
fn sync_fan_out_inline_branch_reads_every_seats_tokens() {
    let starting = GameScenario::new_n_player(3, 42).build().state().players[0].life;
    let runner = sync_constructed_fan_out(roll_then_gain_per_created());
    assert_eq!(runner.state().players[0].life, starting + 9);
}

#[test]
fn sync_fan_out_emblem_static_reads_every_seats_tokens() {
    let mut runner = sync_constructed_fan_out(emblem_pumping_created());
    evaluate_layers(runner.state_mut());
    let state = runner.state();
    assert!(all_tokens(state, "Warrior")
        .iter()
        .all(|id| state.objects[id].power == Some(2)));
}

// ---------------------------------------------------------------------------
// Hidden population filters and static-definition conditions as readers of the
// created referent (CR 608.2e + CR 608.2h): the real cast of "Each player
// creates a 1/1 Fractal" followed by a typed tail, in a two-player game. Paused
// route: P0 controls Parallel Lives and P1 controls Halving Season, so at P0's
// creation both apply and P0 orders them (CR 616.1); P0 applies doubling first
// (1 -> 2 -> 1). P1's creation is untouched (1). The clause's union is both
// seats' Fractals. Synchronous route: no replacements, the same final
// population, no prompt.
// ---------------------------------------------------------------------------

/// An untapped 1/1 green creature token effect named `name` (and of that subtype).
fn untapped_token(name: &str) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), name.to_string()],
        colors: vec![ManaColor::Green],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// What a union cast observed: who was prompted, and at each prompt the
/// paused sidecar's created-token channel (`Some` exactly when the tail was
/// classified as a created-referent reader).
struct UnionCast {
    runner: GameRunner,
    prompted: Vec<PlayerId>,
    channels_at_prompts: Vec<Option<Vec<(PlayerId, usize)>>>,
    /// P0's follow-up "Tap all creatures you control" sorcery, still in hand.
    tap_follow_up: ObjectId,
}

/// P0 casts a spell whose typed definition is "Each player creates a 1/1
/// Fractal" followed by `tail`, then the cast is driven through each real
/// `ReplacementChoice` prompt with the public `GameAction::ChooseReplacement`
/// (doubling first) to Priority with an empty stack.
fn cast_union_fan_out(tail: AbilityDefinition, paused: bool) -> UnionCast {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if paused {
        scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
        scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    }
    let mut definition = AbilityDefinition::new(AbilityKind::Spell, untapped_token("Fractal"));
    definition.player_scope = Some(PlayerFilter::All);
    definition.sub_ability = Some(Box::new(tail));
    let spell = scenario
        .add_spell_to_hand(P0, "Union probe", false)
        .with_ability_definition(definition)
        .id();
    let tap_follow_up = scenario
        .add_spell_to_hand(P0, "Tap probe", false)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                scope: EffectScope::All,
                state: TapStateChange::Tap,
            },
        ))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();

    let mut channels_at_prompts = Vec::new();
    let mut at_prompt = |runner: &mut GameRunner, player: PlayerId| {
        let state = runner.state();
        let WaitingFor::ReplacementChoice { candidates, .. } = &state.waiting_for else {
            panic!("the prompt is a replacement choice");
        };
        // CR 616.1: the affected player orders the replacements; both apply.
        assert_eq!(player, P0, "P0 is the affected player");
        assert!(candidates
            .iter()
            .any(|candidate| candidate.description.contains("twice")));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.description.contains("half")));
        if channels_at_prompts.is_empty() {
            assert!(
                all_tokens(state, "Fractal").is_empty(),
                "reach guard: the first prompt is P0's creation, before any Fractal"
            );
        }
        let wire = serde_json::to_value(state).expect("paused state serializes");
        channels_at_prompts.push(serialized_created_channel(&wire));
        First::ParallelLives
    };
    let prompted = drive(&mut runner, &mut at_prompt, &no_optional);
    let state = runner.state();
    assert_eq!(
        tokens(state, P0, "Fractal").len(),
        1,
        "P0 ends with one Fractal"
    );
    assert_eq!(
        tokens(state, P1, "Fractal").len(),
        1,
        "P1 ends with one Fractal"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(
        state.active_ability_continuation().is_none(),
        "no stranded continuation"
    );
    if !paused {
        assert!(prompted.is_empty(), "the synchronous route never prompts");
    }
    UnionCast {
        runner,
        prompted,
        channels_at_prompts,
        tap_follow_up,
    }
}

/// Reach guard of a paused row whose tail reads the referent: P0 alone was
/// prompted once, and the paused sidecar opened the created-token channel
/// (empty: P0's creation is the one paused).
fn assert_paused_reader_reach(cast: &UnionCast) {
    assert_eq!(cast.prompted, vec![P0]);
    assert_eq!(cast.channels_at_prompts, vec![Some(vec![(P0, 0)])]);
}

fn tail_definition(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

fn suspect_all_created() -> AbilityDefinition {
    tail_definition(Effect::Suspect {
        target: TargetFilter::LastCreated,
        scope: EffectScope::All,
    })
}

fn suspected_fractals(state: &GameState) -> usize {
    all_tokens(state, "Fractal")
        .iter()
        .filter(|id| state.objects[*id].is_suspected)
        .count()
}

/// PR-1 — "Suspect those tokens" (`Suspect { All, LastCreated }`, CR 701.60a):
/// the hidden `EffectScope::All` population reads the clause's union.
#[test]
fn paused_cast_suspect_all_reads_both_seats_tokens() {
    let cast = cast_union_fan_out(suspect_all_created(), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(suspected_fractals(cast.runner.state()), 2);
}

#[test]
fn sync_cast_suspect_all_reads_both_seats_tokens() {
    let cast = cast_union_fan_out(suspect_all_created(), false);
    assert_eq!(suspected_fractals(cast.runner.state()), 2);
}

fn tap_all_created() -> AbilityDefinition {
    tail_definition(Effect::SetTapState {
        target: TargetFilter::LastCreated,
        scope: EffectScope::All,
        state: TapStateChange::Tap,
    })
}

fn tapped_fractals(state: &GameState) -> usize {
    all_tokens(state, "Fractal")
        .iter()
        .filter(|id| state.objects[*id].tapped)
        .count()
}

/// PR-3 — "Tap those tokens" (`SetTapState { All, LastCreated }`,
/// CR 701.26a).
#[test]
fn paused_cast_tap_all_reads_both_seats_tokens() {
    let cast = cast_union_fan_out(tap_all_created(), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(tapped_fractals(cast.runner.state()), 2);
}

#[test]
fn sync_cast_tap_all_reads_both_seats_tokens() {
    let cast = cast_union_fan_out(tap_all_created(), false);
    assert_eq!(tapped_fractals(cast.runner.state()), 2);
}

/// The number of objects in the created referent.
fn created_count() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::LastCreated,
        },
    }
}

fn created_count_is_prime() -> StaticCondition {
    StaticCondition::QuantityQuality {
        quantity: created_count(),
        quality: NumberQuality::Prime,
    }
}

/// A fixed "+1/+0 to creatures you control" grant, so only its condition or
/// duration can read the referent.
fn fixed_grant() -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
}

fn grant_tail(static_def: StaticDefinition, duration: Duration) -> AbilityDefinition {
    tail_definition(Effect::GenericEffect {
        static_abilities: vec![static_def],
        duration: Some(duration),
        target: None,
        end_cost: None,
    })
}

fn conditional_grant(condition: StaticCondition) -> AbilityDefinition {
    grant_tail(fixed_grant().condition(condition), Duration::UntilEndOfTurn)
}

/// After layers: P0's Fractal's power and how many fixed `+1/+0` grants exist.
fn p0_fractal_power_and_grants(cast: &mut UnionCast) -> (Option<i32>, usize) {
    evaluate_layers(cast.runner.state_mut());
    let state = cast.runner.state();
    let fractal = tokens(state, P0, "Fractal");
    let grants = state
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect
                .modifications
                .contains(&ContinuousModification::AddPower { value: 1 })
        })
        .count();
    (state.objects[&fractal[0]].power, grants)
}

/// PR-2 — a fixed grant gated by "the number of those tokens is prime": the
/// condition is evaluated once, when the effect is applied (CR 608.2h +
/// CR 611.2d), so it must see the union (2, prime) and install the grant.
#[test]
fn paused_cast_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(created_count_is_prime()), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

#[test]
fn sync_cast_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(created_count_is_prime()), false);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

fn not_prime() -> StaticCondition {
    StaticCondition::Not {
        condition: Box::new(created_count_is_prime()),
    }
}

/// PR-4 — hostile compound: `Not(Prime)` over the union (2) is false, so no
/// grant. PR-2 is its paired positive in the same harness.
#[test]
fn paused_cast_not_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(not_prime()), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(1), 0));
}

#[test]
fn sync_cast_not_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(not_prime()), false);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(1), 0));
}

fn during_your_turn_and_prime() -> StaticCondition {
    StaticCondition::And {
        conditions: vec![StaticCondition::DuringYourTurn, created_count_is_prime()],
    }
}

/// PR-5 — compound `And[DuringYourTurn, Prime]`: the referent operand is read.
#[test]
fn paused_cast_and_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(during_your_turn_and_prime()), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

#[test]
fn sync_cast_and_prime_static_condition_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(conditional_grant(during_your_turn_and_prime()), false);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

/// "Create a 1/1 Soldier. Suspect those tokens.": the tail's own creation is
/// the nearest publisher of the anaphor after it.
fn soldier_then_suspect_all() -> AbilityDefinition {
    let mut soldier = tail_definition(untapped_token("Soldier"));
    soldier.sub_ability = Some(Box::new(suspect_all_created()));
    soldier
}

fn assert_only_the_soldier_is_suspected(state: &GameState) {
    let soldiers = tokens(state, P0, "Soldier");
    assert_eq!(
        soldiers.len(),
        1,
        "reach guard: the tail created its Soldier"
    );
    assert!(state.objects[&soldiers[0]].is_suspected);
    assert_eq!(suspected_fractals(state), 0);
}

/// PR-6 — nearest publisher: the Soldier is suspected, no Fractal is. The
/// tail is not a reader of the clause's referent, so no channel opens; P0 is
/// prompted for the Fractal and again for the Soldier.
#[test]
fn paused_cast_suspect_all_after_own_token_reads_its_own_token() {
    let cast = cast_union_fan_out(soldier_then_suspect_all(), true);
    assert_eq!(cast.prompted, vec![P0, P0]);
    assert_eq!(cast.channels_at_prompts[0], None);
    assert_only_the_soldier_is_suspected(cast.runner.state());
}

#[test]
fn sync_cast_suspect_all_after_own_token_reads_its_own_token() {
    let cast = cast_union_fan_out(soldier_then_suspect_all(), false);
    assert_only_the_soldier_is_suspected(cast.runner.state());
}

/// "... for as long as there are two or more of those tokens": a fixed grant
/// whose duration starts only if its condition holds when the effect would
/// begin (CR 611.2b).
fn for_as_long_as_two_created() -> AbilityDefinition {
    grant_tail(
        fixed_grant(),
        Duration::ForAsLongAs {
            condition: StaticCondition::QuantityComparison {
                lhs: created_count(),
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            },
        },
    )
}

/// PR-7 — the union holds two tokens, 2 >= 2, so the duration starts and the
/// grant is installed (CR 611.2b).
#[test]
fn paused_cast_for_as_long_as_duration_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(for_as_long_as_two_created(), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

/// PR-7 sync twin and reach guard: the duration path installs the grant.
#[test]
fn sync_cast_for_as_long_as_duration_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(for_as_long_as_two_created(), false);
    assert_eq!(p0_fractal_power_and_grants(&mut cast), (Some(2), 1));
}

/// "When one of those tokens next becomes tapped this turn, you gain 1 life": a
/// delayed trigger whose embedded `WhenNextEvent` matcher names the created
/// referent. Creation snapshots only payload targets (CR 603.7c); the
/// `valid_card` matcher is matched against the live ledger when the tap event
/// fires (CR 603.2, `trigger_matchers::match_taps` → `matches_target_filter`),
/// so it must see the union.
fn when_next_created_token_taps_gain_life() -> AbilityDefinition {
    let mut taps = TriggerDefinition::new(TriggerMode::Taps);
    taps.valid_card = Some(TargetFilter::LastCreated);
    tail_definition(Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::WhenNextEvent {
            trigger: Box::new(taps),
            or_trigger: None,
            lifetime: DelayedTriggerLifetime::ThisTurn,
        },
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )),
        uses_tracked_set: false,
    })
}

/// After the union cast settles, P0 casts the harness's "Tap all creatures you
/// control" sorcery through the public cast, so P0's Fractal (one of "those
/// tokens") becomes tapped and P1's does not. Returns P0's life gain from that
/// point.
fn tap_p0_fractal_and_measure_life_gain(cast: &mut UnionCast) -> i32 {
    let state = cast.runner.state();
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "reach guard: the tail created its delayed trigger"
    );
    let life_before = state.players[0].life;
    let follow_up = cast.tap_follow_up;
    cast.runner.cast(follow_up).commit();
    let prompted = drive(&mut cast.runner, &mut no_replacement, &no_optional);
    assert!(prompted.is_empty());
    let state = cast.runner.state();
    let p0_fractal = tokens(state, P0, "Fractal");
    assert!(
        state.objects[&p0_fractal[0]].tapped,
        "reach guard: P0's Fractal became tapped"
    );
    let p1_fractal = tokens(state, P1, "Fractal");
    assert!(!state.objects[&p1_fractal[0]].tapped);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.active_ability_continuation().is_none());
    state.players[0].life - life_before
}

/// PR-8 — a later-evaluated matcher: P0's Fractal is one of "those tokens",
/// so its tapping fires the `WhenNextEvent` trigger on the paused route as on
/// the synchronous one.
#[test]
fn paused_cast_when_next_event_matcher_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(when_next_created_token_taps_gain_life(), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(tap_p0_fractal_and_measure_life_gain(&mut cast), 1);
}

#[test]
fn sync_cast_when_next_event_matcher_sees_both_seats_tokens() {
    let mut cast = cast_union_fan_out(when_next_created_token_taps_gain_life(), false);
    assert_eq!(tap_p0_fractal_and_measure_life_gain(&mut cast), 1);
}

// ---------------------------------------------------------------------------
// R1 — a tail whose only read of the created referent is nested behind a
// player-count or filter-threshold operand (CR 608.2c). The same two-player
// cast as above; every row also settles a follow-up cast, which resolves
// through the stack after the clause's tail has run.
// ---------------------------------------------------------------------------

/// P0 casts the harness's "Tap all creatures you control" follow-up through
/// the public cast and drives it to an empty stack: a later stack object
/// begins resolving after the player-scope clause and its tail settled.
fn settle_tap_follow_up(cast: &mut UnionCast) {
    let follow_up = cast.tap_follow_up;
    cast.runner.cast(follow_up).commit();
    let prompted = drive(&mut cast.runner, &mut no_replacement, &no_optional);
    assert!(prompted.is_empty());
    let state = cast.runner.state();
    assert!(
        tokens(state, P0, "Fractal")
            .iter()
            .all(|id| state.objects[id].tapped),
        "reach guard: the follow-up resolved"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.active_ability_continuation().is_none());
}

fn two_player_starting_life() -> i32 {
    GameScenario::new_n_player(2, 42).build().state().players[0].life
}

/// "You gain 1 life for each player who controls one or more of those
/// tokens": `PlayerCount(ControlsCount { All, LastCreated, GE, 1 })`.
fn gain_per_player_controlling_created() -> AbilityDefinition {
    tail_definition(Effect::GainLife {
        amount: QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::ControlsCount {
                    relation: PlayerRelation::All,
                    filter: TargetFilter::LastCreated,
                    comparator: Comparator::GE,
                    count: Box::new(QuantityExpr::Fixed { value: 1 }),
                },
            },
        },
        player: TargetFilter::Controller,
    })
}

/// "You gain 1 life for each creature with power less than the number of
/// `referent`": a `PtComparison` threshold counting `referent`'s population.
fn gain_per_creature_below_count_of(referent: TargetFilter) -> AbilityDefinition {
    tail_definition(Effect::GainLife {
        amount: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LT,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter: referent },
                        },
                    },
                ])),
            },
        },
        player: TargetFilter::Controller,
    })
}

/// R1-a — the player count reads both seats' Fractals: each seat controls
/// one, so P0 gains 2 on the paused route as on the synchronous one.
#[test]
fn paused_cast_player_count_over_created_tokens_counts_both_seats() {
    let mut cast = cast_union_fan_out(gain_per_player_controlling_created(), true);
    assert_paused_reader_reach(&cast);
    assert_eq!(
        cast.runner.state().players[0].life,
        two_player_starting_life() + 2
    );
    settle_tap_follow_up(&mut cast);
}

#[test]
fn sync_cast_player_count_over_created_tokens_counts_both_seats() {
    let mut cast = cast_union_fan_out(gain_per_player_controlling_created(), false);
    assert_eq!(
        cast.runner.state().players[0].life,
        two_player_starting_life() + 2
    );
    settle_tap_follow_up(&mut cast);
}

/// R1-b — the threshold is the number of "those tokens" (2), so both 1/1
/// Fractals have power below it and P0 gains 2 on both routes.
#[test]
fn paused_cast_threshold_over_created_tokens_counts_both_seats() {
    let mut cast = cast_union_fan_out(
        gain_per_creature_below_count_of(TargetFilter::LastCreated),
        true,
    );
    assert_paused_reader_reach(&cast);
    assert_eq!(
        cast.runner.state().players[0].life,
        two_player_starting_life() + 2
    );
    settle_tap_follow_up(&mut cast);
}

#[test]
fn sync_cast_threshold_over_created_tokens_counts_both_seats() {
    let mut cast = cast_union_fan_out(
        gain_per_creature_below_count_of(TargetFilter::LastCreated),
        false,
    );
    assert_eq!(
        cast.runner.state().players[0].life,
        two_player_starting_life() + 2
    );
    settle_tap_follow_up(&mut cast);
}

/// R1-b' — the same threshold over the moved set reads no created referent:
/// the paused route opens no created-token channel (P0's prompt is the reach).
#[test]
fn paused_cast_threshold_over_moved_set_opens_no_created_channel() {
    let mut cast = cast_union_fan_out(
        gain_per_creature_below_count_of(TargetFilter::LastZoneChanged),
        true,
    );
    assert_eq!(cast.prompted, vec![P0]);
    assert_eq!(cast.channels_at_prompts, vec![None]);
    settle_tap_follow_up(&mut cast);
}

// ---------------------------------------------------------------------------
// R2 — "Each opponent creates a 1/1 Fractal and puts a number of +1/+1
// counters on it equal to their life total" (a typed definition of the
// Oversimplify class), cast by P0 in a three-player game where P1 controls a
// noncreature host of Crafty Cutpurse's resolved redirect: "each token that
// would be created under an opponent's control this turn is created under your
// control instead". P2's Fractal is created under P1's control (CR 616.1b), yet
// it is still the token P2's instruction created, so it is P2's "it"
// (CR 608.2c + CR 608.2e). No row asserts owner. P1's life is 6 and P2's is 2.
// ---------------------------------------------------------------------------

/// Crafty Cutpurse's verbatim Oracle text (MTGJSON).
const CRAFTY_CUTPURSE: &str = "Flash\nWhen this creature enters, each token that would be created under an opponent's control this turn is created under your control instead.";

/// What a redirected fan-out observed at one replacement prompt.
struct RedirectPrompt {
    chooser: PlayerId,
    candidates: usize,
    fractals_on_battlefield: Vec<ObjectId>,
    channel: Option<Vec<(PlayerId, usize)>>,
}

struct RedirectCast {
    runner: GameRunner,
    prompts: Vec<RedirectPrompt>,
    /// At prompt #2 (P2's generated leg), when round-tripped: the decoded
    /// state's channel.
    decoded_channel: Option<Option<Vec<(PlayerId, usize)>>>,
    follow_up: ObjectId,
}

/// The co-scoped "Each opponent creates a 1/1 Fractal and puts N +1/+1
/// counters on it", where N is the acting opponent's life total.
fn each_opponent_creates_and_counters_it() -> AbilityDefinition {
    let mut placement = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer,
                },
            },
            target: TargetFilter::LastCreated,
        },
    );
    placement.player_scope = Some(PlayerFilter::Opponent);
    placement.sub_link = SubAbilityLink::ContinuationStep;
    let mut definition = AbilityDefinition::new(AbilityKind::Spell, untapped_token("Fractal"));
    definition.player_scope = Some(PlayerFilter::Opponent);
    definition.sub_ability = Some(Box::new(placement));
    definition
}

fn cast_redirect_fan_out(paused: bool, round_trip_at_generated_leg: bool) -> RedirectCast {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 6).with_life(P2, 2);
    let host = scenario
        .add_artifact_from_oracle(P1, "Redirect host", "")
        .id();
    if paused {
        scenario.add_enchantment_from_oracle(P0, "Halving Season", HALVING_SEASON);
        scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Redirect probe", false)
        .with_ability_definition(each_opponent_creates_and_counters_it())
        .id();
    let follow_up = scenario
        .add_spell_to_hand(P0, "Life probe", false)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ))
        .id();
    let mut runner = scenario.build();

    // Crafty Cutpurse's resolved trigger body, with P1's noncreature host as
    // its source, so the redirect survives the cast.
    let parsed = parse_oracle_text(
        CRAFTY_CUTPURSE,
        "Crafty Cutpurse",
        &["Flash".to_string()],
        &["Creature".to_string()],
        &["Human".to_string(), "Pirate".to_string()],
    );
    let body = parsed
        .triggers
        .first()
        .and_then(|trigger| trigger.execute.as_deref())
        .expect("Crafty Cutpurse parses its enters trigger");
    let redirect = build_resolved_from_def(body, host, P1);
    resolve_ability_chain(runner.state_mut(), &redirect, &mut Vec::new(), 0)
        .expect("the redirect installs");
    assert_eq!(
        runner.state().objects[&host].replacement_definitions.len(),
        1,
        "reach guard: the host carries the redirect"
    );

    runner.cast(spell).commit();
    let mut prompts = Vec::new();
    let mut decoded_channel = None;
    let mut at_prompt = |runner: &mut GameRunner, chooser: PlayerId| {
        let state = runner.state();
        let WaitingFor::ReplacementChoice { candidates, .. } = &state.waiting_for else {
            panic!("the prompt is a replacement choice");
        };
        let wire = serde_json::to_value(state).expect("paused state serializes");
        prompts.push(RedirectPrompt {
            chooser,
            candidates: candidates.len(),
            fractals_on_battlefield: all_tokens(state, "Fractal"),
            channel: serialized_created_channel(&wire),
        });
        if prompts.len() == 2 && round_trip_at_generated_leg {
            let decoded: GameState = serde_json::from_value(wire).expect("paused state restores");
            decoded_channel = Some(serialized_created_channel(
                &serde_json::to_value(&decoded).expect("decoded state serializes"),
            ));
            *runner.state_mut() = decoded;
        }
        // CR 616.1: doubling first.
        First::ParallelLives
    };
    drive(&mut runner, &mut at_prompt, &no_optional);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(
        state.active_ability_continuation().is_none(),
        "no stranded continuation"
    );
    RedirectCast {
        runner,
        prompts,
        decoded_channel,
        follow_up,
    }
}

/// Both Fractals, controlled by P1 (the redirect applied to P2's), in creation
/// order: P1's leg's Fractal, then P2's.
fn redirected_fractals(state: &GameState) -> Vec<ObjectId> {
    assert!(
        tokens(state, P0, "Fractal").is_empty() && tokens(state, P2, "Fractal").is_empty(),
        "reach guard: every Fractal is under P1's control"
    );
    let mut fractals = tokens(state, P1, "Fractal");
    fractals.sort_unstable_by_key(|id| id.0);
    fractals
}

/// The follow-up spell begins resolving on the stack after the clause and its
/// tail settled.
fn settle_life_follow_up(cast: &mut RedirectCast) {
    let life_before = cast.runner.state().players[0].life;
    let follow_up = cast.follow_up;
    cast.runner.cast(follow_up).commit();
    let prompted = drive(&mut cast.runner, &mut no_replacement, &no_optional);
    assert!(prompted.is_empty());
    let state = cast.runner.state();
    assert_eq!(
        state.players[0].life,
        life_before + 1,
        "reach guard: the follow-up resolved"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.active_ability_continuation().is_none());
}

/// R2-s — no prompt: P1's instruction puts 6 counters on the Fractal it
/// created, and P2's puts 2 on the Fractal it created, though P1 controls it.
#[test]
fn sync_redirected_fan_out_binds_each_seats_counters_to_its_own_fractal() {
    let mut cast = cast_redirect_fan_out(false, false);
    assert!(
        cast.prompts.is_empty(),
        "the synchronous route never prompts"
    );
    let state = cast.runner.state();
    let fractals = redirected_fractals(state);
    assert_eq!(fractals.len(), 2);
    assert_eq!(
        fractals
            .iter()
            .map(|id| plus_one_counters(state, *id))
            .collect::<Vec<_>>(),
        vec![6, 2],
        "each seat's life total lands on its own instruction's Fractal"
    );
    settle_life_follow_up(&mut cast);
}

/// R2-p — P1's creation pauses (Parallel Lives and Halving Season, CR 616.1),
/// then P2's generated leg pauses again with the redirect already applied
/// (CR 616.1b) and only doubling and halving offered. At that second prompt the
/// per-seat channel records P1's Fractal for P1 and P2 in progress; with
/// `round_trip`, the state round-trips through serde there and play continues
/// from the decoded state. Halving Season halves both placements (both seats
/// are P0's opponents): 6 -> 3 on P1's Fractal, 2 -> 1 on P2's.
fn paused_redirected_fan_out(round_trip: bool) {
    let mut cast = cast_redirect_fan_out(true, round_trip);
    assert_eq!(cast.prompts.len(), 2, "two replacement prompts");
    let first = &cast.prompts[0];
    assert_eq!(
        first.chooser, P1,
        "CR 616.1: P1 orders the replacements to its creation"
    );
    assert_eq!(first.candidates, 2);
    assert!(
        first.fractals_on_battlefield.is_empty(),
        "reach guard: the first prompt precedes any Fractal"
    );
    assert_eq!(first.channel, Some(vec![(P1, 0)]));
    let second = &cast.prompts[1];
    assert_eq!(
        second.candidates, 2,
        "only doubling and halving: the redirect needs no choice (CR 616.1b)"
    );
    assert_eq!(
        second.fractals_on_battlefield.len(),
        1,
        "reach guard: P1's Fractal exists, P2's does not yet"
    );
    let p1_leg_fractal = second.fractals_on_battlefield[0];
    assert_eq!(second.channel, Some(vec![(P1, 1), (P2, 0)]));
    if round_trip {
        assert_eq!(cast.decoded_channel, Some(Some(vec![(P1, 1), (P2, 0)])));
    }
    let state = cast.runner.state();
    let fractals = redirected_fractals(state);
    assert_eq!(fractals.len(), 2);
    assert_eq!(fractals[0], p1_leg_fractal);
    assert_eq!(
        plus_one_counters(state, fractals[0]),
        3,
        "P1's leg: 6, halved"
    );
    assert_eq!(
        plus_one_counters(state, fractals[1]),
        1,
        "P2's leg: 2, halved"
    );
    settle_life_follow_up(&mut cast);
}

#[test]
fn paused_redirected_fan_out_binds_each_seats_counters_to_its_own_fractal() {
    paused_redirected_fan_out(false);
}

#[test]
fn paused_redirected_fan_out_survives_save_and_load_at_the_generated_leg_prompt() {
    paused_redirected_fan_out(true);
}
