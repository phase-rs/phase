//! A declaring clause whose declared player is illegal or was never announced affects no one
//! (CR 608.2b + CR 115.6), and a direct reader of that player resolves it through the one authority.
//! Three players: caster P0, declared player P1 (eliminated while the spell is on the stack in the
//! illegal runs), bystander P2.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::*;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const G: ChosenGroupId = ChosenGroupId(ChosenGroupId::DECLARED_PLAYER_BASE);
/// A pick that declines the slot ("up to" targets, zero chosen).
const DECLINE: PlayerId = PlayerId(99);

fn q(v: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value: v }
}

fn declared() -> TargetFilter {
    TargetFilter::DeclaredPlayer { group: G }
}

fn opponent() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
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

fn tagged(effect: Effect) -> Node {
    Node {
        group: Some(G),
        ..node(effect)
    }
}

fn pick() -> Node {
    node(Effect::TargetOnly {
        target: TargetFilter::Player,
    })
}

fn discard(target: TargetFilter) -> Effect {
    Effect::Discard {
        count: q(1),
        target,
        selection: Default::default(),
        unless_filter: None,
        filter: None,
    }
}

fn lose_life(target: TargetFilter) -> Effect {
    Effect::LoseLife {
        amount: q(3),
        target: Some(target),
    }
}

fn draw(target: TargetFilter) -> Effect {
    Effect::Draw {
        count: q(1),
        target,
    }
}

/// Sequential chain ending in a `PutCounter` guard on the caster's creature (`ctr == 1` proves the
/// chain ran to its last clause).
fn chain(nodes: Vec<Node>) -> AbilityDefinition {
    let guard = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: q(1),
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        },
    );
    nodes.into_iter().rev().fold(guard, |mut rest, nd| {
        rest.sub_link = SubAbilityLink::SequentialSibling;
        let mut def = AbilityDefinition::new(AbilityKind::Spell, nd.effect).sub_ability(rest);
        def.declares_chosen_group = nd.group;
        def.multi_target = nd.multi;
        def
    })
}

/// Everything one seat can be affected in by the effect classes under test.
#[derive(Clone, Debug, PartialEq, Default)]
struct Seat {
    hand: usize,
    library: usize,
    life: i32,
    poison: u32,
    turns_to_skip: u32,
    extra_turns: usize,
    loyalty: u32,
    step_skips: u32,
    monarch: bool,
    eliminated: bool,
    searched: usize,
    shuffled: usize,
    prompted: usize,
    revealed: usize,
}

#[derive(Debug)]
struct Out {
    seats: Vec<Seat>,
    extra_phases: usize,
    counters: u32,
}

#[derive(Clone, Default)]
struct Opts<'a> {
    picks: &'a [PlayerId],
    eliminate_p1: bool,
    extra_hand: usize,
    source_power: Option<i32>,
}

/// Seats whose observable state differs between `a` and `b`.
fn affected(a: &Out, b: &Out) -> Vec<usize> {
    (0..a.seats.len())
        .filter(|&i| a.seats[i] != b.seats[i])
        .collect()
}

fn run(def: AbilityDefinition, opts: &Opts) -> Out {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let creature = sc.add_creature(P0, "C0", 3, 9).id();
    for p in [P0, P1, P2] {
        for i in 0..6 {
            sc.add_card_to_library_top(p, &format!("L{}-{i}", p.0));
        }
        sc.add_card_to_hand(p, &format!("H{}", p.0));
        for k in 0..opts.extra_hand {
            sc.add_card_to_hand(p, &format!("HX{}-{k}", p.0));
        }
    }
    let spell = sc
        .add_spell_to_hand(P0, "Probe", false)
        .with_ability_definition(def)
        .id();
    let mut r: GameRunner = sc.build();
    if let Some(power) = opts.source_power {
        let obj = r.state_mut().objects.get_mut(&spell).expect("spell");
        obj.power = Some(power);
        obj.toughness = Some(power);
    }
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");

    let mut seats = vec![Seat::default(); 3];
    let mut eliminate = opts.eliminate_p1;
    for _ in 0..60 {
        match r.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let target = match opts.picks.get(selection.current_slot) {
                    Some(&p) if p == DECLINE => None,
                    Some(&p) => Some(TargetRef::Player(p)),
                    None => Some(TargetRef::Object(creature)),
                };
                r.act(GameAction::ChooseTarget { target }).expect("target");
            }
            WaitingFor::SearchChoice {
                player,
                library_owner,
                cards,
                ..
            } => {
                seats[library_owner.unwrap_or(player).0 as usize].searched += 1;
                r.act(GameAction::SelectCards {
                    cards: cards.into_iter().take(1).collect(),
                })
                .expect("search");
            }
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                seats[player.0 as usize].prompted += 1;
                r.act(GameAction::SelectCards {
                    cards: cards.into_iter().take(count).collect(),
                })
                .expect("discard");
            }
            WaitingFor::RevealChoice { cards, .. } => {
                for id in &cards {
                    seats[r.state().objects[id].owner.0 as usize].revealed += 1;
                }
                r.act(GameAction::SelectCards {
                    cards: vec![cards[0]],
                })
                .expect("reveal");
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                if std::mem::take(&mut eliminate) {
                    r.state_mut().players[1].is_eliminated = true;
                }
                let res = r.act(GameAction::PassPriority).expect("pass");
                for e in res.events {
                    if let GameEvent::PlayerPerformedAction {
                        player_id,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    } = e
                    {
                        seats[player_id.0 as usize].shuffled += 1;
                    }
                }
            }
            other => {
                if let Some(p) = other.acting_player() {
                    seats[p.0 as usize].prompted += 1;
                }
                break;
            }
        }
    }

    let s = r.state();
    for (i, seat) in seats.iter_mut().enumerate() {
        let p = &s.players[i];
        let id = PlayerId(i as u8);
        seat.hand = p.hand.len();
        seat.library = p.library.len();
        seat.life = p.life;
        seat.poison = p.poison_counters;
        seat.turns_to_skip = s.turns_to_skip.get(i).copied().unwrap_or(0);
        seat.extra_turns = s.extra_turns.iter().filter(|e| e.player == id).count();
        seat.loyalty = s
            .extra_loyalty_activations_this_turn
            .get(&id)
            .copied()
            .unwrap_or(0);
        seat.step_skips = s
            .steps_to_skip
            .get(i)
            .map_or(0, |m| m.values().copied().sum());
        seat.monarch = s.monarch == Some(id);
        seat.eliminated = p.is_eliminated;
        seat.revealed += s
            .last_revealed_ids
            .iter()
            .filter(|oid| s.objects.get(oid).is_some_and(|o| o.owner == id))
            .count();
    }
    Out {
        seats,
        extra_phases: s.extra_phases.len(),
        counters: s.objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
    }
}

/// Effect classes whose player slot is `f`, keyed by name. `AdditionalPhase` is gated on the
/// recipient being the active player, so its declared player is P0 rather than P1.
fn classes(f: TargetFilter) -> Vec<(&'static str, Effect)> {
    vec![
        ("Draw", draw(f.clone())),
        ("Discard", discard(f.clone())),
        ("LoseLife", lose_life(f.clone())),
        (
            "GainLife",
            Effect::GainLife {
                amount: q(3),
                player: f.clone(),
            },
        ),
        (
            "Mill",
            Effect::Mill {
                count: q(2),
                target: f.clone(),
                destination: Zone::Graveyard,
            },
        ),
        (
            "DealDamage",
            Effect::DealDamage {
                amount: q(3),
                target: f.clone(),
                damage_source: None,
                excess: None,
            },
        ),
        (
            "Poison",
            Effect::GivePlayerCounter {
                counter_kind: PlayerCounterKind::Poison,
                count: q(1),
                target: f.clone(),
            },
        ),
        (
            "Scry",
            Effect::Scry {
                count: q(1),
                target: f.clone(),
            },
        ),
        (
            "Surveil",
            Effect::Surveil {
                count: q(1),
                target: f.clone(),
            },
        ),
        (
            "Search",
            Effect::SearchLibrary {
                source_zones: vec![Zone::Library],
                filter: TargetFilter::Any,
                count: q(1),
                reveal: false,
                target_player: Some(f.clone()),
                selection_constraint: Default::default(),
                split: None,
            },
        ),
        ("Shuffle", Effect::Shuffle { target: f.clone() }),
        (
            "RevealHand",
            Effect::RevealHand {
                target: f.clone(),
                card_filter: TargetFilter::Any,
                count: None,
                selection: Default::default(),
                choice_optional: false,
                reveal: false,
            },
        ),
        (
            "RevealTop",
            Effect::RevealTop {
                player: f.clone(),
                count: 1,
            },
        ),
        (
            "ExtraTurn",
            Effect::ExtraTurn {
                target: f.clone(),
                count: q(1),
            },
        ),
        (
            "SkipNextTurn",
            Effect::SkipNextTurn {
                target: f.clone(),
                count: q(1),
            },
        ),
        (
            "SetLife",
            Effect::SetLifeTotal {
                target: f.clone(),
                amount: q(7),
            },
        ),
        (
            "SkipStep",
            Effect::SkipNextStep {
                target: f.clone(),
                step: StepSkipTarget::Step(Phase::Draw),
                count: q(1),
                scope: Default::default(),
            },
        ),
        (
            "DoubleLife",
            Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target: f.clone(),
            },
        ),
        (
            "LoseGame",
            Effect::LoseTheGame {
                target: Some(f.clone()),
            },
        ),
        ("Monarch", Effect::BecomeMonarch { target: f.clone() }),
        (
            "GrantLoyalty",
            Effect::GrantExtraLoyaltyActivations {
                amount: q(1),
                target: f.clone(),
            },
        ),
        (
            "AdditionalPhase",
            Effect::AdditionalPhase {
                recipient: ExtraPhaseRecipient::TargetedPlayer(f.clone()),
                segment: TurnSegment::Phase(PhaseGroup::PostcombatMain),
                after: ExtraPhaseAnchor::ThisPhase { named: None },
                followed_by: vec![],
                count: q(1),
                attacker_restriction: None,
            },
        ),
        (
            "RevealUntil",
            Effect::RevealUntil {
                player: f.clone(),
                filter: TargetFilter::Any,
                count: q(1),
                matched_disposition: Default::default(),
                kept_destination: Zone::Hand,
                rest_destination: Zone::Graveyard,
                rest_order: Default::default(),
                enter_tapped: Default::default(),
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
                kept_destination_if: None,
            },
        ),
        (
            "ExchangeLife",
            Effect::ExchangeLifeWithStat {
                stat: PtStat::Power,
                player: f,
            },
        ),
    ]
}

/// The declared player of `class` in the legal run: the active player for `AdditionalPhase`.
fn declared_seat(class: &str) -> PlayerId {
    if class == "AdditionalPhase" {
        P0
    } else {
        P1
    }
}

fn class_opts<'a>(class: &str, picks: &'a [PlayerId], eliminate_p1: bool) -> Opts<'a> {
    Opts {
        picks,
        eliminate_p1,
        source_power: (class == "ExchangeLife").then_some(5),
        ..Default::default()
    }
}

/// Runs `shape(effect)` legal and illegal against the no-op control `shape(TargetOnly)`, and
/// reports every class whose legal run does not affect exactly the declared player (the reach
/// guard) or whose illegal run affects anyone, or loses the `PutCounter` guard.
fn class_loop(
    filter: TargetFilter,
    noop: Effect,
    picks_for: impl Fn(&str) -> Vec<PlayerId>,
    shape: impl Fn(Effect) -> AbilityDefinition,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, eff) in classes(filter) {
        let declared = declared_seat(name);
        let picks = picks_for(name);
        let legal = run(shape(eff.clone()), &class_opts(name, &picks, false));
        let legal_ctl = run(shape(noop.clone()), &class_opts(name, &picks, false));
        let illegal_picks = if name == "AdditionalPhase" {
            picks_for("illegal")
        } else {
            picks.clone()
        };
        let illegal = run(shape(eff), &class_opts(name, &illegal_picks, true));
        let illegal_ctl = run(shape(noop.clone()), &class_opts(name, &illegal_picks, true));

        if name == "AdditionalPhase" {
            if legal.extra_phases != legal_ctl.extra_phases + 1 {
                failures.push(format!("{name}: legal run did not add one phase"));
            }
        } else if affected(&legal, &legal_ctl) != vec![declared.0 as usize] {
            failures.push(format!(
                "{name}: legal run affected seats {:?}, wanted only {declared:?}",
                affected(&legal, &legal_ctl)
            ));
        }
        let hit = affected(&illegal, &illegal_ctl);
        if !hit.is_empty() || illegal.extra_phases != illegal_ctl.extra_phases {
            failures.push(format!(
                "{name}: illegal run affected seats {hit:?} (extra_phases {} vs {})",
                illegal.extra_phases, illegal_ctl.extra_phases
            ));
        }
        if illegal.counters != 1 {
            failures.push(format!("{name}: illegal run did not reach the guard"));
        }
    }
    failures
}

/// `[pick(P2), tagged effect(Player)]`, the effect declares slot 1 = P1.
#[test]
fn declaring_player_clause_affects_no_one_when_its_player_is_illegal() {
    let failures = class_loop(
        TargetFilter::Player,
        Effect::TargetOnly {
            target: TargetFilter::Player,
        },
        |class| match class {
            "AdditionalPhase" => vec![P2, P0],
            _ => vec![P2, P1],
        },
        |eff| chain(vec![pick(), tagged(eff)]),
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A `DeclaredPlayer` reader acts on the declared player although another player slot
/// precedes or follows the declaring node.
#[test]
fn declared_player_reader_acts_on_the_declared_player_only() {
    let mut failures = class_loop(
        declared(),
        Effect::TargetOnly { target: declared() },
        |class| match class {
            "illegal" => vec![P1, P0],
            "AdditionalPhase" => vec![P0, P2],
            _ => vec![P1, P2],
        },
        |eff| chain(vec![tagged(pick().effect), pick(), node(eff)]),
    );
    failures.extend(class_loop(
        declared(),
        Effect::TargetOnly { target: declared() },
        |class| match class {
            "AdditionalPhase" => vec![P2, P0],
            _ => vec![P2, P1],
        },
        |eff| chain(vec![pick(), tagged(pick().effect), node(eff)]),
    ));
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The parser's "target opponent" filter shape is a declaring filter too.
#[test]
fn typed_opponent_declaring_clause_affects_no_one_when_its_player_is_illegal() {
    let mut failures = Vec::new();
    for (name, eff) in classes(opponent())
        .into_iter()
        .filter(|(n, _)| ["RevealHand", "Discard", "LoseLife", "ExtraTurn"].contains(n))
    {
        let picks = [P1];
        let shape = |e: Effect| chain(vec![tagged(e)]);
        let noop = Effect::TargetOnly { target: opponent() };
        let legal = run(shape(eff.clone()), &class_opts(name, &picks, false));
        let legal_ctl = run(shape(noop.clone()), &class_opts(name, &picks, false));
        let illegal = run(shape(eff), &class_opts(name, &picks, true));
        let illegal_ctl = run(shape(noop), &class_opts(name, &picks, true));
        if affected(&legal, &legal_ctl) != vec![1] {
            failures.push(format!(
                "{name}: legal run affected {:?}",
                affected(&legal, &legal_ctl)
            ));
        }
        let hit = affected(&illegal, &illegal_ctl);
        if !hit.is_empty() || illegal.counters != 1 {
            failures.push(format!(
                "{name}: illegal run affected {hit:?}, ctr {}",
                illegal.counters
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Effects with no declared slot keep their caster fallback.
#[test]
fn caster_fallbacks_are_preserved() {
    let noop = || {
        chain(vec![node(Effect::TargetOnly {
            target: TargetFilter::Controller,
        })])
    };
    let base = run(noop(), &Opts::default());
    for (name, eff) in [
        ("Draw(Controller)", draw(TargetFilter::Controller)),
        ("Discard(Any)", discard(TargetFilter::Any)),
        (
            "LoseLife(None)",
            Effect::LoseLife {
                amount: q(3),
                target: None,
            },
        ),
    ] {
        let out = run(chain(vec![node(eff)]), &Opts::default());
        assert_eq!(affected(&out, &base), vec![0], "{name} affects the caster");
        assert_eq!(out.counters, 1, "{name} ran to the guard");
    }
}

/// The declaring `Player` sub does not inherit the parent's announced player.
#[test]
fn tagged_declaring_sub_does_not_inherit_the_parents_player() {
    let shape = || {
        chain(vec![
            node(draw(TargetFilter::Player)),
            tagged(lose_life(TargetFilter::Player)),
        ])
    };
    let legal = run(
        shape(),
        &Opts {
            picks: &[P2, P1],
            ..Default::default()
        },
    );
    assert_eq!(legal.seats[1].life, 17, "the declared player loses");
    assert_eq!(legal.seats[2].life, 20);
    let illegal = run(
        shape(),
        &Opts {
            picks: &[P2, P1],
            eliminate_p1: true,
            ..Default::default()
        },
    );
    assert_eq!(illegal.seats[2].hand, 2, "the parent's draw reached P2");
    assert_eq!(
        illegal.seats.iter().map(|s| s.life).collect::<Vec<_>>(),
        vec![20, 20, 20],
        "nobody loses life"
    );
    assert_eq!(illegal.counters, 1);
}

/// The illegal-declared-player refusal through the paused-parent entry. The root's
/// discard parks a `DiscardChoice`, so the sub is cloned in the generic paused-parent branch.
#[test]
fn tagged_declaring_sub_does_not_inherit_through_a_paused_parent() {
    let shape = || {
        chain(vec![
            node(discard(TargetFilter::Player)),
            tagged(lose_life(TargetFilter::Player)),
        ])
    };
    let opts = |eliminate_p1| Opts {
        picks: &[P2, P1],
        eliminate_p1,
        extra_hand: 1,
        ..Default::default()
    };
    let legal = run(shape(), &opts(false));
    assert_eq!(legal.seats[2].hand, 1, "the parent's discard reached P2");
    assert_eq!(legal.seats[1].life, 17, "the declared player loses");
    assert_eq!(legal.seats[2].life, 20);
    let illegal = run(shape(), &opts(true));
    assert_eq!(illegal.seats[2].hand, 1, "the parent's discard reached P2");
    assert_eq!(
        illegal.seats.iter().map(|s| s.life).collect::<Vec<_>>(),
        vec![20, 20, 20],
        "nobody loses life"
    );
    assert_eq!(illegal.counters, 1);
}

/// The never-announced refusal through the paused-parent entry. The sub's "up to one
/// target player" is declined, so only the declaring-group gate keeps the parent's player out.
#[test]
fn declined_declaring_sub_does_not_inherit_through_a_paused_parent() {
    let mut sub = tagged(lose_life(TargetFilter::Player));
    sub.multi = Some(MultiTargetSpec::up_to(q(1)));
    let out = run(
        chain(vec![node(discard(TargetFilter::Player)), sub]),
        &Opts {
            picks: &[P2, DECLINE],
            extra_hand: 1,
            ..Default::default()
        },
    );
    assert_eq!(out.seats[2].hand, 1, "the parent's discard reached P2");
    assert_eq!(
        out.seats.iter().map(|s| s.life).collect::<Vec<_>>(),
        vec![20, 20, 20],
        "no one loses life"
    );
    assert_eq!(out.counters, 1);
}

/// A declaring root clause whose "up to one target player" is declined affects no one.
#[test]
fn declined_root_declaring_clause_affects_no_one() {
    let shape = || {
        let mut def = chain(vec![tagged(draw(TargetFilter::Player))]);
        def.multi_target = Some(MultiTargetSpec::up_to(q(1)));
        def
    };
    let announced = run(
        shape(),
        &Opts {
            picks: &[P1],
            ..Default::default()
        },
    );
    let declined = run(
        shape(),
        &Opts {
            picks: &[DECLINE],
            ..Default::default()
        },
    );
    assert_eq!(announced.seats[1].hand, 2, "the announced player draws");
    assert_eq!(
        declined.seats.iter().map(|s| s.hand).collect::<Vec<_>>(),
        vec![1, 1, 1],
        "no one draws"
    );
    assert_eq!(declined.counters, 1);
}

/// The declining sub does not refill from the parent's announced player.
#[test]
fn declined_sub_declaring_clause_affects_no_one() {
    let shape = || {
        let mut sub = tagged(lose_life(TargetFilter::Player));
        sub.multi = Some(MultiTargetSpec::up_to(q(1)));
        chain(vec![node(draw(TargetFilter::Player)), sub])
    };
    let announced = run(
        shape(),
        &Opts {
            picks: &[P2, P1],
            ..Default::default()
        },
    );
    let declined = run(
        shape(),
        &Opts {
            picks: &[P2, DECLINE],
            ..Default::default()
        },
    );
    assert_eq!(announced.seats[1].life, 17, "the announced player loses");
    assert_eq!(declined.seats[2].hand, 2, "the parent's draw reached P2");
    assert_eq!(
        declined.seats.iter().map(|s| s.life).collect::<Vec<_>>(),
        vec![20, 20, 20],
        "no one loses life"
    );
    assert_eq!(declined.counters, 1);
}
