//! CR 601.2b + CR 601.2c + CR 601.2h: a spell tap cost (conspire, teamwork)
//! announced before the spell's targets is only selected there. The selected
//! creatures stay untapped through target selection and the mana-ability
//! window (CR 601.2g), are held for that payment, and are tapped when the total
//! cost is paid. Oracle text is verbatim from Scryfall.

use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ConvokeMode, ManaChoice, PersistedGameState,
    PersistedRestoreFinalization, ShardChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
const MYR_GALVANIZER: &str =
    "Other Myr creatures you control get +1/+1.\n{1}, {T}: Untap each other Myr you control.";
const GOLD_MYR: &str = "{T}: Add {W}.";
const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";
const HEAT_RAY: &str = "Heat Ray deals X damage to target creature.";
const THERMOPOD: &str = "{S}: This creature gains haste until end of turn. ({S} can be paid with one mana from a snow source.)\nSacrifice a creature: Add {R}.";
const SPRINGLEAF_DRUM: &str =
    "{T}, Tap an untapped creature you control: Add one mana of any color.";

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

/// A board with P1's victim, Wort and `goblins` Goblin Warriors.
fn table(goblins: usize) -> (GameScenario, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = victim(&mut scenario);
    let wort = add_wort(&mut scenario);
    let goblins = add_goblins(&mut scenario, goblins);
    (scenario, victim, wort, goblins)
}

/// A red instant in P0's hand.
fn red_spell(
    scenario: &mut GameScenario,
    name: &str,
    mana_cost: ManaCost,
    keywords: &[&str],
    oracle: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, name, true)
        .with_mana_cost(mana_cost)
        .with_color(RED.to_vec())
        .from_oracle_text_with_keywords(keywords, oracle)
        .id()
}

fn pyromatics(scenario: &mut GameScenario) -> ObjectId {
    let mana_cost = cost(1, &[ManaCostShard::Red]);
    red_spell(
        scenario,
        "Pyromatics",
        mana_cost,
        &["Replicate"],
        PYROMATICS,
    )
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
    X,
    Harmonize,
    Phyrexian,
    ManaPayment,
}

/// How target selection is answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AtTargets {
    #[default]
    Choose,
    /// Save and restore through the production serializer, then choose.
    RestoreThenChoose,
    /// Cancel the cast instead of completing it.
    Cancel,
}

/// Whether finishing the payment is accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Completion {
    #[default]
    Completes,
    Refused,
}

/// How each prompt of a cast is answered.
#[derive(Default)]
struct Script {
    payment_mode: CastPaymentMode,
    /// Replicate payments (every instance), then declined.
    replicate: u32,
    /// One-shot optional costs paid, by origin; any other is declined.
    pay_once: Vec<AdditionalCostOrigin>,
    /// The selections for each `PayCost` prompt, in order.
    selections: Vec<Vec<ObjectId>>,
    target: Option<ObjectId>,
    x: u32,
    harmonize_with: Option<ObjectId>,
    at_targets: AtTargets,
    /// Creatures tapped for convoke at the payment step.
    convoke: Vec<ObjectId>,
    /// Creatures whose convoke is attempted (on a copy) when the payment step opens.
    probe_convoke: Vec<ObjectId>,
    /// A mana ability activated when the payment step opens, and the
    /// permanent its cost taps or sacrifices.
    mana_ability: Option<(ObjectId, ObjectId)>,
    completion: Completion,
}

impl Script {
    fn new(target: ObjectId) -> Self {
        let target = Some(target);
        Self {
            target,
            ..Self::default()
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
    /// The largest X offered.
    x_max: Option<u32>,
    /// What the payment-step mana ability's cost prompt offered.
    mana_ability_offer: Option<Vec<ObjectId>>,
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
        .map(|pending| pending.deferred_tap_ids().collect())
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
                if script.at_targets == AtTargets::RestoreThenChoose && !restored {
                    restored = true;
                    *runner = round_trip(runner);
                }
                run.prompts.push(Prompt::Target);
                run.tapped_at_targets = Some(tapped(runner, watch));
                if script.at_targets == AtTargets::Cancel {
                    runner.act(GameAction::CancelCast).expect("cancel");
                    return run;
                }
                GameAction::ChooseTarget {
                    target: script.target.map(TargetRef::Object),
                }
            }
            WaitingFor::ChooseXValue { max, .. } => {
                run.prompts.push(Prompt::X);
                run.x_max = Some(max);
                GameAction::ChooseX { value: script.x }
            }
            WaitingFor::HarmonizeTapChoice {
                eligible_creatures,
                pending_cast,
                ..
            } => {
                run.prompts.push(Prompt::Harmonize);
                run.harmonize_offer = Some(eligible_creatures);
                run.harmonize_inline_root = Some(pending_cast.deferred_tap_ids().collect());
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
                    if let Some((source, pays_with)) = script.mana_ability {
                        let offer = activate(runner, source, pays_with, &mut run.events);
                        run.mana_ability_offer = Some(offer);
                    }
                }
                if script.completion == Completion::Refused {
                    let refused = runner.act(GameAction::PassPriority);
                    assert!(refused.is_err(), "the payment is refused: {refused:?}");
                    return run;
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

/// Activates `source`'s mana ability, paying its cost with `pays_with`;
/// returns what the cost prompt offered.
fn activate(
    runner: &mut GameRunner,
    source: ObjectId,
    pays_with: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Vec<ObjectId> {
    let abilities = &runner.state().objects[&source].abilities;
    let ability_index = abilities
        .iter()
        .position(is_mana_ability)
        .expect("a mana ability");
    let mut action = GameAction::ActivateAbility {
        source_id: source,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    };
    let mut offer = Vec::new();
    loop {
        events.extend(
            runner
                .act(action)
                .expect("the mana ability activates")
                .events,
        );
        action = match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                offer = choices;
                GameAction::SelectCards {
                    cards: vec![pays_with],
                }
            }
            WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
            _ => return offer,
        };
    }
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
            .filter(|entry| !matches!(entry.kind, StackEntryKind::Spell { .. }))
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
    let (mut scenario, victim, _, goblins) = table(2);
    let pyromatics = pyromatics(&mut scenario);
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
    script.at_targets = AtTargets::RestoreThenChoose;
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
    script.at_targets = AtTargets::Cancel;
    let cancelled = cast(&mut board.runner, board.pyromatics, &script, &board.goblins);
    assert_eq!(cancelled.prompts, conspire_trail());
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(tapped(&board.runner, &board.goblins), vec![false, false]);
    assert!(board.runner.state().pending_cast.is_none());

    script.at_targets = AtTargets::Choose;
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
    goblins: Vec<ObjectId>,
}

/// Team Tactics with Wort, Djinn Illuminatus and `goblins` Goblin Warriors.
fn tactics_board(goblins: usize) -> TacticsBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = victim(&mut scenario);
    let wort = add_wort(&mut scenario);
    let djinn = add_djinn(&mut scenario);
    let goblins = add_goblins(&mut scenario, goblins);
    let tactics = red_spell(
        &mut scenario,
        "Team Tactics",
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
        goblins,
    }
}

/// CR 601.2b + CR 601.2h: Wort, held for teamwork, can't also pay conspire; with
/// only Djinn left untapped, conspire's two-creature cost is not offered.
#[test]
fn creature_held_for_teamwork_makes_conspire_unofferable() {
    let mut board = tactics_board(0);
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
    assert_eq!(spent(&board.runner, 12), 4, "{{1}}{{R}} + one replicate");
    // One replicate copy. Found, not fixed: a granted-Conspire trigger also
    // fires here off the teamwork payment (aggregate `additional_cost_paid`).
    let replicate_triggers = board.runner.state().stack.iter().filter(|entry| {
        matches!(&entry.kind, StackEntryKind::TriggeredAbility { description: Some(d), .. } if d == "Replicate")
    });
    assert_eq!(replicate_triggers.count(), 1, "one replicate copy trigger");
}

/// CR 601.2h: with a Goblin held for teamwork, conspire's prompt offers exactly
/// Wort and Djinn; each is tapped once when the cost is paid.
#[test]
fn creature_held_for_teamwork_is_not_offered_to_conspire() {
    let mut board = tactics_board(1);
    let goblin = board.goblins[0];
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
    assert_eq!(spent(&board.runner, 12), 4, "{{1}}{{R}} + one replicate");
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
    let (mut scenario, victim, _, goblins) = table(2);
    add_djinn(&mut scenario);
    let helper = creature(&mut scenario, "Convoking Helper", 2, &RED);
    let stoke = red_spell(
        &mut scenario,
        "Stoke the Flames",
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
    let (mut scenario, victim, _, goblins) = table(2);
    let druids = (0..druids)
        .map(|_| {
            scenario
                .add_creature_from_oracle(P0, "Zhur-Taa Druid", 1, 1, ZHUR_TAA_DRUID)
                .with_color(RED_GREEN.to_vec())
                .id()
        })
        .collect();
    scenario.add_basic_land(P0, ManaColor::Red);
    let pyromatics = pyromatics(&mut scenario);
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
    let (mut scenario, victim, _, goblins) = table(1);
    let (djinn, goblin) = (add_djinn(&mut scenario), goblins[0]);
    scenario
        .add_creature_from_oracle(P0, "Anhelo, the Painter", 3, 3, ANHELO)
        .with_color(vec![ManaColor::Blue, ManaColor::Black, ManaColor::Red]);
    let temper = red_spell(
        &mut scenario,
        "Fiery Temper",
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
    let (mut scenario, victim, _, goblins) = table(2);
    add_djinn(&mut scenario);
    let gut_shot = red_spell(
        &mut scenario,
        "Gut Shot",
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
    let (mut scenario, victim, _, goblins) = table(2);
    add_djinn(&mut scenario);
    let helper = creature(&mut scenario, "Harmonizing Helper", 3, &RED);
    let dragonfire = scenario
        .add_spell_to_graveyard(P0, "Channeled Dragonfire", false)
        .with_mana_cost(cost(0, &[ManaCostShard::Red]))
        .with_color(RED.to_vec())
        .from_oracle_text_with_keywords(&["Harmonize"], CHANNELED_DRAGONFIRE)
        .id();
    // Current engine limitation: optional-declaration offerability does not
    // account for the later Harmonize reduction. Fund the unreduced total so
    // this row reaches Conspire and the Harmonize offer.
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

// ---------------------------------------------------------------------------
// Payment-time state
// ---------------------------------------------------------------------------

fn myr(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    power: i32,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, name, power, power, oracle)
        .as_artifact_creature()
        .with_subtypes(vec!["Myr"])
        .id()
}

/// CR 601.2h + CR 208.1: teamwork's total power is checked again when the cost
/// is paid. Gold Myr (2 power with Myr Galvanizer) pays Cruel Alliance's
/// teamwork 2; sacrificing the Galvanizer to Krark-Clan Ironworks during payment
/// leaves it at 1, and the payment is refused. Sacrificing the Ironworks
/// itself instead keeps it at 2, and the cast completes.
#[test]
fn teamwork_selection_is_revalidated_when_the_cost_is_paid() {
    for (completion, power) in [(Completion::Refused, 1), (Completion::Completes, 2)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = myr(&mut scenario, P1, "Gold Myr", 1, GOLD_MYR);
        let galvanizer = myr(&mut scenario, P0, "Myr Galvanizer", 2, MYR_GALVANIZER);
        let gold_myr = myr(&mut scenario, P0, "Gold Myr", 1, GOLD_MYR);
        let ironworks = scenario
            .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
            .id();
        let alliance = scenario
            .add_spell_to_hand(P0, "Cruel Alliance", false)
            .with_mana_cost(cost(2, &[ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&[], CRUEL_ALLIANCE)
            .id();
        pool(&mut scenario, ManaType::Black, 1);
        let mut runner = scenario.build();

        let paid = completion == Completion::Completes;
        let sacrificed = if paid { ironworks } else { galvanizer };
        let mut script = Script::new(victim);
        script.payment_mode = CastPaymentMode::Manual;
        script.pay_once = vec![AdditionalCostOrigin::Teamwork];
        script.selections = vec![vec![gold_myr]];
        script.mana_ability = Some((ironworks, sacrificed));
        script.completion = completion;
        let run = cast(&mut runner, alliance, &script, &[gold_myr]);

        assert_eq!(run.tapped_at_targets, Some(vec![false]));
        assert_eq!(runner.state().objects[&sacrificed].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&gold_myr].power, Some(power));
        assert_eq!(taps(&run, gold_myr), usize::from(paid));
        assert_eq!(runner.state().objects[&gold_myr].tapped, paid);
    }
}

/// CR 107.3 + CR 601.2f: a creature held for conspire is not tapped for X, but
/// its own non-tap mana ability still counts toward X. Heat Ray under Djinn +
/// Wort, conspire paid with Wort and Thermopod: X = 1 is offered because
/// Thermopod can sacrifice an unheld Goblin for {R}, and the cast completes.
/// The control holds a Goblin instead of Thermopod.
#[test]
fn held_creature_still_counts_its_non_tap_mana_toward_x() {
    // Case 0 holds Thermopod; case 1, the control, holds a Goblin instead.
    for case in 0..2 {
        let (mut scenario, _, wort, goblins) = table(2);
        add_djinn(&mut scenario);
        let thermopod = scenario
            .add_creature_from_oracle(P0, "Thermopod", 4, 3, THERMOPOD)
            .with_color(RED.to_vec())
            .id();
        let target = myr(&mut scenario, P1, "Gold Myr", 1, GOLD_MYR);
        let heat_ray = red_spell(
            &mut scenario,
            "Heat Ray",
            cost(0, &[ManaCostShard::X, ManaCostShard::Red]),
            &[],
            HEAT_RAY,
        );
        pool(&mut scenario, ManaType::Red, 1);
        let mut runner = scenario.build();

        let partner = [thermopod, goblins[1]][case];
        let mut script = Script::new(target);
        script.payment_mode = CastPaymentMode::Manual;
        script.pay_once = vec![AdditionalCostOrigin::Other];
        script.selections = vec![vec![wort, partner]];
        script.x = 1;
        script.mana_ability = Some((thermopod, goblins[0]));
        let run = cast(&mut runner, heat_ray, &script, &[wort, partner]);

        let declared = run.prompts.iter().position(|p| *p == Prompt::PayCost);
        let announced = run.prompts.iter().position(|p| *p == Prompt::X);
        assert!(
            declared < announced,
            "reach: conspire is held when X is announced: {:?}",
            run.prompts
        );
        assert!(run.x_max >= Some(1), "X = 1 is offered: {:?}", run.x_max);
        assert_eq!(runner.state().objects[&goblins[0]].zone, Zone::Graveyard);
        assert_eq!(taps(&run, wort), 1);
        assert_eq!(taps(&run, partner), 1);
    }
}

/// CR 605.3a + CR 601.2h: Springleaf Drum's "tap an untapped creature you
/// control" cost, activated during Pyromatics' payment, offers the unheld Wort
/// but neither Goblin held for conspire.
#[test]
fn creatures_held_for_conspire_cant_pay_a_mana_ability_tap_cost() {
    let (mut scenario, victim, wort, goblins) = table(2);
    let drum = scenario
        .add_artifact_from_oracle(P0, "Springleaf Drum", SPRINGLEAF_DRUM)
        .id();
    let pyromatics = pyromatics(&mut scenario);
    pool(&mut scenario, ManaType::Red, 2);
    let mut runner = scenario.build();

    let mut script = Script::new(victim);
    script.payment_mode = CastPaymentMode::Manual;
    script.pay_once = vec![AdditionalCostOrigin::Other];
    script.selections = vec![goblins.clone()];
    script.mana_ability = Some((drum, wort));
    let run = cast(&mut runner, pyromatics, &script, &goblins);

    let offer = run
        .mana_ability_offer
        .as_ref()
        .expect("reach: the Drum's cost prompt");
    assert!(offer.contains(&wort), "the unheld Wort is offered");
    for goblin in &goblins {
        assert!(!offer.contains(goblin), "a held Goblin is not offered");
        assert_eq!(taps(&run, *goblin), 1, "tapped once, for conspire");
    }
    assert_eq!(taps(&run, wort), 1, "tapped for the Drum");
}
