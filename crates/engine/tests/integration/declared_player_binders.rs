//! Binding sites and the unless-payer read `DeclaredPlayer` (CR 608.2c + CR 608.2b), and the
//! group lookup numbers declared slots as the illegal-slot stamp does. Chains are hand-built.
//! Players: caster P0, declared P1, bystander P2; P1 (or the node's player) is eliminated while
//! the spell is on the stack in the illegal runs.

use engine::game::effects::resolve_ability_chain;
use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::*;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const P2: PlayerId = PlayerId(2);
const G: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE);

fn q(v: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value: v }
}
fn declared() -> TargetFilter {
    TargetFilter::DeclaredPlayer { group: G }
}
fn creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))
}

struct N {
    effect: Effect,
    group: Option<ChosenGroupId>,
    unless: Option<UnlessPayModifier>,
}
fn n(effect: Effect) -> N {
    N {
        effect,
        group: None,
        unless: None,
    }
}
fn pick() -> Effect {
    Effect::TargetOnly {
        target: TargetFilter::Player,
    }
}
fn decl() -> N {
    N {
        group: Some(G),
        ..n(pick())
    }
}
fn chain(nodes: Vec<N>) -> AbilityDefinition {
    let guard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: q(1),
            target: creature(),
        },
    );
    nodes.into_iter().rev().fold(guard, |mut rest, nd| {
        rest.sub_link = SubAbilityLink::SequentialSibling;
        let mut d = AbilityDefinition::new(AbilityKind::Spell, nd.effect).sub_ability(rest);
        d.declares_chosen_group = nd.group;
        d.unless_pay = nd.unless;
        d
    })
}

#[derive(Clone, Copy)]
enum Pick {
    Player(PlayerId),
    Victim,
}

#[derive(Default, Debug)]
struct Out {
    hands: Vec<usize>,
    lives: Vec<i32>,
    phased: Vec<bool>,
    counters: u32,
    victim_exiled: bool,
    unless_prompt: Vec<PlayerId>,
    transients: Vec<TargetFilter>,
    token_hosts: Vec<Option<AttachTarget>>,
}

fn run(def: AbilityDefinition, picks: &[Pick], elim: bool) -> Out {
    run_k(def, picks, elim.then_some(1), false)
}

fn run_k(def: AbilityDefinition, picks: &[Pick], elim: Option<usize>, kicked: bool) -> Out {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let c = sc.add_creature(P0, "C0", 3, 9).id();
    let victim = sc.add_creature(P2, "Victim", 2, 2).id();
    for p in [P0, P1, P2] {
        for i in 0..6 {
            sc.add_card_to_library_top(p, &format!("L{}-{i}", p.0));
        }
    }
    if kicked {
        sc.with_mana_pool(
            P0,
            vec![engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
    }
    let mut sp = sc.add_spell_to_hand(P0, "Probe", false);
    if kicked {
        sp.with_mana_cost(ManaCost::zero())
            .with_additional_cost(AdditionalCost::Kicker {
                costs: vec![AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                }],
                repeatability: AdditionalCostRepeatability::Once,
            });
    }
    let spell = sp.with_ability_definition(def).id();
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
    let mut elim = elim;
    for _ in 0..60 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let t = match picks.get(selection.current_slot) {
                    Some(Pick::Player(p)) => TargetRef::Player(*p),
                    Some(Pick::Victim) => TargetRef::Object(victim),
                    _ => TargetRef::Object(c),
                };
                r.act(GameAction::ChooseTarget { target: Some(t) })
                    .expect("t");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                r.act(GameAction::DecideOptionalCost { pay: true })
                    .expect("kick");
            }
            WaitingFor::ManaPayment { .. } => {
                r.act(GameAction::PassPriority).expect("mana");
            }
            WaitingFor::UnlessPayment { player, .. } => {
                out.unless_prompt.push(player);
                r.act(GameAction::PayUnlessCost { pay: false })
                    .expect("unless");
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if let Some(i) = elim.take() {
                    r.state_mut().players[i].is_eliminated = true;
                }
                r.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let s = r.state();
    out.hands = s.players.iter().map(|p| p.hand.len()).collect();
    out.lives = s.players.iter().map(|p| p.life).collect();
    out.phased = s.players.iter().map(|p| p.is_phased_out()).collect();
    out.victim_exiled = s.objects[&victim].zone == Zone::Exile;
    out.counters = s.objects[&c]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    out.token_hosts = s
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .map(|o| o.attached_to)
        .collect();
    out.transients = s
        .transient_continuous_effects
        .iter()
        .map(|e| e.affected.clone())
        .collect();
    out
}

/// The declaring node first, then a second player slot, so a reader inheriting its parent's
/// targets holds the bystander rather than the declared player.
const PICKS_A: [Pick; 2] = [Pick::Player(P1), Pick::Player(P2)];

fn reader_chain(reader: Effect) -> AbilityDefinition {
    chain(vec![decl(), n(pick()), n(reader)])
}

/// `collect_player_targets` (phase_out) binds the declared player, not the parent's.
#[test]
fn phase_out_binds_the_declared_player() {
    for (label, filter) in [
        ("DeclaredPlayer", declared()),
        (
            "ParentTargetSlot control",
            TargetFilter::ParentTargetSlot { index: 0 },
        ),
    ] {
        let def = || {
            reader_chain(Effect::PhaseOut {
                target: filter.clone(),
            })
        };
        let legal = run(def(), &PICKS_A, false);
        assert_eq!(
            legal.phased,
            [false, true, false],
            "{label}: only the declared player"
        );
        let illegal = run(def(), &PICKS_A, true);
        assert_eq!(illegal.phased, [false, false, false], "{label}: no one");
        assert_eq!(illegal.counters, 1, "{label}: reach guard");
    }
}

/// The transient-effect player binding.
#[test]
fn transient_effect_binds_the_declared_player() {
    for (label, filter, slot) in [
        ("DeclaredPlayer", declared(), None),
        // The carried bystander target must not displace the declared group.
        ("DeclaredPlayer targeted", declared(), Some(declared())),
        (
            "ParentTargetSlot control",
            TargetFilter::ParentTargetSlot { index: 0 },
            None,
        ),
    ] {
        let sd = StaticDefinition::continuous()
            .affected(filter)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        let def = || {
            reader_chain(Effect::GenericEffect {
                static_abilities: vec![sd.clone()],
                duration: Some(Duration::UntilEndOfTurn),
                target: slot.clone(),
                end_cost: None,
            })
        };
        let bound = TargetFilter::SpecificPlayer { id: P1 };
        assert_eq!(run(def(), &PICKS_A, false).transients, [bound], "{label}");
        let illegal = run(def(), &PICKS_A, true);
        assert_eq!(
            illegal.transients,
            Vec::<TargetFilter>::new(),
            "{label}: no one"
        );
        assert_eq!(illegal.counters, 1, "{label}: reach guard");
    }
}

/// A pending stack entry (no resolution carrier) reads the declared player too.
#[test]
fn pending_stack_reach_reads_the_declared_player() {
    let state = GameState::new_two_player(42);
    let source = ObjectId(900);
    let acted_on = |filter: TargetFilter| {
        let mut declaring = ResolvedAbility::new(pick(), vec![TargetRef::Player(P1)], source, P0);
        declaring.declares_chosen_group = Some(G);
        let later = ResolvedAbility::new(pick(), vec![TargetRef::Player(P0)], source, P0);
        let reader = ResolvedAbility::new(Effect::PhaseOut { target: filter }, vec![], source, P0);
        let root = declaring.sub_ability(later.sub_ability(reader));
        let entry = StackEntry {
            id: source,
            source_id: source,
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(root)),
                card_id: CardId(900),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        };
        let mut state = state.clone();
        state.stack.push_back(entry.clone());
        stack_entry_node_reach(&state, &entry)[2].acted_on.clone()
    };
    let slot0 = acted_on(TargetFilter::ParentTargetSlot { index: 0 });
    assert_eq!(
        slot0,
        [TargetRef::Player(P1)],
        "reach guard: the control reads slot 0"
    );
    assert_eq!(acted_on(declared()), slot0);
}

fn unless_chain(payer: TargetFilter) -> AbilityDefinition {
    let mut gated = n(Effect::Draw {
        count: q(1),
        target: TargetFilter::Controller,
    });
    gated.unless = Some(UnlessPayModifier {
        cost: AbilityCost::Mana {
            cost: ManaCost::generic(1),
        },
        payer,
    });
    chain(vec![decl(), n(pick()), gated])
}

/// The declared payer is prompted, and its decision gates the effect (CR 118.12a).
#[test]
fn unless_payer_is_the_declared_player() {
    let declined = run(unless_chain(declared()), &PICKS_A, false);
    assert_eq!(
        declined.unless_prompt,
        [P1],
        "the declared player, not the bystander, is asked"
    );
    assert_eq!(declined.hands[0], 1, "declined: the gated draw applies");
    assert_eq!(declined.counters, 1, "reach guard");
}

/// Illegal leg: no prompt reaches the eliminated declared player; the gated effect on the
/// caster still applies.
#[test]
fn illegal_declared_payer_is_not_prompted() {
    let out = run(unless_chain(declared()), &PICKS_A, true);
    assert_eq!(out.unless_prompt, Vec::<PlayerId>::new());
    assert_eq!(out.hands[0], 1, "the gated draw applies to the caster");
    assert_eq!(out.counters, 1, "reach guard");
}

fn exile_head() -> Effect {
    Effect::ChangeZone {
        origin: None,
        destination: Zone::Exile,
        target: TargetFilter::Typed(TypedFilter::creature()),
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

/// The inheriting life-gain rider ("its controller gains life equal to its power").
fn rider() -> Effect {
    Effect::GainLife {
        amount: QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Target,
            },
        },
        player: TargetFilter::ParentTargetController,
    }
}

#[derive(Clone, Copy, Debug)]
enum Before {
    Plain,
    Rider,
    Delegator,
}

/// head [victim] -> (rider [victim snapshot]) -> tagged [P1] -> (later [P2]) -> Draw(declared),
/// or, for `Delegator`, a kicked spell whose "instead" node announces the victim.
fn chain_after(before: Before, with_later: bool) -> AbilityDefinition {
    let tail = |mut nodes: Vec<N>| {
        nodes.push(decl());
        if with_later {
            nodes.push(n(pick()));
        }
        nodes.push(n(Effect::Draw {
            count: q(1),
            target: declared(),
        }));
        chain(nodes)
    };
    match before {
        Before::Plain => tail(vec![n(exile_head())]),
        Before::Rider => tail(vec![n(exile_head()), n(rider())]),
        Before::Delegator => {
            let instead = AbilityDefinition::new(AbilityKind::Spell, exile_head())
                .condition(AbilityCondition::AdditionalCostPaidInstead)
                .sub_ability(tail(vec![]));
            AbilityDefinition::new(AbilityKind::Spell, exile_head()).sub_ability(instead)
        }
    }
}

fn run_after(before: Before, with_later: bool, eliminate: Option<usize>) -> Out {
    let picks: &[Pick] = if with_later {
        &[Pick::Victim, Pick::Player(P1), Pick::Player(P2)]
    } else {
        &[Pick::Victim, Pick::Player(P1)]
    };
    let kicked = matches!(before, Before::Delegator);
    let out = run_k(chain_after(before, with_later), picks, eliminate, kicked);
    assert!(
        out.victim_exiled,
        "{before:?}: the head's effect landed (reach guard)"
    );
    assert_eq!(out.counters, 1, "{before:?}: reach guard");
    out
}

/// The declared-group lookup numbers the tagged node's slot as the illegal-slot stamp
/// does, after an inheriting rider's snapshot or a paid "instead" delegator's mirror.
fn declared_slot_numbering(before: Before) {
    // (i) the declared player is illegal: the reader affects no one.
    let out = run_after(before, false, Some(1));
    assert_eq!(
        out.hands,
        [0, 0, 0],
        "{before:?}: illegal declared player is not read"
    );
    // (ii) the declared player is legal while a later node's player is illegal.
    let out = run_after(before, true, Some(2));
    assert_eq!(
        out.hands,
        [0, 1, 0],
        "{before:?}: a later illegal slot does not hide it"
    );
    // legal run: the reader acts on the declared player.
    let out = run_after(before, true, None);
    assert_eq!(out.hands, [0, 1, 0], "{before:?}: legal");
}

#[test]
fn plain_chain_numbering_matches_the_stamp() {
    declared_slot_numbering(Before::Plain);
}

#[test]
fn inheriting_rider_does_not_shift_the_declared_slot() {
    declared_slot_numbering(Before::Rider);
}

#[test]
fn paid_instead_delegator_does_not_shift_the_declared_slot() {
    declared_slot_numbering(Before::Delegator);
}

fn curse_token(attach_to: TargetFilter) -> Effect {
    Effect::Token {
        name: "Curse".to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types: vec![
            "Enchantment".to_string(),
            "Aura".to_string(),
            "Curse".to_string(),
        ],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: q(1),
        owner: TargetFilter::Controller,
        attach_to: Some(attach_to),
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// CR 303.4 + CR 608.2b: an Aura token attached to the declared player lands on that player; an
/// illegal declared player names no host, so CR 303.4i denies the token's entry.
#[test]
fn aura_token_attaches_to_the_declared_player() {
    let def = || chain(vec![decl(), n(pick()), n(curse_token(declared()))]);
    let legal = run(def(), &PICKS_A, false);
    assert_eq!(legal.token_hosts, [Some(AttachTarget::Player(P1))]);
    let illegal = run(def(), &PICKS_A, true);
    assert_eq!(illegal.token_hosts, Vec::<Option<AttachTarget>>::new());
    assert_eq!(illegal.counters, 1, "reach guard");
}

fn soldier_token(owner: TargetFilter) -> Effect {
    Effect::Token {
        name: "Soldier".to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: vec!["Creature".to_string(), "Soldier".to_string()],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: q(1),
        owner,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

fn pool_token(owner: TargetFilter) -> Effect {
    Effect::CreateTokenCopyFromPool {
        owner,
        type_filter: TargetFilter::Any,
        mv: Comparator::EQ,
        mv_bound: q(2),
        selection: CardSelectionMode::Random,
        count: q(1),
        tapped: false,
        enters_attacking: false,
    }
}

fn haste_to_last_created(source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::LastCreated)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            duration: Some(Duration::Permanent),
            target: Some(TargetFilter::LastCreated),
            end_cost: None,
        },
        vec![],
        source,
        P0,
    )
}

/// Resolves `producer` then a LastCreated haste grant over a stale ledger entry and returns
/// whether the stale token got haste and the ledger afterwards; `prevent_tokens` installs a
/// CR 614 replacement that stops all token creation.
fn producer_over_stale_ledger(producer: Effect, prevent_tokens: bool) -> (bool, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    let face = CardFace {
        name: "Pool Bear".to_string(),
        mana_cost: ManaCost::generic(2),
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        power: Some(PtValue::Fixed(2)),
        toughness: Some(PtValue::Fixed(2)),
        ..Default::default()
    };
    crate::support::install_synthetic_card_db(&mut state, &[face]);
    let stale = create_object(
        &mut state,
        CardId(1),
        P0,
        "Old Token".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&stale).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.is_token = true;
    }
    state.last_created_token_ids = vec![stale];
    let source = create_object(
        &mut state,
        CardId(2),
        P0,
        "Source".to_string(),
        Zone::Battlefield,
    );
    if prevent_tokens {
        let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::Prevent);
        let obj = state.objects.get_mut(&source).unwrap();
        obj.base_replacement_definitions = std::sync::Arc::new(vec![def.clone()]);
        obj.replacement_definitions = vec![def].into();
    }
    let root = ResolvedAbility::new(producer, vec![], source, P0)
        .sub_ability(haste_to_last_created(source));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &root, &mut events, 0).unwrap();
    engine::game::layers::evaluate_layers(&mut state);
    (
        state.objects[&stale].has_keyword(&Keyword::Haste),
        state.last_created_token_ids.clone(),
    )
}

/// CR 608.2c + CR 609.3: a token producer that creates nothing leaves nothing for a following
/// `LastCreated` reader — whether its owner is unresolved (CR 608.2b) or the creation is prevented.
#[test]
fn a_producer_that_creates_nothing_clears_the_last_created_ledger() {
    let unresolved = || TargetFilter::DeclaredPlayer {
        group: ChosenGroupId::declared_player(0),
    };
    for (label, make, prevent) in [
        ("Token", soldier_token as fn(TargetFilter) -> Effect, false),
        ("Token prevented", soldier_token, true),
        ("pool", pool_token, false),
    ] {
        // Reach: the same producer with a resolvable owner replaces the stale ledger entry,
        // or (prevented) is the case under test.
        let (stale_hasted, ledger) =
            producer_over_stale_ledger(make(TargetFilter::Controller), prevent);
        if !prevent {
            assert_eq!(ledger.len(), 1, "{label}: reach, a token was created");
            assert!(!stale_hasted, "{label}: reach, the stale token is replaced");
        }
        let owner = if prevent {
            TargetFilter::Controller
        } else {
            unresolved()
        };
        let (stale_hasted, ledger) = producer_over_stale_ledger(make(owner), prevent);
        assert!(ledger.is_empty(), "{label}: ledger cleared");
        assert!(!stale_hasted, "{label}: the stale token gets no grant");
    }
}
