//! `TargetFilter::DeclaredPlayer { group }` names the player announced as the target of the
//! chain node tagged with `group` (CR 608.2c + CR 115.1a), read live at resolution, and names
//! no one when that target was illegal (CR 608.2b). Chains are hand-built: the parser does not
//! emit the reference yet. Three players: caster P0, declared player P1, bystander P2.

use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChosenGroupId, DamageSource, Effect, EffectScope,
    MultiTargetSpec, ObjectScope, PreventionAmount, PreventionScope, QuantityExpr, QuantityRef,
    ResolvedAbility, SubAbilityLink, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const G: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE);
const G2: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE + 1);

fn q(value: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value }
}

fn declared(group: ChosenGroupId) -> TargetFilter {
    TargetFilter::DeclaredPlayer { group }
}

fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))
}

/// A target-announcing node with no effect of its own, so no assertion depends on what a
/// declaring clause does for itself.
fn pick_player() -> Effect {
    Effect::TargetOnly {
        target: TargetFilter::Player,
    }
}

struct Node {
    effect: Effect,
    group: Option<ChosenGroupId>,
    multi: Option<MultiTargetSpec>,
}

fn node(effect: Effect) -> Node {
    Node {
        effect,
        group: None,
        multi: None,
    }
}

fn declaring(group: ChosenGroupId) -> Node {
    Node {
        group: Some(group),
        ..node(pick_player())
    }
}

/// `nodes` chained root-first, then a `PutCounter` on a creature that lands in every run.
fn chain(nodes: Vec<Node>) -> AbilityDefinition {
    let reach_guard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: q(1),
            target: creature_filter(),
        },
    );
    nodes.into_iter().rev().fold(reach_guard, |mut rest, n| {
        // Independent instructions, as sentence boundaries make them: a `PreventDamage`
        // parent would otherwise take a continuation sub as its shield rider.
        rest.sub_link = SubAbilityLink::SequentialSibling;
        let mut def = AbilityDefinition::new(AbilityKind::Spell, n.effect).sub_ability(rest);
        def.declares_chosen_group = n.group;
        if let Some(spec) = n.multi {
            def = def.multi_target(spec);
        }
        def
    })
}

#[derive(Clone, Copy)]
enum Pick {
    Player(PlayerId),
    /// The nth creature P0 controls.
    Creature(usize),
}

#[derive(Default)]
struct Run {
    first_slot_count: usize,
    hands: Vec<usize>,
    lives: Vec<i32>,
    poison: Vec<u32>,
    searches: Vec<(PlayerId, Option<PlayerId>)>,
    shields: usize,
    /// `+1/+1` counters on the reach-guard creature (the first one).
    counters: u32,
    damage: Vec<u32>,
}

/// Casts `def` with one pick per target slot; `eliminate_p1` removes P1 while the spell is on
/// the stack, so the slot that named P1 is illegal on resolution.
fn run(def: AbilityDefinition, picks: &[Pick], eliminate_p1: bool, hand_cards: bool) -> Run {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let creatures: Vec<ObjectId> = [(3, 9), (4, 9), (2, 9)]
        .into_iter()
        .enumerate()
        .map(|(i, (power, toughness))| {
            scenario
                .add_creature(P0, &format!("Creature {i}"), power, toughness)
                .id()
        })
        .collect();
    for player in [P0, P1, P2] {
        for i in 0..6 {
            scenario.add_card_to_library_top(player, &format!("Library {}-{i}", player.0));
        }
        if hand_cards {
            scenario.add_card_to_hand(player, &format!("Hand {}", player.0));
        }
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Declared Player Probe", false)
        .with_ability_definition(def)
        .id();
    let mut runner: GameRunner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");

    let mut out = Run::default();
    let mut eliminate = eliminate_p1;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                out.first_slot_count = target_slots.len();
                let target = picks.get(selection.current_slot).map(|pick| match *pick {
                    Pick::Player(player) => TargetRef::Player(player),
                    Pick::Creature(i) => TargetRef::Object(creatures[i]),
                });
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("target");
            }
            WaitingFor::SearchChoice {
                player,
                library_owner,
                cards,
                ..
            } => {
                out.searches.push((player, library_owner));
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(1).collect(),
                    })
                    .expect("search");
            }
            WaitingFor::DiscardChoice { cards, count, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(count).collect(),
                    })
                    .expect("discard");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if std::mem::take(&mut eliminate) {
                    runner.state_mut().players[1].is_eliminated = true;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let state = runner.state();
    out.hands = state.players.iter().map(|p| p.hand.len()).collect();
    out.lives = state.players.iter().map(|p| p.life).collect();
    out.poison = state.players.iter().map(|p| p.poison_counters).collect();
    out.shields = state.pending_damage_replacements.len();
    out.counters = state.objects[&creatures[0]]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    out.damage = creatures
        .iter()
        .map(|id| state.objects[id].damage_marked)
        .collect();
    out
}

use Pick::{Creature as C, Player as Pl};

#[test]
fn reader_names_the_tagged_nodes_own_player_whatever_slots_precede_it() {
    let draw = |group| {
        node(Effect::Draw {
            count: q(1),
            target: declared(group),
        })
    };
    // (a) an earlier other-player slot.
    let a = run(
        chain(vec![node(pick_player()), declaring(G), draw(G)]),
        &[Pl(P2), Pl(P1), C(0)],
        false,
        false,
    );
    assert_eq!(a.hands, [0, 1, 0], "P1 draws, not the earlier slot's P2");
    assert_eq!(a.first_slot_count, 3, "the reader adds no target slot");
    // (b) an earlier object slot.
    let b = run(
        chain(vec![
            node(Effect::TargetOnly {
                target: creature_filter(),
            }),
            declaring(G),
            draw(G),
        ]),
        &[C(1), Pl(P1), C(0)],
        false,
        false,
    );
    assert_eq!(b.hands, [0, 1, 0]);
    assert_eq!(b.first_slot_count, 3);
    // (d) two declaring nodes, each reader acts on its own player.
    let d = run(
        chain(vec![
            declaring(G),
            declaring(G2),
            node(Effect::Draw {
                count: q(1),
                target: declared(G),
            }),
            node(Effect::Draw {
                count: q(2),
                target: declared(G2),
            }),
        ]),
        &[Pl(P1), Pl(P2), C(0)],
        false,
        false,
    );
    assert_eq!(d.hands, [0, 1, 2], "P1 draws 1 and P2 draws 2");
    assert_eq!(d.first_slot_count, 3);
}

/// One class per row of the "no player" dispositions: the same chain resolves with P1 legal and
/// with P1 eliminated while the spell is on the stack. `observe` reads the class's own outcome.
fn legal_and_illegal(reader: Effect, hand_cards: bool) -> (Run, Run) {
    let def = || {
        chain(vec![
            node(pick_player()),
            declaring(G),
            node(reader.clone()),
        ])
    };
    let picks = [Pl(P2), Pl(P1), C(0)];
    (
        run(def(), &picks, false, hand_cards),
        run(def(), &picks, true, hand_cards),
    )
}

#[test]
fn illegal_declared_player_skips_a_draw() {
    let (legal, illegal) = legal_and_illegal(
        Effect::Draw {
            count: q(1),
            target: declared(G),
        },
        false,
    );
    assert_eq!(legal.hands, [0, 1, 0], "reach guard: the legal run draws");
    assert_eq!(legal.counters, 1);
    assert_eq!(illegal.hands, [0, 0, 0], "no one draws");
    assert_eq!(
        illegal.counters, 1,
        "the chain's later effect still happens"
    );
}

#[test]
fn illegal_declared_player_is_dealt_no_damage_and_nothing_inherited_is() {
    let (legal, illegal) = legal_and_illegal(
        Effect::DealDamage {
            amount: q(3),
            target: declared(G),
            damage_source: None,
            excess: None,
        },
        false,
    );
    assert_eq!(legal.lives, [20, 17, 20], "reach guard: P1 is dealt 3");
    assert_eq!(
        illegal.lives,
        [20, 20, 20],
        "the earlier P2 slot is not dealt it"
    );
    assert_eq!(illegal.counters, 1);
}

#[test]
fn illegal_declared_player_gets_no_counter() {
    let (legal, illegal) = legal_and_illegal(
        Effect::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Poison,
            count: q(1),
            target: declared(G),
        },
        false,
    );
    assert_eq!(legal.poison, [0, 1, 0], "reach guard: P1 is poisoned");
    assert_eq!(illegal.poison, [0, 0, 0]);
    assert_eq!(illegal.counters, 1);
}

#[test]
fn illegal_declared_player_searches_no_library() {
    let (legal, illegal) = legal_and_illegal(
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Any,
            count: q(1),
            reveal: false,
            target_player: Some(declared(G)),
            selection_constraint: Default::default(),
            split: None,
        },
        false,
    );
    assert_eq!(
        legal.searches,
        [(P1, Some(P1))],
        "reach guard: P1 searches their own library"
    );
    assert_eq!(
        illegal.searches,
        [],
        "no search, and not the caster's library"
    );
    assert_eq!(illegal.counters, 1);
}

#[test]
fn illegal_declared_player_gets_no_damage_shield() {
    let (legal, illegal) = legal_and_illegal(
        Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: declared(G),
            recipient_scope: EffectScope::Single,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration: None,
        },
        false,
    );
    assert_eq!(
        legal.shields, 1,
        "reach guard: a player-scoped shield is installed"
    );
    assert_eq!(
        illegal.shields, 0,
        "a shield with no recipient would protect everyone"
    );
    assert_eq!(illegal.counters, 1);
}

#[test]
fn illegal_declared_player_discards_nothing() {
    let (legal, illegal) = legal_and_illegal(
        Effect::Discard {
            count: q(1),
            target: declared(G),
            selection: Default::default(),
            unless_filter: None,
            filter: None,
        },
        true,
    );
    assert_eq!(
        legal.hands,
        [1, 0, 1],
        "reach guard: P1 discards their card"
    );
    assert_eq!(illegal.hands, [1, 1, 1], "no one discards");
    assert_eq!(illegal.counters, 1);
}

#[test]
fn each_target_damage_to_a_declared_player() {
    let each_target = Node {
        multi: Some(MultiTargetSpec::fixed(2, 2)),
        ..node(Effect::TargetOnly {
            target: creature_filter(),
        })
    };
    let damage = node(Effect::DealDamage {
        amount: QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Target,
            },
        },
        target: declared(G),
        damage_source: Some(DamageSource::EachTarget),
        excess: None,
    });
    let def = chain(vec![declaring(G), each_target, damage]);
    // Three distinct creatures fill the source and recipient slots; fewer would make the
    // illegal run's assertion vacuous. The last pick is the reach guard's creature.
    let picks = [Pl(P1), C(0), C(1), C(2), C(0)];
    let legal = run(def.clone(), &picks, false, false);
    assert_eq!(legal.first_slot_count, 5);
    assert_eq!(
        legal.lives,
        [20, 11, 20],
        "reach guard: powers 3 + 4 + 2 land on P1"
    );
    assert_eq!(legal.damage, [0, 0, 0], "no creature is the recipient");
    let illegal = run(def, &picks, true, false);
    assert_eq!(illegal.lives, [20, 20, 20]);
    assert_eq!(
        illegal.damage,
        [0, 0, 0],
        "no creature stands in as the recipient"
    );
}

#[test]
fn unnamed_group_and_tagged_node_without_a_player_name_no_one() {
    let draw = |group| {
        node(Effect::Draw {
            count: q(1),
            target: declared(group),
        })
    };
    // (i) no node carries G2.
    let missing = run(
        chain(vec![node(pick_player()), declaring(G), draw(G2), draw(G)]),
        &[Pl(P2), Pl(P1), C(0)],
        false,
        false,
    );
    assert_eq!(
        missing.hands,
        [0, 1, 0],
        "only the G reader draws (reach guard); the G2 reader acts on no one"
    );
    // (ii) the tagged node announced only an object.
    let tagged_object = Node {
        group: Some(G),
        ..node(Effect::TargetOnly {
            target: creature_filter(),
        })
    };
    let object_only = run(
        chain(vec![node(pick_player()), tagged_object, draw(G)]),
        &[Pl(P2), C(1), C(0)],
        false,
        false,
    );
    assert_eq!(object_only.hands, [0, 0, 0]);
    assert_eq!(object_only.counters, 1, "reach guard: the cast resolved");
}

#[test]
fn declared_player_serializes_as_a_tagged_leaf() {
    let json = serde_json::to_value(declared(G)).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!({"type": "DeclaredPlayer", "group": ChosenGroupId::DECLARED_PLAYER_BASE})
    );
    let back: TargetFilter = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, declared(G));
    let old: TargetFilter =
        serde_json::from_str(r#"{"type":"ParentTargetSlot","index":0}"#).expect("old shape");
    assert_eq!(old, TargetFilter::ParentTargetSlot { index: 0 });
}

fn pending_chain(tag: Option<ChosenGroupId>) -> (GameState, StackEntry) {
    let mut state = GameState::new_two_player(42);
    let source = ObjectId(900);
    let mut declaring_node = ResolvedAbility::new(
        pick_player(),
        vec![TargetRef::Player(P1)],
        source,
        PlayerId(0),
    );
    declaring_node.declares_chosen_group = tag;
    let root = declaring_node.sub_ability(ResolvedAbility::new(
        Effect::Mill {
            count: q(1),
            target: declared(G),
            destination: Zone::Graveyard,
        },
        vec![],
        source,
        PlayerId(0),
    ));
    let entry = StackEntry {
        id: source,
        source_id: source,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            ability: Some(Box::new(root)),
            card_id: CardId(900),
            casting_variant: Default::default(),
            actual_mana_spent: 0,
        },
    };
    state.stack.push_back(entry.clone());
    (state, entry)
}

#[test]
fn pending_node_reach_reads_the_declared_player() {
    let acted_on = |tag| {
        let (state, entry) = pending_chain(tag);
        stack_entry_node_reach(&state, &entry)[1].acted_on.clone()
    };
    assert_eq!(
        acted_on(Some(G)),
        vec![TargetRef::Player(P1)],
        "reach guard: the tagged node's player is mill's recipient"
    );
    assert_eq!(acted_on(None), Vec::<TargetRef>::new());
}
