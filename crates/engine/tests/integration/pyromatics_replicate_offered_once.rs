//! CR 702.56a + CR 601.2b: a targeted spell's replicate cost is announced once,
//! before its targets, with the rest of its optional additional costs, and every
//! payment made there copies the spell. Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
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

const POOL: usize = 12;

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    djinn: Option<ObjectId>,
    /// What the caster taps or sacrifices for a cost.
    fodder: Vec<ObjectId>,
}

fn pool(scenario: &mut GameScenario, kind: ManaType) {
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_with(|| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .take(POOL)
            .collect(),
    );
}

fn add_djinn(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS)
        .id()
}

/// Pyromatics in hand aimed at a 9/9, optionally beside Djinn Illuminatus
/// (granted replicate) or Wort, the Raidmother and two red creatures (granted
/// conspire).
fn pyromatics_board(with_djinn: bool, with_wort: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim Ogre", 9, 9).id();
    let djinn = with_djinn.then(|| add_djinn(&mut scenario));
    let mut fodder = Vec::new();
    if with_wort {
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
    pool(&mut scenario, ManaType::Red);
    Board {
        runner: scenario.build(),
        spell,
        victim,
        djinn,
        fodder,
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
    pool(&mut scenario, ManaType::Blue);
    Board {
        runner: scenario.build(),
        spell,
        victim,
        djinn,
        fodder: vec![trinket],
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
        pay: bool,
    },
    /// A one-shot optional cost (conspire, bargain) and the answer.
    Once {
        origin: AdditionalCostOrigin,
        pay: bool,
    },
    PayCost,
    Target,
}

/// Casts the board's spell at the victim, paying replicate instance `i`
/// `pays[i]` times and answering every one-shot optional cost with
/// `pay_once`; returns every prompt answered until the cast completes.
fn cast(board: &mut Board, pays: &[u32], pay_once: bool) -> Vec<Prompt> {
    let card_id = board.runner.state().objects[&board.spell].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast starts");
    let mut trail = Vec::new();
    for _ in 0..20 {
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
                let pay = pays
                    .get(ordinal.unwrap_or(0) as usize)
                    .is_some_and(|wanted| times_kicked < *wanted);
                trail.push(Prompt::Replicate {
                    origin,
                    ordinal,
                    paid: times_kicked,
                    pay,
                });
                GameAction::DecideOptionalCost {
                    index: if pay { 0 } else { 1 },
                }
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                trail.push(Prompt::Once {
                    origin,
                    pay: pay_once,
                });
                GameAction::DecideOptionalCost {
                    index: if pay_once { 0 } else { 1 },
                }
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
        board.runner.act(action).expect("the answer is accepted");
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
        pay: paid < pays,
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
        let mut board = pyromatics_board(false, false);
        let trail = cast(&mut board, &[pays], false);

        let mut expected: Vec<Prompt> = offers(0, pays).collect();
        expected.push(Prompt::Target);
        assert_eq!(trail, expected, "pays={pays}");
        assert_eq!(mana_spent(&board), 2 + 2 * pays as usize, "pays={pays}");
        assert_eq!(
            replicate_records(&board),
            vec![(0, 1); pays as usize],
            "pays={pays}"
        );
        // Reach guard: the replicate trigger resolved.
        assert_eq!(copies_per_trigger(&mut board), vec![pays as usize]);
    }
}

/// CR 702.56b: printed and granted replicate (Djinn Illuminatus) are each
/// offered in their own run before the targets, paid separately, and each
/// trigger copies the spell only for its own payments.
#[test]
fn printed_and_granted_replicate_on_a_targeted_spell_are_each_offered_once() {
    let mut board = pyromatics_board(true, false);
    let trail = cast(&mut board, &[1, 2], false);

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

/// CR 113.7a + CR 702.56b: Djinn Illuminatus leaving after the cast does not
/// take back its instance's payments; both triggers still copy the spell.
#[test]
fn granted_replicate_payments_survive_the_grantor_leaving() {
    let mut board = pyromatics_board(true, false);
    let trail = cast(&mut board, &[1, 2], false);
    let mut expected: Vec<Prompt> = offers(0, 1).chain(offers(1, 2)).collect();
    expected.push(Prompt::Target);
    assert_eq!(trail, expected);
    assert_eq!(mana_spent(&board), 2 + 2 * 3);
    let djinn = board.djinn.expect("the Djinn is on the battlefield");
    engine::game::zones::move_to_zone(
        board.runner.state_mut(),
        djinn,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    assert_eq!(board.runner.state().objects[&djinn].zone, Zone::Graveyard);

    assert_eq!(copies_per_trigger(&mut board), vec![1, 2]);
}

/// CR 601.2b + CR 702.78a: granted conspire (Wort, the Raidmother) is
/// announced with Pyromatics' replicate, before the targets: {1}{R} plus one
/// replicate payment, two creatures tapped, one copy from each.
#[test]
fn conspire_is_announced_with_replicate_before_targets() {
    let mut board = pyromatics_board(false, true);
    let trail = cast(&mut board, &[1], true);

    let mut expected: Vec<Prompt> = offers(0, 1).collect();
    expected.extend([
        Prompt::Once {
            origin: AdditionalCostOrigin::Other,
            pay: true,
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
    let trail = cast(&mut board, &[1], true);

    let mut expected: Vec<Prompt> = offers(0, 1).collect();
    expected.extend([
        Prompt::Once {
            origin: AdditionalCostOrigin::Bargain,
            pay: true,
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

