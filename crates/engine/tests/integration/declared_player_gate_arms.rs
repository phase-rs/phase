//! Hand-built chains for the runtime arms that read a `DeclaredPlayer` referent across a
//! declined "if you do" gate and an optional instruction (CR 608.2b, CR 118.12).
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::*;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

fn group() -> ChosenGroupId {
    ChosenGroupId::declared_player(0)
}
fn q(v: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value: v }
}
fn declared() -> TargetFilter {
    TargetFilter::DeclaredPlayer { group: group() }
}
fn node(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}
fn gain(amount: i32, player: TargetFilter) -> Effect {
    Effect::GainLife {
        amount: q(amount),
        player,
    }
}
fn tagged(mut d: AbilityDefinition, group: ChosenGroupId) -> AbilityDefinition {
    d.declares_chosen_group = Some(group);
    d
}
fn pick() -> AbilityDefinition {
    tagged(
        node(Effect::TargetOnly {
            target: TargetFilter::Player,
        }),
        group(),
    )
}
/// A second legal target: a lone illegal target would fizzle the whole spell (CR 608.2b).
fn creature_pick() -> AbilityDefinition {
    node(Effect::TargetOnly {
        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
    })
}
fn optional_draw() -> AbilityDefinition {
    let mut d = node(Effect::Draw {
        count: q(1),
        target: TargetFilter::Controller,
    });
    d.optional = true;
    d
}
fn gate() -> AbilityDefinition {
    let mut d = node(gain(1, TargetFilter::Controller));
    d.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    d
}
/// Links `nodes` in printed order; the caster's guard (+5 life) ends every chain.
fn link(nodes: Vec<AbilityDefinition>) -> AbilityDefinition {
    nodes
        .into_iter()
        .rev()
        .fold(node(gain(5, TargetFilter::Controller)), |mut rest, nd| {
            rest.sub_link = SubAbilityLink::SequentialSibling;
            nd.sub_ability(rest)
        })
}

#[derive(Debug, Default)]
struct Out {
    lives: Vec<i32>,
    hands: Vec<usize>,
    prompts: Vec<PlayerId>,
}

/// `eliminate_p1` removes the declared player after announcement (CR 608.2b: illegal target).
fn run(def: AbilityDefinition, decline: bool, eliminate_p1: bool) -> Out {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let creature = sc.add_creature(P0, "C0", 3, 9).id();
    for p in [P0, P1, P2] {
        for i in 0..6 {
            sc.add_card_to_library_top(p, &format!("L{}-{i}", p.0));
        }
        for i in 0..2 {
            sc.add_card_to_hand(p, &format!("H{}-{i}", p.0));
        }
    }
    let spell = sc
        .add_spell_to_hand(P0, "Probe", false)
        .with_ability_definition(def)
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
    let mut out = Out::default();
    let mut eliminate = eliminate_p1;
    for _ in 0..60 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                selection,
                target_slots,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let target = if legal.iter().all(|t| matches!(t, TargetRef::Player(_))) {
                    TargetRef::Player(P1)
                } else {
                    TargetRef::Object(creature)
                };
                r.act(GameAction::ChooseTarget {
                    target: Some(target),
                })
                .expect("target");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                out.prompts.push(player);
                r.act(GameAction::DecideOptionalEffect { accept: !decline })
                    .expect("decide");
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if std::mem::take(&mut eliminate) {
                    r.state_mut().players[1].is_eliminated = true;
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    let s = r.state();
    out.lives = s.players.iter().map(|p| p.life).collect();
    out.hands = s.players.iter().map(|p| p.hand.len()).collect();
    out
}

/// Row 12 chain: optional draw, its "if you do" gate, then a survivor reading the declared player.
fn declined_gate_chain(declaring: Vec<AbilityDefinition>) -> AbilityDefinition {
    let mut nodes = declaring;
    nodes.extend([optional_draw(), gate(), node(gain(3, declared()))]);
    link(nodes)
}

/// CR 601.2c + CR 115.1a: the declared player is announced as the ability is put on the stack,
/// so an instruction after a declined gate still reaches them.
#[test]
fn survivor_after_a_declined_gate_acts_on_the_declared_player() {
    let def = || declined_gate_chain(vec![pick(), creature_pick()]);
    let legal = run(def(), true, false);
    assert_eq!(legal.prompts, vec![P0], "reach: the decline path was taken");
    assert_eq!(legal.lives[0], 25, "reach: the chain ran through the guard");
    assert_eq!(legal.lives, vec![25, 23, 20], "the survivor lands on P1");
    let illegal = run(def(), true, true);
    assert_eq!(
        illegal.prompts,
        vec![P0],
        "reach: the decline path was taken"
    );
    assert_eq!(
        illegal.lives,
        vec![25, 20, 20],
        "CR 608.2b: the survivor affects no one, the guard still lands"
    );
}

fn set_link(def: &mut AbilityDefinition, depth: usize, link: SubAbilityLink) {
    let mut cur = def;
    for _ in 0..depth {
        cur = cur.sub_ability.as_deref_mut().expect("depth within chain");
    }
    cur.sub_link = link;
}

/// C2.6: the declaring node is itself the gated node.
#[test]
fn declaring_node_that_is_the_gate_still_names_its_player() {
    let mut declaring_gate = gate();
    declaring_gate.effect = Box::new(gain(1, TargetFilter::Player));
    let def = link(vec![
        creature_pick(),
        optional_draw(),
        tagged(declaring_gate, group()),
        node(gain(3, declared())),
    ]);
    let out = run(def, true, false);
    assert_eq!(out.prompts, vec![P0], "reach: the decline path was taken");
    assert_eq!(out.lives, vec![25, 23, 20]);
}

/// C2.6: the declaring node is a resolution step of the gate, skipped with it.
#[test]
fn declaring_node_skipped_with_the_gate_still_names_its_player() {
    let mut def = link(vec![
        creature_pick(),
        optional_draw(),
        gate(),
        tagged(node(gain(1, TargetFilter::Player)), group()),
        node(gain(3, declared())),
    ]);
    set_link(&mut def, 3, SubAbilityLink::ContinuationStep);
    let out = run(def, true, false);
    assert_eq!(out.prompts, vec![P0], "reach: the decline path was taken");
    assert_eq!(out.lives, vec![25, 23, 20]);
}

fn optional_for_declared_player() -> AbilityDefinition {
    let mut d = node(Effect::Draw {
        count: q(1),
        target: declared(),
    });
    d.optional = true;
    d.optional_player = Some(declared());
    d
}

/// CR 608.2b + CR 608.2d: a legal declared player is asked; an illegal one is asked by no
/// one, and the instruction addressed to them does not happen.
#[test]
fn optional_instruction_is_offered_to_the_declared_player_unless_illegal() {
    let def = || {
        link(vec![
            pick(),
            creature_pick(),
            optional_for_declared_player(),
        ])
    };
    let legal = run(def(), false, false);
    assert_eq!(legal.prompts, vec![P1], "the declared player is asked");
    assert_eq!(legal.hands[1], 3, "reach: P1 accepted and drew");
    let illegal = run(def(), false, true);
    assert_eq!(
        illegal.lives[0], 25,
        "reach: the chain ran through the guard"
    );
    assert_eq!(illegal.prompts, Vec::<PlayerId>::new(), "no one is asked");
    assert_eq!(illegal.hands, vec![2, 2, 2]);
}

/// CR 608.2c: a declaring node tagged in the declared-player range survives a declined gate
/// exactly as the untagged node does; an object-declaring `ClauseId` tag is still refused.
#[test]
fn tagged_declaring_node_survives_a_declined_gate_unless_object_declaring() {
    let chain = |tag: Option<ChosenGroupId>| {
        let declaring = node(gain(3, TargetFilter::Player));
        link(vec![
            optional_draw(),
            gate(),
            tag.map_or(declaring.clone(), |tag| tagged(declaring, tag)),
            creature_pick(),
        ])
    };
    let untagged = run(chain(None), true, false);
    assert_eq!(
        untagged.lives,
        vec![25, 23, 20],
        "reference: the untagged node survives"
    );
    let declared_range = run(chain(Some(group())), true, false);
    assert_eq!(
        declared_range.prompts,
        vec![P0],
        "reach: the decline path was taken"
    );
    assert_eq!(
        declared_range.lives,
        vec![25, 23, 20],
        "the tagged node's effect lands"
    );
    let clause_range = run(chain(Some(ChosenGroupId(1))), true, false);
    assert_eq!(
        clause_range.prompts,
        vec![P0],
        "reach: the decline path was taken"
    );
    assert_eq!(
        clause_range.lives,
        vec![25, 20, 20],
        "an object-declaring tag still refuses"
    );
}

#[test]
fn declared_player_range_is_one_authority() {
    assert!(group().is_declared_player());
    assert!(ChosenGroupId::declared_player(7).is_declared_player());
    assert!(!ChosenGroupId(1).is_declared_player());
    assert!(!ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE - 1).is_declared_player());
}
