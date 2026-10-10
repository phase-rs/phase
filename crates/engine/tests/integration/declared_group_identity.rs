//! Each declared-player group keeps its own player through target announcement and through a
//! delayed trigger's install (CR 608.2c, CR 115.1a, CR 603.7a). Three seats: caster P0,
//! declared players P1 / P2.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChosenGroupId, Comparator,
    ControllerRef, DelayedTriggerCondition, Duration, Effect, EffectScope, FilterProp,
    QuantityExpr, QuantityRef, StaticDefinition, SubAbilityLink, TargetFilter, TargetRef,
    TypeFilter, TypedFilter, UnlessPayModifier,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, GameState, LayersDirty, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use serde_json::{json, Value};

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const G: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE);
const G2: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE + 1);
const GL: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE + 2);

fn declared(group: ChosenGroupId) -> ControllerRef {
    ControllerRef::DeclaredPlayer { group }
}

fn dp(group: ChosenGroupId) -> TargetFilter {
    TargetFilter::DeclaredPlayer { group }
}

fn creature_of(group: ChosenGroupId) -> TypedFilter {
    TypedFilter::new(TypeFilter::Creature).controller(declared(group))
}

fn def(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

fn pick_player() -> Effect {
    Effect::TargetOnly {
        target: TargetFilter::Player,
    }
}

fn declaring(effect: Effect, group: ChosenGroupId) -> AbilityDefinition {
    let mut definition = def(effect);
    definition.declares_chosen_group = Some(group);
    definition
}

fn then(head: AbilityDefinition, mut tail: AbilityDefinition) -> AbilityDefinition {
    tail.sub_link = SubAbilityLink::SequentialSibling;
    head.sub_ability(tail)
}

fn delayed(payload: AbilityDefinition, phase: Phase) -> AbilityDefinition {
    def(Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhase { phase },
        effect: Box::new(payload),
        uses_tracked_set: false,
    })
}

// ---------------------------------------------------------------------------------------------
// Announcement
// ---------------------------------------------------------------------------------------------

/// Declaring clauses `(group, player filter)` followed by a `PutCounter` reader of `filter`.
fn counter_chain(
    declares: &[(ChosenGroupId, TargetFilter)],
    filter: TargetFilter,
) -> AbilityDefinition {
    let reader = def(Effect::PutCounter {
        counter_type: CounterType::Plus1Plus1,
        count: QuantityExpr::Fixed { value: 1 },
        target: filter,
    });
    declares.iter().rev().fold(reader, |rest, (group, who)| {
        let mut rest = rest;
        rest.sub_link = SubAbilityLink::SequentialSibling;
        declaring(
            Effect::TargetOnly {
                target: who.clone(),
            },
            *group,
        )
        .sub_ability(rest)
    })
}

const FIVE: [(PlayerId, &str); 5] = [
    (P0, "P0-A"),
    (P1, "P1-A"),
    (P1, "P1-B"),
    (P2, "P2-A"),
    (P2, "P2-B"),
];

struct Announced {
    ids: Vec<(String, ObjectId)>,
    runner: GameRunner,
    spell: ObjectId,
}

impl Announced {
    fn new(
        declares: &[(ChosenGroupId, TargetFilter)],
        filter: TargetFilter,
        creatures: &[(PlayerId, &str)],
        owner_override: Option<(&str, PlayerId)>,
    ) -> Self {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let ids: Vec<(String, ObjectId)> = creatures
            .iter()
            .map(|(player, name)| {
                (
                    name.to_string(),
                    scenario.add_creature(*player, name, 2, 2).id(),
                )
            })
            .collect();
        let spell = scenario
            .add_spell_to_hand(P0, "Probe", false)
            .with_ability_definition(counter_chain(declares, filter))
            .id();
        let mut runner = scenario.build();
        if let Some((name, owner)) = owner_override {
            let id = ids.iter().find(|(n, _)| n == name).unwrap().1;
            runner.state_mut().objects.get_mut(&id).unwrap().owner = owner;
        }
        Self { ids, runner, spell }
    }

    fn cast(&mut self) -> Result<(), String> {
        let card_id = self.runner.state().objects[&self.spell].card_id;
        self.runner
            .act(GameAction::CastSpell {
                object_id: self.spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    fn object(&self, name: &str) -> TargetRef {
        TargetRef::Object(self.ids.iter().find(|(n, _)| n == name).unwrap().1)
    }

    fn names(&self, targets: &[TargetRef]) -> Vec<String> {
        targets
            .iter()
            .map(|target| match target {
                TargetRef::Object(id) => self.ids.iter().find(|(_, i)| i == id).unwrap().0.clone(),
                TargetRef::Player(p) => format!("Player{}", p.0),
            })
            .collect()
    }

    /// Choose each of `picks` as the declared players, then report the creature slot's offered
    /// and static sets.
    fn offered_at_creature_slot(&mut self, picks: &[PlayerId]) -> (Vec<String>, Vec<String>) {
        for pick in picks {
            self.runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(*pick)),
                })
                .expect("declared player pick");
        }
        let WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } = self.runner.state().waiting_for.clone()
        else {
            panic!(
                "expected the creature slot, got {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(selection.current_slot, picks.len(), "creature slot index");
        (
            self.names(&target_slots[selection.current_slot].legal_targets),
            self.names(&selection.current_legal_targets),
        )
    }

    fn choose(&mut self, name: &str) -> Result<(), String> {
        let target = self.object(name);
        self.runner
            .act(GameAction::ChooseTarget {
                target: Some(target),
            })
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    fn counters(&self) -> Vec<(String, u32)> {
        self.ids
            .iter()
            .map(|(name, id)| {
                (
                    name.clone(),
                    self.runner.state().objects[id]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                )
            })
            .collect()
    }

    fn resolve(&mut self) {
        for _ in 0..10 {
            if matches!(self.runner.state().waiting_for, WaitingFor::Priority { .. })
                && self.runner.state().stack.is_empty()
            {
                return;
            }
            self.runner.act(GameAction::PassPriority).expect("pass");
        }
    }
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn two_groups() -> [(ChosenGroupId, TargetFilter); 2] {
    [(G, TargetFilter::Player), (G2, TargetFilter::Player)]
}

fn assert_offered_then_accepts(
    mut a: Announced,
    picks: &[PlayerId],
    offered: &[&str],
    accept: &str,
    expect_counter: &str,
) {
    a.cast().expect("cast");
    let (_, selection) = a.offered_at_creature_slot(picks);
    assert_eq!(selection, strs(offered), "offered set");
    a.choose(accept).expect("the offered creature is accepted");
    a.resolve();
    assert_eq!(
        a.counters()
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        vec![expect_counter.to_string()],
        "only the accepted creature received the counter"
    );
}

/// A1: the reader of G offers G's player's creatures, not the latest player selected.
#[test]
fn a1_reader_of_the_first_group_offers_that_groups_player() {
    let a = Announced::new(
        &two_groups(),
        TargetFilter::Typed(creature_of(G)),
        &FIVE,
        None,
    );
    assert_offered_then_accepts(a, &[P1, P2], &["P1-A", "P1-B"], "P1-A", "P1-A");
}

/// A2: completion validation (`SelectTargets`) rejects the latest player's creature.
#[test]
fn a2_select_targets_validates_against_the_groups_player() {
    let select = |creature: &str| {
        let mut a = Announced::new(
            &two_groups(),
            TargetFilter::Typed(creature_of(G)),
            &FIVE,
            None,
        );
        a.cast().expect("cast");
        let targets = vec![
            TargetRef::Player(P1),
            TargetRef::Player(P2),
            a.object(creature),
        ];
        a.runner
            .act(GameAction::SelectTargets { targets })
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    };
    assert_eq!(select("P1-A"), Ok(()));
    assert!(select("P2-A").is_err(), "P2's creature is not group G's");
}

/// A3: `And[ctrl G, Not ctrl G2]` stays castable and offers G's player's creatures.
#[test]
fn a3_and_not_of_two_groups_is_castable() {
    let filter = TargetFilter::And {
        filters: vec![
            TargetFilter::Typed(creature_of(G)),
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Typed(creature_of(G2))),
            },
        ],
    };
    let a = Announced::new(&two_groups(), filter, &FIVE, None);
    assert_offered_then_accepts(a, &[P1, P2], &["P1-A", "P1-B"], "P1-A", "P1-A");
}

/// A4: `Or[ctrl G, ctrl G2]` offers both groups' creatures.
#[test]
fn a4_or_of_two_groups_offers_both() {
    let filter = TargetFilter::Or {
        filters: vec![
            TargetFilter::Typed(creature_of(G)),
            TargetFilter::Typed(creature_of(G2)),
        ],
    };
    let a = Announced::new(&two_groups(), filter, &FIVE, None);
    assert_offered_then_accepts(
        a,
        &[P1, P2],
        &["P1-A", "P1-B", "P2-A", "P2-B"],
        "P2-A",
        "P2-A",
    );
}

/// A5: controller G plus `Owned{G2}` reads both groups from the same filter.
#[test]
fn a5_controller_group_and_owner_group_are_independent() {
    let filter = TargetFilter::Typed(creature_of(G).properties(vec![FilterProp::Owned {
        controller: declared(G2),
    }]));
    let a = Announced::new(&two_groups(), filter, &FIVE, Some(("P1-B", P2)));
    assert_offered_then_accepts(a, &[P1, P2], &["P1-B"], "P1-B", "P1-B");
}

/// A6: with the picks swapped, G is P2.
#[test]
fn a6_swapped_picks_swap_the_offered_creatures() {
    let a = Announced::new(
        &two_groups(),
        TargetFilter::Typed(creature_of(G)),
        &FIVE,
        None,
    );
    assert_offered_then_accepts(a, &[P2, P1], &["P2-A", "P2-B"], "P2-A", "P2-A");
}

/// A7: G (any player), G2 (opponent only), reader of G: the static slot build must not
/// read G as the opponent-only group.
#[test]
fn a7_static_build_keeps_each_groups_candidates() {
    let opponent = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    let declares = [(G, TargetFilter::Player), (G2, opponent)];
    let filter = TargetFilter::Typed(creature_of(G));

    let mut only_p0 = Announced::new(&declares, filter.clone(), &[(P0, "P0-A")], None);
    only_p0
        .cast()
        .expect("castable when only the caster has a creature");
    let (_, offered) = only_p0.offered_at_creature_slot(&[P0, P1]);
    assert_eq!(offered, strs(&["P0-A"]));

    let mut all_seats = Announced::new(
        &declares,
        filter,
        &[(P0, "P0-A"), (P1, "P1-A"), (P2, "P2-A")],
        None,
    );
    all_seats.cast().expect("cast");
    let (_, offered) = all_seats.offered_at_creature_slot(&[P0, P1]);
    assert_eq!(offered, strs(&["P0-A"]));
    all_seats
        .choose("P0-A")
        .expect("G's player's creature is accepted");
}

/// A8: control. A single group keeps its one player (green with and without the fix).
#[test]
fn a8_single_group_control() {
    let opponent = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    let mut a = Announced::new(
        &[(G, opponent)],
        TargetFilter::Typed(creature_of(G)),
        &[(P0, "P0-A"), (P1, "P1-A"), (P1, "P1-B"), (P2, "P2-A")],
        None,
    );
    a.cast().expect("cast");
    let (_, offered) = a.offered_at_creature_slot(&[P1]);
    assert_eq!(offered, strs(&["P1-A", "P1-B"]));
}

// ---------------------------------------------------------------------------------------------
// Delayed install
// ---------------------------------------------------------------------------------------------

fn q(value: i32) -> Value {
    json!({"type":"Fixed","value":value})
}

fn effect_json(value: Value) -> Effect {
    serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("effect json {value}: {e}"))
}

fn kind(name: &str, target: &TargetFilter) -> Effect {
    let t = serde_json::to_value(target).unwrap();
    match name {
        "Draw" => effect_json(json!({"type":"Draw","count":q(1),"target":t})),
        "LoseLife" => effect_json(json!({"type":"LoseLife","amount":q(3),"target":t})),
        "DealDamage" => effect_json(json!({"type":"DealDamage","amount":q(3),"target":t})),
        "Mill" => {
            effect_json(json!({"type":"Mill","count":q(2),"target":t,"destination":"Graveyard"}))
        }
        "Discard" => effect_json(json!({"type":"Discard","count":q(1),"target":t})),
        "Scry" => effect_json(json!({"type":"Scry","count":q(1),"target":t})),
        "Surveil" => effect_json(json!({"type":"Surveil","count":q(1),"target":t})),
        "ExileTop" => effect_json(json!({"type":"ExileTop","player":t,"count":q(2)})),
        "RevealTop" => effect_json(json!({"type":"RevealTop","player":t,"count":2})),
        "Dig" => effect_json(
            json!({"type":"Dig","player":t,"count":q(2),"destination":"Hand","keep_count":1}),
        ),
        "GainLife" => effect_json(json!({"type":"GainLife","amount":q(3),"player":t})),
        "Shuffle" => effect_json(json!({"type":"Shuffle","target":t})),
        "Sacrifice" => effect_json(json!({"type":"Sacrifice","target":t,"count":q(1)})),
        "DestroyAll" => effect_json(json!({"type":"DestroyAll","target":t})),
        "PhaseOut" => effect_json(json!({"type":"PhaseOut","target":t})),
        "GenericEffect" => Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::Hexproof)],
            duration: Some(engine::types::ability::Duration::UntilEndOfTurn),
            target: Some(target.clone()),
            end_cost: None,
        },
        other => panic!("unknown effect kind {other}"),
    }
}

fn lose(target: TargetFilter) -> Effect {
    kind("LoseLife", &target)
}

fn head() -> AbilityDefinition {
    declaring(pick_player(), G)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum After {
    Nothing,
    Hexproof,
    Eliminate,
    ExtraCreature,
    /// P1's Bear dies.
    BearDies,
    BearDiesThenEliminate,
    /// P1's Bear gets a +1/+1 counter and deals 2 damage to P0.
    Records,
    RecordsThenEliminate,
}

#[derive(Clone, Copy)]
enum Pick {
    Player(PlayerId),
    Bear(usize),
}

#[derive(Debug)]
struct Out {
    life: Vec<i32>,
    hand: Vec<usize>,
    prompts: usize,
    bears: Vec<bool>,
    last_wait: String,
    fingerprint: String,
    asked: Vec<String>,
    affected: Vec<String>,
    shuffled: Vec<u8>,
    /// `(owner, controller)` of every token on the battlefield.
    tokens: Vec<(u8, u8)>,
    /// `StackPushed` events over the whole run.
    pushed: usize,
    /// Delayed triggers still installed at the end of the run.
    delayed_left: usize,
}

fn grant_hexproof(state: &mut GameState, player: PlayerId) {
    let grantor = engine::game::zones::create_object(
        state,
        CardId(951),
        player,
        "Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&grantor).unwrap().static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    state.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(state);
    assert!(engine::game::static_abilities::player_has_hexproof(
        state, player
    ));
}

fn seed_records(state: &mut GameState) {
    use engine::types::card_type::CoreType;
    let bear = state
        .objects
        .values()
        .find(|o| o.name == "Bear" && o.controller == P1)
        .expect("P1's Bear")
        .id;
    state
        .counter_added_this_turn
        .push(engine::types::game_state::CounterAddedRecord {
            actor: P1,
            object_id: bear,
            counter_type: CounterType::Plus1Plus1,
            count: 1,
            name: "Bear".into(),
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            power: Some(2),
            toughness: Some(2),
            colors: vec![],
            mana_value: 0,
            controller: P1,
            owner: P1,
            counters: Default::default(),
        });
    state
        .damage_dealt_this_turn
        .push_back(engine::types::game_state::DamageRecord {
            source_id: bear,
            source_controller: P1,
            source_controller_snapshot: P1,
            source_owner: P1,
            target: TargetRef::Player(P0),
            target_controller: P0,
            amount: 2,
            source_name: "Bear".into(),
            source_core_types: vec![CoreType::Creature],
            ..Default::default()
        });
}

fn apply(state: &mut GameState, change: After) {
    match change {
        After::Nothing => {}
        After::Hexproof => grant_hexproof(state, P1),
        After::Eliminate => {
            engine::game::elimination::eliminate_player(state, P1, &mut Vec::new());
            assert!(!engine::game::players::is_alive(state, P1));
        }
        After::Records => seed_records(state),
        After::RecordsThenEliminate => {
            seed_records(state);
            apply(state, After::Eliminate);
        }
        After::BearDiesThenEliminate => {
            apply(state, After::BearDies);
            apply(state, After::Eliminate);
        }
        After::BearDies => {
            let bear = state
                .objects
                .values()
                .find(|o| o.name == "Bear" && o.controller == P1)
                .expect("P1's Bear")
                .id;
            engine::game::zones::move_to_zone(state, bear, Zone::Graveyard, &mut Vec::new());
        }
        After::ExtraCreature => {
            let id = engine::game::zones::create_object(
                state,
                CardId(952),
                P1,
                "Extra Bear".to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types =
                vec![engine::types::card_type::CoreType::Creature];
        }
    }
}

/// Cast `root`, choosing `picks` in order, apply `before_resolve` while the spell is on the
/// stack and `after_install` once it has resolved, then advance through `phases`.
fn run(
    root: AbilityDefinition,
    picks: &[Pick],
    phases: &[Phase],
    before_resolve: After,
    after_install: After,
) -> Out {
    run_deciding(root, picks, phases, before_resolve, after_install, None)
}

/// [`run`], answering every optional-effect and unless-payment prompt with `decide` (accept /
/// pay) and recording who was asked.
fn run_deciding(
    root: AbilityDefinition,
    picks: &[Pick],
    phases: &[Phase],
    before_resolve: After,
    after_install: After,
    decide: Option<bool>,
) -> Out {
    run_on(
        Board::Plain,
        root,
        picks,
        phases,
        Changes {
            before_resolve,
            after_install,
            on_stack: After::Nothing,
            on_reflexive: After::Nothing,
        },
        decide,
    )
}

/// [`run`], applying `on_stack` once the delayed ability has triggered and sits on the stack.
fn run_late(
    root: AbilityDefinition,
    picks: &[Pick],
    phases: &[Phase],
    before_resolve: After,
    after_install: After,
    on_stack: After,
) -> Out {
    run_on(
        Board::Plain,
        root,
        picks,
        phases,
        Changes {
            before_resolve,
            after_install,
            on_stack,
            on_reflexive: After::Nothing,
        },
        None,
    )
}

fn stack_pushes(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
        .count()
}

/// The creatures on the table: one Bear each for P1 and P2 (`Out.bears[0..2]`), and with
/// `CasterBear` a third for the caster (`Out.bears[2]`).
#[derive(Clone, Copy)]
enum Board {
    Plain,
    CasterBear,
}

/// When each [`After`] change lands: while the spell is on the stack, once the delayed ability is
/// installed, once it has triggered and sits on the stack, and once a reflexive trigger it created
/// sits on the stack.
struct Changes {
    before_resolve: After,
    after_install: After,
    on_stack: After,
    on_reflexive: After,
}

fn run_on(
    board: Board,
    root: AbilityDefinition,
    picks: &[Pick],
    phases: &[Phase],
    changes: Changes,
    decide: Option<bool>,
) -> Out {
    let Changes {
        before_resolve,
        after_install,
        on_stack,
        on_reflexive,
    } = changes;
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let mut bear_owners = vec![P1, P2];
    if matches!(board, Board::CasterBear) {
        bear_owners.push(P0);
    }
    let bears: Vec<ObjectId> = bear_owners
        .iter()
        .map(|p| {
            let mut bear = scenario.add_creature(*p, "Bear", 2, 2);
            if *p == P0 {
                // A caster creature must not stop the run at the declare-attackers prompt.
                bear.defender();
            }
            bear.id()
        })
        .collect();
    for player in [P0, P1, P2] {
        for i in 0..8 {
            scenario.add_card_to_library_top(player, &format!("Library {}-{i}", player.0));
        }
    }
    for player in [P1, P2] {
        scenario.add_card_to_hand(player, "Hand A");
        scenario.add_card_to_hand(player, "Hand B");
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Probe", false)
        .with_ability_definition(root)
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

    let mut chosen = 0;
    let mut prompts = 0;
    let mut applied_before = false;
    let mut applied_on_stack = false;
    let mut applied_on_reflexive = false;
    let mut pushes_at_install: Option<usize> = None;
    let mut last_wait = String::new();
    let mut asked: Vec<String> = Vec::new();
    let mut events: Vec<GameEvent> = Vec::new();
    let mut drive = |runner: &mut GameRunner, events: &mut Vec<GameEvent>| {
        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                    let target = picks.get(chosen).map(|pick| match pick {
                        Pick::Player(p) => TargetRef::Player(*p),
                        Pick::Bear(i) => TargetRef::Object(bears[*i]),
                    });
                    chosen += 1;
                    prompts += 1;
                    runner
                        .act(GameAction::ChooseTarget { target })
                        .expect("target pick");
                }
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if !applied_before {
                        applied_before = true;
                        apply(runner.state_mut(), before_resolve);
                    } else if !applied_on_stack && runner.state().phase != Phase::PreCombatMain {
                        applied_on_stack = true;
                        pushes_at_install = Some(stack_pushes(events));
                        apply(runner.state_mut(), on_stack);
                    } else if !applied_on_reflexive
                        && pushes_at_install.is_some_and(|n| stack_pushes(events) > n)
                    {
                        applied_on_reflexive = true;
                        apply(runner.state_mut(), on_reflexive);
                    }
                    events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
                }
                WaitingFor::OptionalEffectChoice { player, .. } if decide.is_some() => {
                    asked.push(format!("optional:{}", player.0));
                    runner
                        .act(GameAction::DecideOptionalEffect {
                            accept: decide == Some(true),
                        })
                        .expect("decide optional");
                }
                WaitingFor::UnlessPayment { player, .. } if decide.is_some() => {
                    asked.push(format!("unless:{}", player.0));
                    let pay = decide == Some(true);
                    if pay {
                        let _ = runner.state_mut().add_mana_to_pool(
                            player,
                            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
                        );
                    }
                    runner
                        .act(GameAction::PayUnlessCost { pay })
                        .expect("decide unless");
                }
                other => {
                    last_wait = format!("{other:?}").chars().take(140).collect();
                    break;
                }
            }
        }
    };
    drive(&mut runner, &mut events);
    apply(runner.state_mut(), after_install);
    for phase in phases {
        advance_collecting(&mut runner, *phase, &mut events);
        drive(&mut runner, &mut events);
    }
    assert!(
        applied_on_stack || matches!(on_stack, After::Nothing),
        "the delayed ability never reached the stack"
    );
    assert!(
        applied_on_reflexive || matches!(on_reflexive, After::Nothing),
        "no reflexive trigger reached the stack"
    );
    let state = runner.state();
    Out {
        life: state.players.iter().map(|p| p.life).collect(),
        hand: state.players.iter().map(|p| p.hand.len()).collect(),
        prompts,
        bears: bears
            .iter()
            .map(|b| {
                state
                    .objects
                    .get(b)
                    .is_some_and(|o| o.zone == Zone::Battlefield)
            })
            .collect(),
        last_wait,
        asked,
        shuffled: events
            .iter()
            .filter_map(|event| match event {
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } => Some(player_id.0),
                _ => None,
            })
            .collect(),
        pushed: events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
            .count(),
        delayed_left: state.delayed_triggers.len(),
        tokens: state
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Battlefield)
            .map(|o| (o.owner.0, o.controller.0))
            .collect(),
        affected: state
            .transient_continuous_effects
            .iter()
            .map(|t| format!("{:?}", t.affected))
            .collect(),
        fingerprint: format!(
            "{}|{:?}|{:?}",
            serde_json::to_string(&state.players).unwrap(),
            {
                let mut revealed: Vec<_> = state.revealed_cards.iter().copied().collect();
                revealed.sort();
                revealed
            },
            state
                .transient_continuous_effects
                .iter()
                .map(|t| format!("{:?}", t.affected))
                .collect::<Vec<_>>()
        ),
    }
}

/// `GameRunner::advance_to_phase`, keeping the events it would drop.
fn advance_collecting(runner: &mut GameRunner, phase: Phase, events: &mut Vec<GameEvent>) {
    let state = runner.state_mut();
    let mut waiting = engine::game::turns::auto_advance(state, events);
    for _ in 0..32 {
        if runner.state().phase == phase || !matches!(waiting, WaitingFor::Priority { .. }) {
            break;
        }
        for _ in 0..2 {
            let Ok(result) = runner.act(GameAction::PassPriority) else {
                return;
            };
            events.extend(result.events);
            waiting = result.waiting_for;
        }
    }
}

fn payload_run(payload: AbilityDefinition, before: After, after: After) -> Out {
    run(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End],
        before,
        after,
    )
}

fn same_chain_run(effect: Effect) -> Out {
    run(
        then(head(), def(effect)),
        &[Pick::Player(P1)],
        &[],
        After::Nothing,
        After::Nothing,
    )
}

fn assert_out(out: &Out, life: &[i32], hand: &[usize], bears: &[bool], prompts: usize) {
    assert_eq!(
        (&out.life[..], &out.hand[..], &out.bears[..], out.prompts),
        (life, hand, bears, prompts)
    );
}

const PLAYER_KINDS: [&str; 13] = [
    "Draw",
    "LoseLife",
    "GainLife",
    "DealDamage",
    "Mill",
    "Discard",
    "Scry",
    "Surveil",
    "ExileTop",
    "RevealTop",
    "Dig",
    "PhaseOut",
    "GenericEffect",
];

/// Nothing happens: the baseline every effect kind must visibly differ from.
fn noop_payload() -> AbilityDefinition {
    def(Effect::TargetOnly {
        target: TargetFilter::Controller,
    })
}

fn differs(a: &Out, b: &Out) -> bool {
    a.fingerprint != b.fingerprint || a.last_wait != b.last_wait
}

/// D1: a payload that reads G behaves as the same clause in the creating chain.
#[test]
fn d1_delayed_payload_matches_same_chain_for_every_player_effect() {
    let noop = payload_run(noop_payload(), After::Nothing, After::Nothing);
    for name in PLAYER_KINDS {
        let reference = same_chain_run(kind(name, &dp(G)));
        let delayed = payload_run(def(kind(name, &dp(G))), After::Nothing, After::Nothing);
        assert!(
            differs(&reference, &noop),
            "{name}: the same-chain clause affects P1"
        );
        assert_eq!(
            (&reference.fingerprint, &reference.last_wait),
            (&delayed.fingerprint, &delayed.last_wait),
            "{name}: delayed vs same-chain"
        );
    }
}

/// D1 hexproof leg (CR 115.10a, CR 702.11c): hexproof granted after install does not stop a
/// player who is affected, not targeted.
#[test]
fn d1_hexproof_granted_after_install_still_affects_the_player() {
    for name in PLAYER_KINDS {
        let reference = same_chain_run(kind(name, &dp(G)));
        let hexproof = payload_run(def(kind(name, &dp(G))), After::Nothing, After::Hexproof);
        assert_eq!(
            (&hexproof.fingerprint, &hexproof.last_wait),
            (&reference.fingerprint, &reference.last_wait),
            "{name}: hexproof after install"
        );
    }
}

/// D1 gone leg (CR 800.4a): a player eliminated after install is no one at fire, so the payload
/// does what an empty payload does.
#[test]
fn d1_player_eliminated_after_install_is_no_one() {
    for name in PLAYER_KINDS {
        let alive = payload_run(def(kind(name, &dp(G))), After::Nothing, After::Nothing);
        let gone = payload_run(def(kind(name, &dp(G))), After::Nothing, After::Eliminate);
        let empty = payload_run(noop_payload(), After::Nothing, After::Eliminate);
        let untouched = payload_run(noop_payload(), After::Nothing, After::Nothing);
        assert!(
            differs(&alive, &untouched),
            "{name}: reach guard, alive P1 is affected"
        );
        assert_eq!(
            (&gone.fingerprint, &gone.last_wait),
            (&empty.fingerprint, &empty.last_wait),
            "{name}: gone vs empty payload"
        );
    }
}

/// D1 object leg: a creature filter controlled by G reaches only G's player's objects.
#[test]
fn d1_object_effects_reach_only_the_groups_players_objects() {
    for name in ["Sacrifice", "DestroyAll"] {
        let filter = TargetFilter::Typed(creature_of(G));
        let reference = same_chain_run(kind(name, &filter));
        let delayed = payload_run(def(kind(name, &filter)), After::Nothing, After::Nothing);
        assert_eq!(reference.bears, vec![false, true], "{name}: same chain");
        assert_eq!(
            delayed.bears, reference.bears,
            "{name}: delayed vs same-chain"
        );
        assert_eq!(
            delayed.fingerprint, reference.fingerprint,
            "{name}: fingerprint"
        );
    }
}

/// D1c: a quantity over G's player's creatures counts that player's creatures.
#[test]
fn d1c_quantity_filter_reads_the_groups_player() {
    let draw = Effect::Draw {
        count: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(creature_of(G)),
            },
        },
        target: TargetFilter::Controller,
    };
    assert_out(
        &payload_run(def(draw), After::Nothing, After::Nothing),
        &[20, 20, 20],
        &[1, 2, 2],
        &[true, true],
        1,
    );
}

/// D1b: a mass-population target controlled by G destroys only G's player's creatures.
#[test]
fn d1b_mass_population_target_reads_the_groups_player() {
    let destroy = kind("DestroyAll", &TargetFilter::Typed(creature_of(G)));
    assert_out(
        &payload_run(def(destroy), After::Nothing, After::Nothing),
        &[20, 20, 20],
        &[0, 2, 2],
        &[false, true],
        1,
    );
}

fn mixed() -> AbilityDefinition {
    then(def(lose(TargetFilter::Controller)), def(lose(dp(G))))
}

/// M1: `[LoseLife(Controller), LoseLife(G)]`, alive reach guard.
#[test]
fn m1_mixed_payload_alive() {
    assert_out(
        &payload_run(mixed(), After::Nothing, After::Nothing),
        &[17, 17, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

/// M2 (USER-mandated): hexproof granted after install leaves P1 affected.
#[test]
fn m2_mixed_payload_hexproof_after_install() {
    assert_out(
        &payload_run(mixed(), After::Nothing, After::Hexproof),
        &[17, 17, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

/// M3: real elimination after install, P1 is untouched and P0 still loses.
#[test]
fn m3_mixed_payload_player_eliminated_after_install() {
    assert_out(
        &payload_run(mixed(), After::Nothing, After::Eliminate),
        &[17, 20, 20],
        &[0, 0, 2],
        &[false, true],
        1,
    );
}

/// Same chain as the creating spell: a second target, so the install is not the only prompt.
fn with_bear(tail: AbilityDefinition) -> AbilityDefinition {
    let bear = def(Effect::TargetOnly {
        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
    });
    then(declaring(pick_player(), G), then(bear, tail))
}

fn with_bear_run(before: After) -> Out {
    run(
        with_bear(delayed(mixed(), Phase::End)),
        &[Pick::Player(P1), Pick::Bear(1)],
        &[Phase::End],
        before,
        After::Nothing,
    )
}

/// M4: hexproof before resolve makes the declared player illegal at install, so it names no one.
#[test]
fn m4_hexproof_before_resolve_is_illegal_at_install() {
    assert_out(
        &with_bear_run(After::Hexproof),
        &[17, 20, 20],
        &[0, 2, 2],
        &[true, true],
        2,
    );
}

/// M5: a player who left before resolve names no one.
#[test]
fn m5_player_left_before_resolve_is_no_one() {
    assert_out(
        &with_bear_run(After::Eliminate),
        &[17, 20, 20],
        &[0, 0, 2],
        &[false, true],
        2,
    );
}

/// M6: alive two-target control.
#[test]
fn m6_two_target_control() {
    assert_out(
        &with_bear_run(After::Nothing),
        &[17, 17, 20],
        &[0, 2, 2],
        &[true, true],
        2,
    );
}

/// D2: distinct groups G and G2 each keep their own player.
#[test]
fn d2_distinct_groups_keep_their_players() {
    let payload = then(def(lose(dp(G))), def(lose(dp(G2))));
    let root = then(
        declaring(pick_player(), G),
        then(declaring(pick_player(), G2), delayed(payload, Phase::End)),
    );
    let out = run(
        root,
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End],
        After::Nothing,
        After::Nothing,
    );
    assert_out(&out, &[20, 17, 17], &[0, 2, 2], &[true, true], 2);
}

/// R6: a payload-local declaration stays local while the outer group binds; the outer group
/// adds no prompt.
#[test]
fn r6_payload_local_group_stays_local_and_outer_group_binds() {
    let payload = then(
        declaring(pick_player(), GL),
        then(def(lose(dp(GL))), def(lose(dp(G)))),
    );
    let out = run(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End],
        After::Nothing,
        After::Nothing,
    );
    assert_out(&out, &[20, 17, 17], &[0, 2, 2], &[true, true], 2);

    let outer_only = payload_run(def(lose(dp(G))), After::Nothing, After::Nothing);
    assert_eq!(outer_only.prompts, 1, "the outer group alone prompts once");
}

/// D5: a nested payload whose root reads G.
#[test]
fn d5_nested_payload_root_reads_the_outer_group() {
    let inner = delayed(def(lose(dp(G))), Phase::Upkeep);
    let out = run(
        then(head(), delayed(inner, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End, Phase::Upkeep],
        After::Nothing,
        After::Nothing,
    );
    assert_out(&out, &[20, 17, 20], &[0, 2, 2], &[true, true], 1);
}

/// D5b: a nested payload whose sub-ability, not its root, reads G.
#[test]
fn d5b_nested_payload_sub_ability_reads_the_outer_group() {
    let inner = delayed(mixed(), Phase::Upkeep);
    let out = run(
        then(head(), delayed(inner, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End, Phase::Upkeep],
        After::Nothing,
        After::Nothing,
    );
    assert_out(&out, &[17, 17, 20], &[0, 2, 2], &[true, true], 1);
}

fn quantity_check(lhs: i32) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Fixed { value: lhs },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }
}

fn with_else(condition: AbilityCondition) -> AbilityDefinition {
    let mut root = def(lose(TargetFilter::Controller));
    root.condition = Some(condition);
    root.else_ability = Some(Box::new(def(lose(dp(G)))));
    root
}

/// E1: the else branch of a payload reads G.
#[test]
fn e1_else_branch_reads_the_outer_group() {
    assert_out(
        &payload_run(with_else(quantity_check(0)), After::Nothing, After::Nothing),
        &[20, 17, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

/// E2: control, the true condition takes the main branch.
#[test]
fn e2_true_condition_takes_the_main_branch() {
    assert_out(
        &payload_run(with_else(quantity_check(1)), After::Nothing, After::Nothing),
        &[17, 20, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

// ---------------------------------------------------------------------------------------------
// Definition-level readers of a payload
// ---------------------------------------------------------------------------------------------

fn deciding_run(
    payload: AbilityDefinition,
    before: After,
    after_install: After,
    decide: bool,
) -> Out {
    run_deciding(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End],
        before,
        after_install,
        Some(decide),
    )
}

fn draw_for(group: ChosenGroupId) -> AbilityDefinition {
    def(kind("Draw", &dp(group)))
}

fn may_draw() -> AbilityDefinition {
    let mut payload = draw_for(G);
    payload.optional = true;
    payload.optional_player = Some(dp(G));
    payload
}

fn unless_lose() -> AbilityDefinition {
    let mut payload = def(lose(dp(G)));
    payload.unless_pay = Some(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(1),
        },
        payer: dp(G),
    });
    payload
}

/// I1: `optional_player` naming G offers the "may" to G's player (CR 608.2d, CR 603.7a).
#[test]
fn i1_optional_player_offers_the_groups_player_the_may() {
    let accepted = deciding_run(may_draw(), After::Nothing, After::Nothing, true);
    assert_eq!(accepted.asked, ["optional:1"]);
    assert_eq!(accepted.hand, [0, 3, 2]);
    let declined = deciding_run(may_draw(), After::Nothing, After::Nothing, false);
    assert_eq!(declined.asked, ["optional:1"]);
    assert_eq!(declined.hand, [0, 2, 2]);
}

/// I2: `unless_pay.payer` naming G offers the payment to G's player (CR 118.12a, CR 603.7a).
#[test]
fn i2_unless_payer_offers_the_payment_to_the_groups_player() {
    let declined = deciding_run(unless_lose(), After::Nothing, After::Nothing, false);
    assert_eq!(declined.asked, ["unless:1"]);
    assert_eq!(declined.life, [20, 17, 20]);
    let paid = deciding_run(unless_lose(), After::Nothing, After::Nothing, true);
    assert_eq!(paid.asked, ["unless:1"]);
    assert_eq!(paid.life, [20, 20, 20]);
}

/// I3: a declared player who left before the chain resolved is asked nothing (CR 608.2b); the
/// available-player control is I1 / I2.
#[test]
fn i3_unavailable_group_player_is_asked_nothing_and_nothing_happens() {
    for payload in [may_draw(), unless_lose()] {
        let out = deciding_run(payload, After::Eliminate, After::Nothing, true);
        assert!(out.asked.is_empty(), "{:?}", out.asked);
        assert_eq!((out.life[0], out.life[2]), (20, 20));
        assert_eq!((out.hand[0], out.hand[2]), (0, 2));
    }
}

/// I3b: a declared player who left after the delayed payload was installed is likewise asked
/// nothing (CR 608.2b, CR 603.7a), not replaced by the controller.
#[test]
fn i3b_group_player_gone_after_install_is_asked_nothing() {
    for payload in [may_draw(), unless_lose()] {
        let out = deciding_run(payload, After::Nothing, After::Eliminate, true);
        assert!(out.asked.is_empty(), "{:?}", out.asked);
        assert_eq!((out.life[0], out.life[2]), (20, 20));
        assert_eq!((out.hand[0], out.hand[2]), (0, 2));
    }
}

fn bears_of_g() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(creature_of(G)),
        },
    }
}

/// I4a: a payload `condition` reads G's player's board.
#[test]
fn i4a_condition_reads_the_groups_player() {
    let mut gated = def(lose(TargetFilter::Controller));
    gated.condition = Some(AbilityCondition::QuantityCheck {
        lhs: bears_of_g(),
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    });
    assert_out(
        &payload_run(gated, After::Nothing, After::Nothing),
        &[17, 20, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

/// I4b: a payload `repeat_for` reads G's player's board.
#[test]
fn i4b_repeat_for_reads_the_groups_player() {
    let mut repeated = def(lose(TargetFilter::Controller));
    repeated.repeat_for = Some(bears_of_g());
    assert_out(
        &payload_run(repeated, After::Nothing, After::Nothing),
        &[17, 20, 20],
        &[0, 2, 2],
        &[true, true],
        1,
    );
}

// ---------------------------------------------------------------------------------------------
// Zone-change record door
// ---------------------------------------------------------------------------------------------

/// P0's life lost to a count of `Typed(creature)` under `controller` among this turn's deaths,
/// after P1 is declared; `died` lists the owners of the creatures that died.
fn deaths_counted(controller: ControllerRef, died: &[PlayerId]) -> i32 {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let victims: Vec<ObjectId> = died
        .iter()
        .map(|player| scenario.add_creature(*player, "Victim", 2, 2).id())
        .collect();
    let count = QuantityExpr::Ref {
        qty: QuantityRef::ZoneChangeCountThisTurn {
            from: Some(Zone::Battlefield),
            to: Some(Zone::Graveyard),
            filter: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Creature).controller(controller),
            ),
        },
    };
    let reader = effect_json(json!({
        "type": "LoseLife",
        "amount": serde_json::to_value(&count).unwrap(),
        "target": serde_json::to_value(TargetFilter::Controller).unwrap(),
    }));
    let spell = scenario
        .add_spell_to_hand(P0, "Probe", false)
        .with_ability_definition(then(head(), def(reader)))
        .id();
    let mut runner = scenario.build();
    for victim in victims {
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            victim,
            Zone::Graveyard,
            &mut Vec::new(),
        );
    }
    assert_eq!(runner.state().zone_changes_this_turn.len(), died.len());
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("declare P1");
    runner.advance_until_stack_empty();
    20 - runner.state().players[0].life
}

fn assert_record_door(controller: ControllerRef) {
    assert_eq!(
        deaths_counted(controller.clone(), &[P1]),
        1,
        "{controller:?}: P1's creature"
    );
    assert_eq!(
        deaths_counted(controller.clone(), &[P0]),
        0,
        "{controller:?}: P0's creature"
    );
    assert_eq!(
        deaths_counted(controller.clone(), &[P0, P1, P2]),
        1,
        "{controller:?}: all three"
    );
}

/// R1: a zone-change record's controller is read against the declared player (CR 608.2c).
#[test]
fn r1_record_controller_reads_the_declared_player() {
    assert_record_door(declared(G));
}

/// R2: a snapshot controller id compares against the record's controller.
#[test]
fn r2_record_controller_compares_a_specific_player() {
    assert_record_door(ControllerRef::SpecificPlayer { id: P1 });
}

// ---------------------------------------------------------------------------------------------
// Force-attack population
// ---------------------------------------------------------------------------------------------

fn force_attack_all(group: ChosenGroupId) -> AbilityDefinition {
    def(Effect::ForceAttack {
        target: TargetFilter::Typed(creature_of(group)),
        required_defender: TargetFilter::Controller,
        duration: Duration::UntilEndOfTurn,
        scope: EffectScope::All,
    })
}

/// F1: a broadcast force-attack population keeps the declared player as a concrete id, and
/// installs nothing once that player is gone (CR 611.2c, CR 608.2b).
#[test]
fn f1_force_attack_population_lowers_the_declared_player() {
    let root = then(head(), force_attack_all(G));
    let live = run(
        root.clone(),
        &[Pick::Player(P1)],
        &[],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!(live.affected.len(), 1, "{:?}", live.affected);
    assert!(
        live.affected[0].contains("SpecificPlayer") && !live.affected[0].contains("DeclaredPlayer"),
        "{:?}",
        live.affected
    );
    let gone = run(
        root,
        &[Pick::Player(P1)],
        &[],
        After::Eliminate,
        After::Nothing,
    );
    assert!(gone.affected.is_empty(), "{:?}", gone.affected);
}

// ---------------------------------------------------------------------------------------------
// Carried declarations: effect fields read through the resolving root's lookup
// ---------------------------------------------------------------------------------------------

fn spirit(owner: TargetFilter, count: QuantityExpr) -> Effect {
    effect_json(json!({
        "type": "Token", "name": "Spirit", "types": ["Creature", "Spirit"],
        "power": {"type": "Fixed", "value": 1}, "toughness": {"type": "Fixed", "value": 1},
        "count": serde_json::to_value(count).unwrap(), "owner": serde_json::to_value(owner).unwrap(),
    }))
}

fn one() -> QuantityExpr {
    QuantityExpr::Fixed { value: 1 }
}

fn shuffled(out: &Out) -> String {
    format!("{:?}", out.shuffled)
}

fn life(out: &Out) -> String {
    format!("{:?}", out.life)
}

fn tokens(out: &Out) -> String {
    let mut tokens = out.tokens.clone();
    tokens.sort();
    format!("{tokens:?}")
}

/// Runs `effect` once reading G in the creating chain and once in an end-step payload, with
/// `observe` as the observable, against an empty payload as the baseline, for four fates of
/// G's player: alive, hexproof after install, eliminated after install, illegal before resolve.
fn assert_carried(name: &str, effect: Effect, observe: fn(&Out) -> String) {
    let baseline = same_chain_run(Effect::TargetOnly {
        target: TargetFilter::Controller,
    });
    let reference = same_chain_run(effect.clone());
    assert_ne!(
        observe(&reference),
        observe(&baseline),
        "{name}: reach guard, the same-chain clause is observable"
    );
    let delayed_after = |after| payload_run(def(effect.clone()), After::Nothing, after);
    let empty_after = |after| payload_run(noop_payload(), After::Nothing, after);
    assert_eq!(
        observe(&delayed_after(After::Nothing)),
        observe(&reference),
        "{name}: delayed vs same chain"
    );
    assert_eq!(
        observe(&delayed_after(After::Hexproof)),
        observe(&reference),
        "{name}: hexproof after install is affected, not targeted"
    );
    assert_eq!(
        observe(&delayed_after(After::Eliminate)),
        observe(&empty_after(After::Eliminate)),
        "{name}: eliminated after install is no one"
    );
    for before in [After::Hexproof, After::Eliminate] {
        let payload = |payload| {
            run(
                with_bear(delayed(payload, Phase::End)),
                &[Pick::Player(P1), Pick::Bear(1)],
                &[Phase::End],
                before,
                After::Nothing,
            )
        };
        assert_eq!(
            observe(&payload(def(effect.clone()))),
            observe(&payload(noop_payload())),
            "{name}: {before:?} before resolve is no one at install"
        );
    }
}

/// CR 701.24a: "that player shuffles their library" in the payload.
#[test]
fn c1_shuffle_target_carries_the_declared_player() {
    assert_carried("Shuffle", kind("Shuffle", &dp(G)), shuffled);
    assert_eq!(
        payload_run(def(kind("Shuffle", &dp(G))), After::Nothing, After::Nothing).shuffled,
        vec![1]
    );
}

/// CR 119.3: `GainLife.player`.
#[test]
fn c2_gain_life_player_carries_the_declared_player() {
    assert_carried("GainLife", kind("GainLife", &dp(G)), life);
    assert_eq!(
        payload_run(
            def(kind("GainLife", &dp(G))),
            After::Nothing,
            After::Nothing
        )
        .life,
        vec![20, 23, 20]
    );
}

/// CR 111.2: `Token.owner`.
#[test]
fn c3_token_owner_carries_the_declared_player() {
    assert_carried("Token.owner", spirit(dp(G), one()), tokens);
    assert_eq!(
        payload_run(def(spirit(dp(G), one())), After::Nothing, After::Nothing)
            .tokens
            .iter()
            .map(|(owner, _)| *owner)
            .collect::<Vec<_>>(),
        vec![1]
    );
}

fn creatures_of_g() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(creature_of(G)),
        },
    }
}

/// `Token.count`: the creature population is the declared player's, counted when the payload
/// resolves.
#[test]
fn c4_token_count_counts_the_declared_players_creatures_live() {
    let spirits = spirit(TargetFilter::Controller, creatures_of_g());
    assert_carried("Token.count", spirits.clone(), tokens);
    let counted = |after| {
        payload_run(def(spirits.clone()), After::Nothing, after)
            .tokens
            .len()
    };
    assert_eq!(counted(After::Nothing), 1, "P1 controls one creature");
    assert_eq!(
        counted(After::ExtraCreature),
        2,
        "counted at firing, not at install"
    );
}

/// A payload-local group stays local; the outer one is carried; distinct outer groups keep
/// their own players.
#[test]
fn c5_local_group_shadows_and_distinct_groups_stay_distinct() {
    let payload = then(
        declaring(pick_player(), GL),
        then(def(kind("Shuffle", &dp(GL))), def(kind("Shuffle", &dp(G)))),
    );
    let out = run(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!(
        out.shuffled,
        vec![2, 1],
        "local GL=P2 first, carried G=P1 second"
    );

    let payload = then(def(kind("GainLife", &dp(G))), def(kind("Shuffle", &dp(G2))));
    let out = run(
        then(
            head(),
            then(declaring(pick_player(), G2), delayed(payload, Phase::End)),
        ),
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!((out.life, out.shuffled), (vec![20, 23, 20], vec![2]));
}

/// A payload that installs another delayed trigger hands the carried players on.
#[test]
fn c6_nested_payload_carries_the_outer_group() {
    let inner = delayed(
        then(def(kind("Shuffle", &dp(G))), def(kind("GainLife", &dp(G)))),
        Phase::Upkeep,
    );
    let out = run(
        then(head(), delayed(inner, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End, Phase::Upkeep],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!((out.life, out.shuffled), (vec![20, 23, 20], vec![1]));
}

/// A lasting effect whose affected filter reads G is lowered to G's player's objects when the
/// payload resolves, as in the creating chain.
#[test]
fn c7_lasting_effect_filter_reads_the_carried_group() {
    use engine::types::ability::ContinuousModification;
    use engine::types::keywords::Keyword;
    let flying_for_g = || Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::Typed(creature_of(G)))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }])],
        duration: Some(Duration::UntilEndOfTurn),
        target: None,
        end_cost: None,
    };
    let reference = same_chain_run(flying_for_g());
    let delayed = payload_run(def(flying_for_g()), After::Nothing, After::Nothing);
    assert_eq!(
        reference.affected.len(),
        1,
        "reach guard: the clause grants once"
    );
    assert_eq!(delayed.affected, reference.affected);
}

/// A payload-local group read by a payload it installs is carried to that payload.
#[test]
fn c8_nested_payload_carries_a_payload_local_group() {
    let payload = then(
        declaring(pick_player(), GL),
        delayed(def(kind("Shuffle", &dp(GL))), Phase::Upkeep),
    );
    let out = run(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End, Phase::Upkeep],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!(out.shuffled, vec![2]);
}

/// The payload's own declaration of a group wins over the carried player of the same group.
#[test]
fn c9_payload_local_declaration_shadows_the_carried_player() {
    let payload = then(declaring(pick_player(), G), def(kind("Shuffle", &dp(G))));
    let out = run(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1), Pick::Player(P2)],
        &[Phase::End],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!(out.shuffled, vec![2]);
}

// ---------------------------------------------------------------------------------------------
// Targeted payload slots that read a carried group
// ---------------------------------------------------------------------------------------------

fn destroy(target: TargetFilter) -> AbilityDefinition {
    def(Effect::Destroy {
        target,
        cant_regenerate: false,
    })
}

fn destroy_creature_of(group: ChosenGroupId) -> AbilityDefinition {
    destroy(TargetFilter::Typed(creature_of(group)))
}

fn targeted_run(payload: AbilityDefinition, picks: &[Pick], after: After) -> Out {
    run(
        then(head(), delayed(payload, Phase::End)),
        picks,
        &[Phase::End],
        After::Nothing,
        after,
    )
}

/// CR 603.7a + CR 608.2c: a payload slot filter that reads a carried group offers that player's
/// objects, at the payload root and at a sub-node.
#[test]
fn b1_targeted_slot_reads_the_carried_group() {
    let picks = [Pick::Player(P1), Pick::Bear(0)];
    let reference = run(
        then(head(), destroy_creature_of(G)),
        &picks,
        &[],
        After::Nothing,
        After::Nothing,
    );
    assert_eq!(
        reference.bears,
        vec![false, true],
        "reach guard: same chain"
    );
    let root = targeted_run(destroy_creature_of(G), &picks, After::ExtraCreature);
    assert_eq!(
        (root.bears, root.prompts),
        (vec![false, true], 2),
        "root node"
    );
    let sub = then(
        def(Effect::TargetOnly {
            target: TargetFilter::Controller,
        }),
        destroy_creature_of(G),
    );
    let sub = targeted_run(sub, &picks, After::ExtraCreature);
    assert_eq!((sub.bears, sub.prompts), (vec![false, true], 2), "sub node");
}

/// A group the payload itself declares earlier takes the payload's player, not the carried one.
#[test]
fn b1b_payload_local_announcement_wins_over_the_carried_player() {
    let payload = then(declaring(pick_player(), G), destroy_creature_of(G));
    let out = targeted_run(
        payload,
        &[Pick::Player(P1), Pick::Player(P2), Pick::Bear(1)],
        After::Nothing,
    );
    assert_eq!(out.bears, vec![true, false]);
}

// ---------------------------------------------------------------------------------------------
// CR 603.4 fire-time gate that reads a carried group
// ---------------------------------------------------------------------------------------------

fn count_check(filter: TypedFilter, comparator: Comparator, n: i32) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(filter),
            },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: n },
    }
}

fn history_check(qty: QuantityRef, comparator: Comparator, n: i32) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref { qty },
        comparator,
        rhs: QuantityExpr::Fixed { value: n },
    }
}

fn zone_changes(comparator: Comparator, n: i32) -> AbilityCondition {
    history_check(
        QuantityRef::ZoneChangeCountThisTurn {
            from: None,
            to: None,
            filter: TargetFilter::Typed(creature_of(G)),
        },
        comparator,
        n,
    )
}

fn counters_added(comparator: Comparator, n: i32) -> AbilityCondition {
    history_check(
        QuantityRef::CounterAddedThisTurn {
            actor: engine::types::ability::CountScope::All,
            counters: engine::types::counter::CounterMatch::Any,
            target: TargetFilter::Typed(creature_of(G)),
        },
        comparator,
        n,
    )
}

fn damage_dealt(comparator: Comparator, n: i32) -> AbilityCondition {
    history_check(
        QuantityRef::DamageDealtThisTurn {
            source: Box::new(TargetFilter::Typed(creature_of(G))),
            target: Box::new(TargetFilter::Any),
            aggregate: engine::types::ability::AggregateFunction::Sum,
            group_by: None,
            damage_kind: engine::types::ability::DamageKindFilter::Any,
            channel: engine::types::ability::DamageChannel::Total,
        },
        comparator,
        n,
    )
}

fn negated(condition: AbilityCondition) -> AbilityCondition {
    AbilityCondition::Not {
        condition: Box::new(condition),
    }
}

fn gated_pushes(condition: Option<AbilityCondition>, after: After) -> usize {
    let mut payload = def(lose(TargetFilter::Controller));
    payload.condition = condition;
    payload_run(payload, After::Nothing, after).pushed
}

/// A false gate keeps the delayed ability off the stack (CR 603.4); the true gate and the
/// ungated payload put it there. Covers both `QuantityCheck` bridges and `Not`.
#[test]
fn g1_false_gate_on_a_carried_group_does_not_trigger() {
    let lands = || TypedFilter::new(TypeFilter::Land).controller(declared(G));
    let creatures = || creature_of(G);
    let on_stack = gated_pushes(None, After::Nothing);
    let not = |c| AbilityCondition::Not {
        condition: Box::new(c),
    };
    for (name, condition, triggers) in [
        (
            "comparison, true",
            count_check(creatures(), Comparator::LE, 5),
            true,
        ),
        (
            "comparison, false",
            count_check(creatures(), Comparator::GE, 5),
            false,
        ),
        (
            "is-present, true",
            count_check(creatures(), Comparator::GE, 1),
            true,
        ),
        (
            "is-present, false",
            count_check(lands(), Comparator::GE, 1),
            false,
        ),
        (
            "not, true",
            not(count_check(creatures(), Comparator::GE, 5)),
            true,
        ),
        (
            "not, false",
            not(count_check(creatures(), Comparator::LE, 5)),
            false,
        ),
    ] {
        assert_eq!(
            gated_pushes(Some(condition), After::Nothing),
            on_stack - usize::from(!triggers),
            "{name}"
        );
    }
}

/// A gate on a carried player who left the game names no one at the event as at resolution, and
/// does nothing (CR 603.4 + CR 608.2b).
#[test]
fn g2_gate_on_a_carried_player_who_left_does_nothing() {
    let gated = |after| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = Some(count_check(creature_of(G), Comparator::GE, 1));
        payload_run(payload, After::Nothing, after)
    };
    let untouched = payload_run(noop_payload(), After::Nothing, After::Nothing);
    assert!(
        differs(&gated(After::Nothing), &untouched),
        "reach guard: with P1 present the gate is true and the effect happens"
    );
    let empty = payload_run(noop_payload(), After::Nothing, After::Eliminate);
    let gone = gated(After::Eliminate);
    assert_eq!(
        (&gone.fingerprint, &gone.last_wait),
        (&empty.fingerprint, &empty.last_wait)
    );
}

/// CR 603.4 + CR 608.2b: a carried player who left the game is no one at the event as at
/// resolution. A false gate keeps the ability off the stack and consumes a one-shot; a true gate
/// puts it there, and the body then takes effect exactly when resolution alone decides the same
/// gate (the else-branch variant, which cannot be hoisted).
#[test]
fn g3_gate_on_a_carried_player_who_left_reads_no_one_at_the_event_and_at_resolution() {
    let creatures = || creature_of(G);
    let not = |c| AbilityCondition::Not {
        condition: Box::new(c),
    };
    let died_power = |comparator, n| AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeAggregateThisTurn {
                from: None,
                to: None,
                filter: TargetFilter::Typed(creatures()),
                function: engine::types::ability::AggregateFunction::Sum,
                property: engine::types::ability::ObjectProperty::Power,
            },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: n },
    };
    let owned = TypedFilter::new(TypeFilter::Creature).properties(vec![FilterProp::Owned {
        controller: declared(G),
    }]);
    let gated = |condition: &AbilityCondition, else_ability: bool, after| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = Some(condition.clone());
        if else_ability {
            payload.else_ability = Some(Box::new(noop_payload()));
        }
        payload_run(payload, After::Nothing, after)
    };
    let ungated = |after| payload_run(def(lose(TargetFilter::Controller)), After::Nothing, after);
    let untouched = payload_run(noop_payload(), After::Nothing, After::Eliminate);
    let effective = |out: &Out| out.life[0] != untouched.life[0];
    let (mut took_effect, mut did_not) = (false, false);
    // (shape, true with P1 present, true of no one, reads a ledger seeded for P1)
    for (name, condition, true_of_one, true_of_none, seeded) in [
        (
            "presence",
            count_check(creatures(), Comparator::GE, 1),
            Some(true),
            false,
            false,
        ),
        (
            "comparison",
            count_check(creatures(), Comparator::GE, 5),
            Some(false),
            false,
            false,
        ),
        (
            "at most none",
            count_check(creatures(), Comparator::LE, 0),
            Some(false),
            true,
            false,
        ),
        (
            "owned presence",
            count_check(owned, Comparator::GE, 1),
            Some(true),
            false,
            false,
        ),
        (
            "not presence",
            not(count_check(creatures(), Comparator::GE, 1)),
            Some(false),
            true,
            false,
        ),
        (
            "not at most none",
            not(count_check(creatures(), Comparator::LE, 0)),
            Some(true),
            false,
            false,
        ),
        (
            "zone changes",
            zone_changes(Comparator::GE, 1),
            Some(false),
            false,
            false,
        ),
        (
            "no zone changes",
            zone_changes(Comparator::LE, 0),
            Some(true),
            true,
            false,
        ),
        (
            "not zone changes",
            not(zone_changes(Comparator::GE, 1)),
            Some(true),
            true,
            false,
        ),
        (
            "died power",
            died_power(Comparator::GE, 1),
            None,
            false,
            false,
        ),
        (
            "no died power",
            died_power(Comparator::LE, 0),
            None,
            true,
            false,
        ),
        (
            "counter added",
            counters_added(Comparator::GE, 1),
            Some(true),
            false,
            true,
        ),
        (
            "no counter added",
            counters_added(Comparator::LE, 0),
            Some(false),
            true,
            true,
        ),
        (
            "damage dealt",
            damage_dealt(Comparator::GE, 1),
            Some(true),
            false,
            true,
        ),
        (
            "not damage dealt",
            not(damage_dealt(Comparator::GE, 1)),
            Some(false),
            true,
            true,
        ),
    ] {
        let (present, left) = if seeded {
            (After::Records, After::RecordsThenEliminate)
        } else {
            (After::Nothing, After::Eliminate)
        };
        if let Some(true_of_one) = true_of_one {
            assert_eq!(
                gated(&condition, false, present).pushed,
                ungated(After::Nothing).pushed - usize::from(!true_of_one),
                "{name}: with P1 present a false gate stays off the stack"
            );
        }
        let gone = gated(&condition, false, left);
        let by_resolution = gated(&condition, true, left);
        assert!(by_resolution.pushed > 0, "{name}: reach guard");
        assert_eq!(
            gone.pushed,
            ungated(After::Eliminate).pushed - usize::from(!true_of_none),
            "{name}: the event check reads no one"
        );
        assert_eq!(
            effective(&by_resolution),
            true_of_none,
            "{name}: the resolution check reads no one"
        );
        assert_eq!(
            effective(&gone),
            effective(&by_resolution),
            "{name}: the hoisted and the resolution check agree"
        );
        if !true_of_none {
            assert_eq!(gone.delayed_left, 0, "{name}: a false one-shot is consumed");
        }
        took_effect |= effective(&by_resolution);
        did_not |= !effective(&by_resolution);
    }
    assert!(took_effect && did_not, "reach guard: both outcomes occur");
}

/// CR 603.4 + CR 603.7b: for a duration-bearing "whenever" generator whose carried player left, a
/// false gate declines the occurrence and a true one puts the ability on the stack.
#[test]
fn g4_duration_bearing_generator_on_a_carried_player_who_left_reads_no_one() {
    let whenever = |payload: AbilityDefinition| {
        let mut trigger = engine::types::ability::TriggerDefinition::new(
            engine::types::triggers::TriggerMode::Phase,
        );
        trigger.phase = Some(Phase::End);
        def(Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WheneverEvent {
                trigger: Box::new(trigger),
                expiry: engine::types::ability::WheneverEventExpiry::EndOfTurn,
            },
            effect: Box::new(payload),
            uses_tracked_set: false,
        })
    };
    let gated = |condition: Option<AbilityCondition>, after| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = condition;
        run(
            then(head(), whenever(payload)),
            &[Pick::Player(P1)],
            &[Phase::End],
            After::Nothing,
            after,
        )
    };
    let presence = count_check(creature_of(G), Comparator::GE, 1);
    let ungated = gated(None, After::Eliminate);
    let false_gate = gated(Some(presence.clone()), After::Eliminate);
    assert_ne!(
        ungated.life, false_gate.life,
        "reach guard: the ungated generator fires and takes effect"
    );
    assert_eq!(false_gate.pushed, ungated.pushed - 1, "false gate");
    let true_gate = gated(
        Some(AbilityCondition::Not {
            condition: Box::new(presence),
        }),
        After::Eliminate,
    );
    assert_eq!(true_gate.pushed, ungated.pushed, "true gate");
    assert_eq!(true_gate.life, ungated.life, "true gate takes effect");
}

/// CR 603.4 + CR 603.7a: a gate on a group the payload declares itself names a player only once
/// the payload is on the stack, so the event check leaves it to resolution, which reads the
/// announced player.
#[test]
fn g6_gate_on_a_payload_local_group_is_decided_at_resolution() {
    let gated = |condition: Option<AbilityCondition>| {
        let mut payload = declaring(lose(TargetFilter::Player), G);
        payload.condition = condition;
        run(
            then(head(), delayed(payload, Phase::End)),
            &[Pick::Player(P1), Pick::Player(P2)],
            &[Phase::End],
            After::Nothing,
            After::Nothing,
        )
    };
    let ungated = gated(None);
    assert!(ungated.life[2] < 20, "reach guard: the payload hits P2");
    for (name, comparator, n, takes_effect) in [
        ("true gate", Comparator::GE, 1, true),
        ("false gate", Comparator::GE, 5, false),
    ] {
        let out = gated(Some(count_check(creature_of(G), comparator, n)));
        assert_eq!(out.pushed, ungated.pushed, "{name}: on the stack");
        let expected = if takes_effect {
            &ungated.life
        } else {
            &vec![20; 3]
        };
        assert_eq!(&out.life, expected, "{name}: resolution decides");
    }
}

/// CR 603.4 + CR 608.2b: a group that was never carried (illegal at install) reads no one on
/// both legs, exactly as a carried player who left does.
#[test]
fn g5_gate_on_a_group_that_was_never_carried_reads_no_one() {
    let gated = |condition: Option<AbilityCondition>| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = condition;
        run(
            with_bear(delayed(payload, Phase::End)),
            &[Pick::Player(P1), Pick::Bear(1)],
            &[Phase::End],
            After::Hexproof,
            After::Nothing,
        )
    };
    let presence = count_check(creature_of(G), Comparator::GE, 1);
    let ungated = gated(None);
    let false_gate = gated(Some(presence.clone()));
    assert_ne!(
        ungated.life, false_gate.life,
        "reach guard: the ungated payload fires and takes effect"
    );
    assert_eq!(false_gate.pushed, ungated.pushed - 1, "false");
    let true_gate = gated(Some(AbilityCondition::Not {
        condition: Box::new(presence),
    }));
    assert_eq!(true_gate.pushed, ungated.pushed, "true");
    assert_eq!(true_gate.life, ungated.life, "true takes effect");
}

/// A one-shot and a duration-bearing delayed ability whose payload is `payload`.
fn delayed_forms(payload: AbilityDefinition) -> [AbilityDefinition; 2] {
    let mut trigger =
        engine::types::ability::TriggerDefinition::new(engine::types::triggers::TriggerMode::Phase);
    trigger.phase = Some(Phase::End);
    [
        delayed(payload.clone(), Phase::End),
        def(Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WheneverEvent {
                trigger: Box::new(trigger),
                expiry: engine::types::ability::WheneverEventExpiry::EndOfTurn,
            },
            effect: Box::new(payload),
            uses_tracked_set: false,
        }),
    ]
}

/// Runs `payload` with P1 declared, applying `install` once the delayed ability exists and
/// `on_stack` once it has triggered.
fn departure_run(payload: &AbilityDefinition, form: usize, install: After, on_stack: After) -> Out {
    run_late(
        then(head(), delayed_forms(payload.clone())[form].clone()),
        &[Pick::Player(P1)],
        &[Phase::End],
        After::Nothing,
        install,
        on_stack,
    )
}

/// CR 603.4 + CR 608.2b: a gate on history of a carried player who leaves after the event, with
/// the payload on the stack, names no one at resolution: the outcome is that of the same final
/// state reached with the player gone before the event.
fn assert_departure_after_the_event_reads_no_one(gate: &AbilityCondition, departure: After) {
    let mut payload = def(lose(TargetFilter::Controller));
    payload.condition = Some(gate.clone());
    for form in 0..2 {
        let late = departure_run(&payload, form, After::Nothing, departure);
        let early = departure_run(&payload, form, departure, After::Nothing);
        assert!(
            early.life[0] < 20,
            "form {form}: reach guard: the gate is true of no one and the payload takes effect"
        );
        assert_eq!(late.life, early.life, "form {form}");
    }
}

#[test]
fn h1_counter_history_gate_of_a_player_who_leaves_with_the_payload_on_the_stack() {
    for gate in [
        counters_added(Comparator::LE, 0),
        negated(counters_added(Comparator::GE, 1)),
    ] {
        assert_departure_after_the_event_reads_no_one(&gate, After::RecordsThenEliminate);
    }
}

#[test]
fn h2_damage_history_gate_of_a_player_who_leaves_with_the_payload_on_the_stack() {
    for gate in [
        damage_dealt(Comparator::LE, 0),
        negated(damage_dealt(Comparator::GE, 1)),
    ] {
        assert_departure_after_the_event_reads_no_one(&gate, After::RecordsThenEliminate);
    }
}

#[test]
fn h3_zone_change_history_gate_of_a_player_who_leaves_with_the_payload_on_the_stack() {
    for gate in [
        zone_changes(Comparator::LE, 0),
        negated(zone_changes(Comparator::GE, 1)),
    ] {
        assert_departure_after_the_event_reads_no_one(&gate, After::BearDiesThenEliminate);
    }
}

/// Presence agrees whether the departed player is read as no one or by identity, because the
/// player's objects leave with them; the schedule still drives the payload to resolution.
#[test]
fn h4_presence_gate_of_a_player_who_leaves_with_the_payload_on_the_stack() {
    let outcome = |comparator, n| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = Some(count_check(creature_of(G), comparator, n));
        let late = departure_run(&payload, 0, After::Nothing, After::Eliminate);
        let early = departure_run(&payload, 0, After::Eliminate, After::Nothing);
        assert_eq!(late.pushed, 1, "the payload was on the stack at the event");
        assert_eq!(late.life, early.life);
        late.life
    };
    assert_eq!(outcome(Comparator::LE, 5), vec![17, 20, 20], "reach guard");
    assert_eq!(outcome(Comparator::GE, 1), vec![20, 20, 20]);
}

/// CR 603.4 + CR 608.2b: "no counter was put on a creature the declared player controls this
/// turn", true at the event; a counter lands and the player leaves with the payload on the stack.
/// With the player still in the game the gate is false and the payload does nothing.
#[test]
fn h5_counter_put_while_the_payload_waits_and_the_player_leaves() {
    let run = |gate: Option<AbilityCondition>, on_stack| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = gate;
        departure_run(&payload, 0, After::Nothing, on_stack).life
    };
    let no_counter = Some(counters_added(Comparator::LE, 0));
    assert_eq!(run(None, After::Records), vec![17, 20, 20], "ungated");
    assert_eq!(
        run(no_counter.clone(), After::Records),
        vec![20, 20, 20],
        "the player is still in the game: the gate is false"
    );
    assert_eq!(
        run(no_counter, After::RecordsThenEliminate),
        vec![17, 20, 20],
        "the player left: the gate reads no one"
    );
}

/// CR 603.4: an intervening "if" that becomes false while the ability waits on the stack makes
/// the whole ability do nothing, an unconditional second clause included.
#[test]
fn h6_gate_false_by_resolution_stops_the_whole_ability() {
    let run = |on_stack| {
        let mut payload = def(lose(TargetFilter::Controller));
        payload.condition = Some(count_check(creature_of(G), Comparator::GE, 1));
        let mut second = def(lose(TargetFilter::Controller));
        second.sub_link = SubAbilityLink::SequentialSibling;
        departure_run(&payload.sub_ability(second), 0, After::Nothing, on_stack).life
    };
    assert_eq!(
        run(After::Nothing),
        vec![14, 20, 20],
        "reach guard: both clauses run while the gate holds"
    );
    assert_eq!(run(After::BearDies), vec![20, 20, 20]);
}

/// CR 603.12: a reflexive trigger created while a declared-player payload resolves has no stored
/// recheck and is not given one, so when its guard turns false while it waits only the guarded
/// clause is skipped and the unconditional clause after it still runs.
#[test]
fn h7_reflexive_trigger_gate_false_by_resolution_skips_only_its_clause() {
    let run = |guard: Option<AbilityCondition>, on_reflexive| {
        let mut gated = def(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        });
        gated.condition = Some(match guard {
            Some(guard) => AbilityCondition::when_you_do_with_guard(guard),
            None => AbilityCondition::WhenYouDo,
        });
        let payload =
            def(lose(dp(G))).sub_ability(then(gated, def(lose(TargetFilter::Controller))));
        run_on(
            Board::Plain,
            payload_root(payload),
            &[Pick::Player(P1)],
            &[Phase::End],
            Changes {
                before_resolve: After::Nothing,
                after_install: After::Nothing,
                on_stack: After::Nothing,
                on_reflexive,
            },
            None,
        )
        .life
    };
    let present = || Some(count_check(creature_of(G), Comparator::GE, 1));
    assert_eq!(
        run(None, After::BearDies),
        vec![22, 17, 20],
        "reach guard: the reflexive trigger is driven and both clauses run"
    );
    assert_eq!(
        run(present(), After::Nothing),
        vec![22, 17, 20],
        "the guard still holds: both clauses run"
    );
    assert_eq!(
        run(present(), After::BearDies),
        vec![17, 17, 20],
        "the guard is false at resolution: only the unconditional clause runs"
    );
}

/// A declared player who was an illegal target as the creating chain began to resolve names no
/// one at install, so the payload's slot offers none of their objects.
#[test]
fn b1c_slot_of_an_install_time_illegal_player_offers_nothing() {
    let run = |before| {
        run(
            with_bear(delayed(destroy_creature_of(G), Phase::End)),
            &[Pick::Player(P1), Pick::Bear(1)],
            &[Phase::End],
            before,
            After::Nothing,
        )
    };
    assert_eq!(
        run(After::Nothing).bears,
        vec![false, true],
        "reach guard: a legal P1 loses a bear"
    );
    assert_eq!(run(After::Hexproof).bears, vec![true, true]);
}

/// A payload node that declares the group but announces no player names no one; it does not
/// fall back to the carried player.
#[test]
fn b4_payload_declaration_without_a_player_names_no_one() {
    let silent = declaring(
        Effect::TargetOnly {
            target: TargetFilter::Controller,
        },
        G,
    );
    let reader = || def(kind("Shuffle", &dp(G)));
    assert_eq!(
        payload_run(reader(), After::Nothing, After::Nothing).shuffled,
        vec![1],
        "reach guard: without the local declaration the carried player shuffles"
    );
    let out = payload_run(then(silent, reader()), After::Nothing, After::Nothing);
    assert!(out.shuffled.is_empty());
}

/// The selection-time mirror of b4: a silent local declaration leaves a reader slot without
/// candidates instead of offering the carried player's objects.
#[test]
fn b4b_slot_after_a_silent_declaration_offers_no_carried_objects() {
    let silent = declaring(
        Effect::TargetOnly {
            target: TargetFilter::Controller,
        },
        G,
    );
    let picks = [Pick::Player(P1), Pick::Bear(0)];
    let run = |payload| targeted_run(payload, &picks, After::ExtraCreature);
    let reference = run(destroy_creature_of(G));
    assert_eq!(
        (reference.bears, reference.prompts),
        (vec![false, true], 2),
        "reach guard: without the local declaration the carried player's bear is offered"
    );
    let out = run(then(silent, destroy_creature_of(G)));
    assert_eq!((out.bears, out.prompts), (vec![true, true], 1));
}

// ---------------------------------------------------------------------------------------------
// A group that names no one offers nothing at selection (CR 608.2b, CR 601.2c)
// ---------------------------------------------------------------------------------------------

/// Populations read through group G: controlled, wrapped, owner-scoped.
fn reader_populations() -> [(&'static str, TargetFilter); 3] {
    let creature = || TypedFilter::new(TypeFilter::Creature);
    [
        ("controlled", TargetFilter::Typed(creature_of(G))),
        (
            "wrapped",
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(creature_of(G)),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::Typed(
                            creature().properties(vec![FilterProp::Token]),
                        )),
                    },
                ],
            },
        ),
        (
            "owned",
            TargetFilter::Typed(creature().properties(vec![FilterProp::Owned {
                controller: declared(G),
            }])),
        ),
    ]
}

fn caster_board_run(root: AbilityDefinition, picks: &[Pick], before: After, after: After) -> Out {
    run_on(
        Board::CasterBear,
        root,
        picks,
        &[Phase::End],
        Changes {
            before_resolve: before,
            after_install: after,
            on_stack: After::Nothing,
            on_reflexive: After::Nothing,
        },
        None,
    )
}

fn payload_root(payload: AbilityDefinition) -> AbilityDefinition {
    then(head(), delayed(payload, Phase::End))
}

/// A payload slot reading a group that names no one is offered nothing, so neither the caster's
/// creature nor the latest selected player's is a choice. Beside it, the legal carried player.
#[test]
fn b5_payload_slot_of_a_group_naming_no_one_offers_nothing() {
    let silent = || {
        declaring(
            Effect::TargetOnly {
                target: TargetFilter::Controller,
            },
            G,
        )
    };
    for (name, population) in reader_populations() {
        let reader = || destroy(population.clone());
        let legal = caster_board_run(
            payload_root(reader()),
            &[Pick::Player(P1), Pick::Bear(0)],
            After::Nothing,
            After::ExtraCreature,
        );
        assert_eq!(
            (legal.bears.clone(), legal.prompts),
            (vec![false, true, true], 2),
            "{name}: reach guard, the carried player's creature is a choice ({})",
            legal.last_wait
        );
        let beside = caster_board_run(
            payload_root(then(def(pick_player()), reader())),
            &[Pick::Player(P1), Pick::Player(P2), Pick::Bear(0)],
            After::Nothing,
            After::ExtraCreature,
        );
        assert_eq!(
            (beside.bears, beside.prompts),
            (vec![false, true, true], 3),
            "{name}: reach guard, an earlier unrelated pick leaves the carried player in force"
        );
        // A hexproof P1 is an illegal target as the spell resolves, so a creature target keeps
        // the spell alive and the group is not carried.
        let uncarried = |payload| with_bear(delayed(payload, Phase::End));
        for (case, root, picks, before, after, bears, prompts) in [
            (
                "silent local declaration",
                payload_root(then(silent(), reader())),
                vec![Pick::Player(P1), Pick::Bear(2)],
                After::Nothing,
                After::Nothing,
                vec![true, true, true],
                1,
            ),
            (
                "never carried",
                uncarried(reader()),
                vec![Pick::Player(P1), Pick::Bear(1), Pick::Bear(2)],
                After::Hexproof,
                After::Nothing,
                vec![true, true, true],
                2,
            ),
            (
                "never carried, earlier player pick",
                uncarried(then(def(pick_player()), reader())),
                vec![
                    Pick::Player(P1),
                    Pick::Bear(1),
                    Pick::Player(P2),
                    Pick::Bear(1),
                ],
                After::Hexproof,
                After::Nothing,
                vec![true, true, true],
                2,
            ),
            (
                "carried player left",
                payload_root(reader()),
                vec![Pick::Player(P1), Pick::Bear(2)],
                After::Nothing,
                After::Eliminate,
                vec![false, true, true],
                1,
            ),
        ] {
            let out = caster_board_run(root, &picks, before, after);
            assert_eq!(
                (out.bears, out.prompts),
                (bears, prompts),
                "{name}, {case}: nothing is offered"
            );
        }
    }
}
