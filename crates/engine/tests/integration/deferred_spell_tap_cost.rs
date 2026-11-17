//! CR 601.2b + CR 601.2c + CR 601.2h: a spell tap cost (conspire, teamwork)
//! announced before the spell's targets is only selected there. The selected
//! creatures stay untapped through target selection and the mana-ability
//! window (CR 601.2g), are held for that payment, and are tapped when the total
//! cost is paid. Oracle text is verbatim from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ConvokeMode, PersistedGameState, PersistedRestoreFinalization,
    ShardChoice, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WORT: &str = "When Wort enters, create two 1/1 red and green Goblin Warrior creature \
tokens.\nEach red or green instant or sorcery spell you cast has conspire. (As you cast the \
spell, you may tap two untapped creatures you control that share a color with it. When you do, \
copy it and you may choose new targets for the copy.)";
const DJINN_ILLUMINATUS: &str = "Flying\nEach instant and sorcery spell you cast has replicate. \
The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid \
its replicate cost. You may choose new targets for the copies.)";
const ANHELO: &str = "Deathtouch\nThe first instant or sorcery spell you cast each turn has \
casualty 2. (As you cast that spell, you may sacrifice a creature with power 2 or greater. When \
you do, copy the spell and you may choose new targets for the copy.)";
const ZHUR_TAA_DRUID: &str =
    "{T}: Add {G}.\nWhenever you tap this creature for mana, it deals 1 damage to each opponent.";
const PYROMATICS: &str = "Replicate {1}{R} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nPyromatics deals 1 damage to any target.";
const TEAM_TACTICS: &str = "Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)\nTarget creature gains double strike until end of turn. If this spell was cast using teamwork, that creature also gains trample until end of turn.";
const CRUEL_ALLIANCE: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nExile target creature with mana value 3 or less. If this spell was cast using teamwork, instead exile target creature and you gain 3 life.";
const STOKE_THE_FLAMES: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nStoke the Flames deals 4 damage to any target.";
const FIERY_TEMPER: &str = "Fiery Temper deals 3 damage to any target.\nMadness {R} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)";
const GUT_SHOT: &str =
    "({R/P} can be paid with either {R} or 2 life.)\nGut Shot deals 1 damage to any target.";
const CHANNELED_DRAGONFIRE: &str = "Channeled Dragonfire deals 2 damage to any target.\nHarmonize {5}{R}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)";

const RED: [ManaColor; 1] = [ManaColor::Red];
const RED_GREEN: [ManaColor; 2] = [ManaColor::Red, ManaColor::Green];

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn pool(scenario: &mut GameScenario, kind: ManaType, n: usize) {
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_with(|| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .take(n)
            .collect(),
    );
}

fn creature(scenario: &mut GameScenario, name: &str, power: i32, colors: &[ManaColor]) -> ObjectId {
    scenario
        .add_creature(P0, name, power, power)
        .with_color(colors.to_vec())
        .id()
}

fn add_wort(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT)
        .with_color(RED_GREEN.to_vec())
        .id()
}

fn add_djinn(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS)
        .with_color(vec![ManaColor::Blue, ManaColor::Red])
        .id()
}

fn add_goblins(scenario: &mut GameScenario, n: usize) -> Vec<ObjectId> {
    (0..n)
        .map(|i| creature(scenario, &format!("Goblin Warrior {i}"), 1, &RED_GREEN))
        .collect()
}

fn victim(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P1, "Victim Ogre", 2, 9)
        .with_mana_cost(cost(2, &[]))
        .id()
}

/// A board with P1's victim, Wort and `goblins` Goblin Warriors, optionally
/// Djinn Illuminatus.
fn table(djinn: bool, goblins: usize) -> (GameScenario, ObjectId, Option<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = victim(&mut scenario);
    add_wort(&mut scenario);
    let djinn = djinn.then(|| add_djinn(&mut scenario));
    let goblins = add_goblins(&mut scenario, goblins);
    (scenario, victim, djinn, goblins)
}

fn red_spell(
    scenario: &mut GameScenario,
    name: &str,
    instant: bool,
    mana_cost: ManaCost,
    keywords: &[&str],
    oracle: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, name, instant)
        .with_mana_cost(mana_cost)
        .with_color(RED.to_vec())
        .from_oracle_text_with_keywords(keywords, oracle)
        .id()
}

/// How the caster answers an optional-cost offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaymentDecision {
    Pay,
    Decline,
}

impl PaymentDecision {
    fn action(self) -> GameAction {
        GameAction::DecideOptionalCost {
            index: match self {
                Self::Pay => 0,
                Self::Decline => 1,
            },
        }
    }
}

/// One answered prompt, in order.
#[derive(Debug, PartialEq, Eq)]
enum Prompt {
    Replicate(PaymentDecision),
    Once(AdditionalCostOrigin, PaymentDecision),
    PayCost,
    Target,
    Harmonize,
    Phyrexian,
    ManaPayment,
}

/// How each prompt of a cast is answered.
struct Script {
    payment_mode: CastPaymentMode,
    /// Replicate payments (every instance), then declined.
    replicate: u32,
    /// One-shot optional costs paid, by origin; any other is declined.
    pay_once: Vec<AdditionalCostOrigin>,
    /// The selections for each `PayCost` prompt, in order.
    selections: Vec<Vec<ObjectId>>,
    target: ObjectId,
    harmonize_with: Option<ObjectId>,
    /// Save and restore through the production serializer at target selection.
    restore_at_targets: bool,
    /// Creatures tapped for convoke at the payment step.
    convoke: Vec<ObjectId>,
    /// Creatures whose convoke is attempted (on a copy) when the payment step opens.
    probe_convoke: Vec<ObjectId>,
    /// Cancel the cast at target selection instead of completing it.
    cancel_at_targets: bool,
}

impl Script {
    fn new(target: ObjectId) -> Self {
        Self {
            payment_mode: CastPaymentMode::Auto,
            replicate: 0,
            pay_once: Vec::new(),
            selections: Vec::new(),
            target,
            harmonize_with: None,
            restore_at_targets: false,
            convoke: Vec::new(),
            probe_convoke: Vec::new(),
            cancel_at_targets: false,
        }
    }
}

/// What a cast did, observed at its prompts.
#[derive(Default)]
struct Run {
    prompts: Vec<Prompt>,
    events: Vec<GameEvent>,
    /// The choices each `PayCost` prompt offered.
    offers: Vec<Vec<ObjectId>>,
    /// Tapped state of the watched creatures at target selection.
    tapped_at_targets: Option<Vec<bool>>,
    /// Tapped state of the watched creatures when the payment step opened.
    tapped_at_mana_payment: Option<Vec<bool>>,
    /// The deferred selection `state.pending_cast` held at the payment step.
    root_at_mana_payment: Option<Vec<ObjectId>>,
    convoke_mode: Option<ConvokeMode>,
    /// The AI's convoke candidates, and the probed creatures the engine refused.
    convoke_candidates: Vec<ObjectId>,
    convoke_refused: Vec<ObjectId>,
    /// Creatures the `HarmonizeTapChoice` offered, and whether a payment root
    /// was published while it was open.
    harmonize_offer: Option<Vec<ObjectId>>,
    harmonize_inline_root: Option<Vec<ObjectId>>,
    harmonize_published_root: Option<bool>,
    /// The deferred selection `state.pending_cast` held at the Phyrexian prompt.
    root_at_phyrexian: Option<Vec<ObjectId>>,
}

/// The engine's own save/undo/P2P restore pipeline.
fn round_trip(runner: &GameRunner) -> GameRunner {
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

fn tapped(runner: &GameRunner, ids: &[ObjectId]) -> Vec<bool> {
    ids.iter()
        .map(|id| runner.state().objects[id].tapped)
        .collect()
}

fn root_selection(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    runner
        .state()
        .pending_cast
        .as_ref()
        .map(|pending| pending.deferred_tapped_permanents.clone())
}

/// Casts `spell`, answering every prompt from `script` through public actions,
/// until P0 holds priority with the spell on the stack.
fn cast(runner: &mut GameRunner, spell: ObjectId, script: &Script, watch: &[ObjectId]) -> Run {
    let mut run = Run::default();
    let card_id = runner.state().objects[&spell].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: script.payment_mode,
        })
        .expect("the cast starts");
    run.events.extend(result.events);
    let mut selections = script.selections.iter();
    let mut replicated = 0;
    let mut restored = false;
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::CastingVariantChoice { options, .. } => GameAction::ChooseCastingVariant {
                index: options
                    .iter()
                    .position(|option| option.variant == CastingVariant::Harmonize)
                    .expect("reach: harmonize is offered"),
            },
            WaitingFor::OptionalCostChoice {
                cost:
                    AdditionalCost::Optional {
                        repeatability: AdditionalCostRepeatability::Repeatable,
                        ..
                    },
                ..
            } => {
                let decision = if replicated < script.replicate {
                    replicated += 1;
                    PaymentDecision::Pay
                } else {
                    PaymentDecision::Decline
                };
                run.prompts.push(Prompt::Replicate(decision));
                decision.action()
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                let decision = if script.pay_once.contains(&origin) {
                    PaymentDecision::Pay
                } else {
                    PaymentDecision::Decline
                };
                run.prompts.push(Prompt::Once(origin, decision));
                decision.action()
            }
            WaitingFor::PayCost { choices, .. } => {
                run.prompts.push(Prompt::PayCost);
                run.offers.push(choices);
                GameAction::SelectCards {
                    cards: selections
                        .next()
                        .expect("a scripted selection for each PayCost")
                        .clone(),
                }
            }
            WaitingFor::TargetSelection { .. } => {
                if script.restore_at_targets && !restored {
                    restored = true;
                    *runner = round_trip(runner);
                }
                run.prompts.push(Prompt::Target);
                run.tapped_at_targets = Some(tapped(runner, watch));
                if script.cancel_at_targets {
                    runner.act(GameAction::CancelCast).expect("cancel");
                    return run;
                }
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(script.target)),
                }
            }
            WaitingFor::HarmonizeTapChoice {
                eligible_creatures,
                pending_cast,
                ..
            } => {
                run.prompts.push(Prompt::Harmonize);
                run.harmonize_offer = Some(eligible_creatures);
                run.harmonize_inline_root = Some(pending_cast.deferred_tapped_permanents.clone());
                run.harmonize_published_root = Some(runner.state().pending_cast.is_some());
                GameAction::HarmonizeTap {
                    creature_id: script.harmonize_with,
                }
            }
            WaitingFor::PhyrexianPayment { shards, .. } => {
                run.prompts.push(Prompt::Phyrexian);
                run.root_at_phyrexian = root_selection(runner);
                GameAction::SubmitPhyrexianChoices {
                    choices: vec![ShardChoice::PayMana; shards.len()],
                }
            }
            WaitingFor::ManaPayment { convoke_mode, .. } => {
                if run.tapped_at_mana_payment.is_none() {
                    run.prompts.push(Prompt::ManaPayment);
                    run.tapped_at_mana_payment = Some(tapped(runner, watch));
                    run.root_at_mana_payment = root_selection(runner);
                    run.convoke_mode = convoke_mode;
                    run.convoke_candidates = engine::ai_support::candidate_actions(runner.state())
                        .into_iter()
                        .filter_map(|candidate| match candidate.action {
                            GameAction::TapForConvoke { object_id, .. } => Some(object_id),
                            _ => None,
                        })
                        .collect();
                    let mut probe = round_trip(runner);
                    run.convoke_refused = script
                        .probe_convoke
                        .iter()
                        .copied()
                        .filter(|&object_id| {
                            probe
                                .act(GameAction::TapForConvoke {
                                    object_id,
                                    mana_type: ManaType::Red,
                                })
                                .is_err()
                        })
                        .collect();
                    for &creature in &script.convoke {
                        let result = runner
                            .act(GameAction::TapForConvoke {
                                object_id: creature,
                                mana_type: ManaType::Red,
                            })
                            .expect("an unreserved creature convokes");
                        run.events.extend(result.events);
                    }
                }
                GameAction::PassPriority
            }
            // CR 603.3b: simultaneous copy triggers go on the stack in the
            // caster's order.
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt {other:?} after {:?}", run.prompts),
        };
        let result = runner.act(action).expect("the answer is accepted");
        run.events.extend(result.events);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes, got {:?} after {:?}",
        runner.state().waiting_for,
        run.prompts
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    run
}

/// CR 701.26a: how many times `id` became tapped during the cast.
fn taps(run: &Run, id: ObjectId) -> usize {
    run.events
        .iter()
        .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == id))
        .count()
}

fn first_tap(run: &Run, id: ObjectId) -> usize {
    run.events
        .iter()
        .position(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == id))
        .expect("reach: the creature was tapped")
}

/// Resolves the copy triggers above `spell` (replicate, conspire, casualty) and
/// returns how many copies of it they made (CR 707.10).
fn copies(runner: &mut GameRunner, spell: ObjectId) -> usize {
    let mut made = 0;
    for _ in 0..60 {
        let triggers = runner
            .state()
            .stack
            .iter()
            .filter(|entry| {
                !matches!(
                    entry.kind,
                    engine::types::game_state::StackEntryKind::Spell { .. }
                )
            })
            .count();
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::Priority { .. } if triggers > 0 => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while the copy triggers resolve: {other:?}"),
        };
        let result = runner.act(action).expect("resolve the copy triggers");
        made += result
            .events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::SpellCopied { original_id, .. } if *original_id == spell)
            })
            .count();
    }
    made
}
fn spent(runner: &GameRunner, from: usize) -> usize {
    from - runner.state().players[0].mana_pool.total()
}

// ---------------------------------------------------------------------------
// P1: Pyromatics + Wort, the Raidmother (granted conspire)
// ---------------------------------------------------------------------------

struct PyromaticsBoard {
    runner: GameRunner,
    pyromatics: ObjectId,
    victim: ObjectId,
    goblins: Vec<ObjectId>,
}

fn pyromatics_board() -> PyromaticsBoard {
    let (mut scenario, victim, _, goblins) = table(false, 2);
    let pyromatics = red_spell(
        &mut scenario,
        "Pyromatics",
        true,
        cost(1, &[ManaCostShard::Red]),
        &["Replicate"],
        PYROMATICS,
    );
    pool(&mut scenario, ManaType::Red, 12);
    PyromaticsBoard {
        runner: scenario.build(),
        pyromatics,
        victim,
        goblins,
    }
}
fn conspire_script(board: &PyromaticsBoard) -> Script {
    let mut script = Script::new(board.victim);
    script.replicate = 1;
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![board.goblins.clone()];
    script
}

fn conspire_trail() -> Vec<Prompt> {
    vec![
        Prompt::Replicate(PaymentDecision::Pay),
        Prompt::Replicate(PaymentDecision::Decline),
        Prompt::Once(AdditionalCostOrigin::Other, PaymentDecision::Pay),
        Prompt::PayCost,
        Prompt::Target,
    ]
}

/// The common completion: one tap per Goblin, 4 mana, one replicate copy and
/// one conspire copy.
fn assert_conspire_paid(board: &mut PyromaticsBoard, run: &Run) {
    for goblin in &board.goblins {
        assert!(board.runner.state().objects[goblin].tapped);
        assert_eq!(taps(run, *goblin), 1, "each Goblin is tapped once");
    }
    assert_eq!(spent(&board.runner, 12), 4, "{{1}}{{R}} + one replicate");
    assert_eq!(
        copies(&mut board.runner, board.pyromatics),
        2,
        "replicate + conspire"
    );
}

/// CR 601.2b + CR 601.2c + CR 601.2h (Matt's F1 witness): conspire is
/// announced before targets, its Goblins stay untapped through target
/// declaration and the Auto payment, and are tapped once when the cost is paid.
#[test]
fn conspire_selected_before_targets_taps_when_the_cost_is_paid() {
    let mut board = pyromatics_board();
    let script = conspire_script(&board);
    let run = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);

    assert_eq!(run.prompts, conspire_trail());
    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_conspire_paid(&mut board, &run);
    // RF4-1 closure 2: the Auto payment leaves no payment root behind.
    assert!(board.runner.state().pending_cast.is_none());
}

/// CR 601.2g + CR 601.2h: with manual payment the Goblins are still untapped,
/// and held, when the payment step opens (RF4-1 closure 1).
#[test]
fn conspire_creatures_stay_untapped_through_the_manual_payment_window() {
    let mut board = pyromatics_board();
    let mut script = conspire_script(&board);
    script.payment_mode = CastPaymentMode::Manual;
    let run = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);

    let mut trail = conspire_trail();
    trail.push(Prompt::ManaPayment);
    assert_eq!(run.prompts, trail);
    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_eq!(run.tapped_at_mana_payment, Some(vec![false, false]));
    assert_eq!(run.root_at_mana_payment, Some(board.goblins.clone()));
    assert_conspire_paid(&mut board, &run);
}

/// The selection survives a save and restore at target selection.
#[test]
fn conspire_selection_survives_a_restore_at_target_selection() {
    let mut board = pyromatics_board();
    let mut script = conspire_script(&board);
    script.restore_at_targets = true;
    let run = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);

    assert_eq!(run.prompts, conspire_trail());
    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_conspire_paid(&mut board, &run);
}

/// CR 601.2: cancelling at target selection taps nothing and keeps no stale
/// selection; casting again announces and offers conspire afresh.
#[test]
fn cancelled_conspire_cast_recasts_cleanly() {
    let mut board = pyromatics_board();
    let mut script = conspire_script(&board);
    script.cancel_at_targets = true;
    let cancelled = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);
    assert_eq!(cancelled.prompts, conspire_trail());
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(tapped(&board.runner, &board.goblins), vec![false, false]);
    assert!(board.runner.state().pending_cast.is_none());

    script.cancel_at_targets = false;
    let run = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);
    assert_eq!(run.prompts, conspire_trail());
    for goblin in &board.goblins {
        assert!(
            run.offers[0].contains(goblin),
            "the Goblins are offered again"
        );
    }
    assert_conspire_paid(&mut board, &run);
}
// ---------------------------------------------------------------------------
// RF1-2 carriers: Team Tactics under Djinn Illuminatus (pre-target route)
// ---------------------------------------------------------------------------

struct TacticsBoard {
    runner: GameRunner,
    tactics: ObjectId,
    victim: ObjectId,
    wort: ObjectId,
    djinn: ObjectId,
    goblin: Option<ObjectId>,
}

fn tactics_board(with_goblin: bool) -> TacticsBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = victim(&mut scenario);
    let wort = add_wort(&mut scenario);
    let djinn = add_djinn(&mut scenario);
    let goblin = with_goblin.then(|| add_goblins(&mut scenario, 1)[0]);
    let tactics = red_spell(
        &mut scenario,
        "Team Tactics",
        true,
        cost(1, &[ManaCostShard::Red]),
        &[],
        TEAM_TACTICS,
    );
    pool(&mut scenario, ManaType::Red, 12);
    TacticsBoard {
        runner: scenario.build(),
        tactics,
        victim,
        wort,
        djinn,
        goblin,
    }
}

/// CR 601.2b + CR 601.2h: Wort, held for teamwork, can't also pay conspire; with
/// only Djinn left untapped, conspire's two-creature cost is not offered.
#[test]
fn creature_held_for_teamwork_makes_conspire_unofferable() {
    let mut board = tactics_board(false);
    let mut script = Script::new(board.victim);
    script.replicate = 1;
    script.pay_once = vec![AdditionalCostOrigin::Teamwork, AdditionalCostOrigin::Other];
    script.selections = vec![vec![board.wort]];
    let watch = [board.wort, board.djinn];
    let run = cast(&mut board.runner, board.tactics, &script, &watch);

    assert_eq!(
        run.prompts,
        vec![
            Prompt::Replicate(PaymentDecision::Pay),
            Prompt::Replicate(PaymentDecision::Decline),
            Prompt::Once(AdditionalCostOrigin::Teamwork, PaymentDecision::Pay),
            Prompt::PayCost,
            Prompt::Target,
        ],
        "conspire is not offered"
    );
    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_eq!(taps(&run, board.wort), 1);
    assert_eq!(taps(&run, board.djinn), 0);
}

/// CR 601.2h: with a Goblin held for teamwork, conspire's prompt offers exactly
/// Wort and Djinn; each is tapped once when the cost is paid.
#[test]
fn creature_held_for_teamwork_is_not_offered_to_conspire() {
    let mut board = tactics_board(true);
    let goblin = board.goblin.expect("the Goblin");
    let mut script = Script::new(board.victim);
    script.replicate = 1;
    script.pay_once = vec![AdditionalCostOrigin::Teamwork, AdditionalCostOrigin::Other];
    script.selections = vec![vec![goblin], vec![board.wort, board.djinn]];
    let watch = [board.wort, board.djinn, goblin];
    let run = cast(&mut board.runner, board.tactics, &script, &watch);

    assert_eq!(
        run.prompts,
        vec![
            Prompt::Replicate(PaymentDecision::Pay),
            Prompt::Replicate(PaymentDecision::Decline),
            Prompt::Once(AdditionalCostOrigin::Teamwork, PaymentDecision::Pay),
            Prompt::PayCost,
            Prompt::Once(AdditionalCostOrigin::Other, PaymentDecision::Pay),
            Prompt::PayCost,
            Prompt::Target,
        ]
    );
    let mut conspire_offer = run.offers[1].clone();
    conspire_offer.sort();
    let mut expected = vec![board.wort, board.djinn];
    expected.sort();
    assert_eq!(conspire_offer, expected, "the held Goblin is not offered");
    assert_eq!(run.tapped_at_targets, Some(vec![false, false, false]));
    for id in watch {
        assert_eq!(taps(&run, id), 1);
    }
    assert_eq!(
        copies(&mut board.runner, board.tactics),
        2,
        "replicate + conspire"
    );
}

// ---------------------------------------------------------------------------
// P3: convoke and mana sources against held creatures
// ---------------------------------------------------------------------------

/// CR 702.51a + CR 601.2h: Stoke the Flames under Djinn + Wort. The Goblins held
/// for conspire can't convoke; an unheld creature does, and the cast completes.
#[test]
fn creatures_held_for_conspire_cant_convoke() {
    let (mut scenario, victim, _, goblins) = table(true, 2);
    let helper = creature(&mut scenario, "Convoking Helper", 2, &RED);
    let stoke = red_spell(
        &mut scenario,
        "Stoke the Flames",
        true,
        cost(2, &[ManaCostShard::Red, ManaCostShard::Red]),
        &[],
        STOKE_THE_FLAMES,
    );
    pool(&mut scenario, ManaType::Red, 3);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![goblins.clone()];
    script.convoke = vec![helper];
    script.probe_convoke = goblins.clone();
    let run = cast(&mut runner, stoke, &script, &goblins);

    assert_eq!(
        run.convoke_mode,
        Some(ConvokeMode::Convoke),
        "reach: convoke payment opens"
    );
    assert!(
        run.convoke_candidates.contains(&helper),
        "the unheld creature convokes"
    );
    for goblin in &goblins {
        assert!(
            !run.convoke_candidates.contains(goblin),
            "a held Goblin is not offered"
        );
    }
    assert_eq!(run.convoke_refused, goblins, "a held Goblin can't convoke");
    assert_eq!(run.tapped_at_mana_payment, Some(vec![false, false]));
    assert_eq!(taps(&run, helper), 1, "the helper convoked");
    for goblin in &goblins {
        assert_eq!(taps(&run, *goblin), 1, "each Goblin pays conspire once");
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
struct DruidBoard {
    runner: GameRunner,
    pyromatics: ObjectId,
    victim: ObjectId,
    goblins: Vec<ObjectId>,
    druids: Vec<ObjectId>,
}

/// One Mountain and `druids` mature Zhur-Taa Druids beside Pyromatics, Wort
/// and two Goblins; P0's pool is empty.
fn druid_board(druids: usize) -> DruidBoard {
    let (mut scenario, victim, _, goblins) = table(false, 2);
    let druids = (0..druids)
        .map(|_| {
            scenario
                .add_creature_from_oracle(P0, "Zhur-Taa Druid", 1, 1, ZHUR_TAA_DRUID)
                .with_color(RED_GREEN.to_vec())
                .id()
        })
        .collect();
    scenario.add_basic_land(P0, ManaColor::Red);
    let pyromatics = red_spell(
        &mut scenario,
        "Pyromatics",
        true,
        cost(1, &[ManaCostShard::Red]),
        &["Replicate"],
        PYROMATICS,
    );
    DruidBoard {
        runner: scenario.build(),
        pyromatics,
        victim,
        goblins,
        druids,
    }
}

fn tapped_for_mana(run: &Run, id: ObjectId) -> Option<usize> {
    run.events.iter().position(
        |event| matches!(event, GameEvent::TappedForMana { source_id, .. } if *source_id == id),
    )
}

/// CR 601.2g + CR 601.2h: mana abilities are activated before costs are paid.
/// With an empty pool, the unheld Druid taps for mana before the held Goblins
/// are tapped for conspire.
#[test]
fn mana_is_activated_before_held_creatures_are_tapped() {
    let mut board = druid_board(1);
    let mut script = Script::new(board.victim);
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![board.goblins.clone()];
    let run = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);

    let druid_mana = tapped_for_mana(&run, board.druids[0]).expect("reach: the Druid paid mana");
    for goblin in &board.goblins {
        assert!(
            druid_mana < first_tap(&run, *goblin),
            "mana before the conspire tap"
        );
        assert_eq!(taps(&run, *goblin), 1);
    }
}

/// CR 601.2h: a Druid held for conspire is not tapped for that spell's mana;
/// the Auto payer takes the {G} from the unheld Druid instead.
#[test]
fn creature_held_for_conspire_is_not_tapped_for_mana() {
    let mut board = druid_board(2);
    let (held_druid, free_druid) = (board.druids[0], board.druids[1]);
    let held = vec![held_druid, board.goblins[0]];
    let mut script = Script::new(board.victim);
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![held.clone()];
    let run = cast(&mut board.runner, board.pyromatics, &script, &held);

    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_eq!(tapped_for_mana(&run, held_druid), None);
    assert!(
        tapped_for_mana(&run, free_druid).is_some(),
        "reach: the unheld Druid paid mana"
    );
    assert_eq!(taps(&run, held_druid), 1, "tapped once, for conspire");
}

// ---------------------------------------------------------------------------
// Other routes
// ---------------------------------------------------------------------------

/// CR 601.2h + CR 702.153a: the same creature pays conspire (tap) and casualty
/// (sacrifice). It stays untapped through target selection, is tapped, then
/// sacrificed; casualty and conspire each copy the spell.
#[test]
fn creature_tapped_for_conspire_is_then_sacrificed_for_casualty() {
    let (mut scenario, victim, djinn, goblins) = table(true, 1);
    let (djinn, goblin) = (djinn.expect("Djinn"), goblins[0]);
    scenario
        .add_creature_from_oracle(P0, "Anhelo, the Painter", 3, 3, ANHELO)
        .with_color(vec![ManaColor::Blue, ManaColor::Black, ManaColor::Red]);
    let temper = red_spell(
        &mut scenario,
        "Fiery Temper",
        true,
        cost(1, &[ManaCostShard::Red, ManaCostShard::Red]),
        &[],
        FIERY_TEMPER,
    );
    pool(&mut scenario, ManaType::Red, 3);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.pay_once = vec![AdditionalCostOrigin::Casualty, AdditionalCostOrigin::Other];
    script.selections = vec![vec![djinn], vec![djinn, goblin]];
    let run = cast(&mut runner, temper, &script, &[djinn, goblin]);

    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    let sacrificed = run
        .events
        .iter()
        .position(|event| {
            matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. }
                if *object_id == djinn)
        })
        .expect("reach: Djinn was sacrificed");
    assert!(
        first_tap(&run, djinn) < sacrificed,
        "tapped, then sacrificed"
    );
    assert_eq!(runner.state().objects[&djinn].zone, Zone::Graveyard);
    assert_eq!(taps(&run, goblin), 1);
    assert_eq!(copies(&mut runner, temper), 2, "casualty + conspire");
}

/// CR 702.194a: Cruel Alliance's teamwork is announced before its targets (its
/// teamwork changes them); the tapper stays untapped through target selection.
#[test]
fn teamwork_announced_before_targets_taps_when_the_cost_is_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Big Victim", 5, 5)
        .with_mana_cost(cost(5, &[]))
        .id();
    let tapper = scenario.add_creature(P0, "Teamwork Bear", 2, 2).id();
    let alliance = scenario
        .add_spell_to_hand(P0, "Cruel Alliance", false)
        .with_mana_cost(cost(2, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&[], CRUEL_ALLIANCE)
        .id();
    pool(&mut scenario, ManaType::Black, 3);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.pay_once = vec![AdditionalCostOrigin::Teamwork];
    script.selections = vec![vec![tapper]];
    let run = cast(&mut runner, alliance, &script, &[tapper]);

    assert_eq!(
        run.prompts,
        vec![
            Prompt::Once(AdditionalCostOrigin::Teamwork, PaymentDecision::Pay),
            Prompt::PayCost,
            Prompt::Target,
        ]
    );
    assert_eq!(run.tapped_at_targets, Some(vec![false]));
    assert_eq!(taps(&run, tapper), 1);
}

/// CR 107.4f + CR 601.2h: Gut Shot's Phyrexian choice pauses payment with the
/// held Goblins still untapped and still held (RF4-1 closure 5); answering it
/// pays, tapping each once.
#[test]
fn phyrexian_payment_pause_keeps_the_selection_held() {
    let (mut scenario, victim, _, goblins) = table(true, 2);
    let gut_shot = red_spell(
        &mut scenario,
        "Gut Shot",
        true,
        cost(0, &[ManaCostShard::PhyrexianRed]),
        &[],
        GUT_SHOT,
    );
    pool(&mut scenario, ManaType::Red, 1);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![goblins.clone()];
    let run = cast(&mut runner, gut_shot, &script, &goblins);

    assert!(
        run.prompts.contains(&Prompt::Phyrexian),
        "reach: {:?}",
        run.prompts
    );
    assert_eq!(run.tapped_at_targets, Some(vec![false, false]));
    assert_eq!(run.root_at_phyrexian, Some(goblins.clone()));
    for goblin in &goblins {
        assert_eq!(taps(&run, *goblin), 1);
    }
    assert!(runner.state().pending_cast.is_none());
}

/// CR 702.180a + CR 601.2h (probe H, RF4-1 closure 3): Channeled Dragonfire
/// cast for its harmonize cost under Djinn + Wort. The harmonize offer excludes
/// the Goblins held for conspire and offers the unheld creature; while it is
/// open only the prompt's own root exists; answering it completes the cast.
#[test]
fn harmonize_does_not_offer_creatures_held_for_conspire() {
    let (mut scenario, victim, _, goblins) = table(true, 2);
    let helper = creature(&mut scenario, "Harmonizing Helper", 3, &RED);
    let dragonfire = scenario
        .add_spell_to_graveyard(P0, "Channeled Dragonfire", false)
        .with_mana_cost(cost(0, &[ManaCostShard::Red]))
        .with_color(RED.to_vec())
        .from_oracle_text_with_keywords(&["Harmonize"], CHANNELED_DRAGONFIRE)
        .id();
    // CR 601.2b: the declaration is offered only when the unreduced harmonize
    // total is payable; the tap reduction is chosen at payment.
    pool(&mut scenario, ManaType::Red, 7);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![goblins.clone()];
    script.harmonize_with = Some(helper);
    let run = cast(&mut runner, dragonfire, &script, &goblins);

    let offer = run
        .harmonize_offer
        .as_ref()
        .expect("reach: harmonize offered");

    assert!(offer.contains(&helper), "the unheld creature is offered");
    for goblin in &goblins {
        assert!(!offer.contains(goblin), "a held Goblin is not offered");
    }
    assert_eq!(run.harmonize_inline_root, Some(goblins.clone()));
    assert_eq!(run.harmonize_published_root, Some(false));
    assert_eq!(taps(&run, helper), 1);
    for goblin in &goblins {
        assert_eq!(taps(&run, *goblin), 1);
    }
}
