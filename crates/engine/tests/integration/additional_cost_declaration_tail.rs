//! CR 601.2b + CR 601.2f + CR 601.2h + CR 118.9d: every additional-cost
//! declaration of a cast goes through one tail. A cast variant's non-mana
//! residual (escape's exile, retrace/jump-start's discard, a compound
//! alternative cost's rider) is owed whichever additional costs the spell also
//! declares, and a matching Defiler's optional life payment is offered whichever
//! of them the spell has (phase-rs/phase#9780).
//!
//! Before, every additional-cost route (an object's own required cost, kicker,
//! the keyword queue, optional and Choice costs, and the casts that declare
//! them before their targets) returned before the residual and the Defiler, so
//! Underworld Breach's escape exiled nothing and Defiler of Flesh was never
//! offered for Arbiter of Woe, Gatekeeper of Malakir or Darkstar Augur.
//!
//! Oracle text below is from Scryfall.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, PlayerFilter, QuantityExpr,
    QuantityModification, ReplacementDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use crate::blitz_em_dash_graveyard_cast::{add_permission_source, MULDROTHA};

const VILLAGE_RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";
const BOLAS_CITADEL: &str = "You may look at the top card of your library any time.\nYou may play lands and cast spells from the top of your library. If you cast a spell this way, pay life equal to its mana value rather than pay its mana cost.\n{T}, Sacrifice ten nonland permanents: Each opponent loses 10 life.";
const UNDERWORLD_BREACH: &str = "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard. (You may cast cards from your graveyard for their escape cost.)\nAt the beginning of the end step, sacrifice this enchantment.";
const ARBITER_OF_WOE: &str = "As an additional cost to cast this spell, sacrifice a creature.\nFlying\nWhen this creature enters, each opponent discards a card and loses 2 life. You draw a card and gain 2 life.";
const DEFILER_OF_FLESH: &str = "Menace\nAs an additional cost to cast black permanent spells, you may pay 2 life. Those spells cost {B} less to cast if you paid life this way. This effect reduces only the amount of black mana you pay.\nWhenever you cast a black permanent spell, target creature you control gets +1/+1 and gains menace until end of turn.";
const GATEKEEPER_OF_MALAKIR: &str = "Kicker {B} (You may pay an additional {B} as you cast this spell.)\nWhen this creature enters, if it was kicked, target player sacrifices a creature of their choice.";
const DARKSTAR_AUGUR: &str = "Offspring {B} (You may pay an additional {B} as you cast this spell. If you do, when this creature enters, create a 1/1 token copy of it.)\nFlying\nAt the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to its mana value.";
const GROW_FROM_THE_ASHES: &str = "Kicker {2} (You may pay an additional {2} as you cast this spell.)\nSearch your library for a basic land card, put it onto the battlefield, then shuffle. If this spell was kicked, instead search your library for two basic land cards, put them onto the battlefield, then shuffle.";
const VAMPIRE_NIGHTHAWK: &str = "Flying\nDeathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink (Damage dealt by this creature also causes you to gain that much life.)";
const MORKRUT_BEHEMOTH: &str = "As an additional cost to cast this spell, sacrifice a creature or pay {1}{B}.\nMenace (This creature can't be blocked except by two or more creatures.)";
const BONE_SHARDS: &str = "As an additional cost to cast this spell, sacrifice a creature or discard a card.\nDestroy target creature or planeswalker.";
const TERROR_OF_THE_PEAKS: &str = "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.\nWhenever another creature you control enters, this creature deals damage equal to that creature's power to any target.";
const GOREX_THE_TOMBSHELL: &str = "As an additional cost to cast this spell, you may exile any number of creature cards from your graveyard. This spell costs {2} less to cast for each card exiled this way.\nDeathtouch\nWhenever Gorex attacks or dies, choose a card at random exiled with Gorex and put that card into its owner's hand.";
const ARCO_FLAGELLANT: &str = "Squad {2} (As an additional cost to cast this spell, you may pay {2} any number of times. When this creature enters, create that many tokens that are copies of it.)\nThis creature can't block.\nEndurant \u{2014} Pay 3 life: This creature gains indestructible until end of turn.";
const CHANGING_LOYALTY: &str = "Flash\nReplicate {2} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies. Copies become tokens.)\nEnchant creature\nWhen enchanted creature dies, return it to the battlefield under your control.";
const BLOODCHIEFS_THIRST: &str = "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\nDestroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker.";
const AETHER_TIDE: &str = "As an additional cost to cast this spell, discard X creature cards.\nReturn X target creatures to their owners' hands.";
const AUDACIOUS_SWAP: &str = "Casualty 2 (As you cast this spell, you may sacrifice a creature with power 2 or greater. When you do, copy this spell and you may choose a new target for the copy.)\nThe owner of target nonenchantment permanent shuffles it into their library, then exiles the top card of their library. If it's a land card, they put it onto the battlefield. Otherwise, they may cast it without paying its mana cost.";
const INSCRIPTION_OF_ABUNDANCE: &str = "Kicker {2}{G}\nChoose one. If this spell was kicked, choose any number instead.\n\u{2022} Put two +1/+1 counters on target creature.\n\u{2022} Target player gains X life, where X is the greatest power among creatures they control.\n\u{2022} Target creature you control fights target creature you don't control.";
const ROLLING_THUNDER: &str =
    "Rolling Thunder deals X damage divided as you choose among any number of targets.";
const PYROMATICS: &str = "Replicate {1}{R} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nPyromatics deals 1 damage to any target.";

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: shards.to_vec(),
    }
}

fn mana(kinds: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .flat_map(|(kind, n)| {
            std::iter::repeat_with(move || ManaUnit::new(*kind, ObjectId(0), false, vec![]))
                .take(*n)
        })
        .collect()
}

/// Defiler of Flesh on P0's battlefield with its parsed statics (not its cast
/// trigger, which would add an unrelated target prompt).
fn add_defiler_of_flesh(scenario: &mut GameScenario) -> ObjectId {
    let parsed = parse_oracle_text(
        DEFILER_OF_FLESH,
        "Defiler of Flesh",
        &["Menace".into()],
        &["Creature".into()],
        &["Phyrexian".into(), "Horror".into()],
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("DefilerCostReduction")),
        "reach: Defiler of Flesh parses to a Defiler cost reduction"
    );
    let mut defiler = scenario.add_creature(P0, "Defiler of Flesh", 4, 4);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
    defiler.id()
}

fn add_breach(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Underworld Breach", UNDERWORLD_BREACH);
}

/// Three creature cards in P0's graveyard for escape's "exile three other cards".
fn add_fodder(scenario: &mut GameScenario) -> Vec<ObjectId> {
    (0..3)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Fodder {i}"), 1, 1)
                .id()
        })
        .collect()
}

/// How each prompt of a cast is answered.
#[derive(Clone, Default)]
struct Answers {
    defiler: bool,
    /// Kicker / Optional prompts (and the first Repeatable offer).
    optional: bool,
    /// How many times a Repeatable optional cost is paid.
    repeat: u32,
    /// `true` picks a Choice's preferred branch, `false` its fallback.
    choice_preferred: bool,
    escape: bool,
    targets: Vec<ObjectId>,
    x: u32,
    modes: Vec<usize>,
    /// Objects the caster prefers when a cost asks for a selection.
    picks: Vec<ObjectId>,
    distribution: Vec<(ObjectId, u32)>,
    replacement_index: Option<usize>,
    branch_index: Option<usize>,
}

/// What a cast did until it stopped asking.
struct Trail {
    prompts: Vec<&'static str>,
    events: Vec<GameEvent>,
    refused: bool,
}

impl Trail {
    fn count(&self, kind: &str) -> usize {
        self.prompts.iter().filter(|p| **p == kind).count()
    }

    /// CR 400.7: each `ids` member that changed zones to exile, counted per move.
    fn exiles(&self, ids: &[ObjectId]) -> usize {
        self.events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. }
                    if ids.contains(object_id))
            })
            .count()
    }
}

fn cast_action(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn cast(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> Trail {
    let first = cast_action(runner, spell);
    drive(runner, first, answers)
}

fn drive(runner: &mut GameRunner, first: GameAction, answers: &Answers) -> Trail {
    let mut trail = Trail {
        prompts: Vec::new(),
        events: Vec::new(),
        refused: false,
    };
    match runner.act(first) {
        Ok(result) => trail.events.extend(result.events),
        Err(_) => {
            trail.refused = true;
            return trail;
        }
    }
    let mut targets = answers.targets.iter().copied();
    let mut repeats_paid = 0;
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::CastingVariantChoice { options, .. } => {
                let index = options
                    .iter()
                    .position(|o| (o.variant == CastingVariant::Escape) == answers.escape)
                    .expect("reach: the requested casting variant is offered");
                GameAction::ChooseCastingVariant { index }
            }
            WaitingFor::AlternativeCastChoice { .. } => GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            },
            WaitingFor::DefilerPayment { .. } => GameAction::DecideOptionalCost {
                pay: answers.defiler,
            },
            WaitingFor::OptionalCostChoice { cost, .. } => {
                let pay = match cost {
                    engine::types::ability::AdditionalCost::Choice(..) => answers.choice_preferred,
                    engine::types::ability::AdditionalCost::Optional {
                        repeatability:
                            engine::types::ability::AdditionalCostRepeatability::Repeatable,
                        ..
                    } => {
                        let pay = repeats_paid < answers.repeat;
                        repeats_paid += u32::from(pay);
                        pay
                    }
                    _ => answers.optional,
                };
                GameAction::DecideOptionalCost { pay }
            }
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: answers.x },
            WaitingFor::ModeChoice { .. } => GameAction::SelectModes {
                indices: answers.modes.clone(),
            },
            WaitingFor::TargetSelection { selection, .. } => GameAction::ChooseTarget {
                target: targets
                    .next()
                    .map(TargetRef::Object)
                    .or_else(|| selection.current_legal_targets.first().cloned()),
            },
            WaitingFor::DistributeAmong { .. } => GameAction::DistributeAmong {
                distribution: answers
                    .distribution
                    .iter()
                    .map(|(id, n)| (TargetRef::Object(*id), *n))
                    .collect(),
            },
            WaitingFor::PayCost { choices, count, .. } => {
                let mut cards: Vec<ObjectId> = answers
                    .picks
                    .iter()
                    .copied()
                    .filter(|pick| choices.contains(pick))
                    .take(count)
                    .collect();
                for choice in &choices {
                    if cards.len() >= count {
                        break;
                    }
                    if !cards.contains(choice) {
                        cards.push(*choice);
                    }
                }
                GameAction::SelectCards { cards }
            }
            WaitingFor::ReplacementChoice { .. } if answers.replacement_index.is_some() => {
                GameAction::ChooseReplacement {
                    index: answers.replacement_index.unwrap(),
                }
            }
            WaitingFor::ChooseOneOfBranch { .. } if answers.branch_index.is_some() => {
                GameAction::ChooseBranch {
                    index: answers.branch_index.unwrap(),
                }
            }
            _ => break,
        };
        trail.prompts.push(runner.waiting_for_kind());
        match runner.act(action) {
            Ok(result) => trail.events.extend(result.events),
            Err(_) => {
                trail.refused = true;
                break;
            }
        }
    }
    trail
}

/// The cast's completion boundary: the spell is on the stack and P0 holds
/// priority (CR 601.2i), nothing left to answer.
fn assert_cast_completes(runner: &GameRunner, spell: ObjectId, trail: &Trail) {
    assert!(
        !trail.refused,
        "the cast must not be refused: {:?}",
        trail.prompts
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast must end with P0 holding priority, got {:?} after {:?}",
        runner.state().waiting_for,
        trail.prompts
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "the spell is on the stack"
    );
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

// ---------------------------------------------------------------------------
// Required additional cost (Village Rites, Arbiter of Woe)
// ---------------------------------------------------------------------------

/// Regression row (already correct at the base): CR 601.2f + CR 118.9 — Bolas's
/// Citadel's "pay life equal to its mana value" is the alternative cost, and
/// Village Rites' sacrifice is added to it: sacrifice + 1 life.
#[test]
fn citadel_cast_of_a_required_cost_spell_pays_the_life_and_the_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_CITADEL);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let rites = scenario
        .add_spell_to_library_top(P0, "Village Rites", true)
        .with_mana_cost(cost(0, &[ManaCostShard::Black]))
        .from_oracle_text(VILLAGE_RITES)
        .id();
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        rites,
        &Answers {
            picks: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, rites, &trail);
    assert_eq!(
        zone(&runner, bear),
        Zone::Graveyard,
        "the sacrifice is paid"
    );
    assert_eq!(life(&runner), 19, "Citadel's 1 life (mana value) is paid");
}

/// CR 702.138a + CR 601.2f + CR 601.2h: Breach's escape cost is {B} plus
/// "exile three other cards from your graveyard", and Village Rites' sacrifice
/// is added to it.
#[test]
fn escape_of_a_required_cost_spell_exiles_three_and_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_breach(&mut scenario);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let rites = scenario
        .add_spell_to_graveyard(P0, "Village Rites", true)
        .with_mana_cost(cost(0, &[ManaCostShard::Black]))
        .from_oracle_text(VILLAGE_RITES)
        .id();
    let fodder = add_fodder(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        rites,
        &Answers {
            escape: true,
            picks: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, rites, &trail);
    assert_eq!(
        zone(&runner, bear),
        Zone::Graveyard,
        "the sacrifice is paid"
    );
    assert_eq!(pool(&runner), 0, "{{B}} is paid");
    assert_eq!(
        trail.exiles(&fodder),
        3,
        "escape exiles three other cards, once"
    );
}

/// Arbiter of Woe from hand with Defiler of Flesh out and `{4}{B}{B}` in pool.
fn arbiter_with_defiler() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let defiler = add_defiler_of_flesh(&mut scenario);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let arbiter = scenario
        .add_creature_to_hand_from_oracle(P0, "Arbiter of Woe", 5, 4, ARBITER_OF_WOE)
        .with_mana_cost(cost(4, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 2), (ManaType::Colorless, 4)]));
    (scenario.build(), arbiter, bear, defiler)
}

/// CR 601.2b + CR 601.2f: a spell's own required additional cost does not stop
/// a Defiler from offering its optional life and {B} reduction. The Defiler is
/// announced first, then the sacrifice is paid.
#[test]
fn defiler_is_offered_for_a_spell_with_its_own_required_cost() {
    let (mut runner, arbiter, bear, _) = arbiter_with_defiler();

    let trail = cast(
        &mut runner,
        arbiter,
        &Answers {
            defiler: true,
            picks: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, arbiter, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "PayCost"]);
    assert_eq!(
        zone(&runner, bear),
        Zone::Graveyard,
        "the sacrifice is paid"
    );
    assert_eq!(life(&runner), 18, "the Defiler's 2 life");
    assert_eq!(pool(&runner), 1, "{{4}}{{B}}{{B}} less {{B}}");
}

/// Control: declining the Defiler pays the full cost and the sacrifice.
#[test]
fn declined_defiler_with_a_required_cost_pays_the_full_cost() {
    let (mut runner, arbiter, bear, _) = arbiter_with_defiler();

    let trail = cast(
        &mut runner,
        arbiter,
        &Answers {
            defiler: false,
            picks: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, arbiter, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert_eq!(zone(&runner, bear), Zone::Graveyard);
    assert_eq!(life(&runner), 20);
    assert_eq!(pool(&runner), 0);
}

/// Hostile source: the Defiler itself is the creature sacrificed for Arbiter's
/// cost after its reduction was accepted. CR 601.2f locks the total when it is
/// determined, so the reduction still applies.
#[test]
fn defiler_sacrificed_to_the_required_cost_keeps_its_accepted_reduction() {
    let (mut runner, arbiter, bear, defiler) = arbiter_with_defiler();

    let trail = cast(
        &mut runner,
        arbiter,
        &Answers {
            defiler: true,
            picks: vec![defiler],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, arbiter, &trail);
    assert_eq!(
        zone(&runner, defiler),
        Zone::Graveyard,
        "the Defiler was sacrificed"
    );
    assert_eq!(zone(&runner, bear), Zone::Battlefield);
    assert_eq!(life(&runner), 18);
    assert_eq!(
        pool(&runner),
        1,
        "the accepted {{B}} reduction still applies"
    );
}

/// Hostile permission: Arbiter of Woe cast from the graveyard under Muldrotha,
/// with Muldrotha itself sacrificed for Arbiter's cost after the Defiler is
/// accepted. CR 601.2a: the cast keeps the permission it was announced under.
#[test]
fn required_cost_cast_under_a_permission_whose_source_is_sacrificed_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let arbiter = scenario
        .add_creature_to_graveyard(P0, "Arbiter of Woe", 5, 4)
        .with_mana_cost(cost(4, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Flying"], ARBITER_OF_WOE)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 2), (ManaType::Colorless, 4)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        arbiter,
        &Answers {
            defiler: true,
            picks: vec![muldrotha],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, arbiter, &trail);
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::GraveyardPermission { source, .. },
                ..
            } if *source == muldrotha
        )),
        "the cast keeps Muldrotha's permission"
    );
    assert_eq!(zone(&runner, muldrotha), Zone::Graveyard);
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "the cast spends Muldrotha's creature slot, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1);
}

/// Two permissions with different terms admit Arbiter of Woe from the
/// graveyard: Underworld Breach (escape: + exile three) and Muldrotha (a
/// creature slot, no extra cost). Each leg pays only its own terms, and the
/// Defiler is offered once in each.
#[test]
fn two_graveyard_authorities_each_pay_their_own_terms_with_the_defiler() {
    for escape in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
        add_defiler_of_flesh(&mut scenario);
        add_breach(&mut scenario);
        let muldrotha = add_permission_source(
            &mut scenario,
            "Muldrotha, the Gravetide",
            MULDROTHA,
            &["Elemental", "Avatar"],
        );
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let arbiter = scenario
            .add_creature_to_graveyard(P0, "Arbiter of Woe", 5, 4)
            .with_mana_cost(cost(4, &[ManaCostShard::Black, ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&["Flying"], ARBITER_OF_WOE)
            .id();
        let fodder = add_fodder(&mut scenario);
        scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 2), (ManaType::Colorless, 4)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            arbiter,
            &Answers {
                escape,
                defiler: true,
                picks: vec![bear],
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, arbiter, &trail);
        assert_eq!(trail.count("CastingVariantChoice"), 1, "escape={escape}");
        assert_eq!(trail.count("DefilerPayment"), 1, "escape={escape}");
        assert_eq!(zone(&runner, bear), Zone::Graveyard, "escape={escape}");
        assert_eq!(
            trail.exiles(&fodder),
            if escape { 3 } else { 0 },
            "escape={escape}: only escape exiles"
        );
        assert_eq!(
            runner
                .state()
                .graveyard_cast_permissions_used_per_type
                .contains(&(muldrotha, CoreType::Creature)),
            !escape,
            "escape={escape}: only Muldrotha's cast spends its creature slot"
        );
        assert_eq!(life(&runner), 18, "escape={escape}");
        assert_eq!(pool(&runner), 1, "escape={escape}");
    }
}

// ---------------------------------------------------------------------------
// Kicker, the keyword queue, Optional and Choice costs
// ---------------------------------------------------------------------------

fn gatekeeper_with_defiler(black: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let gatekeeper = scenario
        .add_creature_to_hand(P0, "Gatekeeper of Malakir", 2, 2)
        .with_mana_cost(cost(0, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Kicker"], GATEKEEPER_OF_MALAKIR)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, black)]));
    (scenario.build(), gatekeeper)
}

/// CR 601.2b + CR 702.33a: the Defiler and the kicker are both announced; the
/// kicker is still offered after the Defiler. Kicked {B}{B}+{B} less {B}.
#[test]
fn kicker_is_offered_after_the_defiler() {
    let (mut runner, gatekeeper) = gatekeeper_with_defiler(3);

    let trail = cast(
        &mut runner,
        gatekeeper,
        &Answers {
            defiler: true,
            optional: true,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, gatekeeper, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1, "kicked {{B}}{{B}}{{B}} less {{B}}");
}

/// CR 601.2b + CR 702.175a: an offspring cost (the keyword queue) is still
/// offered after the Defiler. {2}{B}+{B} less {B}.
#[test]
fn offspring_is_offered_after_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let augur = scenario
        .add_creature_to_hand(P0, "Darkstar Augur", 2, 3)
        .with_mana_cost(cost(2, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Offspring", "Flying"], DARKSTAR_AUGUR)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 2), (ManaType::Colorless, 2)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        augur,
        &Answers {
            defiler: true,
            optional: true,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, augur, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1, "{{2}}{{B}} + offspring {{B}} less {{B}}");
}

/// CR 702.157a: a squad cost rides the keyword queue (Squad is collected into
/// the queue, and the synthesized `Optional{Repeatable}` additional cost is
/// deduplicated against it), and is still offered after the Defiler.
#[test]
fn squad_is_offered_after_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let arco = scenario
        .add_creature_to_hand(P0, "Arco-Flagellant", 3, 1)
        .with_mana_cost(cost(2, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Squad"], ARCO_FLAGELLANT)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1), (ManaType::Colorless, 4)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        arco,
        &Answers {
            defiler: true,
            repeat: 1,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, arco, &trail);
    assert_eq!(trail.prompts.first(), Some(&"DefilerPayment"));
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert!(
        trail.count("OptionalCostChoice") >= 1,
        "{:?}",
        trail.prompts
    );
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1, "{{2}}{{B}} + one squad {{2}} less {{B}}");
}

/// CR 702.56a + CR 303.4a: an Aura's target is chosen first (its Enchant
/// target), then its replicate cost is declared with the rest of the keyword
/// queue, and the Defiler is still offered. Changing Loyalty is a black Aura.
#[test]
fn aura_replicate_is_declared_with_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let loyalty = scenario
        .add_spell_to_hand(P0, "Changing Loyalty", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(cost(1, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Flash", "Replicate", "Enchant"], CHANGING_LOYALTY)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1), (ManaType::Colorless, 3)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        loyalty,
        &Answers {
            defiler: true,
            repeat: 1,
            targets: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, loyalty, &trail);
    assert_eq!(
        trail.prompts,
        [
            "TargetSelection",
            "DefilerPayment",
            "OptionalCostChoice",
            "OptionalCostChoice"
        ],
        "the Aura's target, then the Defiler, then replicate (paid once, then declined)"
    );
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1, "{{1}}{{B}} + replicate {{2}} less {{B}}");
}

/// CR 702.138a + CR 702.33a: an escaped kicker spell still exiles its three
/// cards, kicked or not.
#[test]
fn escaped_kicker_spell_exiles_three_kicked_or_not() {
    for kick in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_breach(&mut scenario);
        let grow = scenario
            .add_spell_to_graveyard(P0, "Grow from the Ashes", false)
            .with_mana_cost(cost(2, &[ManaCostShard::Green]))
            .from_oracle_text_with_keywords(&["Kicker"], GROW_FROM_THE_ASHES)
            .id();
        let fodder = add_fodder(&mut scenario);
        scenario.with_mana_pool(P0, mana(&[(ManaType::Green, 5)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            grow,
            &Answers {
                escape: true,
                optional: kick,
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, grow, &trail);
        assert_eq!(trail.count("OptionalCostChoice"), 1, "kick={kick}");
        assert_eq!(trail.exiles(&fodder), 3, "kick={kick}");
        assert_eq!(pool(&runner), if kick { 0 } else { 2 }, "kick={kick}");
        assert_eq!(
            kickers_paid_on_stack(&runner),
            usize::from(kick),
            "kick={kick}"
        );
    }
}

/// CR 702.33a: how many kicker costs the spell on the stack was cast with.
fn kickers_paid_on_stack(runner: &GameRunner) -> usize {
    runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                ability: Some(ability),
                ..
            } => Some(ability.context.kickers_paid.len()),
            _ => None,
        })
        .expect("a spell with an ability is on the stack")
}

fn morkrut_with_defiler() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let morkrut = scenario
        .add_creature_to_hand_from_oracle(P0, "Morkrut Behemoth", 7, 6, MORKRUT_BEHEMOTH)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 2), (ManaType::Colorless, 4)]));
    (scenario.build(), morkrut, bear)
}

/// CR 601.2b: a Choice additional cost is announced after the Defiler. The
/// preferred branch sacrifices a creature: {4}{B} less {B}.
#[test]
fn choice_cost_preferred_branch_after_the_defiler() {
    let (mut runner, morkrut, bear) = morkrut_with_defiler();

    let trail = cast(
        &mut runner,
        morkrut,
        &Answers {
            defiler: true,
            choice_preferred: true,
            picks: vec![bear],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, morkrut, &trail);
    assert_eq!(
        trail.prompts,
        ["DefilerPayment", "OptionalCostChoice", "PayCost"]
    );
    assert_eq!(zone(&runner, bear), Zone::Graveyard);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 2, "{{4}}{{B}} less {{B}}");
}

/// The fallback branch pays {1}{B}: {4}{B} + {1}{B} less {B}.
#[test]
fn choice_cost_fallback_branch_after_the_defiler() {
    let (mut runner, morkrut, bear) = morkrut_with_defiler();

    let trail = cast(
        &mut runner,
        morkrut,
        &Answers {
            defiler: true,
            choice_preferred: false,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, morkrut, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(zone(&runner, bear), Zone::Battlefield);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0, "{{4}}{{B}} + {{1}}{{B}} less {{B}}");
}

/// CR 601.2b + CR 702.138a: with no creature to sacrifice, the Choice's
/// fallback is paid without a prompt, and escape's three cards are still exiled.
#[test]
fn escaped_choice_cost_with_an_unofferable_preferred_branch_pays_the_fallback() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_breach(&mut scenario);
    let morkrut = scenario
        .add_creature_to_graveyard(P0, "Morkrut Behemoth", 7, 6)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Menace"], MORKRUT_BEHEMOTH)
        .id();
    let fodder = add_fodder(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 7)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        morkrut,
        &Answers {
            escape: true,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, morkrut, &trail);
    assert_eq!(trail.count("OptionalCostChoice"), 0, "{:?}", trail.prompts);
    assert_eq!(trail.exiles(&fodder), 3);
    assert_eq!(pool(&runner), 0, "{{4}}{{B}} + {{1}}{{B}}");
}

/// CR 601.2b + CR 601.2h: with neither branch payable (no creature to
/// sacrifice, only {4}{B}), the cast is refused and nothing is paid.
#[test]
fn choice_cost_with_both_branches_unpayable_is_refused() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let morkrut = scenario
        .add_creature_to_hand_from_oracle(P0, "Morkrut Behemoth", 7, 6, MORKRUT_BEHEMOTH)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1), (ManaType::Colorless, 4)]));
    let mut runner = scenario.build();

    let trail = cast(&mut runner, morkrut, &Answers::default());

    assert!(
        trail.refused,
        "both branches unpayable: {:?}",
        trail.prompts
    );
    assert_eq!(zone(&runner, morkrut), Zone::Hand);
    assert_eq!(pool(&runner), 5);
    assert_eq!(life(&runner), 20);
}

/// CR 601.2f + CR 601.2h: Terror of the Peaks' 3 life is part of the total
/// whichever branch of Bone Shards' Choice is paid.
#[test]
fn choice_cost_keeps_an_imposed_tax_on_both_branches() {
    for preferred in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let filler = scenario.add_card_to_hand(P0, "Filler Card");
        let terror = scenario
            .add_creature(P1, "Terror of the Peaks", 5, 4)
            .from_oracle_text_with_keywords(&["Flying"], TERROR_OF_THE_PEAKS)
            .id();
        let shards = scenario
            .add_spell_to_hand_from_oracle(P0, "Bone Shards", false, BONE_SHARDS)
            .with_mana_cost(cost(0, &[ManaCostShard::Black]))
            .id();
        scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            shards,
            &Answers {
                choice_preferred: preferred,
                targets: vec![terror],
                picks: vec![bear, filler],
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, shards, &trail);
        assert_eq!(
            trail.count("OptionalCostChoice"),
            1,
            "preferred={preferred}"
        );
        if preferred {
            assert_eq!(zone(&runner, bear), Zone::Graveyard, "sacrificed");
        } else {
            assert_eq!(zone(&runner, filler), Zone::Graveyard, "discarded");
        }
        assert_eq!(life(&runner), 17, "preferred={preferred}: Terror's 3 life");
    }
}

fn gorex_with_defiler(graveyard_creatures: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let gorex = scenario
        .add_creature_to_hand_from_oracle(P0, "Gorex, the Tombshell", 4, 4, GOREX_THE_TOMBSHELL)
        .with_mana_cost(cost(6, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    let dead: Vec<ObjectId> = (0..graveyard_creatures)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Dead {i}"), 1, 1)
                .id()
        })
        .collect();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 7)]));
    (scenario.build(), gorex, dead)
}

/// CR 601.2b: Gorex's optional exile is announced after the Defiler. Exiling
/// one creature card: {6}{B}{B} less {2} less {B}.
#[test]
fn optional_cost_is_offered_after_the_defiler() {
    let (mut runner, gorex, dead) = gorex_with_defiler(1);

    let trail = cast(
        &mut runner,
        gorex,
        &Answers {
            defiler: true,
            optional: true,
            picks: dead.clone(),
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, gorex, &trail);
    assert_eq!(trail.prompts.first(), Some(&"DefilerPayment"));
    assert_eq!(trail.count("OptionalCostChoice"), 1, "{:?}", trail.prompts);
    assert_eq!(zone(&runner, dead[0]), Zone::Exile);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 2, "{{6}}{{B}}{{B}} less {{2}} less {{B}}");
}

/// CR 601.2b: with no creature card to exile, Gorex's optional cost isn't
/// offered, and the Defiler still is.
#[test]
fn unofferable_optional_cost_still_offers_the_defiler() {
    let (mut runner, gorex, _) = gorex_with_defiler(0);

    let trail = cast(
        &mut runner,
        gorex,
        &Answers {
            defiler: true,
            optional: true,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, gorex, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0, "{{6}}{{B}}{{B}} less {{B}}");
}

// ---------------------------------------------------------------------------
// No additional cost: a cast variant's residual with the Defiler
// ---------------------------------------------------------------------------

fn nighthawk_under_breach() -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    add_breach(&mut scenario);
    let nighthawk = scenario
        .add_creature_to_graveyard(P0, "Vampire Nighthawk", 2, 3)
        .with_mana_cost(cost(1, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Flying", "Deathtouch", "Lifelink"], VAMPIRE_NIGHTHAWK)
        .id();
    let fodder = add_fodder(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 3)]));
    (scenario.build(), nighthawk, fodder)
}

/// CR 601.2b + CR 702.138a: an escaped black permanent offers the Defiler, and
/// one cast pays the Defiler and the escape residual exactly once each.
#[test]
fn escaped_permanent_offers_the_defiler_and_pays_each_cost_once() {
    for accept in [true, false] {
        let (mut runner, nighthawk, fodder) = nighthawk_under_breach();

        let trail = cast(
            &mut runner,
            nighthawk,
            &Answers {
                escape: true,
                defiler: accept,
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, nighthawk, &trail);
        assert_eq!(trail.count("DefilerPayment"), 1, "accept={accept}");
        for card in &fodder {
            assert_eq!(trail.exiles(&[*card]), 1, "accept={accept}: exiled once");
        }
        let life_paid = trail
            .events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::LifeChanged { player_id, amount, .. }
                if *player_id == P0 && *amount < 0)
            })
            .count();
        assert_eq!(life_paid, usize::from(accept), "accept={accept}");
        assert_eq!(life(&runner), if accept { 18 } else { 20 });
        assert_eq!(pool(&runner), if accept { 1 } else { 0 });
    }
}

/// CR 601.2d + CR 107.3a: an X-divided spell escaped under Breach divides its
/// damage after X is paid, from the division the pending cast carries through
/// the residual's payment.
#[test]
fn escaped_divided_spell_keeps_its_division() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_breach(&mut scenario);
    let ogre = scenario.add_creature(P1, "Big Ogre", 3, 3).id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let thunder = scenario
        .add_spell_to_graveyard(P0, "Rolling Thunder", false)
        .with_mana_cost(cost(
            0,
            &[ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
        ))
        .from_oracle_text(ROLLING_THUNDER)
        .id();
    let fodder = add_fodder(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Red, 5)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        thunder,
        &Answers {
            escape: true,
            x: 3,
            targets: vec![ogre, bear],
            distribution: vec![(ogre, 2), (bear, 1)],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, thunder, &trail);
    assert_eq!(trail.exiles(&fodder), 3);
    assert_eq!(pool(&runner), 0, "X=3 plus {{R}}{{R}}");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert_eq!(runner.state().objects[&ogre].damage_marked, 2);
    assert_eq!(runner.state().objects[&bear].damage_marked, 1);
}

// ---------------------------------------------------------------------------
// Costs declared before targets (the settlement route)
// ---------------------------------------------------------------------------

fn escape_board(
    name: &str,
    keywords: &[&str],
    oracle: &str,
    mana_cost: ManaCost,
    pool_kind: ManaType,
    pool_n: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_breach(&mut scenario);
    let victim = scenario
        .add_creature(P1, "Victim Bear", 2, 2)
        .with_mana_cost(cost(1, &[ManaCostShard::Green]))
        .id();
    scenario.add_creature(P0, "Big Friend", 3, 3);
    let spell = scenario
        .add_spell_to_graveyard(P0, name, false)
        .with_mana_cost(mana_cost)
        .from_oracle_text_with_keywords(keywords, oracle)
        .id();
    let fodder = add_fodder(&mut scenario);
    scenario.add_creature_to_hand(P0, "Hand Creature", 1, 1);
    scenario.with_mana_pool(P0, mana(&[(pool_kind, pool_n)]));
    (scenario.build(), spell, fodder, victim)
}

/// CR 601.2b + CR 702.33d: Bloodchief's Thirst declares its kicker before its
/// targets, and its escape residual is still paid, once.
#[test]
fn kicker_instead_spell_declared_before_targets_still_exiles_three() {
    let (mut runner, thirst, fodder, victim) = escape_board(
        "Bloodchief's Thirst",
        &["Kicker"],
        BLOODCHIEFS_THIRST,
        cost(0, &[ManaCostShard::Black]),
        ManaType::Black,
        1,
    );

    let trail = cast(
        &mut runner,
        thirst,
        &Answers {
            escape: true,
            targets: vec![victim],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, thirst, &trail);
    assert_eq!(trail.exiles(&fodder), 3, "exiled once each");
}

/// CR 601.2b + CR 107.3a: Aether Tide announces X and its discard before its
/// targets, and its escape residual is still paid.
#[test]
fn required_x_cost_declared_before_targets_still_exiles_three() {
    let (mut runner, tide, fodder, victim) = escape_board(
        "Aether Tide",
        &[],
        AETHER_TIDE,
        cost(0, &[ManaCostShard::X, ManaCostShard::Blue]),
        ManaType::Blue,
        2,
    );

    let trail = cast(
        &mut runner,
        tide,
        &Answers {
            escape: true,
            x: 1,
            targets: vec![victim],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, tide, &trail);
    assert_eq!(trail.count("ChooseXValue"), 1);
    assert_eq!(trail.exiles(&fodder), 3);
    assert!(
        runner.state().players[0].hand.is_empty(),
        "X=1: one creature card discarded"
    );
}

/// Control: a declined casualty re-enters the authority after its targets and
/// pays the escape residual there, once.
#[test]
fn casualty_declared_before_targets_pays_the_residual_once() {
    let (mut runner, swap, fodder, victim) = escape_board(
        "Audacious Swap",
        &["Casualty"],
        AUDACIOUS_SWAP,
        cost(3, &[ManaCostShard::Red]),
        ManaType::Red,
        4,
    );

    let trail = cast(
        &mut runner,
        swap,
        &Answers {
            escape: true,
            targets: vec![victim],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, swap, &trail);
    assert_eq!(trail.count("OptionalCostChoice"), 1);
    assert_eq!(trail.exiles(&fodder), 3);
}

/// CR 702.56a + CR 601.2b: a targeted spell's replicate cost is declared
/// before its targets; once they settle, the cast re-enters the additional-cost
/// authority, which pays the escape residual, replicated or not.
#[test]
fn replicate_declared_before_targets_still_exiles_three() {
    for replicate in [0, 1] {
        let (mut runner, pyromatics, fodder, victim) = escape_board(
            "Pyromatics",
            &["Replicate"],
            PYROMATICS,
            cost(1, &[ManaCostShard::Red]),
            ManaType::Red,
            4,
        );

        let trail = cast(
            &mut runner,
            pyromatics,
            &Answers {
                escape: true,
                repeat: replicate,
                targets: vec![victim],
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, pyromatics, &trail);
        assert!(
            trail.count("OptionalCostChoice") >= 1,
            "replicate={replicate}: {:?}",
            trail.prompts
        );
        assert_eq!(trail.exiles(&fodder), 3, "replicate={replicate}");
        assert_eq!(
            pool(&runner),
            if replicate == 1 { 0 } else { 2 },
            "replicate={replicate}"
        );
    }
}

/// CR 601.2b + CR 700.2: Inscription of Abundance declares its kicker before
/// its modes; the escape residual is still paid once the targets settle.
#[test]
fn kicker_modal_spell_declared_before_modes_still_exiles_three() {
    for kick in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_breach(&mut scenario);
        let friend = scenario.add_creature(P0, "Big Friend", 3, 3).id();
        let inscription = scenario
            .add_spell_to_graveyard(P0, "Inscription of Abundance", true)
            .with_mana_cost(cost(1, &[ManaCostShard::Green]))
            .from_oracle_text_with_keywords(&["Kicker"], INSCRIPTION_OF_ABUNDANCE)
            .id();
        let fodder = add_fodder(&mut scenario);
        scenario.with_mana_pool(P0, mana(&[(ManaType::Green, 5)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            inscription,
            &Answers {
                escape: true,
                optional: kick,
                modes: vec![0],
                targets: vec![friend],
                ..Answers::default()
            },
        );

        assert_cast_completes(&runner, inscription, &trail);
        assert_eq!(
            trail.prompts,
            ["OptionalCostChoice", "ModeChoice", "PayCost"],
            "kick={kick}: kicker before modes, the lone target chosen for us, then escape's exile"
        );
        assert_eq!(trail.exiles(&fodder), 3, "kick={kick}");
        assert_eq!(
            kickers_paid_on_stack(&runner),
            usize::from(kick),
            "kick={kick}"
        );
        assert_eq!(pool(&runner), if kick { 0 } else { 3 }, "kick={kick}");
    }
}

// ---------------------------------------------------------------------------
// The Defiler decision resumes through the pending cast
// ---------------------------------------------------------------------------

/// Defensive branch (controlled state): the Defiler's life became unpayable
/// between its offer and its acceptance. Nothing in the engine can do that
/// while `DefilerPayment` is open (only the decision or a cancel is accepted),
/// so the life total is set directly. The cast resumes through the pending
/// cast, so the kicker is still offered, at full cost.
#[test]
fn defiler_with_unpayable_life_at_acceptance_still_offers_the_kicker() {
    let (mut runner, gatekeeper) = gatekeeper_with_defiler(3);
    runner
        .act(cast_action(&runner, gatekeeper))
        .expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner.state_mut().players[0].life = 1;

    let trail = drive(
        &mut runner,
        GameAction::DecideOptionalCost { pay: true },
        &Answers::default(),
    );

    assert_cast_completes(&runner, gatekeeper, &trail);
    assert_eq!(trail.prompts, ["OptionalCostChoice"]);
    assert_eq!(life(&runner), 1, "no life paid");
    assert_eq!(pool(&runner), 1, "unkicked {{B}}{{B}}, no reduction");
}

/// A `LoseLife` replacement on P0's permanent that keeps the amount but then
/// runs an interactive choice (CR 616.1). Synthetic: no printed card applies an
/// interactive LoseLife replacement to its controller's own life payment. The
/// nearest printed analogue, Bloodletter of Aclazotz ("If an opponent would
/// lose life during your turn, they lose twice that much life instead."), is
/// opponent-only on its controller's turn, so it can't pause a sorcery-speed
/// caster's Defiler payment. Same fixture as zero_life_mid_cast_payment.rs.
fn life_loss_rider() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 0 })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )],
                kind: ChooseOneOfKind::Ordinary,
            },
        ))
}

/// CR 616.1 + CR 601.2b: the Defiler's life payment pauses for a replacement
/// substitution; after it is answered, the kicker is still offered and the life
/// is paid once.
#[test]
fn defiler_payment_paused_by_a_substitution_still_offers_the_kicker() {
    let (mut runner, gatekeeper) = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
        add_defiler_of_flesh(&mut scenario);
        scenario
            .add_creature(P0, "Life-Loss Rider", 0, 1)
            .with_replacement_definition(life_loss_rider());
        let gatekeeper = scenario
            .add_creature_to_hand(P0, "Gatekeeper of Malakir", 2, 2)
            .with_mana_cost(cost(0, &[ManaCostShard::Black, ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&["Kicker"], GATEKEEPER_OF_MALAKIR)
            .id();
        scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 3)]));
        (scenario.build(), gatekeeper)
    };

    let trail = cast(
        &mut runner,
        gatekeeper,
        &Answers {
            defiler: true,
            optional: true,
            branch_index: Some(0),
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, gatekeeper, &trail);
    assert_eq!(
        trail.prompts,
        ["DefilerPayment", "ChooseOneOfBranch", "OptionalCostChoice"]
    );
    assert_eq!(life(&runner), 19, "2 life paid once, 1 gained by the rider");
    assert_eq!(pool(&runner), 1, "kicked, less the Defiler's {{B}}");
}

/// CR 616.1 + CR 601.2b: the Defiler's life payment waits for two replacements
/// to be ordered; after ordering, the kicker is still offered and the life is
/// paid once. Synthetic for the same reason as `life_loss_rider`.
#[test]
fn defiler_payment_paused_for_replacement_ordering_still_offers_the_kicker() {
    let (mut runner, gatekeeper) = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
        add_defiler_of_flesh(&mut scenario);
        scenario
            .add_creature(P0, "Life-Loss Modifiers", 0, 1)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::LoseLife)
                    .quantity_modification(QuantityModification::DOUBLE)
                    .description("Double".to_string()),
            )
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::LoseLife)
                    .quantity_modification(QuantityModification::Plus { value: 1 })
                    .description("Plus one".to_string()),
            );
        let gatekeeper = scenario
            .add_creature_to_hand(P0, "Gatekeeper of Malakir", 2, 2)
            .with_mana_cost(cost(0, &[ManaCostShard::Black, ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&["Kicker"], GATEKEEPER_OF_MALAKIR)
            .id();
        scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 3)]));
        (scenario.build(), gatekeeper)
    };

    let trail = cast(
        &mut runner,
        gatekeeper,
        &Answers {
            defiler: true,
            optional: true,
            replacement_index: Some(0),
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, gatekeeper, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert_eq!(trail.count("ReplacementChoice"), 1, "{:?}", trail.prompts);
    assert_eq!(trail.prompts.last(), Some(&"OptionalCostChoice"));
    let life_payments = trail
        .events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::LifeChanged { player_id, amount, .. }
            if *player_id == P0 && *amount < 0)
        })
        .count();
    assert_eq!(life_payments, 1, "the Defiler's life is paid once");
    assert_eq!(
        life(&runner),
        15,
        "ordered double first, then +1: the Defiler's 2 becomes 5"
    );
    assert_eq!(pool(&runner), 1, "kicked, less the Defiler's {{B}}");
}
