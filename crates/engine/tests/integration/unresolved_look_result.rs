//! A look/reveal whose declared player is unavailable, or that finds nothing, replaces the chain's
//! result with an empty one (CR 608.2c + CR 609.3): dependents reached through the chain hand-off
//! act on nothing instead of an earlier producer's cards or their own source.
//! Chain: pick (declares P1) -> R1 RevealTop{Controller} -> producer -> [hop] -> dependent ->
//! SequentialSibling PutCounter guard. P1 is eliminated mid-stack in the "missing player" runs.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::*;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const G: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE);

fn q(v: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value: v }
}

fn declared() -> TargetFilter {
    TargetFilter::DeclaredPlayer { group: G }
}

fn reveal_top(player: TargetFilter) -> Effect {
    Effect::RevealTop { player, count: 1 }
}

fn dig(keep: u32, count: i32, source: DigSource) -> Effect {
    Effect::Dig {
        player: declared(),
        count: q(count),
        destination: None,
        keep_count: Some(keep),
        keep_count_expr: None,
        up_to: false,
        filter: TargetFilter::Any,
        rest_destination: None,
        rest_split_top_count: None,
        rest_order: DigRestOrder::Preserve,
        reveal: true,
        enter_tapped: false,
        enters_attacking: false,
        source,
    }
}

fn reveal_until(any_number: bool) -> Effect {
    Effect::RevealUntil {
        player: declared(),
        filter: TargetFilter::Any,
        count: q(1),
        matched_disposition: if any_number {
            RevealUntilDisposition::ChooseAnyNumber
        } else {
            RevealUntilDisposition::KeepEach
        },
        kept_destination: Zone::Hand,
        rest_destination: Zone::Graveyard,
        rest_order: DigRestOrder::Preserve,
        enter_tapped: Default::default(),
        enters_attacking: false,
        kept_optional_to: None,
        enters_under: None,
        kept_destination_if: None,
    }
}

fn to_hand(target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin: None,
        destination: Zone::Hand,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: Default::default(),
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

#[derive(Clone, Copy)]
enum Dependent {
    ParentTarget,
    LastRevealed,
    LoseTrackedSetSize,
}

impl Dependent {
    fn effect(self) -> Effect {
        match self {
            Dependent::ParentTarget => to_hand(TargetFilter::ParentTarget),
            Dependent::LastRevealed => to_hand(TargetFilter::LastRevealed),
            Dependent::LoseTrackedSetSize => Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                target: Some(TargetFilter::Controller),
            },
        }
    }
}

struct Case {
    producer: Effect,
    dependent: Dependent,
    /// Eliminate the declared player while the spell is on the stack.
    eliminate_p1: bool,
    p1_library_empty: bool,
    /// A prior `RevealTop{Controller}` that leaves a stale result behind.
    with_r1: bool,
    /// An intermediate hop between the producer and the dependent; `true` = SequentialSibling.
    hop: Option<bool>,
}

impl Case {
    fn new(producer: Effect, dependent: Dependent) -> Self {
        Self {
            producer,
            dependent,
            eliminate_p1: false,
            p1_library_empty: false,
            with_r1: true,
            hop: None,
        }
    }
    fn eliminated(mut self) -> Self {
        self.eliminate_p1 = true;
        self
    }
    fn empty_library(mut self) -> Self {
        self.p1_library_empty = true;
        self
    }
    fn no_r1(mut self) -> Self {
        self.with_r1 = false;
        self
    }
    fn hop(mut self, sequential: bool) -> Self {
        self.hop = Some(sequential);
        self
    }
}

struct Outcome {
    p0_life: i32,
    p0_hand: Vec<String>,
    p1_hand: Vec<String>,
    spell_zone: Zone,
    last_revealed: Vec<String>,
    guard_counters: Option<u32>,
}

fn run(case: Case) -> Outcome {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let creature = sc.add_creature(P0, "C0", 3, 9).id();
    for p in [P0, P1] {
        if p == P1 && case.p1_library_empty {
            continue;
        }
        for i in 0..4 {
            sc.add_card_to_library_top(p, &format!("L{}-{i}", p.0));
        }
    }
    let mut guard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: q(1),
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        },
    );
    guard.sub_link = SubAbilityLink::SequentialSibling;
    let mut tail = AbilityDefinition::new(AbilityKind::Spell, case.dependent.effect());
    tail.sub_ability = Some(Box::new(guard));
    let below = match case.hop {
        None => tail,
        Some(sequential) => {
            let mut hop = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::Demonstrative,
                        },
                    },
                    target: Some(TargetFilter::Controller),
                },
            );
            tail.sub_link = if sequential {
                SubAbilityLink::SequentialSibling
            } else {
                SubAbilityLink::ContinuationStep
            };
            hop.sub_ability = Some(Box::new(tail));
            hop
        }
    };
    let mut producer = AbilityDefinition::new(AbilityKind::Spell, case.producer);
    producer.sub_ability = Some(Box::new(below));
    let chain = if case.with_r1 {
        producer.sub_link = SubAbilityLink::ContinuationStep;
        let mut r1 =
            AbilityDefinition::new(AbilityKind::Spell, reveal_top(TargetFilter::Controller));
        r1.sub_ability = Some(Box::new(producer));
        r1
    } else {
        producer.sub_link = SubAbilityLink::SequentialSibling;
        producer
    };
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Player,
        },
    );
    let mut chain = chain;
    chain.sub_link = SubAbilityLink::SequentialSibling;
    root.sub_ability = Some(Box::new(chain));
    root.declares_chosen_group = Some(G);
    let spell = sc
        .add_spell_to_hand(P0, "Probe", false)
        .with_ability_definition(root)
        .id();
    let mut r: GameRunner = sc.build();
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
    let mut eliminate = case.eliminate_p1;
    for _ in 0..40 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let t = if selection.current_slot == 0 {
                    TargetRef::Player(P1)
                } else {
                    TargetRef::Object(creature)
                };
                r.act(GameAction::ChooseTarget { target: Some(t) })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if std::mem::take(&mut eliminate) {
                    r.state_mut().players[1].is_eliminated = true;
                }
                r.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    let s = r.state();
    let names = |ids: &[ObjectId]| -> Vec<String> {
        ids.iter().map(|i| s.objects[i].name.clone()).collect()
    };
    let hand = |p: usize| -> Vec<ObjectId> { s.players[p].hand.iter().copied().collect() };
    Outcome {
        p0_life: s.players[0].life,
        p0_hand: names(&hand(0)),
        p1_hand: names(&hand(1)),
        spell_zone: s.objects[&spell].zone,
        last_revealed: names(&s.last_revealed_ids),
        guard_counters: s.objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
    }
}

/// The dependents act on nothing, the spell does not move itself, and the independent guard ran.
fn acted_on_nothing(o: &Outcome) {
    assert!(o.p0_hand.is_empty(), "P0 hand {:?}", o.p0_hand);
    assert!(o.p1_hand.is_empty(), "P1 hand {:?}", o.p1_hand);
    assert_eq!(o.p0_life, 20, "life");
    assert_ne!(o.spell_zone, Zone::Hand, "spell moved itself");
    assert!(
        o.last_revealed.is_empty(),
        "stale result {:?}",
        o.last_revealed
    );
    assert_eq!(o.guard_counters, Some(1), "guard did not run");
}

/// A `TrackedSetSize` reader after an unresolved producer reads an empty set.
fn read_empty_tracked_set(o: &Outcome) {
    assert_eq!(o.p0_life, 20, "stale tracked set was read");
    assert_eq!(o.guard_counters, Some(1), "guard did not run");
}

/// The legal declared player's top card moved to that player's hand.
fn moved_p1_top_card(o: &Outcome) {
    let p1_card = vec!["L1-3".to_string()];
    assert_eq!(o.p1_hand, p1_card, "P1's top card to P1's hand");
    assert!(o.p0_hand.is_empty(), "P0 hand {:?}", o.p0_hand);
    assert_eq!(o.last_revealed, p1_card, "last revealed");
    assert_eq!(o.guard_counters, Some(1), "guard did not run");
}

macro_rules! row {
    ($name:ident, $case:expr, $check:expr) => {
        #[test]
        fn $name() {
            $check(&run($case));
        }
    };
}

fn rt() -> Effect {
    reveal_top(declared())
}
fn peek() -> Effect {
    dig(0, 1, DigSource::Library)
}

// Unresolved producers (each is red when its producer's exits stop publishing an empty result).
row!(
    rt_missing_player,
    Case::new(rt(), Dependent::ParentTarget).eliminated(),
    acted_on_nothing
);
row!(
    rt_empty_library,
    Case::new(rt(), Dependent::ParentTarget).empty_library(),
    acted_on_nothing
);
row!(
    rt_missing_player_without_r1,
    Case::new(rt(), Dependent::ParentTarget)
        .eliminated()
        .no_r1(),
    acted_on_nothing
);
row!(
    dig_missing_player,
    Case::new(peek(), Dependent::ParentTarget).eliminated(),
    acted_on_nothing
);
row!(
    dig_empty_library,
    Case::new(peek(), Dependent::LastRevealed).empty_library(),
    acted_on_nothing
);
row!(
    dig_count_zero,
    Case::new(dig(0, 0, DigSource::Library), Dependent::LastRevealed),
    acted_on_nothing
);
row!(
    dig_prior_look_without_prior_look,
    Case::new(dig(0, 1, DigSource::PriorLook), Dependent::LastRevealed),
    acted_on_nothing
);
row!(
    dig_missing_player_without_r1,
    Case::new(peek(), Dependent::ParentTarget)
        .eliminated()
        .no_r1(),
    acted_on_nothing
);
row!(
    reveal_until_missing_player,
    Case::new(reveal_until(false), Dependent::LoseTrackedSetSize).eliminated(),
    read_empty_tracked_set
);
row!(
    reveal_until_empty_library,
    Case::new(reveal_until(false), Dependent::LoseTrackedSetSize).empty_library(),
    read_empty_tracked_set
);
row!(
    reveal_until_choose_any_number_empty_library,
    Case::new(reveal_until(true), Dependent::LoseTrackedSetSize).empty_library(),
    read_empty_tracked_set
);

// The verdict reaches a `ParentTarget` dependent behind an intermediate hop of either link kind.
row!(
    rt_empty_library_behind_continuation_hop,
    Case::new(rt(), Dependent::ParentTarget)
        .empty_library()
        .hop(false),
    acted_on_nothing
);
row!(
    rt_empty_library_behind_sequential_hop,
    Case::new(rt(), Dependent::ParentTarget)
        .empty_library()
        .hop(true),
    acted_on_nothing
);
row!(
    dig_empty_library_behind_continuation_hop,
    Case::new(peek(), Dependent::ParentTarget)
        .empty_library()
        .hop(false),
    acted_on_nothing
);
row!(
    rt_missing_player_behind_hop_without_r1,
    Case::new(rt(), Dependent::ParentTarget)
        .eliminated()
        .no_r1()
        .hop(true),
    acted_on_nothing
);
row!(
    dig_missing_player_behind_hop_without_r1,
    Case::new(peek(), Dependent::ParentTarget)
        .eliminated()
        .no_r1()
        .hop(true),
    acted_on_nothing
);
row!(
    dig_empty_library_behind_sequential_hop,
    Case::new(peek(), Dependent::ParentTarget)
        .empty_library()
        .hop(true),
    acted_on_nothing
);

// Legal controls: the same chains with a resolvable declared player.
row!(
    legal_rt,
    Case::new(rt(), Dependent::ParentTarget),
    moved_p1_top_card
);
row!(
    legal_dig_peek,
    Case::new(peek(), Dependent::ParentTarget),
    moved_p1_top_card
);
row!(
    legal_rt_behind_hop,
    Case::new(rt(), Dependent::ParentTarget).hop(true),
    moved_p1_top_card
);
row!(
    legal_dig_behind_hop,
    Case::new(peek(), Dependent::ParentTarget).hop(true),
    moved_p1_top_card
);
// RevealUntil keeps its hit itself, and a `LastRevealed` reader cannot be cast against it.
row!(
    legal_reveal_until_hit,
    Case::new(reveal_until(false), Dependent::LoseTrackedSetSize),
    moved_p1_top_card
);
row!(
    legal_rt_tracked_set_size,
    Case::new(rt(), Dependent::LoseTrackedSetSize),
    |o: &Outcome| assert_eq!(o.p0_life, 19, "tracked set is the one revealed card")
);
