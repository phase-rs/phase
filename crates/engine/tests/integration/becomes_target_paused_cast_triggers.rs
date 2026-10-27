//! CR 601.2c + CR 602.2b + CR 603.2 + CR 603.3: a "becomes the target" trigger
//! (ward, CR 702.21a; Forsaken Wastes; Shapers' Sanctuary) triggers when the
//! target is announced, however the announcement's costs are then paid. Its
//! ability waits until the spell or ability has finished being announced and
//! goes on the stack with that announcement's own triggers (CR 603.3b).
//!
//! Every row is driven through the production reducer with printed Oracle text,
//! across each prompt an announcement can pause on after its targets are
//! declared: interactive mana payment, an additional sacrifice cost, a Phyrexian
//! payment, a damage division, an Aura's enchant target, and a replacement
//! choice raised by a cost.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, PersistedGameState, PersistedRestoreFinalization, ShardChoice, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const FORSAKEN_WASTES: &str = "Players can't gain life.\nAt the beginning of each player's upkeep, that player loses 1 life.\nWhenever this enchantment becomes the target of a spell, that spell's controller loses 5 life.";
const WATERFALL_AERIALIST: &str = "Flying\nWard {2} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {2}.)";
const SHAPERS_SANCTUARY: &str = "Whenever a creature you control becomes the target of a spell or ability an opponent controls, you may draw a card.";
const KAMBAL: &str =
    "Whenever an opponent casts a noncreature spell, that player loses 2 life and you gain 2 life.";
const MONASTERY_SWIFTSPEAR: &str = "Haste\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)";
const MANABARBS: &str =
    "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const SKULKING_GHOST: &str =
    "Flying\nWhen this creature becomes the target of a spell or ability, sacrifice it.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const PYROTECHNICS: &str =
    "Pyrotechnics deals 4 damage divided as you choose among any number of targets.";
const BONE_SPLINTERS: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDestroy target creature.";
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";
const ROD_OF_RUIN: &str = "{3}, {T}: This artifact deals 1 damage to any target.";
const KESSIG_WOLF_RUN: &str =
    "{T}: Add {C}.\n{X}{R}{G}, {T}: Target creature gets +X/+0 and gains trample until end of turn.";
const GOBLIN_BOMBARDMENT: &str =
    "Sacrifice a creature: This enchantment deals 1 damage to any target.";
const HEX_PARASITE: &str = "{X}{B/P}: Remove up to X counters from target permanent. For each counter removed this way, this creature gets +1/+0 until end of turn. ({B/P} can be paid with either {B} or 2 life.)";
const SHREWD_HATCHLING: &str = "This creature enters with four -1/-1 counters on it.\n{U/R}: Target creature can't block this creature this turn.\nWhenever you cast a blue spell, remove a -1/-1 counter from this creature.\nWhenever you cast a red spell, remove a -1/-1 counter from this creature.";
const DOUBLECAST: &str = "When you next cast an instant or sorcery spell this turn, copy that spell. You may choose new targets for the copy.";
const TAUREAN_MAULER: &str = "Changeling (This card is every creature type.)\nWhenever an opponent casts a spell, you may put a +1/+1 counter on this creature.";

// ---------------------------------------------------------------------------
// Board helpers
// ---------------------------------------------------------------------------

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

fn board() -> GameScenario {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc
}

fn three_player_board() -> GameScenario {
    let mut sc = GameScenario::new_n_player(3, 9691);
    sc.at_phase(Phase::PreCombatMain);
    sc
}

fn ward_creature(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_creature(player, "Waterfall Aerialist", 3, 1)
        .from_oracle_text_with_keywords(&["Flying", "Ward"], WATERFALL_AERIALIST)
        .id()
}

fn forsaken_wastes(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_enchantment_from_oracle(player, "Forsaken Wastes", FORSAKEN_WASTES)
        .id()
}

fn mana(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn shock(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(player, "Shock", true, SHOCK)
        .with_mana_cost(mana(&[ManaCostShard::Red], 0))
        .id()
}

fn disenchant(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(player, "Disenchant", true, DISENCHANT)
        .with_mana_cost(mana(&[ManaCostShard::White], 1))
        .id()
}

fn bone_splinters(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(player, "Bone Splinters", false, BONE_SPLINTERS)
        .with_mana_cost(mana(&[ManaCostShard::Black], 0))
        .id()
}

fn pyrotechnics(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    sc.add_spell_to_hand_from_oracle(player, "Pyrotechnics", false, PYROTECHNICS)
        .with_mana_cost(mana(&[ManaCostShard::Red], 4))
        .id()
}

fn pacifism(sc: &mut GameScenario, player: PlayerId) -> ObjectId {
    let mut builder = sc.add_creature_to_hand(player, "Pacifism", 0, 0);
    builder
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(mana(&[ManaCostShard::White], 1))
        .from_oracle_text_with_keywords(&["Enchant creature"], PACIFISM);
    builder.id()
}

fn cast(runner: &mut GameRunner, spell: ObjectId, mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: mode,
        })
        .expect("the cast is announced");
}

fn activate(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation is announced");
}

/// Prompt names seen while driving an announcement.
#[derive(Default)]
struct Answers {
    target: Option<ObjectId>,
    x: Option<u32>,
    sacrifice: Option<ObjectId>,
    /// Tapped for mana at `ManaPayment` before the payment is finished.
    tap: Option<ObjectId>,
    /// Stop (without answering) when this prompt is reached.
    stop_at: Option<&'static str>,
}

fn prompt_name(wf: &WaitingFor) -> String {
    format!("{wf:?}")
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Answers each announcement prompt until priority (or `stop_at`), returning
/// the prompts it answered — the reach guard that the pause actually occurred.
fn drive(runner: &mut GameRunner, answers: &Answers) -> Vec<String> {
    let mut prompts = Vec::new();
    let mut tapped = false;
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        let name = prompt_name(&wf);
        if answers.stop_at == Some(name.as_str()) {
            prompts.push(name);
            return prompts;
        }
        let action = match wf {
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => return prompts,
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(answers.target.expect("a target"))],
            },
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX {
                value: answers.x.expect("an X value"),
            },
            WaitingFor::ManaPayment { .. } => match answers.tap.filter(|_| !tapped) {
                Some(source) => {
                    tapped = true;
                    GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    }
                }
                None => GameAction::PassPriority,
            },
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: vec![answers.sacrifice.expect("a sacrifice")],
            },
            WaitingFor::PhyrexianPayment { ref shards, .. } => GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife; shards.len()],
            },
            WaitingFor::DistributeAmong {
                total, ref targets, ..
            } => GameAction::DistributeAmong {
                distribution: vec![(targets[0].clone(), total)],
            },
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            other => panic!("unexpected announcement prompt {other:?}"),
        };
        prompts.push(name);
        runner.act(action).expect("the prompt accepts its answer");
    }
    panic!("the announcement did not settle");
}

fn triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .count()
}

fn stack_position(runner: &GameRunner, pred: impl Fn(&StackEntryKind, ObjectId) -> bool) -> usize {
    runner
        .state()
        .stack
        .iter()
        .position(|entry| pred(&entry.kind, entry.id))
        .expect("the stack holds the entry")
}

fn spell_on_stack(runner: &GameRunner, spell: ObjectId) -> bool {
    runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.id == spell && matches!(entry.kind, StackEntryKind::Spell { .. }))
}

fn activation_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        matches!(&entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == source)
    })
}

/// Resolves the whole stack, answering ward's "unless pays" with `pay_ward`.
fn resolve_all(runner: &mut GameRunner, pay_ward: bool) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::UnlessPayment { .. } => GameAction::PayUnlessCost { pay: pay_ward },
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        runner.act(action).expect("resolution prompt accepts");
    }
    panic!("the stack did not resolve");
}

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

// ---------------------------------------------------------------------------
// Spells: the issue board and every pause route
// ---------------------------------------------------------------------------

/// The reported board: Forsaken Wastes targeted by a spell paid through the
/// interactive mana payment. CR 603.2: it triggers; the spell's controller
/// loses 5 life when it resolves.
#[test]
fn forsaken_wastes_triggers_when_the_spell_is_paid_interactively() {
    let mut sc = board();
    let wastes = forsaken_wastes(&mut sc, P1);
    let spell = disenchant(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::White, ManaType::Colorless]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(&mut runner, &Answers::default());
    assert_eq!(
        prompts,
        ["ManaPayment"],
        "reach guard: the cast paused at payment"
    );
    assert!(spell_on_stack(&runner, spell));
    assert_eq!(triggers_from(&runner, wastes), 1);

    let life = runner.state().players[P0.0 as usize].life;
    resolve_all(&mut runner, false);
    assert_eq!(runner.state().players[P0.0 as usize].life, life - 5);
}

/// The same cast paid automatically (control): still exactly one trigger, so
/// the paused route did not start double-collecting the unpaused one.
#[test]
fn forsaken_wastes_triggers_once_when_the_spell_is_paid_automatically() {
    let mut sc = board();
    let wastes = forsaken_wastes(&mut sc, P1);
    let spell = disenchant(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::White, ManaType::Colorless]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Auto);
    assert!(drive(&mut runner, &Answers::default()).is_empty());
    assert!(spell_on_stack(&runner, spell));
    assert_eq!(triggers_from(&runner, wastes), 1);
}

/// CR 702.21a: ward triggers for an interactively paid spell, goes above it,
/// and counters it when its controller declines to pay.
#[test]
fn ward_counters_an_interactively_paid_spell_when_unpaid() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
    assert!(spell_on_stack(&runner, spell));
    assert_eq!(triggers_from(&runner, ward), 1);
    assert!(
        stack_position(&runner, |kind, _| matches!(
            kind,
            StackEntryKind::TriggeredAbility { .. }
        )) > stack_position(&runner, |_, id| id == spell),
        "ward goes on the stack above the spell it targets"
    );

    resolve_all(&mut runner, false);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&ward].damage_marked,
        0,
        "Shock was countered"
    );
}

/// CR 702.21a: paying ward lets the interactively paid spell resolve.
#[test]
fn ward_lets_an_interactively_paid_spell_resolve_when_paid() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(
        P0,
        pool(&[ManaType::Red, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert_eq!(triggers_from(&runner, ward), 1);
    resolve_all(&mut runner, true);
    assert_eq!(
        runner.state().objects[&ward].zone,
        Zone::Graveyard,
        "Shock resolved and killed the 3/1"
    );
}

/// An additional sacrifice cost is a pause after target declaration even under
/// automatic payment (the AI's path), and with interactive payment after it.
#[test]
fn ward_triggers_across_an_additional_sacrifice_cost() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut sc = board();
        let ward = ward_creature(&mut sc, P1);
        let fodder = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let spell = bone_splinters(&mut sc, P0);
        sc.with_mana_pool(P0, pool(&[ManaType::Black]));
        let mut runner = sc.build();

        cast(&mut runner, spell, mode);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(ward),
                sacrifice: Some(fodder),
                ..Default::default()
            },
        );
        assert!(
            prompts.iter().any(|p| p == "PayCost"),
            "reach guard ({mode:?}): the sacrifice prompt paused the cast: {prompts:?}"
        );
        assert!(spell_on_stack(&runner, spell), "{mode:?}");
        assert_eq!(triggers_from(&runner, ward), 1, "{mode:?}");
    }
}

/// CR 601.2d: a damage division is announced after targets; ward still
/// triggers, under automatic and interactive payment.
#[test]
fn ward_triggers_across_a_damage_division() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut sc = board();
        let ward = ward_creature(&mut sc, P1);
        let spell = pyrotechnics(&mut sc, P0);
        sc.with_mana_pool(
            P0,
            pool(&[
                ManaType::Red,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ]),
        );
        let mut runner = sc.build();

        cast(&mut runner, spell, mode);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(ward),
                ..Default::default()
            },
        );
        assert!(
            prompts.iter().any(|p| p == "DistributeAmong"),
            "reach guard ({mode:?}): {prompts:?}"
        );
        assert!(spell_on_stack(&runner, spell), "{mode:?}");
        assert_eq!(triggers_from(&runner, ward), 1, "{mode:?}");
    }
}

/// CR 303.4a + CR 702.5a: an Aura spell targets what it will enchant; ward
/// triggers when it is paid interactively.
#[test]
fn ward_triggers_for_an_interactively_paid_aura_spell() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    let aura = pacifism(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::White, ManaType::Colorless]));
    let mut runner = sc.build();

    cast(&mut runner, aura, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
    assert!(spell_on_stack(&runner, aura));
    assert_eq!(triggers_from(&runner, ward), 1);
}

/// CR 616.1: the sacrifice cost raises a replacement-order choice (Rest in
/// Peace and Leyline of the Void both apply), which parks the cast in a typed
/// cost-move root rather than a prompt. Exactly one ward trigger. Control: one
/// replacement, no choice.
#[test]
fn ward_triggers_once_across_a_replacement_choice_raised_by_a_cost() {
    for leylines in [false, true] {
        let mut sc = board();
        let ward = ward_creature(&mut sc, P1);
        sc.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
        if leylines {
            sc.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);
        }
        let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let spell = bone_splinters(&mut sc, P0);
        sc.with_mana_pool(P0, pool(&[ManaType::Black]));
        let mut runner = sc.build();

        cast(&mut runner, spell, CastPaymentMode::Manual);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(ward),
                sacrifice: Some(bears),
                ..Default::default()
            },
        );
        assert_eq!(
            prompts.iter().any(|p| p == "ReplacementChoice"),
            leylines,
            "reach guard: the replacement choice appears only with both replacements: {prompts:?}"
        );
        assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
        assert!(spell_on_stack(&runner, spell));
        assert_eq!(triggers_from(&runner, ward), 1, "leylines={leylines}");
    }
}

/// Hostile: the targeted object leaves between target declaration and the end
/// of the announcement — Skulking Ghost is targeted by Bone Splinters and then
/// sacrificed to pay for it. CR 603.2: it triggered when it became the target;
/// the triggered ability exists independently of its source (CR 113.7a).
#[test]
fn a_target_sacrificed_to_pay_for_the_spell_still_triggered() {
    let mut sc = board();
    let ghost = sc
        .add_creature(P0, "Skulking Ghost", 2, 1)
        .from_oracle_text_with_keywords(&["Flying"], SKULKING_GHOST)
        .id();
    sc.add_creature(P1, "Grizzly Bears", 2, 2);
    let spell = bone_splinters(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Black]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ghost),
            sacrifice: Some(ghost),
            ..Default::default()
        },
    );
    assert!(prompts.iter().any(|p| p == "PayCost"), "{prompts:?}");
    assert_eq!(
        runner.state().objects[&ghost].zone,
        Zone::Graveyard,
        "reach guard: the target left as the cost was paid"
    );
    assert_eq!(triggers_from(&runner, ghost), 1);
}

/// A tap trigger collected by the mana ability's own frame during the payment
/// prompt (Manabarbs) is not collected a second time by the announcement. It
/// triggered during the cast too, so P1 orders it with ward (CR 603.3b).
#[test]
fn a_mana_ability_trigger_during_interactive_payment_triggers_once() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let barbs = sc
        .add_enchantment_from_oracle(P1, "Manabarbs", MANABARBS)
        .id();
    let mountain = sc.add_basic_land(P0, ManaColor::Red);
    let spell = shock(&mut sc, P0);
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            tap: Some(mountain),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment", "ManaPayment"]);
    assert!(runner.state().objects[&mountain].tapped, "reach guard");
    assert!(spell_on_stack(&runner, spell));
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "P1 orders ward and Manabarbs together, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert_eq!(triggers.len(), 2, "one ward and ONE Manabarbs trigger");
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order the batch");
    assert_eq!(triggers_from(&runner, ward), 1);
    assert_eq!(triggers_from(&runner, barbs), 1);
}

/// The paused announcement survives the production save/restore pipeline.
#[test]
fn ward_triggers_after_a_save_and_restore_at_the_payment_prompt() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            stop_at: Some("ManaPayment"),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
    let mut runner = round_trip(&runner);
    drive(&mut runner, &Answers::default());
    assert!(spell_on_stack(&runner, spell));
    assert_eq!(triggers_from(&runner, ward), 1);
}

/// CR 733.1: no abilities trigger as a result of an undone action. A cast
/// cancelled at the payment prompt leaves no ward trigger; casting it again
/// produces exactly one.
#[test]
fn a_cancelled_cast_triggers_nothing_and_a_recast_triggers_once() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            stop_at: Some("ManaPayment"),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
    runner
        .act(GameAction::CancelCast)
        .expect("cancel at payment");
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().deferred_triggers.is_empty());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);

    cast(&mut runner, spell, CastPaymentMode::Manual);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert!(spell_on_stack(&runner, spell));
    assert_eq!(triggers_from(&runner, ward), 1);
}

// ---------------------------------------------------------------------------
// One batch with the spell's own cast triggers (CR 603.3b)
// ---------------------------------------------------------------------------

/// P1 controls both ward and Kambal, Consul of Allocation. Ward triggered at
/// target declaration, Kambal when the spell became cast (CR 601.2i); both are
/// put on the stack the next time a player would receive priority as ONE
/// batch, so P1 orders them (CR 603.3b).
#[test]
fn ward_and_a_cast_trigger_of_the_same_player_are_ordered_together() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let kambal = sc
        .add_creature(P1, "Kambal, Consul of Allocation", 2, 3)
        .from_oracle_text(KAMBAL)
        .id();
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
    let WaitingFor::OrderTriggers {
        player, triggers, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "P1 orders ward and Kambal together, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert_eq!(triggers.len(), 2);
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order the batch");
    assert_eq!(triggers_from(&runner, ward), 1);
    assert_eq!(triggers_from(&runner, kambal), 1);
}

/// APNAP across players: the caster's prowess and the opponent's ward are put
/// on the stack in the same batch, active player's first (CR 603.3b), so ward
/// is on top.
#[test]
fn ward_and_the_casters_prowess_go_on_the_stack_in_apnap_order() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let swiftspear = sc
        .add_creature(P0, "Monastery Swiftspear", 1, 2)
        .from_oracle_text_with_keywords(&["Haste", "Prowess"], MONASTERY_SWIFTSPEAR)
        .id();
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert_eq!(triggers_from(&runner, ward), 1);
    assert_eq!(triggers_from(&runner, swiftspear), 1);
    let ward_at = stack_position(
        &runner,
        |kind, _| matches!(kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ward),
    );
    let prowess_at = stack_position(
        &runner,
        |kind, _| matches!(kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == swiftspear),
    );
    assert!(ward_at > prowess_at, "ward (non-active player) is on top");
}

/// CR 603.7b + CR 603.3b: Doublecast's delayed trigger watches for the
/// caster's next instant or sorcery. It triggers on that spell's `SpellCast`
/// and joins the same batch as the opponent's target and cast triggers, even
/// when that opponent's two triggers make the settlement pause for an ordering
/// choice before any player receives priority. Control: without Taurean
/// Mauler, P1 has one trigger and nothing pauses.
#[test]
fn a_delayed_cast_trigger_joins_a_batch_that_pauses_for_ordering() {
    for with_mauler in [true, false] {
        let mut sc = board();
        let sanctuary = sc
            .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
            .id();
        let mauler = with_mauler.then(|| {
            sc.add_creature(P1, "Taurean Mauler", 2, 2)
                .from_oracle_text_with_keywords(&["Changeling"], TAUREAN_MAULER)
                .id()
        });
        let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let doublecast = sc
            .add_spell_to_hand_from_oracle(P0, "Doublecast", false, DOUBLECAST)
            .with_mana_cost(mana(&[ManaCostShard::Red, ManaCostShard::Red], 0))
            .id();
        let spell = shock(&mut sc, P0);
        sc.with_mana_pool(P0, pool(&[ManaType::Red, ManaType::Red, ManaType::Red]));
        let mut runner = sc.build();

        cast(&mut runner, doublecast, CastPaymentMode::Auto);
        resolve_all(&mut runner, false);
        assert_eq!(
            runner.state().objects[&doublecast].zone,
            Zone::Graveyard,
            "reach guard: Doublecast resolved and left its delayed trigger"
        );

        cast(&mut runner, spell, CastPaymentMode::Manual);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(bears),
                ..Default::default()
            },
        );
        assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
        if with_mauler {
            let WaitingFor::OrderTriggers { player, .. } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "reach guard: P1 orders Sanctuary and Mauler, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(player, P1);
            runner
                .act(GameAction::OrderTriggers { order: vec![0, 1] })
                .expect("order P1's triggers");
        }
        assert!(spell_on_stack(&runner, spell));
        assert_eq!(triggers_from(&runner, sanctuary), 1);
        if let Some(mauler) = mauler {
            assert_eq!(triggers_from(&runner, mauler), 1);
        }
        assert_eq!(
            triggers_from(&runner, doublecast),
            1,
            "with_mauler={with_mauler}: exactly one Doublecast copy trigger"
        );
        let copy_at = stack_position(
            &runner,
            |kind, _| matches!(kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == doublecast),
        );
        let sanctuary_at = stack_position(
            &runner,
            |kind, _| matches!(kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == sanctuary),
        );
        assert!(
            copy_at < sanctuary_at,
            "one APNAP batch: the active player's delayed trigger is below the opponent's"
        );
    }
}

// ---------------------------------------------------------------------------
// Activated abilities
// ---------------------------------------------------------------------------

/// A sacrifice cost prompt after target declaration (Goblin Bombardment).
#[test]
fn ward_triggers_for_an_activation_paused_on_its_sacrifice_cost() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let bombardment = sc
        .add_enchantment_from_oracle(P0, "Goblin Bombardment", GOBLIN_BOMBARDMENT)
        .id();
    let fodder = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = sc.build();

    activate(&mut runner, bombardment, 0);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            sacrifice: Some(fodder),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "PayCost"]);
    assert!(activation_on_stack(&runner, bombardment));
    assert_eq!(triggers_from(&runner, ward), 1);
}

/// A Phyrexian payment prompt after target declaration (Hex Parasite). The
/// negative control: Forsaken Wastes cares only about spells.
#[test]
fn an_activation_paused_on_a_phyrexian_payment_triggers_ward_but_not_a_spell_trigger() {
    for wastes in [false, true] {
        let mut sc = board();
        let observer = if wastes {
            forsaken_wastes(&mut sc, P1)
        } else {
            ward_creature(&mut sc, P1)
        };
        let parasite = sc
            .add_creature(P0, "Hex Parasite", 1, 1)
            .as_artifact()
            .from_oracle_text(HEX_PARASITE)
            .id();
        let mut runner = sc.build();

        activate(&mut runner, parasite, 0);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(observer),
                x: Some(0),
                ..Default::default()
            },
        );
        assert!(
            prompts.iter().any(|p| p == "PhyrexianPayment"),
            "reach guard: {prompts:?}"
        );
        assert!(activation_on_stack(&runner, parasite));
        assert_eq!(triggers_from(&runner, observer), usize::from(!wastes));
    }
}

/// Controls: activations whose targets and payment complete in one action
/// (no pause after declaration) — unchanged, exactly one ward trigger.
#[test]
fn unpaused_activations_still_trigger_ward_once() {
    // Rod of Ruin: {3}, {T}.
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let rod = sc
        .add_artifact_from_oracle(P0, "Rod of Ruin", ROD_OF_RUIN)
        .id();
    sc.with_mana_pool(P0, pool(&[ManaType::Colorless; 3]));
    let mut runner = sc.build();
    activate(&mut runner, rod, 0);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert!(activation_on_stack(&runner, rod));
    assert_eq!(triggers_from(&runner, ward), 1, "Rod of Ruin");

    // Shrewd Hatchling: a hybrid {U/R} activation cost.
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let hatchling = sc
        .add_creature(P0, "Shrewd Hatchling", 6, 6)
        .from_oracle_text(SHREWD_HATCHLING)
        .id();
    sc.with_mana_pool(P0, pool(&[ManaType::Blue]));
    let mut runner = sc.build();
    let index = runner.state().objects[&hatchling]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, engine::types::ability::AbilityKind::Activated))
        .expect("the {U/R} activation");
    activate(&mut runner, hatchling, index);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            ..Default::default()
        },
    );
    assert!(activation_on_stack(&runner, hatchling));
    assert_eq!(triggers_from(&runner, ward), 1, "Shrewd Hatchling");

    // Kessig Wolf Run: X announced before its single legal target.
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let run = sc
        .add_land_from_oracle(P0, "Kessig Wolf Run", KESSIG_WOLF_RUN)
        .id();
    sc.with_mana_pool(
        P0,
        pool(&[ManaType::Red, ManaType::Green, ManaType::Colorless]),
    );
    let mut runner = sc.build();
    activate(&mut runner, run, 1);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            x: Some(1),
            ..Default::default()
        },
    );
    assert!(prompts.iter().any(|p| p == "ChooseXValue"), "{prompts:?}");
    assert!(activation_on_stack(&runner, run));
    assert_eq!(triggers_from(&runner, ward), 1, "Kessig Wolf Run");
}

/// CR 733.1: an activation cancelled at its sacrifice prompt triggers nothing;
/// activating it again triggers ward once.
#[test]
fn a_cancelled_activation_triggers_nothing_and_a_reactivation_triggers_once() {
    let mut sc = board();
    let ward = ward_creature(&mut sc, P1);
    let bombardment = sc
        .add_enchantment_from_oracle(P0, "Goblin Bombardment", GOBLIN_BOMBARDMENT)
        .id();
    let fodder = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = sc.build();

    activate(&mut runner, bombardment, 0);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            stop_at: Some("PayCost"),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "PayCost"]);
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the activation");
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().deferred_triggers.is_empty());

    activate(&mut runner, bombardment, 0);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            sacrifice: Some(fodder),
            ..Default::default()
        },
    );
    assert!(activation_on_stack(&runner, bombardment));
    assert_eq!(triggers_from(&runner, ward), 1);
}

// ---------------------------------------------------------------------------
// The announcing player leaves the game (CR 800.4)
// ---------------------------------------------------------------------------

/// CR 800.4a + CR 603.2: the caster concedes at the payment prompt. The cast
/// is abandoned, but its target was declared: Shapers' Sanctuary triggered and
/// is put on the stack before any player receives priority.
#[test]
fn a_caster_leaving_at_the_payment_prompt_still_puts_the_target_trigger_on_the_stack() {
    let mut sc = three_player_board();
    let sanctuary = sc
        .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
        .id();
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(bears),
            stop_at: Some("ManaPayment"),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes mid-payment");
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert!(!spell_on_stack(&runner, spell), "the announcement is gone");
    assert_eq!(
        triggers_from(&runner, sanctuary),
        1,
        "Sanctuary is on the stack, not merely deferred: {:?} / {:?}",
        runner.state().waiting_for,
        runner.state().deferred_triggers.len()
    );
    assert!(runner.state().deferred_triggers.is_empty());
}

/// CR 800.4a + CR 603.2: the activator concedes at its sacrifice prompt. The
/// activation is abandoned; the surviving player's target trigger is not.
#[test]
fn an_activator_leaving_at_the_cost_prompt_still_puts_the_target_trigger_on_the_stack() {
    let mut sc = three_player_board();
    let sanctuary = sc
        .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
        .id();
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bombardment = sc
        .add_enchantment_from_oracle(P0, "Goblin Bombardment", GOBLIN_BOMBARDMENT)
        .id();
    sc.add_creature(P0, "Fodder", 1, 1);
    let mut runner = sc.build();

    activate(&mut runner, bombardment, 0);
    let prompts = drive(
        &mut runner,
        &Answers {
            target: Some(bears),
            stop_at: Some("PayCost"),
            ..Default::default()
        },
    );
    assert_eq!(prompts, ["TargetSelection", "PayCost"]);

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes mid-activation");
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert!(!activation_on_stack(&runner, bombardment));
    assert_eq!(triggers_from(&runner, sanctuary), 1);
    assert!(runner.state().deferred_triggers.is_empty());
}

/// CR 800.4d + CR 800.4f: ward controlled by a surviving player still triggers
/// when the caster leaves; its "unless that player pays" is not paid by the
/// departed player, and it resolves without asking them.
#[test]
fn ward_of_a_surviving_player_resolves_after_the_caster_leaves() {
    let mut sc = three_player_board();
    let ward = ward_creature(&mut sc, P1);
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Manual);
    drive(
        &mut runner,
        &Answers {
            target: Some(ward),
            stop_at: Some("ManaPayment"),
            ..Default::default()
        },
    );
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes mid-payment");
    assert_eq!(triggers_from(&runner, ward), 1);
    resolve_all(&mut runner, true);
    assert!(runner.state().stack.is_empty());
    assert!(!runner.state().players[P1.0 as usize].is_eliminated);
    assert!(!runner.state().players[P2.0 as usize].is_eliminated);
}

/// CR 800.4d: a triggered ability that would be controlled by a player who has
/// left the game isn't put on the stack. Shapers' Sanctuary's controller
/// concedes while the caster is paying; when the cast finishes, the staged
/// trigger is retired rather than published.
#[test]
fn a_departed_observers_staged_trigger_is_not_put_on_the_stack() {
    let mut sc = three_player_board();
    let sanctuary = sc
        .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
        .id();
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(P0, pool(&[ManaType::Red]));
    let mut runner = sc.build();

    // Control first: P1 stays, and Sanctuary triggers for its own creature.
    let mut control = GameRunner::from_state(runner.state().clone());
    cast(&mut control, spell, CastPaymentMode::Manual);
    drive(
        &mut control,
        &Answers {
            target: Some(bears),
            ..Default::default()
        },
    );
    assert_eq!(triggers_from(&control, sanctuary), 1, "reach guard");

    cast(&mut runner, spell, CastPaymentMode::Manual);
    drive(
        &mut runner,
        &Answers {
            target: Some(bears),
            stop_at: Some("ManaPayment"),
            ..Default::default()
        },
    );
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes while P0 pays");
    assert!(runner.state().players[P1.0 as usize].is_eliminated);
    drive(&mut runner, &Answers::default());
    assert!(spell_on_stack(&runner, spell), "P0's cast still completes");
    assert_eq!(triggers_from(&runner, sanctuary), 0);
    assert!(runner
        .state()
        .deferred_triggers
        .iter()
        .all(|context| context.pending.controller != P1));
}

/// CR 800.4a + CR 603.3: the caster concedes while a replacement choice raised
/// by its sacrifice cost is open (Rest in Peace and Leyline of the Void), so
/// the cast is parked in a typed cost root, not a prompt. Sanctuary's staged
/// trigger is on the stack before any player receives priority. Control: the
/// same board answered normally triggers Sanctuary once.
#[test]
fn a_caster_leaving_at_a_cost_replacement_choice_still_puts_the_target_trigger_on_the_stack() {
    for concede in [false, true] {
        let mut sc = three_player_board();
        let sanctuary = sc
            .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
            .id();
        sc.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
        sc.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);
        let target = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let fodder = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let spell = bone_splinters(&mut sc, P0);
        sc.with_mana_pool(P0, pool(&[ManaType::Black]));
        let mut runner = sc.build();

        cast(&mut runner, spell, CastPaymentMode::Manual);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(target),
                sacrifice: Some(fodder),
                stop_at: concede.then_some("ReplacementChoice"),
                ..Default::default()
            },
        );
        assert!(
            prompts.iter().any(|p| p == "ReplacementChoice"),
            "reach guard: the cost raised a replacement choice: {prompts:?}"
        );
        if concede {
            runner
                .act(GameAction::Concede { player_id: P0 })
                .expect("P0 concedes at the replacement choice");
            assert!(runner.state().players[P0.0 as usize].is_eliminated);
            assert!(!spell_on_stack(&runner, spell), "the announcement is gone");
            assert!(runner.state().deferred_triggers.is_empty());
            assert!(
                serde_json::to_value(runner.state())
                    .unwrap()
                    .get("spell_cast_reversal")
                    .is_none(),
                "the abandoned caster retains no payment undo authority"
            );
        } else {
            assert!(spell_on_stack(&runner, spell));
        }
        assert_eq!(
            triggers_from(&runner, sanctuary),
            1,
            "concede={concede}: Sanctuary is on the stack, wait {:?}",
            runner.state().waiting_for
        );
    }
}

/// CR 603.3 + CR 117.5: the caster responds to another player's spell and
/// concedes at the payment prompt. That spell stays on the stack as a passive
/// object; Sanctuary's trigger still goes on the stack, above it, before any
/// player receives priority. Control: no spell underneath.
#[test]
fn a_caster_leaving_above_another_spell_still_puts_the_target_trigger_on_the_stack() {
    for underneath in [false, true] {
        let mut sc = three_player_board();
        let sanctuary = sc
            .add_enchantment_from_oracle(P1, "Shapers' Sanctuary", SHAPERS_SANCTUARY)
            .id();
        let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let p2_shock = shock(&mut sc, P2);
        let p0_shock = shock(&mut sc, P0);
        sc.with_mana_pool(P0, pool(&[ManaType::Red]));
        sc.with_mana_pool(P2, pool(&[ManaType::Red]));
        let mut runner = sc.build();

        if underneath {
            // P0 and P1 pass; P2 casts Shock at P1, then passes back to P0.
            runner.act(GameAction::PassPriority).expect("P0 passes");
            runner.act(GameAction::PassPriority).expect("P1 passes");
            cast(&mut runner, p2_shock, CastPaymentMode::Auto);
            if matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ) {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("P2 targets P1");
            }
            assert!(spell_on_stack(&runner, p2_shock), "reach guard");
            runner.act(GameAction::PassPriority).expect("P2 passes");
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 },
                "reach guard: P0 may respond"
            );
        }

        cast(&mut runner, p0_shock, CastPaymentMode::Manual);
        let prompts = drive(
            &mut runner,
            &Answers {
                target: Some(bears),
                stop_at: Some("ManaPayment"),
                ..Default::default()
            },
        );
        assert_eq!(prompts, ["TargetSelection", "ManaPayment"]);
        runner
            .act(GameAction::Concede { player_id: P0 })
            .expect("P0 concedes mid-payment");
        assert!(!spell_on_stack(&runner, p0_shock));
        assert_eq!(spell_on_stack(&runner, p2_shock), underneath);
        assert_eq!(
            triggers_from(&runner, sanctuary),
            1,
            "underneath={underneath}: Sanctuary is on the stack, wait {:?}",
            runner.state().waiting_for
        );
        assert!(runner.state().deferred_triggers.is_empty());
        if underneath {
            assert!(
                stack_position(&runner, |kind, _| matches!(
                    kind,
                    StackEntryKind::TriggeredAbility { .. }
                )) > stack_position(&runner, |_, id| id == p2_shock),
                "Sanctuary goes above P2's Shock"
            );
        }
    }
}

const TOSHIRO_UMEZAWA: &str = "Bushido 1 (Whenever this creature blocks or becomes blocked, it gets +1/+1 until end of turn.)\nWhenever a creature an opponent controls dies, you may cast target instant card from your graveyard. If that spell would be put into a graveyard, exile it instead.";

/// CR 601.2a + CR 601.2c: an announcement that begins part-way through an
/// action. Toshiro Umezawa's trigger offers Disenchant from P0's graveyard
/// while it resolves; the offered cast automatically targets Forsaken Wastes
/// (its only legal target) and pauses at payment in the same action that
/// resolved the trigger. Forsaken Wastes triggers; P0 loses 5 life.
#[test]
fn a_cast_offered_by_a_resolving_ability_keeps_its_target_trigger() {
    let mut sc = board();
    let wastes = forsaken_wastes(&mut sc, P1);
    let victim = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    sc.add_creature(P0, "Toshiro Umezawa", 2, 2)
        .from_oracle_text_with_keywords(&["Bushido"], TOSHIRO_UMEZAWA);
    let offered = sc
        .add_spell_to_graveyard(P0, "Disenchant", true)
        .from_oracle_text(DISENCHANT)
        .with_mana_cost(mana(&[ManaCostShard::White], 1))
        .id();
    let spell = shock(&mut sc, P0);
    sc.with_mana_pool(
        P0,
        pool(&[ManaType::Red, ManaType::White, ManaType::Colorless]),
    );
    let mut runner = sc.build();

    cast(&mut runner, spell, CastPaymentMode::Auto);
    drive(
        &mut runner,
        &Answers {
            target: Some(victim),
            ..Default::default()
        },
    );
    let mut prompts = Vec::new();
    for _ in 0..32 {
        let wf = runner.state().waiting_for.clone();
        let action = match wf {
            WaitingFor::Priority { .. } if spell_on_stack(&runner, offered) => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::TriggerTargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(offered)],
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::CastOffer { .. } => GameAction::GraveyardPaidCastChoice {
                choice: engine::types::actions::CastChoice::Cast,
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        prompts.push(prompt_name(&runner.state().waiting_for));
        runner.act(action).expect("the prompt accepts its answer");
    }
    assert!(
        prompts.iter().any(|p| p == "CastOffer") && prompts.iter().any(|p| p == "ManaPayment"),
        "reach guard: the offered cast paused at payment: {prompts:?}"
    );
    assert!(spell_on_stack(&runner, offered));
    assert_eq!(triggers_from(&runner, wastes), 1);

    let life = runner.state().players[P0.0 as usize].life;
    resolve_all(&mut runner, false);
    assert_eq!(runner.state().players[P0.0 as usize].life, life - 5);
}
