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

fn apply(state: &mut GameState, change: After) {
    match change {
        After::Nothing => {}
        After::Hexproof => grant_hexproof(state, P1),
        After::Eliminate => {
            engine::game::elimination::eliminate_player(state, P1, &mut Vec::new());
            assert!(!engine::game::players::is_alive(state, P1));
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
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bears: Vec<ObjectId> = [P1, P2]
        .iter()
        .map(|p| scenario.add_creature(*p, "Bear", 2, 2).id())
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
    let mut last_wait = String::new();
    let mut asked: Vec<String> = Vec::new();
    let mut drive = |runner: &mut GameRunner| {
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
                    }
                    runner.act(GameAction::PassPriority).expect("pass");
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
    drive(&mut runner);
    apply(runner.state_mut(), after_install);
    for phase in phases {
        runner.advance_to_phase(*phase);
        drive(&mut runner);
    }
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

const PLAYER_KINDS: [&str; 12] = [
    "Draw",
    "LoseLife",
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

fn deciding_run(payload: AbilityDefinition, before: After, decide: bool) -> Out {
    run_deciding(
        then(head(), delayed(payload, Phase::End)),
        &[Pick::Player(P1)],
        &[Phase::End],
        before,
        After::Nothing,
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
    let accepted = deciding_run(may_draw(), After::Nothing, true);
    assert_eq!(accepted.asked, ["optional:1"]);
    assert_eq!(accepted.hand, [0, 3, 2]);
    let declined = deciding_run(may_draw(), After::Nothing, false);
    assert_eq!(declined.asked, ["optional:1"]);
    assert_eq!(declined.hand, [0, 2, 2]);
}

/// I2: `unless_pay.payer` naming G offers the payment to G's player (CR 118.12a, CR 603.7a).
#[test]
fn i2_unless_payer_offers_the_payment_to_the_groups_player() {
    let declined = deciding_run(unless_lose(), After::Nothing, false);
    assert_eq!(declined.asked, ["unless:1"]);
    assert_eq!(declined.life, [20, 17, 20]);
    let paid = deciding_run(unless_lose(), After::Nothing, true);
    assert_eq!(paid.asked, ["unless:1"]);
    assert_eq!(paid.life, [20, 20, 20]);
}

/// I3: a declared player who left before the chain resolved is asked nothing (CR 608.2b); the
/// available-player control is I1 / I2.
#[test]
fn i3_unavailable_group_player_is_asked_nothing_and_nothing_happens() {
    for payload in [may_draw(), unless_lose()] {
        let out = deciding_run(payload, After::Eliminate, true);
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
