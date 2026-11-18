//! CR 702.56a + CR 601.2b: a targeted spell's replicate cost is announced once,
//! before its targets, with the rest of its optional additional costs, and every
//! payment made there copies the spell. Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PYROMATICS: &str = "Replicate {1}{R} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nPyromatics deals 1 damage to any target.";

const DJINN_ILLUMINATUS: &str = "Flying\nEach instant and sorcery spell you cast has replicate. \
The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid \
its replicate cost. You may choose new targets for the copies.)";

const WORT: &str = "When Wort enters, create two 1/1 red and green Goblin Warrior creature \
tokens.\nEach red or green instant or sorcery spell you cast has conspire. (As you cast the \
spell, you may tap two untapped creatures you control that share a color with it. When you do, \
copy it and you may choose new targets for the copy.)";

const JOHANNS_STOPGAP: &str = "Bargain (You may sacrifice an artifact, enchantment, or token as \
you cast this spell.)\nThis spell costs {2} less to cast if it's bargained.\nReturn target \
nonland permanent to its owner's hand. Draw a card.";

const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

const POOL: usize = 12;

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    djinn: Option<ObjectId>,
    /// What the caster taps or sacrifices for a cost.
    fodder: Vec<ObjectId>,
    /// P1's Lightning Bolts.
    bolts: Vec<ObjectId>,
    /// Every event the cast produced.
    events: Vec<GameEvent>,
}

fn pool(scenario: &mut GameScenario, player: PlayerId, kind: ManaType, n: usize) {
    scenario.with_mana_pool(
        player,
        std::iter::repeat_with(|| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .take(n)
            .collect(),
    );
}

/// What sits beside Pyromatics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Beside {
    Nothing,
    /// Djinn Illuminatus (granted replicate).
    Djinn,
    /// Djinn Illuminatus, and two Lightning Bolts in P1's hand to kill it.
    DjinnAndBolts,
    /// Wort, the Raidmother (granted conspire) and two red creatures.
    Wort,
}

fn add_djinn(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS)
        .id()
}

/// Pyromatics in hand aimed at a 9/9.
fn pyromatics_board(beside: Beside) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim Ogre", 9, 9).id();
    let djinn =
        matches!(beside, Beside::Djinn | Beside::DjinnAndBolts).then(|| add_djinn(&mut scenario));
    let mut bolts = Vec::new();
    if beside == Beside::DjinnAndBolts {
        for _ in 0..2 {
            bolts.push(
                scenario
                    .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, LIGHTNING_BOLT)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::Red],
                        generic: 0,
                    })
                    .id(),
            );
        }
        pool(&mut scenario, P1, ManaType::Red, 2);
    }
    let mut fodder = Vec::new();
    if beside == Beside::Wort {
        scenario.add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT);
        for name in ["Red Goblin A", "Red Goblin B"] {
            fodder.push(
                scenario
                    .add_creature(P0, name, 1, 1)
                    .with_color(vec![ManaColor::Red])
                    .id(),
            );
        }
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Pyromatics", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .from_oracle_text_with_keywords(&["Replicate"], PYROMATICS)
        .id();
    pool(&mut scenario, P0, ManaType::Red, POOL);
    Board {
        runner: scenario.build(),
        spell,
        victim,
        djinn,
        fodder,
        bolts,
        events: Vec::new(),
    }
}

/// Johann's Stopgap (bargain) beside Djinn Illuminatus (granted replicate),
/// with an artifact to bargain away.
fn stopgap_board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let victim = scenario.add_creature(P1, "Victim Ogre", 9, 9).id();
    let djinn = Some(add_djinn(&mut scenario));
    let trinket = scenario
        .add_artifact_from_oracle(P0, "Bargain Trinket", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Johann's Stopgap", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .from_oracle_text_with_keywords(&["Bargain"], JOHANNS_STOPGAP)
        .id();
    pool(&mut scenario, P0, ManaType::Blue, POOL);
    Board {
        runner: scenario.build(),
        spell,
        victim,
        djinn,
        fodder: vec![trinket],
        bolts: Vec::new(),
        events: Vec::new(),
    }
}

/// How the caster answers an optional-cost offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaymentDecision {
    Pay,
    Decline,
}

impl PaymentDecision {
    /// The `DecideOptionalCost` answer: option 0 pays, option 1 declines.
    fn action(self) -> GameAction {
        GameAction::DecideOptionalCost {
            index: match self {
                Self::Pay => 0,
                Self::Decline => 1,
            },
        }
    }
}

/// One answered prompt of the cast.
#[derive(Debug, PartialEq, Eq)]
enum Prompt {
    /// A repeatable optional-cost offer: its origin, the queued instance's
    /// ordinal (`None` when no instance is queued), its payments so far, and
    /// the answer.
    Replicate {
        origin: AdditionalCostOrigin,
        ordinal: Option<u32>,
        paid: u32,
        decision: PaymentDecision,
    },
    /// A one-shot optional cost (conspire, bargain) and the answer.
    Once {
        origin: AdditionalCostOrigin,
        decision: PaymentDecision,
    },
    PayCost,
    Target,
}

/// Casts the board's spell at the victim, paying replicate instance `i`
/// `pays[i]` times and answering every one-shot optional cost with
/// `once`; returns every prompt answered until the cast completes.
fn cast(board: &mut Board, pays: &[u32], once: PaymentDecision) -> Vec<Prompt> {
    cast_restoring_after(board, pays, once, None)
}

/// The engine's own save/undo/P2P restore pipeline.
fn round_trip(runner: &GameRunner) -> GameRunner {
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

/// [`cast`], saving and restoring the game once `restore_after` prompts have
/// been answered.
fn cast_restoring_after(
    board: &mut Board,
    pays: &[u32],
    once: PaymentDecision,
    restore_after: Option<usize>,
) -> Vec<Prompt> {
    let card_id = board.runner.state().objects[&board.spell].card_id;
    let result = board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast starts");
    board.events.extend(result.events);
    let mut trail = Vec::new();
    let mut restored = false;
    for _ in 0..20 {
        if !restored && restore_after == Some(trail.len()) {
            restored = true;
            board.runner = round_trip(&board.runner);
        }
        let action = match board.runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost:
                    AdditionalCost::Optional {
                        repeatability: AdditionalCostRepeatability::Repeatable,
                        ..
                    },
                origin,
                times_kicked,
                pending_cast,
                ..
            } => {
                let ordinal = pending_cast
                    .additional_cost_queue
                    .first()
                    .map(|instance| instance.origin_ordinal);
                let wanted = pays.get(ordinal.unwrap_or(0) as usize).copied();
                let decision = match wanted {
                    Some(wanted) if times_kicked < wanted => PaymentDecision::Pay,
                    _ => PaymentDecision::Decline,
                };
                trail.push(Prompt::Replicate {
                    origin,
                    ordinal,
                    paid: times_kicked,
                    decision,
                });
                decision.action()
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                trail.push(Prompt::Once {
                    origin,
                    decision: once,
                });
                once.action()
            }
            WaitingFor::PayCost { choices, count, .. } => {
                trail.push(Prompt::PayCost);
                let cards: Vec<ObjectId> = board
                    .fodder
                    .iter()
                    .copied()
                    .filter(|id| choices.contains(id))
                    .take(count)
                    .collect();
                assert_eq!(cards.len(), count, "reach: the fodder pays the cost");
                GameAction::SelectCards { cards }
            }
            WaitingFor::TargetSelection { .. } => {
                trail.push(Prompt::Target);
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(board.victim)),
                }
            }
            // CR 603.3b: simultaneous copy triggers go on the stack in the
            // caster's order.
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            _ => break,
        };
        let result = board.runner.act(action).expect("the answer is accepted");
        board.events.extend(result.events);
    }
    assert!(
        matches!(board.runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes, got {:?} after {trail:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(board.runner.state().objects[&board.spell].zone, Zone::Stack);
    trail
}

/// The offers one replicate instance makes when it is paid `pays` times: one
/// per payment, then the declined one.
fn offers(ordinal: u32, pays: u32) -> impl Iterator<Item = Prompt> {
    (0..=pays).map(move |paid| Prompt::Replicate {
        origin: AdditionalCostOrigin::Replicate,
        ordinal: Some(ordinal),
        paid,
        decision: if paid < pays {
            PaymentDecision::Pay
        } else {
            PaymentDecision::Decline
        },
    })
}

fn mana_spent(board: &Board) -> usize {
    POOL - board.runner.state().players[0].mana_pool.total()
}

/// CR 702.56a + CR 702.56b: each payment's record names its own instance.
fn replicate_records(board: &Board) -> Vec<(u32, u32)> {
    board.runner.state().objects[&board.spell]
        .additional_cost_payments
        .iter()
        .filter(|payment| payment.origin == AdditionalCostOrigin::Replicate)
        .map(|payment| (payment.origin_ordinal, payment.count))
        .collect()
}

/// Resolves the stack's copy triggers (and the copies they put on top),
/// leaving the original spell; returns how many copies each trigger made,
/// sorted.
fn copies_per_trigger(board: &mut Board) -> Vec<usize> {
    let triggers = |board: &Board| {
        board
            .runner
            .state()
            .stack
            .iter()
            .filter(|entry| !matches!(entry.kind, StackEntryKind::Spell { .. }))
            .count()
    };
    let mut open = triggers(board);
    let mut per_trigger = Vec::new();
    let mut copies = 0;
    for _ in 0..60 {
        match board.runner.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => {
                copies += 1;
                board
                    .runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the copy's target");
            }
            WaitingFor::Priority { .. } => {
                let now = triggers(board);
                if now < open {
                    per_trigger.push(copies);
                    copies = 0;
                    open = now;
                }
                if now == 0 {
                    break;
                }
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority");
            }
            ref other => panic!("unexpected prompt while the triggers resolve: {other:?}"),
        }
    }
    per_trigger.sort_unstable();
    per_trigger
}

/// CR 702.56a + CR 601.2b + CR 601.2f: replicate is offered in one run before
/// the targets, never again after them; each payment adds {1}{R} once and
/// makes one copy.
#[test]
fn targeted_replicate_is_offered_once_before_targets() {
    for pays in 0..=2u32 {
        let mut board = pyromatics_board(Beside::Nothing);
        let trail = cast(&mut board, &[pays], PaymentDecision::Decline);

        let mut expected: Vec<Prompt> = offers(0, pays).collect();
        expected.push(Prompt::Target);
        assert_eq!(trail, expected, "pays={pays}");
        assert_eq!(mana_spent(&board), 2 + 2 * pays as usize, "pays={pays}");
        assert_eq!(
            replicate_records(&board),
            vec![(0, 1); pays as usize],
            "pays={pays}"
        );
        let copies = copies_per_trigger(&mut board);
        assert_eq!(copies.iter().sum::<usize>(), pays as usize, "pays={pays}");
        if pays > 0 {
            // Reach guard: the replicate trigger resolved.
            assert_eq!(copies, vec![pays as usize]);
        }
    }
}

/// CR 601.2b: a game saved between two replicate payments resumes the same
/// announcement; replicate is still not offered after the targets.
#[test]
fn targeted_replicate_saved_mid_announcement_is_offered_once() {
    let mut board = pyromatics_board(Beside::Nothing);
    let trail = cast_restoring_after(&mut board, &[2], PaymentDecision::Decline, Some(1));

    let mut expected: Vec<Prompt> = offers(0, 2).collect();
    expected.push(Prompt::Target);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 2 + 2 * 2);
    assert_eq!(copies_per_trigger(&mut board), vec![2]);
}

/// CR 702.56b: printed and granted replicate (Djinn Illuminatus) are each
/// offered in their own run before the targets, paid separately, and each
/// trigger copies the spell only for its own payments.
#[test]
fn printed_and_granted_replicate_on_a_targeted_spell_are_each_offered_once() {
    let mut board = pyromatics_board(Beside::Djinn);
    let trail = cast(&mut board, &[1, 2], PaymentDecision::Decline);

    let mut expected: Vec<Prompt> = offers(0, 1).chain(offers(1, 2)).collect();
    expected.push(Prompt::Target);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 2 + 2 * 3);
    assert_eq!(replicate_records(&board), vec![(0, 1), (1, 1), (1, 1)]);
    assert_eq!(
        copies_per_trigger(&mut board),
        vec![1, 2],
        "one copy for printed, two for granted"
    );
}

/// CR 117.3d: P0 passes with the replicate triggers on the stack; P1 casts a
/// Lightning Bolt at `target` and both players pass so it resolves.
fn respond_with_bolt(board: &mut Board, bolt: ObjectId, target: ObjectId) {
    board
        .runner
        .act(GameAction::PassPriority)
        .expect("P0 passes");
    assert!(
        matches!(board.runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 receives priority, got {:?}",
        board.runner.state().waiting_for
    );
    let card_id = board.runner.state().objects[&bolt].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Lightning Bolt");
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("Lightning Bolt targets the Djinn");
    for _ in 0..2 {
        board.runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(board.runner.state().objects[&bolt].zone, Zone::Graveyard);
}

/// CR 113.7a + CR 702.56b: Djinn Illuminatus dying after the cast, before the
/// replicate triggers resolve, does not take back its instance's payments;
/// both triggers still copy the spell.
#[test]
fn granted_replicate_payments_survive_the_grantor_dying() {
    let mut board = pyromatics_board(Beside::DjinnAndBolts);
    let trail = cast(&mut board, &[1, 2], PaymentDecision::Decline);
    let mut expected: Vec<Prompt> = offers(0, 1).chain(offers(1, 2)).collect();
    expected.push(Prompt::Target);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 2 + 2 * 3);
    let pyromatics_casts = board
        .events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == board.spell)
        })
        .count();
    assert_eq!(pyromatics_casts, 1, "Pyromatics is cast once");

    let djinn = board.djinn.expect("the Djinn is on the battlefield");
    for bolt in board.bolts.clone() {
        respond_with_bolt(&mut board, bolt, djinn);
    }
    // CR 704.5g: six damage on a 3/5.
    assert_eq!(board.runner.state().objects[&djinn].zone, Zone::Graveyard);
    assert_eq!(board.runner.state().objects[&board.spell].zone, Zone::Stack);

    assert_eq!(copies_per_trigger(&mut board), vec![1, 2]);
}

/// CR 601.2b + CR 702.78a: granted conspire (Wort, the Raidmother) is
/// announced with Pyromatics' replicate, before the targets: {1}{R} plus one
/// replicate payment, two creatures tapped, one copy from each.
#[test]
fn conspire_is_announced_with_replicate_before_targets() {
    let mut board = pyromatics_board(Beside::Wort);
    let trail = cast(&mut board, &[1], PaymentDecision::Pay);

    let mut expected: Vec<Prompt> = offers(0, 1).collect();
    expected.extend([
        Prompt::Once {
            origin: AdditionalCostOrigin::Other,
            decision: PaymentDecision::Pay,
        },
        Prompt::PayCost,
        Prompt::Target,
    ]);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 4);
    for goblin in &board.fodder {
        assert!(
            board.runner.state().objects[goblin].tapped,
            "tapped for conspire"
        );
    }
    assert_eq!(copies_per_trigger(&mut board), vec![1, 1]);
}

/// CR 601.2b + CR 702.166a: bargain is announced with Djinn Illuminatus's
/// granted replicate, before the targets: {3}{U} plus one {3}{U} replicate
/// payment, {2} less for the bargain, one copy.
#[test]
fn bargain_is_announced_with_granted_replicate_before_targets() {
    let mut board = stopgap_board();
    let trail = cast(&mut board, &[1], PaymentDecision::Pay);

    let mut expected: Vec<Prompt> = offers(0, 1).collect();
    expected.extend([
        Prompt::Once {
            origin: AdditionalCostOrigin::Bargain,
            decision: PaymentDecision::Pay,
        },
        Prompt::PayCost,
        Prompt::Target,
    ]);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 4 + 4 - 2);
    assert_eq!(
        board.runner.state().objects[&board.fodder[0]].zone,
        Zone::Graveyard,
        "bargained away"
    );
    assert_eq!(copies_per_trigger(&mut board), vec![1]);
}
