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
                index: if answers.defiler { 0 } else { 1 },
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
                GameAction::DecideOptionalCost {
                    index: if pay { 0 } else { 1 },
                }
            }
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: answers.x },
            WaitingFor::ModeChoice { .. } => GameAction::SelectModes {
                indices: answers.modes.clone(),
            },
            // CR 601.2c + CR 601.2d: scripted targets first. Once they run out,
            // an optional slot ("any number of targets") is declined rather than
            // announcing an extra target the scripted division would leave
            // without its required share; a required slot takes the first legal.
            WaitingFor::TargetSelection {
                selection,
                target_slots,
                ..
            } => GameAction::ChooseTarget {
                target: targets.next().map(TargetRef::Object).or_else(|| {
                    let optional = target_slots
                        .get(selection.current_slot)
                        .is_some_and(|slot| slot.optional);
                    if optional {
                        None
                    } else {
                        selection.current_legal_targets.first().cloned()
                    }
                }),
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
            // CR 107.4f: a Phyrexian or life-for-mana shard is paid with life.
            WaitingFor::PhyrexianPayment { shards, .. } => GameAction::SubmitPhyrexianChoices {
                choices: vec![engine::types::game_state::ShardChoice::PayLife; shards.len()],
            },
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
        // CR 601.2b: one replicate run (each payment, then the decline),
        // never offered again after the targets.
        assert_eq!(
            trail.count("OptionalCostChoice"),
            replicate as usize + 1,
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
        GameAction::DecideOptionalCost { index: 0 },
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

// ---------------------------------------------------------------------------
// Alternative costs that replace the mana cost (an Oracle "rather than" cost,
// a granted alternative, an exile or library alternative-ability cost): a
// matching Defiler is announced with the cast's other costs, ahead of the
// alternative-cost Choice (CR 601.2b names no order), and the Choice prices
// the alternative with the reductions the cast then carries (#9781).
// ---------------------------------------------------------------------------

const BRINGER_OF_THE_BLACK_DAWN: &str = "You may pay {W}{U}{B}{R}{G} rather than pay this spell's mana cost.\nTrample\nAt the beginning of your upkeep, you may pay 2 life. If you do, search your library for a card, then shuffle and put that card on top.";
const DEFILER_OF_DREAMS: &str = "Flying\nAs an additional cost to cast blue permanent spells, you may pay 2 life. Those spells cost {U} less to cast if you paid life this way. This effect reduces only the amount of blue mana you pay.\nWhenever you cast a blue permanent spell, draw a card.";
const ZAHID: &str = "You may pay {3}{U} and tap an untapped artifact you control rather than pay this spell's mana cost.\nFlying";
const DEMON_OF_DEATHS_GATE: &str = "You may pay 6 life and sacrifice three black creatures rather than pay this spell's mana cost.\nFlying, trample";
const AS_FORETOLD: &str = "At the beginning of your upkeep, put a time counter on this enchantment.\nOnce each turn, you may pay {0} rather than pay the mana cost for a spell you cast with mana value X or less, where X is the number of time counters on this enchantment.";
const PRIMAL_PRAYERS: &str = "When this enchantment enters, you get {E}{E} (two energy counters).\nYou may cast creature spells with mana value 3 or less by paying {E} rather than paying their mana costs. If you cast a spell this way, you may cast it as though it had flash.";
const ASMORANOMARDICADAISTINACULDACAR: &str = "As long as you've discarded a card this turn, you may pay {B/R} to cast this spell.\nWhen Asmoranomardicadaistinaculdacar enters, you may search your library for a card named The Underworld Cookbook, reveal it, put it into your hand, then shuffle.\nSacrifice two Foods: Target creature deals 6 damage to itself.";

const WURG: [(ManaType, usize); 4] = [
    (ManaType::White, 1),
    (ManaType::Blue, 1),
    (ManaType::Red, 1),
    (ManaType::Green, 1),
];

fn wubrg() -> Vec<(ManaType, usize)> {
    let mut pool = WURG.to_vec();
    pool.push((ManaType::Black, 1));
    pool
}

fn black_in_pool(runner: &GameRunner) -> usize {
    runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Black)
}

/// Defiler of Dreams on P0's battlefield with its parsed statics.
fn add_defiler_of_dreams(scenario: &mut GameScenario) -> ObjectId {
    let parsed = parse_oracle_text(
        DEFILER_OF_DREAMS,
        "Defiler of Dreams",
        &["Flying".into()],
        &["Creature".into()],
        &["Phyrexian".into(), "Sphinx".into()],
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("DefilerCostReduction")),
        "reach: Defiler of Dreams parses to a Defiler cost reduction"
    );
    let mut defiler = scenario.add_creature(P0, "Defiler of Dreams", 3, 5);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
    defiler.id()
}

/// Bringer of the Black Dawn ({7}{B}{B}) in hand with Defiler of Flesh out,
/// P0 at `life` and `pool` in P0's mana pool.
fn bringer_with_defiler(life: i32, pool: &[(ManaType, usize)]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
    add_defiler_of_flesh(&mut scenario);
    let bringer = add_bringer(&mut scenario);
    scenario.with_mana_pool(P0, mana(pool));
    (scenario.build(), bringer)
}

fn add_bringer(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Bringer of the Black Dawn",
            5,
            5,
            BRINGER_OF_THE_BLACK_DAWN,
        )
        .with_mana_cost(cost(7, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id()
}

fn offered(runner: &GameRunner, spell: ObjectId) -> bool {
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == spell))
}

const ACCEPT_BOTH: Answers = Answers {
    defiler: true,
    optional: false,
    repeat: 0,
    choice_preferred: true,
    escape: false,
    targets: Vec::new(),
    x: 0,
    modes: Vec::new(),
    picks: Vec::new(),
    distribution: Vec::new(),
    replacement_index: None,
    branch_index: None,
};

/// CR 118.9 + CR 118.9d + CR 601.2b: Bringer's {W}{U}{B}{R}{G} less the
/// Defiler's {B} is {W}{U}{R}{G}. With exactly that, the alternative is payable
/// only with the Defiler: the Defiler is offered, then the alternative, and the
/// cast completes for the four mana and 2 life.
#[test]
fn oracle_alternative_cost_affordable_only_with_a_defiler_completes() {
    let (mut runner, bringer) = bringer_with_defiler(20, &WURG);
    assert!(
        offered(&runner, bringer),
        "the alternative cast is a legal action"
    );

    let trail = cast(&mut runner, bringer, &ACCEPT_BOTH);

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18, "the Defiler's 2 life is paid");
    assert_eq!(pool(&runner), 0, "{{W}}{{U}}{{R}}{{G}} is paid");
}

/// CR 118.9d + CR 601.2b: the Choice prices the alternative with the Defiler the
/// caster accepted, whose life is already paid; it never asks for a second
/// payment of that life. At 3 life the Defiler leaves 1, and the alternative,
/// which costs no life, is still offered and paid.
#[test]
fn oracle_alternative_cost_after_an_accepted_defiler_at_low_life_completes() {
    let (mut runner, bringer) = bringer_with_defiler(3, &WURG);

    let trail = cast(&mut runner, bringer, &ACCEPT_BOTH);

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 1);
    assert_eq!(pool(&runner), 0);
}

/// CR 601.2h: a declined Defiler isn't credited to the alternative. With
/// {W}{U}{R}{G}, neither the alternative ({W}{U}{B}{R}{G}) nor the printed cost
/// can then be paid, so the decline is refused, as every Defiler decline that
/// leaves the cast unpayable is; the cast waits at the Defiler prompt with
/// nothing paid.
#[test]
fn declined_defiler_is_not_credited_to_the_alternative() {
    let (mut runner, bringer) = bringer_with_defiler(20, &WURG);

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            defiler: false,
            ..ACCEPT_BOTH
        },
    );

    assert!(trail.refused, "the decline is refused: {:?}", trail.prompts);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    assert_eq!(life(&runner), 20);
    assert_eq!(pool(&runner), 4);
}

/// CR 118.9d + CR 601.2b: with {W}{U}{B}{R}{G} the alternative is payable
/// outright, and the Defiler's optional life payment still applies to it: its
/// {B} is left in the pool.
#[test]
fn oracle_alternative_cost_offers_the_defiler() {
    let (mut runner, bringer) = bringer_with_defiler(20, &wubrg());

    let trail = cast(&mut runner, bringer, &ACCEPT_BOTH);

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18, "the Defiler's 2 life is paid");
    assert_eq!(pool(&runner), 1, "one mana is left");
    assert_eq!(
        black_in_pool(&runner),
        1,
        "the Defiler's {{B}} is left over"
    );
}

/// Control: declining the Defiler, the same alternative is paid in full.
#[test]
fn declined_defiler_still_offers_a_payable_alternative() {
    let (mut runner, bringer) = bringer_with_defiler(20, &wubrg());

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            defiler: false,
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 20);
    assert_eq!(pool(&runner), 0);
}

/// CR 118.9b + CR 601.2f: the Choice's declined branch is the printed cost with
/// the accepted Defiler: {7}{B}{B} less {B} is eight mana, exactly the pool
/// {W}{U}{R}{G}{B}{B}{B}{B}.
#[test]
fn declined_alternative_pays_the_printed_cost_with_the_accepted_defiler() {
    let mut wurg_bbbb = WURG.to_vec();
    wurg_bbbb.push((ManaType::Black, 4));
    let (mut runner, bringer) = bringer_with_defiler(20, &wurg_bbbb);

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            choice_preferred: false,
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0, "{{7}}{{B}} takes all eight mana");
}

/// CR 601.2h + CR 119.4: at 1 life the Defiler's 2 life can't be paid, so it
/// isn't offered and the alternative-cost Choice comes at once.
#[test]
fn oracle_alternative_cost_is_offered_at_once_when_no_defiler_can_be() {
    let (mut runner, bringer) = bringer_with_defiler(1, &wubrg());

    let trail = cast(&mut runner, bringer, &ACCEPT_BOTH);

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["OptionalCostChoice"]);
    assert_eq!(life(&runner), 1);
    assert_eq!(pool(&runner), 0);
}

/// Defensive branch (controlled state, as #10539's row): the Defiler's life
/// became unpayable between its offer and its acceptance. The cast resumes
/// through the pending cast, so the alternative is still offered.
#[test]
fn defiler_with_unpayable_life_at_acceptance_still_offers_the_alternative() {
    let (mut runner, bringer) = bringer_with_defiler(20, &wubrg());
    runner
        .act(cast_action(&runner, bringer))
        .expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner.state_mut().players[0].life = 1;

    let trail = drive(
        &mut runner,
        GameAction::DecideOptionalCost { index: 0 },
        &ACCEPT_BOTH,
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["OptionalCostChoice"]);
    assert_eq!(life(&runner), 1, "no life paid");
    assert_eq!(pool(&runner), 0, "the alternative is paid in full");
}

/// CR 616.1 + CR 601.2b: the Defiler's life payment pauses for a replacement
/// substitution; after it, the alternative is still offered and the life is
/// paid once.
#[test]
fn defiler_payment_paused_by_a_substitution_still_offers_the_alternative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    scenario
        .add_creature(P0, "Life-Loss Rider", 0, 1)
        .with_replacement_definition(life_loss_rider());
    let bringer = add_bringer(&mut scenario);
    scenario.with_mana_pool(P0, mana(&wubrg()));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            branch_index: Some(0),
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(
        trail.prompts,
        ["DefilerPayment", "ChooseOneOfBranch", "OptionalCostChoice"]
    );
    assert_eq!(life(&runner), 19, "2 life paid once, 1 gained by the rider");
    assert_eq!(pool(&runner), 1, "the Defiler's {{B}} is left over");
}

/// CR 616.1 + CR 601.2b: the Defiler's life payment waits for two replacements
/// to be ordered; after ordering, the alternative is still offered and the life
/// is paid once.
#[test]
fn defiler_payment_paused_for_replacement_ordering_still_offers_the_alternative() {
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
    let bringer = add_bringer(&mut scenario);
    scenario.with_mana_pool(P0, mana(&wubrg()));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            replacement_index: Some(0),
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert_eq!(trail.count("ReplacementChoice"), 1, "{:?}", trail.prompts);
    assert_eq!(trail.prompts.last(), Some(&"OptionalCostChoice"));
    assert_eq!(
        life(&runner),
        15,
        "ordered double first, then +1: the Defiler's 2 becomes 5"
    );
    assert_eq!(pool(&runner), 1);
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

/// CR 601.2b: a game saved at the Defiler prompt resumes into the alternative
/// Choice without offering the Defiler again: one Defiler prompt, then one
/// Choice.
#[test]
fn saved_at_the_defiler_prompt_resumes_into_the_alternative() {
    let (mut runner, bringer) = bringer_with_defiler(20, &wubrg());
    runner
        .act(cast_action(&runner, bringer))
        .expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    let mut restored = round_trip(&runner);
    assert_eq!(restored.waiting_for_kind(), "DefilerPayment");

    let trail = drive(
        &mut restored,
        GameAction::DecideOptionalCost { index: 0 },
        &ACCEPT_BOTH,
    );

    assert_cast_completes(&restored, bringer, &trail);
    assert_eq!(
        trail.prompts,
        ["OptionalCostChoice"],
        "no second Defiler offer"
    );
    assert_eq!(life(&restored), 18);
    assert_eq!(pool(&restored), 1);
}

/// A game saved at the alternative Choice completes without a Defiler prompt.
#[test]
fn saved_at_the_alternative_choice_completes() {
    let (mut runner, bringer) = bringer_with_defiler(20, &wubrg());
    runner
        .act(cast_action(&runner, bringer))
        .expect("the cast starts");
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");
    assert_eq!(runner.waiting_for_kind(), "OptionalCostChoice");
    let mut restored = round_trip(&runner);

    let trail = drive(
        &mut restored,
        GameAction::DecideOptionalCost { index: 0 },
        &ACCEPT_BOTH,
    );

    assert_cast_completes(&restored, bringer, &trail);
    assert_eq!(trail.count("DefilerPayment"), 0);
    assert_eq!(life(&restored), 18);
    assert_eq!(pool(&restored), 1);
}

/// Demon of Death's Gate ({6}{B}{B}{B}) in hand with Defiler of Flesh, P0 at 7
/// life with three black creatures and {6}{B}{B}.
fn demon_with_defiler() -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 7);
    add_defiler_of_flesh(&mut scenario);
    let fodder = (0..3)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Black Fodder {i}"), 1, 1)
                .with_color(vec![ManaColor::Black])
                .id()
        })
        .collect();
    let demon = scenario
        .add_creature_to_hand_from_oracle(P0, "Demon of Death's Gate", 9, 9, DEMON_OF_DEATHS_GATE)
        .with_mana_cost(cost(
            6,
            &[
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
        ))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 6), (ManaType::Black, 2)]));
    let runner = scenario.build();
    assert!(
        !runner.state().objects[&demon].casting_options.is_empty(),
        "reach: Demon of Death's Gate carries its alternative cost"
    );
    (runner, demon, fodder)
}

/// CR 601.2h + CR 119.4: the Defiler is offered before the alternative is
/// chosen. Once its 2 life is paid, 5 life can't pay the alternative's 6, so
/// the alternative is no longer offered and the cast continues at the printed
/// cost with the Defiler: {6}{B}{B}, the whole pool.
#[test]
fn defiler_life_that_leaves_the_alternative_unpayable_drops_the_choice() {
    let (mut runner, demon, fodder) = demon_with_defiler();

    let trail = cast(&mut runner, demon, &ACCEPT_BOTH);

    assert_cast_completes(&runner, demon, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(life(&runner), 5);
    assert_eq!(pool(&runner), 0);
    for id in fodder {
        assert_eq!(
            zone(&runner, id),
            Zone::Battlefield,
            "nothing is sacrificed"
        );
    }
}

/// Control: declining the Defiler, the alternative's 6 life and three
/// sacrifices are paid and no mana is.
#[test]
fn declined_defiler_keeps_a_life_alternative() {
    let (mut runner, demon, fodder) = demon_with_defiler();

    let trail = cast(
        &mut runner,
        demon,
        &Answers {
            defiler: false,
            picks: fodder.clone(),
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, demon, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1);
    assert_eq!(trail.count("OptionalCostChoice"), 1);
    assert_eq!(life(&runner), 1);
    assert_eq!(pool(&runner), 8);
    for id in fodder {
        assert_eq!(zone(&runner, id), Zone::Graveyard);
    }
}

/// CR 118.9d + CR 601.2h: Zahid's "{3}{U} and tap an untapped artifact you
/// control" less Defiler of Dreams' {U} is {3} and the tap. With {3}, the
/// composite alternative is payable only with the Defiler.
#[test]
fn composite_alternative_affordable_only_with_a_defiler_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_dreams(&mut scenario);
    let artifact = scenario
        .add_creature(P0, "Spare Artifact", 0, 1)
        .as_artifact()
        .id();
    let zahid = scenario
        .add_creature_to_hand_from_oracle(P0, "Zahid, Djinn of the Lamp", 5, 6, ZAHID)
        .with_mana_cost(cost(4, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3)]));
    let mut runner = scenario.build();
    assert!(
        offered(&runner, zahid),
        "the alternative cast is a legal action"
    );

    let trail = cast(
        &mut runner,
        zahid,
        &Answers {
            picks: vec![artifact],
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, zahid, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1, "{:?}", trail.prompts);
    assert_eq!(trail.count("OptionalCostChoice"), 1, "{:?}", trail.prompts);
    assert!(
        runner.state().objects[&artifact].tapped,
        "the artifact is tapped"
    );
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0);
}

/// Whether P0 also controls a Priest of Yawgmoth, a mana ability that consumes
/// another artifact: tapped (it attacked last combat), or untapped and
/// controlled since the previous turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SparePriest {
    Absent,
    Tapped,
    Untapped,
}

const PRIEST_OF_YAWGMOTH: &str =
    "{T}, Sacrifice an artifact: Add an amount of {B} equal to the sacrificed artifact's mana value.";

/// Zahid with Defiler of Dreams, Sol Ring ("{T}: Add {C}{C}.") and a Plains,
/// an empty pool and, when `spare` is set, an untapped Ornithopter.
fn zahid_with_sol_ring(
    spare: bool,
    priest: SparePriest,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_dreams(&mut scenario);
    let ring = scenario
        .add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    let ornithopter = spare.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Ornithopter", 0, 2, "Flying")
            .as_artifact_creature()
            .id()
    });
    let priest_kind = priest;
    let priest = match priest {
        SparePriest::Absent => None,
        SparePriest::Tapped | SparePriest::Untapped => Some(
            scenario
                .add_creature_from_oracle(P0, "Priest of Yawgmoth", 1, 2, PRIEST_OF_YAWGMOTH)
                .id(),
        ),
    };
    let zahid = scenario
        .add_creature_to_hand_from_oracle(P0, "Zahid, Djinn of the Lamp", 5, 6, ZAHID)
        .with_mana_cost(cost(4, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    let mut runner = scenario.build();
    if let Some(priest) = priest {
        runner.state_mut().objects.get_mut(&priest).unwrap().tapped =
            priest_kind == SparePriest::Tapped;
        let consumes_another = runner.state().objects[&priest].abilities.iter().any(|a| {
            matches!(&a.cost, Some(engine::types::ability::AbilityCost::Composite { costs })
                if costs.iter().any(|c| matches!(c,
                    engine::types::ability::AbilityCost::Sacrifice(s) if s.target != TargetFilter::SelfRef)))
        });
        assert!(
            consumes_another,
            "reach: the Priest sacrifices another artifact for mana"
        );
        let selections =
            engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0);
        assert_eq!(
            selections.iter().any(|s| s.source.object_id == priest),
            priest_kind == SparePriest::Untapped,
            "reach: only the untapped Priest can activate (with the Ring as fuel)"
        );
    }
    (runner, zahid, ring, ornithopter)
}

/// CR 118.3 + CR 601.2h: Zahid's {3} (with the Defiler) needs Sol Ring's mana,
/// and its tap leg needs an untapped artifact; the Ring can't pay both. A
/// Priest of Yawgmoth doesn't change that: tapped it can't make mana, and
/// untapped its only fuel is the Ring held for the tap leg.
#[test]
fn composite_alternative_cant_tap_its_mana_artifact_twice() {
    for priest in [
        SparePriest::Absent,
        SparePriest::Tapped,
        SparePriest::Untapped,
    ] {
        let (mut runner, zahid, ring, _) = zahid_with_sol_ring(false, priest);
        assert_eq!(
            engine::game::oracle_alternative_reservation_verdict(runner.state(), P0, zahid),
            Some(engine::game::ReservationVerdict::Rejected)
        );
        assert!(!offered(&runner, zahid), "the cast is not a legal action");

        let trail = cast(&mut runner, zahid, &ACCEPT_BOTH);

        assert!(trail.refused, "the cast is refused: {:?}", trail.prompts);
        assert_eq!(zone(&runner, zahid), Zone::Hand);
        assert!(!runner.state().objects[&ring].tapped);
        assert_eq!(life(&runner), 20);
    }
}

/// Control: with a spare Ornithopter for the tap leg, the Ring and the Plains
/// pay the {3}.
#[test]
fn composite_alternative_taps_a_spare_artifact() {
    for priest in [
        SparePriest::Absent,
        SparePriest::Tapped,
        SparePriest::Untapped,
    ] {
        let (mut runner, zahid, ring, ornithopter) = zahid_with_sol_ring(true, priest);
        let ornithopter = ornithopter.unwrap();
        assert!(offered(&runner, zahid), "the cast is a legal action");

        let trail = cast(
            &mut runner,
            zahid,
            &Answers {
                picks: vec![ornithopter],
                ..ACCEPT_BOTH
            },
        );

        assert_cast_completes(&runner, zahid, &trail);
        assert_eq!(
            trail.prompts,
            ["DefilerPayment", "OptionalCostChoice", "PayCost"]
        );
        assert!(runner.state().objects[&ornithopter].tapped);
        assert!(runner.state().objects[&ring].tapped);
        assert_eq!(life(&runner), 18);
    }
}

const TRINISPHERE: &str = "As long as this artifact is untapped, each spell that would cost less than three mana to cast costs three mana to cast. (Additional mana in the cost may be paid with any color of mana or colorless mana. For example, a spell that would cost {1}{B} to cast costs {2}{B} to cast instead.)";

const ORIMS_CURE: &str = "If you control a Plains, you may tap an untapped creature you control rather than pay this spell's mana cost.\nPrevent the next 4 damage that would be dealt to any target this turn.";

/// An untapped Trinisphere under P1's control.
fn add_trinisphere(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P1, "Trinisphere", TRINISPHERE)
        .id()
}

/// Reach: Trinisphere's Oracle text parsed to a cost floor.
fn assert_floor_parsed(runner: &GameRunner, trinisphere: ObjectId) {
    assert!(
        runner.state().objects[&trinisphere]
            .static_definitions
            .iter_all()
            .any(|s| format!("{:?}", s.mode).contains("Minimum")),
        "reach: Trinisphere parses to a cost floor"
    );
}

/// Demon of Death's Gate in hand, three black creatures, P0 at 20 life, an
/// opposing Trinisphere and `colorless` colourless mana; no Defiler.
fn demon_under_trinisphere(colorless: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let trinisphere = add_trinisphere(&mut scenario);
    let fodder = (0..3)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Black Fodder {i}"), 1, 1)
                .with_color(vec![ManaColor::Black])
                .id()
        })
        .collect();
    let demon = scenario
        .add_creature_to_hand_from_oracle(P0, "Demon of Death's Gate", 9, 9, DEMON_OF_DEATHS_GATE)
        .with_mana_cost(cost(
            6,
            &[
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
        ))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, colorless)]));
    let runner = scenario.build();
    assert_floor_parsed(&runner, trinisphere);
    (runner, demon, fodder)
}

/// CR 601.2f + CR 118.9d: Trinisphere floors the alternative's {0} mana part to
/// {3}, so with no mana the alternative can't be paid.
#[test]
fn non_mana_alternative_under_a_floor_is_not_offered_without_mana() {
    let (mut runner, demon, fodder) = demon_under_trinisphere(0);
    assert!(!offered(&runner, demon), "the cast is not a legal action");

    let trail = cast(&mut runner, demon, &ACCEPT_BOTH);

    assert!(trail.refused, "the cast is refused: {:?}", trail.prompts);
    assert_eq!(zone(&runner, demon), Zone::Hand);
    assert_eq!(life(&runner), 20);
    for id in fodder {
        assert_eq!(zone(&runner, id), Zone::Battlefield);
    }
}

/// Control: with {3}, the alternative completes for the floor, 6 life and the
/// three creatures.
#[test]
fn non_mana_alternative_under_a_floor_pays_the_floor() {
    let (mut runner, demon, fodder) = demon_under_trinisphere(3);
    assert!(offered(&runner, demon), "the cast is a legal action");

    let trail = cast(
        &mut runner,
        demon,
        &Answers {
            picks: fodder.clone(),
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, demon, &trail);
    assert_eq!(trail.prompts, ["OptionalCostChoice", "PayCost"]);
    assert_eq!(life(&runner), 14);
    assert_eq!(pool(&runner), 0, "the floored {{3}} is paid");
    for id in fodder {
        assert_eq!(zone(&runner, id), Zone::Graveyard);
    }
}

/// Orim's Cure in hand with `plains` Plains, Llanowar Elves ("{T}: Add {G}."),
/// an empty pool and an opposing Trinisphere.
fn orims_cure_under_trinisphere(plains: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let trinisphere = add_trinisphere(&mut scenario);
    for _ in 0..plains {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let elves = scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .id();
    let cure = scenario
        .add_spell_to_hand_from_oracle(P0, "Orim's Cure", true, ORIMS_CURE)
        .with_mana_cost(cost(1, &[ManaCostShard::White]))
        .with_color(vec![ManaColor::White])
        .id();
    let runner = scenario.build();
    assert_floor_parsed(&runner, trinisphere);
    assert!(
        !runner.state().objects[&cure].casting_options.is_empty(),
        "reach: Orim's Cure carries its alternative cost"
    );
    (runner, cure, elves)
}

/// CR 118.3 + CR 601.2f: with two Plains the floored {3} needs the Elf's mana,
/// and the tap leg needs the Elf; it can't pay both, so the alternative isn't
/// offered and the printed {1}{W} (floored to {2}{W}) is cast.
#[test]
fn tap_alternative_under_a_floor_cant_tap_its_mana_creature_twice() {
    let (mut runner, cure, elves) = orims_cure_under_trinisphere(2);

    let trail = cast(&mut runner, cure, &ACCEPT_BOTH);

    assert_cast_completes(&runner, cure, &trail);
    assert_eq!(trail.count("OptionalCostChoice"), 0, "{:?}", trail.prompts);
    assert!(runner.state().objects[&elves].tapped, "tapped for {{G}}");
}

/// Control: with a third Plains the alternative is offered; the Elf is tapped
/// for it and the Plains pay the floored {3}.
#[test]
fn tap_alternative_under_a_floor_taps_its_creature_with_spare_mana() {
    let (mut runner, cure, elves) = orims_cure_under_trinisphere(3);

    let trail = cast(
        &mut runner,
        cure,
        &Answers {
            picks: vec![elves],
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, cure, &trail);
    assert_eq!(trail.count("OptionalCostChoice"), 1, "{:?}", trail.prompts);
    assert!(runner.state().objects[&elves].tapped);
    assert_eq!(pool(&runner), 0);
}

/// Bringer with Defiler of Flesh, {W}{U}{R}{G} and As Foretold with nine time
/// counters ({0} once each turn for a spell with mana value 9 or less).
fn bringer_with_as_foretold() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let foretold = scenario
        .add_enchantment_from_oracle(P0, "As Foretold", AS_FORETOLD)
        .id();
    scenario.with_counter(foretold, engine::types::counter::CounterType::Time, 9);
    let bringer = add_bringer(&mut scenario);
    scenario.with_mana_pool(P0, mana(&WURG));
    (scenario.build(), bringer, foretold)
}

/// CR 118.9a + CR 601.4: only one alternative cost applies, and the player
/// chooses which. With the Defiler accepted, Bringer's own {W}{U}{B}{R}{G}
/// less {B} and As Foretold's {0} are both payable: the spell's own is offered
/// first, and declining it offers As Foretold's, whose slot is then spent.
#[test]
fn accepted_defiler_offers_each_payable_alternative_in_turn() {
    let (mut runner, bringer, foretold) = bringer_with_as_foretold();

    let first = cast_action(&runner, bringer);
    runner.act(first).expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");
    assert_eq!(offered_alternative(&runner), Some(wubrg_cost()));
    runner
        .act(GameAction::DecideOptionalCost { index: 1 })
        .expect("decline the spell's own alternative");
    assert_eq!(
        offered_alternative(&runner),
        Some(ManaCost::zero()),
        "As Foretold's alternative is offered next"
    );
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("choose As Foretold's alternative");

    assert_cast_completes(
        &runner,
        bringer,
        &Trail {
            prompts: Vec::new(),
            events: Vec::new(),
            refused: false,
        },
    );
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 4, "{{0}} is paid");
    assert!(runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&foretold));
}

/// Choosing the spell's own alternative spends no grant slot.
#[test]
fn accepted_defiler_then_the_spells_own_alternative_spends_no_grant() {
    let (mut runner, bringer, foretold) = bringer_with_as_foretold();

    let trail = cast(&mut runner, bringer, &ACCEPT_BOTH);

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(pool(&runner), 0, "the spell's own alternative is paid");
    assert!(!runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&foretold));
}

/// CR 118.9a: with no Defiler, Bringer's own {W}{U}{B}{R}{G} is payable outright,
/// and declining it still offers As Foretold's {0}.
#[test]
fn declined_own_alternative_offers_the_payable_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let foretold = scenario
        .add_enchantment_from_oracle(P0, "As Foretold", AS_FORETOLD)
        .id();
    scenario.with_counter(foretold, engine::types::counter::CounterType::Time, 9);
    let bringer = add_bringer(&mut scenario);
    scenario.with_mana_pool(P0, mana(&wubrg()));
    let mut runner = scenario.build();

    let first = cast_action(&runner, bringer);
    runner.act(first).expect("the cast starts");
    assert_eq!(offered_alternative(&runner), Some(wubrg_cost()));
    runner
        .act(GameAction::DecideOptionalCost { index: 1 })
        .expect("decline the spell's own alternative");
    assert_eq!(offered_alternative(&runner), Some(ManaCost::zero()));
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("choose As Foretold's alternative");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, bringer), Zone::Stack);
    assert_eq!(pool(&runner), 5, "{{0}} is paid");
    assert!(runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&foretold));
}

fn wubrg_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
    }
}

/// The mana alternative an alternative-cost Choice offers, if one is pending.
fn offered_alternative(runner: &GameRunner) -> Option<ManaCost> {
    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice {
            cost: engine::types::ability::AdditionalCost::Choice(costs),
            ..
        } => match costs.first() {
            Some(engine::types::ability::AbilityCost::Mana { cost }) => Some(cost.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Declined, Bringer's own alternative is no longer payable, so As Foretold's
/// {0} is the alternative offered; its once-per-turn slot is spent.
#[test]
fn declined_defiler_offers_the_payable_grant() {
    let (mut runner, bringer, foretold) = bringer_with_as_foretold();

    let trail = cast(
        &mut runner,
        bringer,
        &Answers {
            defiler: false,
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, bringer, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 20);
    assert_eq!(pool(&runner), 4, "{{0}} is paid");
    assert!(runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&foretold));
}

/// CR 118.9 + CR 702.8a: Primal Prayers' {E} alternative also lets the spell be
/// cast as though it had flash, so cast in an opponent's turn it is the cost
/// the cast must use; the Defiler is announced with it.
#[test]
fn timing_required_alternative_offers_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Primal Prayers", PRIMAL_PRAYERS);
    let nighthawk = scenario
        .add_creature_to_hand_from_oracle(P0, "Vampire Nighthawk", 2, 3, VAMPIRE_NIGHTHAWK)
        .with_mana_cost(cost(1, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.players[0].energy = 1;
    }
    assert!(
        offered(&runner, nighthawk),
        "the flash cast is a legal action"
    );

    let trail = cast(&mut runner, nighthawk, &ACCEPT_BOTH);

    assert_cast_completes(&runner, nighthawk, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(runner.state().players[0].energy, 0, "{{E}} is paid");
    assert_eq!(life(&runner), 18);
}

/// Zahid, Djinn of the Lamp as P0's commander, in the command zone and cast
/// `prior_casts` times before, with Defiler of Dreams and `pool`.
fn commander_zahid(prior_casts: u32, pool: &[(ManaType, usize)]) -> (GameRunner, ObjectId) {
    let mut scenario =
        GameScenario::new_with_format(engine::types::format::FormatConfig::commander(), 2, 0x9781);
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_dreams(&mut scenario);
    let zahid = scenario
        .add_creature_to_hand_from_oracle(P0, "Zahid, Djinn of the Lamp", 5, 6, ZAHID)
        .with_mana_cost(cost(4, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    scenario.with_commander(zahid);
    scenario.with_mana_pool(P0, mana(pool));
    let mut runner = scenario.build();
    if prior_casts > 0 {
        runner
            .state_mut()
            .commander_cast_count
            .insert(zahid, prior_casts);
    }
    assert_eq!(zone(&runner, zahid), Zone::Command);
    (runner, zahid)
}

/// CR 903.8 + CR 601.2f: cast once before, Zahid costs {2} more:
/// {6}{U}{U}, less the Defiler's {U}, is {6}{U}. The tax is counted once.
#[test]
fn commander_tax_is_counted_once_with_a_defiler() {
    let (mut runner, zahid) = commander_zahid(1, &[(ManaType::Colorless, 6), (ManaType::Blue, 1)]);
    assert!(offered(&runner, zahid), "the taxed cast is a legal action");

    let trail = cast(&mut runner, zahid, &ACCEPT_BOTH);

    assert_cast_completes(&runner, zahid, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0);
}

/// Control: never cast before, {4}{U}{U} less {U} is {4}{U}.
#[test]
fn untaxed_commander_with_a_defiler_completes() {
    let (mut runner, zahid) = commander_zahid(0, &[(ManaType::Colorless, 4), (ManaType::Blue, 1)]);
    assert!(offered(&runner, zahid));

    let trail = cast(&mut runner, zahid, &ACCEPT_BOTH);

    assert_cast_completes(&runner, zahid, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 0);
}

/// CR 118.9 + CR 118.9d + CR 601.2b: Bolas's Citadel's "pay life equal to its
/// mana value" is the alternative cost of a black creature cast from the top
/// of the library, and the Defiler's optional life payment applies to it.
#[test]
fn citadel_alternative_ability_cost_offers_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_CITADEL);
    add_defiler_of_flesh(&mut scenario);
    let nighthawk = scenario
        .add_spell_to_library_top(P0, "Vampire Nighthawk", false)
        .as_creature()
        .with_mana_cost(cost(1, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text(VAMPIRE_NIGHTHAWK)
        .id();
    let mut runner = scenario.build();

    let trail = cast(&mut runner, nighthawk, &ACCEPT_BOTH);

    assert_cast_completes(&runner, nighthawk, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment"]);
    assert_eq!(life(&runner), 15, "Citadel's 3 life and the Defiler's 2");
}

/// CR 601.2b + CR 601.2f: Citadel with a kicked Gatekeeper of Malakir: the
/// Defiler is offered alongside the kicker and reduces the kicker's {B} to
/// nothing. Citadel's life is the mana value, 2.
#[test]
fn citadel_alternative_ability_cost_with_a_kicker_offers_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_CITADEL);
    add_defiler_of_flesh(&mut scenario);
    let gatekeeper = scenario
        .add_spell_to_library_top(P0, "Gatekeeper of Malakir", false)
        .as_creature()
        .with_mana_cost(cost(0, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .from_oracle_text_with_keywords(&["Kicker"], GATEKEEPER_OF_MALAKIR)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Black, 1)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        gatekeeper,
        &Answers {
            optional: true,
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, gatekeeper, &trail);
    assert_eq!(trail.count("DefilerPayment"), 1, "{:?}", trail.prompts);
    assert_eq!(trail.count("OptionalCostChoice"), 1, "{:?}", trail.prompts);
    assert_eq!(life(&runner), 16, "Citadel's 2 life and the Defiler's 2");
    assert_eq!(pool(&runner), 1, "the kicker's {{B}} is reduced to nothing");
}
/// Both printed doors of Charred Foyer // Warped Space, from the small
/// raw-MTGJSON fixture.
fn warped_space_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/charred_foyer_warped_space_9751_atomic.json");
        engine::database::card_db::CardDatabase::from_mtgjson(&path)
            .expect("parse both printed Room doors")
    })
}

fn add_pool(runner: &mut GameRunner, kinds: &[(ManaType, usize)]) {
    for unit in mana(kinds) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Park the game at the end of P0's turn and advance to P0's next precombat
/// main phase, through P1's turn and P0's upkeep (CR 503.1).
fn to_next_p0_precombat_main(runner: &mut GameRunner) {
    {
        let state = runner.state_mut();
        state.turn_number = 1;
        state.active_player = P0;
        state.phase = Phase::End;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    for _ in 0..6 {
        runner.advance_to_phase(Phase::PreCombatMain);
        if runner.state().active_player == P0 && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("no attack");
            }
            _ => panic!("next P0 main stopped at {:?}", runner.state().waiting_for),
        }
    }
    panic!("next P0 main was not reached");
}

/// Charred Foyer // Warped Space on P0's battlefield with both doors unlocked
/// (Warped Space cast, Charred Foyer unlocked, both through the production
/// path), Defiler of Flesh, and `top` on top of P0's library. Advances to P0's
/// next precombat main: Charred Foyer's upkeep trigger exiles `top` with a
/// play permission. P0's pool is then empty.
fn foyer_board(
    top: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId, ObjectId) {
    use engine::game::scenario_db::GameScenarioDbExt;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let room = scenario.add_real_card(P0, "Charred Foyer", Zone::Hand, warped_space_db());
    scenario.add_card_to_library_top(P0, "Library Filler");
    let top = top(&mut scenario);
    for i in 0..3 {
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
    }
    let mut runner = scenario.build();

    add_pool(&mut runner, &[(ManaType::Red, 2), (ManaType::Colorless, 4)]);
    runner.cast(room).modal_back_face(true).resolve();
    assert_eq!(zone(&runner, room), Zone::Battlefield);
    add_pool(&mut runner, &[(ManaType::Red, 1), (ManaType::Colorless, 3)]);
    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: room,
            door: engine::game::game_object::RoomDoor::Left,
        })
        .expect("unlock Charred Foyer");
    if runner.waiting_for_kind() == "ManaPayment" {
        runner
            .act(GameAction::PassPriority)
            .expect("finish paying the unlock cost");
    }
    assert_eq!(
        runner.state().objects[&room].room_unlocks,
        Some(engine::game::game_object::RoomUnlockState {
            left_unlocked: true,
            right_unlocked: true
        }),
        "reach: both doors are unlocked"
    );
    assert!(
        format!("{:?}", runner.state().objects[&room].static_definitions)
            .contains("CastWithAlternativeCost"),
        "reach: Warped Space's grant is live"
    );
    assert_eq!(pool(&runner), 0);

    to_next_p0_precombat_main(&mut runner);
    assert_eq!(
        zone(&runner, top),
        Zone::Exile,
        "reach: Charred Foyer exiled it"
    );
    assert!(
        runner.state().objects[&top]
            .casting_permissions
            .iter()
            .any(|p| matches!(
                p,
                engine::types::ability::CastingPermission::PlayFromExile { .. }
            )),
        "reach: Charred Foyer's play permission"
    );
    assert_eq!(pool(&runner), 0);
    (runner, room, top)
}

/// CR 118.6a + CR 118.9 + CR 601.2b: Asmoranomardicadaistinaculdacar has no
/// mana cost, so cast from Charred Foyer's exile its only payable cost is
/// Warped Space's {0} alternative. A matching Defiler is announced with it
/// (CR 118.9d), then the alternative is chosen; its printed branch is the
/// nonexistent cost (an impulse permission keeps the alternative a choice).
#[test]
fn granted_alternative_for_a_card_with_no_mana_cost_offers_the_defiler() {
    let (mut runner, room, asmor) = foyer_board(|scenario| {
        scenario
            .add_spell_to_library_top(P0, "Asmoranomardicadaistinaculdacar", false)
            .as_creature()
            .as_legendary()
            .with_subtypes(vec!["Human", "Wizard"])
            .with_mana_cost(ManaCost::NoCost)
            .from_oracle_text(ASMORANOMARDICADAISTINACULDACAR)
            // CR 202.2e: it is black and red by its colour indicator;
            // `from_oracle_text` derives colour from the (absent) mana cost, so
            // the printed colours are set after it.
            .with_color(vec![ManaColor::Black, ManaColor::Red])
            .id()
    });
    assert_eq!(
        runner.state().objects[&asmor].mana_cost,
        ManaCost::NoCost,
        "reach: no mana cost, so the alternative is the only payable cost"
    );
    assert!(offered(&runner, asmor), "the exile cast is a legal action");

    runner
        .act(cast_action(&runner, asmor))
        .expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");
    let WaitingFor::OptionalCostChoice {
        cost: engine::types::ability::AdditionalCost::Choice(costs),
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the alternative is offered after the Defiler, got {:?}",
            runner.state().waiting_for
        );
    };
    let [preferred, fallback] = costs.as_slice() else {
        panic!("expected [alternative, printed] options, got {costs:?}");
    };
    assert_eq!(
        preferred,
        &engine::types::ability::AbilityCost::Mana {
            cost: ManaCost::zero()
        },
        "reach: Warped Space's {{0}}"
    );
    assert_eq!(
        fallback,
        &engine::types::ability::AbilityCost::Mana {
            cost: ManaCost::NoCost
        },
        "reach: the printed branch is the nonexistent cost"
    );
    let trail = drive(
        &mut runner,
        GameAction::DecideOptionalCost { index: 0 },
        &ACCEPT_BOTH,
    );

    assert_cast_completes(&runner, asmor, &trail);
    assert_eq!(life(&runner), 18);
    assert!(
        runner
            .state()
            .alt_cost_grant_permissions_used
            .contains(&room),
        "Warped Space's once-per-turn slot is spent"
    );
}
/// Control: a black permanent with a printed cost, cast through the same Room,
/// is offered the Defiler and then the {0}-versus-printed choice.
#[test]
fn granted_alternative_for_a_printed_cost_offers_the_defiler_then_the_choice() {
    let (mut runner, room, nighthawk) = foyer_board(|scenario| {
        scenario
            .add_spell_to_library_top(P0, "Vampire Nighthawk", false)
            .as_creature()
            .with_mana_cost(cost(1, &[ManaCostShard::Black, ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text(VAMPIRE_NIGHTHAWK)
            .id()
    });

    let trail = cast(&mut runner, nighthawk, &ACCEPT_BOTH);

    assert_cast_completes(&runner, nighthawk, &trail);
    assert_eq!(trail.prompts, ["DefilerPayment", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 18);
    assert!(runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&room));
}

// ---------------------------------------------------------------------------
// Resources another leg holds (#10918 review): an object or life paying one
// leg of the cast can't also pay its mana (CR 118.3 + CR 119.4 + CR 601.2g-h).
// ---------------------------------------------------------------------------

const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";
const SEPHARA: &str = "You may pay {W} and tap four untapped creatures you control with flying rather than pay this spell's mana cost.\nFlying, lifelink\nOther creatures you control with flying have indestructible.";
const HUMILITY: &str = "All creatures lose all abilities and have base power and toughness 1/1.";
const DEFILER_OF_FAITH: &str = "Vigilance\nAs an additional cost to cast white permanent spells, you may pay 2 life. Those spells cost {W} less to cast if you paid life this way. This effect reduces only the amount of white mana you pay.\nWhenever you cast a white permanent spell, create a 1/1 white Soldier creature token.";
const EVERFLOWING_CHALICE: &str = "Multikicker {2} (You may pay an additional {2} any number of times as you cast this spell.)\nThis artifact enters with a charge counter on it for each time it was kicked.\n{T}: Add {C} for each charge counter on this artifact.";
const FIST_OF_SUNS: &str =
    "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.";
const KRRIK_GRANT: &str = "For each {B} in a cost, you may pay 2 life rather than pay that mana.";
const ENSLAVE: &str = "Enchant creature\nYou control enchanted creature.\nAt the beginning of your upkeep, enchanted creature deals 1 damage to its owner.";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
}

/// `n` creatures named `name` under P0's control, from `oracle`, coloured `color`.
fn creatures(
    scenario: &mut GameScenario,
    name: &str,
    n: usize,
    oracle: &str,
    color: ManaColor,
) -> Vec<ObjectId> {
    (0..n)
        .map(|_| {
            scenario
                .add_creature_from_oracle(P0, name, 1, 1, oracle)
                .with_color(vec![color])
                .id()
        })
        .collect()
}

fn ornithopters(scenario: &mut GameScenario, n: usize) -> Vec<ObjectId> {
    (0..n)
        .map(|_| {
            scenario
                .add_creature_from_oracle(P0, "Ornithopter", 0, 2, "Flying")
                .as_artifact_creature()
                .id()
        })
        .collect()
}

/// A Defiler from its Oracle text, its statics only.
fn add_parsed_defiler(scenario: &mut GameScenario, name: &str, oracle: &str) {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".into()], &[]);
    let mut defiler = scenario.add_creature(P0, name, 5, 5);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
}

fn add_zahid(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Zahid, Djinn of the Lamp", 5, 6, ZAHID)
        .with_mana_cost(cost(4, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id()
}

fn add_sephara(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Sephara, Sky's Blade", 7, 7, SEPHARA)
        .with_mana_cost(cost(
            4,
            &[
                ManaCostShard::White,
                ManaCostShard::White,
                ManaCostShard::White,
            ],
        ))
        .with_color(vec![ManaColor::White])
        .id()
}

fn add_demon(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Demon of Death's Gate", 9, 9, DEMON_OF_DEATHS_GATE)
        .with_mana_cost(cost(
            6,
            &[
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
        ))
        .with_color(vec![ManaColor::Black])
        .id()
}

/// Cast `spell` with manual mana payment, answering until the mana payment.
fn manual_cast(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> Trail {
    let first = GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Manual,
    };
    drive(runner, first, answers)
}

/// CR 605.3a: activate Ashnod's Altar, sacrificing `victim`.
fn sacrifice_to_altar(runner: &mut GameRunner, altar: ObjectId, victim: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the Altar");
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("sacrifice to the Altar");
}

/// Ends a manual mana payment, returning whether the spell was cast.
fn finish_payment(runner: &mut GameRunner, spell: ObjectId) -> bool {
    assert_eq!(runner.waiting_for_kind(), "ManaPayment");
    runner
        .act(GameAction::PassPriority)
        .expect("pay from the pool")
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
}

fn verdict(runner: &GameRunner, spell: ObjectId) -> engine::game::ReservationVerdict {
    engine::game::oracle_alternative_reservation_verdict(runner.state(), P0, spell)
        .expect("reach: the spell has an Oracle alternative")
}

/// Demon of Death's Gate in hand, `corpses` Walking Corpses, Ashnod's Altar,
/// `floating` {C} and an opposing Trinisphere.
fn demon_with_altar(
    corpses: usize,
    floating: usize,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = board();
    let trinisphere = add_trinisphere(&mut scenario);
    let corpses = creatures(
        &mut scenario,
        "Walking Corpse",
        corpses,
        "",
        ManaColor::Black,
    );
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let demon = add_demon(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, floating)]));
    let runner = scenario.build();
    assert_floor_parsed(&runner, trinisphere);
    (runner, demon, altar, corpses)
}

/// CR 601.2g: with three Corpses held for the leg, the fourth fuels one Altar
/// activation, which with {C} pays the floored {3}. The fourth goes to the Altar
/// during payment.
#[test]
fn altar_fuelled_floor_is_offered_and_paid_during_payment() {
    let (mut runner, demon, altar, corpses) = demon_with_altar(4, 1);
    assert_eq!(
        verdict(&runner, demon),
        engine::game::ReservationVerdict::Witness
    );
    let trail = manual_cast(
        &mut runner,
        demon,
        &Answers {
            picks: corpses[..3].to_vec(),
            ..ACCEPT_BOTH
        },
    );
    assert_eq!(trail.prompts, ["OptionalCostChoice", "PayCost"]);
    sacrifice_to_altar(&mut runner, altar, corpses[3]);

    assert!(finish_payment(&mut runner, demon));
    assert_eq!(life(&runner), 14);
    assert_eq!(pool(&runner), 0);
    assert!(corpses.iter().all(|&c| zone(&runner, c) == Zone::Graveyard));
}

/// Control: the Altar activated at priority first; the cast pays from the pool.
#[test]
fn altar_mana_made_before_the_cast_pays_the_floor() {
    let (mut runner, demon, altar, corpses) = demon_with_altar(4, 1);
    sacrifice_to_altar(&mut runner, altar, corpses[3]);

    let trail = cast(
        &mut runner,
        demon,
        &Answers {
            picks: corpses[..3].to_vec(),
            ..ACCEPT_BOTH
        },
    );

    assert_cast_completes(&runner, demon, &trail);
    assert_eq!((life(&runner), pool(&runner)), (14, 0));
}

/// CR 601.2g-h: Matt's board. Four Walking Corpses and Scathe Zombies, Ashnod's
/// Altar, two tapped Swamps and an empty pool, under Trinisphere: three
/// creatures pay the leg and the other two fuel two Altar activations during
/// payment, which pay the floored {3} with {C} left over.
#[test]
fn altar_activated_twice_pays_the_floor() {
    let mut scenario = board();
    add_trinisphere(&mut scenario);
    let mut fodder = creatures(&mut scenario, "Walking Corpse", 4, "", ManaColor::Black);
    fodder.extend(creatures(
        &mut scenario,
        "Scathe Zombies",
        1,
        "",
        ManaColor::Black,
    ));
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let demon = add_demon(&mut scenario);
    let mut runner = scenario.build();
    for swamp in swamps {
        runner.state_mut().objects.get_mut(&swamp).unwrap().tapped = true;
    }
    assert_eq!(
        verdict(&runner, demon),
        engine::game::ReservationVerdict::Witness
    );
    assert!(offered(&runner, demon));

    let trail = manual_cast(
        &mut runner,
        demon,
        &Answers {
            picks: fodder[..3].to_vec(),
            ..ACCEPT_BOTH
        },
    );
    assert_eq!(trail.prompts, ["OptionalCostChoice", "PayCost"]);
    sacrifice_to_altar(&mut runner, altar, fodder[3]);
    sacrifice_to_altar(&mut runner, altar, fodder[4]);

    assert!(finish_payment(&mut runner, demon));
    assert_eq!((life(&runner), pool(&runner)), (14, 1));
    assert!(fodder.iter().all(|&c| zone(&runner, c) == Zone::Graveyard));
}

const LAKE_OF_THE_DEAD: &str = "If this land would enter, sacrifice a Swamp instead. If you do, put this land onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add {B}.\n{T}, Sacrifice a Swamp: Add {B}{B}{B}{B}.";

/// CR 601.2g + CR 605.3a: Wurmcoil Engine ({6}) with an untapped Lake of the
/// Dead, a tapped Swamp, two Ashnod's Altars and two Walking Corpses: Lake's
/// sacrifice ability ({B}{B}{B}{B}) and one Altar activation pay it. Ordinary
/// castability sizes each Altar by one activation, so the sacrificial-mana
/// payment keeps Lake untapped for its four-mana ability rather than tapping
/// it for {B}. Paired with a Manual cast.
#[test]
fn ordinary_payment_keeps_the_four_mana_land_for_its_sacrifice() {
    for payment_mode in [
        CastPaymentMode::AutoExceptSacrificialMana,
        CastPaymentMode::Manual,
    ] {
        let mut scenario = board();
        let lake = scenario
            .add_land_from_oracle(P0, "Lake of the Dead", LAKE_OF_THE_DEAD)
            .id();
        let swamp = scenario.add_basic_land(P0, ManaColor::Black);
        let altars = [
            scenario
                .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
                .id(),
            scenario
                .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
                .id(),
        ];
        let corpses = creatures(&mut scenario, "Walking Corpse", 2, "", ManaColor::Black);
        let wurmcoil = scenario
            .add_creature_to_hand_from_oracle(P0, "Wurmcoil Engine", 6, 6, "Deathtouch, lifelink")
            .as_artifact_creature()
            .with_mana_cost(ManaCost::generic(6))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().objects.get_mut(&swamp).unwrap().tapped = true;
        let lake_four =
            engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
                .into_iter()
                .find(|s| s.source.object_id == lake && s.ability_index == Some(1));
        assert!(
            lake_four.is_some(),
            "reach: Lake's four-mana ability is a selection"
        );

        let card_id = runner.state().objects[&wurmcoil].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: wurmcoil,
                card_id,
                targets: vec![],
                payment_mode,
            })
            .expect("cast Wurmcoil Engine")
            .events;
        assert!(
            !runner.state().objects[&lake].tapped,
            "{payment_mode:?}: Lake stays untapped for its sacrifice ability"
        );
        assert_eq!(pool(&runner), 0, "{payment_mode:?}: nothing was prepaid");

        let mut sources = vec![(lake, 1, swamp), (altars[0], 0, corpses[0])].into_iter();
        let mut victim = None;
        for _ in 0..12 {
            if runner.state().objects[&wurmcoil].zone == Zone::Stack {
                break;
            }
            let action = match &runner.state().waiting_for {
                WaitingFor::PayCost { .. } => GameAction::SelectCards {
                    cards: vec![victim.take().expect("a sacrifice is pending")],
                },
                WaitingFor::ManaSourceSelection { options, .. } => match sources.next() {
                    Some((source, index, fodder)) => {
                        victim = Some(fodder);
                        GameAction::ActivateManaSource {
                            selection: options
                                .iter()
                                .find(|s| {
                                    s.source.object_id == source && s.ability_index == Some(index)
                                })
                                .cloned()
                                .expect("the source is offered"),
                        }
                    }
                    None => GameAction::PassPriority,
                },
                WaitingFor::ManaPayment { .. } => match sources.next() {
                    Some((source, index, fodder)) => {
                        victim = Some(fodder);
                        GameAction::ActivateAbility {
                            source_id: source,
                            ability_index: index,
                            payment_mode: CastPaymentMode::Auto,
                        }
                    }
                    None => GameAction::PassPriority,
                },
                other => panic!("{payment_mode:?}: unexpected {other:?}"),
            };
            events.extend(runner.act(action).expect("answer the payment").events);
        }
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::SpellCast { object_id, .. } if *object_id == wurmcoil)
            ),
            "{payment_mode:?}: the cast completes"
        );
        assert_eq!(zone(&runner, swamp), Zone::Graveyard);
        assert_eq!(zone(&runner, corpses[0]), Zone::Graveyard);
        assert_eq!(zone(&runner, corpses[1]), Zone::Battlefield);
    }
}

/// CR 118.3 + CR 601.2h: with three Corpses all held for the leg, the Altar has
/// no fuel, so {C} can't pay the floored {3}: refused, nothing spent.
#[test]
fn altar_without_spare_fuel_cant_pay_the_floor() {
    let (mut runner, demon, _, corpses) = demon_with_altar(3, 1);
    assert_eq!(
        verdict(&runner, demon),
        engine::game::ReservationVerdict::Rejected
    );
    assert!(!offered(&runner, demon));
    let trail = cast(&mut runner, demon, &ACCEPT_BOTH);
    assert!(trail.refused, "{:?}", trail.prompts);
    assert_eq!(
        (zone(&runner, demon), life(&runner), pool(&runner)),
        (Zone::Hand, 20, 1)
    );
    assert!(corpses
        .iter()
        .all(|&c| zone(&runner, c) == Zone::Battlefield));
}

/// CR 118.3 + CR 601.2h: Blood Pets ("Sacrifice this creature: Add {B}.") the
/// alternative sacrifices can't also make its floored {3}: with exactly three
/// it isn't offered; with six, three pay the leg and three the mana.
#[test]
fn self_sacrificing_fuel_held_for_the_leg_cant_pay_the_floor() {
    for (pets, offered_now) in [(3, false), (6, true)] {
        let mut scenario = board();
        add_trinisphere(&mut scenario);
        let pets = creatures(
            &mut scenario,
            "Blood Pet",
            pets,
            "Sacrifice this creature: Add {B}.",
            ManaColor::Black,
        );
        let demon = add_demon(&mut scenario);
        let mut runner = scenario.build();
        assert_eq!(offered(&runner, demon), offered_now, "{} Pets", pets.len());

        let trail = cast(
            &mut runner,
            demon,
            &Answers {
                picks: pets[..3].to_vec(),
                ..ACCEPT_BOTH
            },
        );

        if offered_now {
            // CR 605.3a: sacrificial mana is the player's to activate.
            for &pet in &pets[3..] {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: pet,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("sacrifice a Pet for {B}");
            }
            assert!(finish_payment(&mut runner, demon), "{:?}", trail.prompts);
            assert_eq!(life(&runner), 14);
            assert!(pets.iter().all(|&p| zone(&runner, p) == Zone::Graveyard));
        } else {
            assert!(trail.refused, "{:?}", trail.prompts);
            assert_eq!((zone(&runner, demon), life(&runner)), (Zone::Hand, 20));
            assert!(pets.iter().all(|&p| zone(&runner, p) == Zone::Battlefield));
        }
    }
}

/// CR 601.2g-h: Sephara's {W}, less Defiler of Faith's {W}, is floored to {3},
/// which two Altar activations pay while the four Ornithopters are tapped for
/// the leg.
#[test]
fn altar_fuels_a_floored_printed_mana_leg() {
    let mut scenario = board();
    add_trinisphere(&mut scenario);
    add_parsed_defiler(&mut scenario, "Defiler of Faith", DEFILER_OF_FAITH);
    let flyers = ornithopters(&mut scenario, 4);
    let corpses = creatures(&mut scenario, "Walking Corpse", 2, "", ManaColor::Black);
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let sephara = add_sephara(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(
        verdict(&runner, sephara),
        engine::game::ReservationVerdict::Witness
    );

    let trail = manual_cast(
        &mut runner,
        sephara,
        &Answers {
            picks: flyers.clone(),
            ..ACCEPT_BOTH
        },
    );
    assert_eq!(
        trail.prompts,
        ["DefilerPayment", "OptionalCostChoice", "PayCost"]
    );
    sacrifice_to_altar(&mut runner, altar, corpses[0]);
    sacrifice_to_altar(&mut runner, altar, corpses[1]);

    assert!(finish_payment(&mut runner, sephara));
    assert_eq!((life(&runner), pool(&runner)), (18, 1));
    assert!(flyers.iter().all(|&f| runner.state().objects[&f].tapped));
}

/// Zahid with Defiler of Dreams, a Gold token, two Plains and, when `spare`,
/// an Ornithopter: the Gold can't be both the tapped artifact and the third
/// mana.
#[test]
fn self_sacrificing_artifact_held_for_the_tap_cant_pay_the_mana() {
    for spare in [false, true] {
        let mut scenario = board();
        add_defiler_of_dreams(&mut scenario);
        let gold = scenario
            .add_artifact_from_oracle(
                P0,
                "Gold",
                "Sacrifice this artifact: Add one mana of any color.",
            )
            .id();
        scenario.add_basic_land(P0, ManaColor::White);
        scenario.add_basic_land(P0, ManaColor::White);
        let spare = spare.then(|| ornithopters(&mut scenario, 1)[0]);
        let zahid = add_zahid(&mut scenario);
        let mut runner = scenario.build();
        assert_eq!(offered(&runner, zahid), spare.is_some());

        let trail = cast(
            &mut runner,
            zahid,
            &Answers {
                picks: spare.into_iter().collect(),
                ..ACCEPT_BOTH
            },
        );

        if spare.is_some() {
            assert_cast_completes(&runner, zahid, &trail);
            assert_eq!(life(&runner), 18);
            assert_eq!(zone(&runner, gold), Zone::Graveyard, "the Gold paid {{1}}");
        } else {
            assert!(trail.refused, "{:?}", trail.prompts);
            assert_eq!(zone(&runner, gold), Zone::Battlefield);
        }
    }
}

/// Zahid, `{3}{U}` floating, P1's Humility, and the artifacts `build` adds.
fn zahid_under_humility(
    build: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = board();
    let artifacts = build(&mut scenario);
    scenario.add_enchantment_from_oracle(P1, "Humility", HUMILITY);
    let zahid = add_zahid(&mut scenario);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3), (ManaType::Blue, 1)]));
    (scenario.build(), zahid, artifacts)
}

/// Humility removes the Ornithopter's abilities, the injected prohibition
/// included, but leaves it no mana ability to misuse: the cast is legal, in
/// either insertion order, tapping the artifact chosen.
#[test]
fn humility_on_the_tapped_artifact_keeps_the_alternative() {
    let relic = |scenario: &mut GameScenario| {
        scenario
            .add_artifact_from_oracle(P0, "Darksteel Relic", "Indestructible")
            .id()
    };
    for relic_first in [false, true] {
        let (mut runner, zahid, artifacts) = zahid_under_humility(|scenario| {
            if relic_first {
                vec![relic(scenario), ornithopters(scenario, 1)[0]]
            } else {
                vec![ornithopters(scenario, 1)[0], relic(scenario)]
            }
        });
        let relic = artifacts[usize::from(!relic_first)];

        let trail = cast(
            &mut runner,
            zahid,
            &Answers {
                picks: vec![relic],
                ..ACCEPT_BOTH
            },
        );

        assert_cast_completes(&runner, zahid, &trail);
        assert!(runner.state().objects[&relic].tapped);
        assert_eq!(pool(&runner), 0);
    }
    let (mut runner, zahid, artifacts) = zahid_under_humility(|scenario| ornithopters(scenario, 1));
    let trail = cast(&mut runner, zahid, &ACCEPT_BOTH);
    assert_cast_completes(&runner, zahid, &trail);
    assert!(runner.state().objects[&artifacts[0]].tapped);
}

/// A Food token enchanted by Ensoul Artifact and made black by Darkest Hour, two
/// black Walking Corpses, then Humility, then Ninja Pizza ("Foods you control
/// have '{T}, Sacrifice this artifact: Add one mana of any color.'", a later
/// timestamp), and Demon of Death's Gate. `trinisphere` is `None` for no floor
/// and an empty pool, else `Some(n)`: Trinisphere and `n` floating {C}.
///
/// CR 118.3c + CR 601.2g: Pizza's mana ability is never needed with no floor or
/// with {3} floating, so the Food is sacrificed for the leg and the cast
/// completes; with {2} the Food can't both be the third sacrifice and make the
/// third mana, so the alternative is refused. The reservation holds the Food
/// although Humility removed its own statics (CR 613.7).
#[test]
fn granted_self_sacrifice_surviving_humility_is_held_by_the_reservation() {
    for trinisphere in [None, Some(3), Some(2)] {
        let mut scenario = board();
        if trinisphere.is_some() {
            add_trinisphere(&mut scenario);
        }
        let food = scenario
            .add_artifact_from_oracle(
                P0,
                "Food",
                "{2}, {T}, Sacrifice this token: You gain 3 life.",
            )
            .with_subtypes(vec!["Food"])
            .id();
        let ensoul = scenario
            .add_enchantment_from_oracle(P0, "Ensoul Artifact", "Enchant artifact\nEnchanted artifact is a creature with base power and toughness 5/5 in addition to its other types.")
            .with_subtypes(vec!["Aura"])
            .id();
        let corpses = creatures(&mut scenario, "Walking Corpse", 2, "", ManaColor::Black);
        scenario.add_enchantment_from_oracle(P0, "Darkest Hour", "All creatures are black.");
        scenario.add_enchantment_from_oracle(P1, "Humility", HUMILITY);
        scenario.add_enchantment_from_oracle(
            P0,
            "Ninja Pizza",
            "Foods you control have \"{T}, Sacrifice this artifact: Add one mana of any color.\"",
        );
        let demon = add_demon(&mut scenario);
        let floating = trinisphere.unwrap_or(0);
        scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, floating)]));
        let mut runner = scenario.build();
        // CR 111.10b: a Food token, controlled since the previous turn.
        runner.state_mut().objects.get_mut(&food).unwrap().is_token = true;
        runner
            .state_mut()
            .objects
            .get_mut(&ensoul)
            .unwrap()
            .attached_to = Some(engine::game::game_object::AttachTarget::Object(food));
        runner
            .state_mut()
            .objects
            .get_mut(&food)
            .unwrap()
            .attachments
            .push(ensoul);
        runner.state_mut().layers_dirty.mark_full();
        engine::game::layers::flush_layers(runner.state_mut());
        let food_obj = &runner.state().objects[&food];
        assert!(
            food_obj.card_types.core_types.contains(&CoreType::Creature)
                && food_obj.color.contains(&ManaColor::Black),
            "reach: the Food is a black creature"
        );
        assert!(
            food_obj.static_definitions.iter_all().next().is_none(),
            "reach: Humility removes the Food's own statics"
        );
        let food_mana =
            engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
                .into_iter()
                .filter(|s| s.source.object_id == food)
                .count();
        assert_eq!(food_mana, 1, "reach: Pizza's grant makes mana");

        if trinisphere == Some(2) {
            assert!(
                engine::game::alternative_mana_payable_with_nothing_held(
                    runner.state(),
                    P0,
                    demon,
                    &ManaCost::generic(3)
                ),
                "reach: the floored {{3}} is payable with nothing held"
            );
            // The reservation's prohibition lives only on the probe's scratch.
            let real = runner.state().clone();
            let objects = serde_json::to_value(&real.objects).unwrap();
            let unchanged = |state: &engine::types::game_state::GameState| {
                state.transient_continuous_effects == real.transient_continuous_effects
                    && state.next_timestamp == real.next_timestamp
                    && state.next_continuous_effect_id == real.next_continuous_effect_id
                    && state.next_end_effect_group_id == real.next_end_effect_group_id
                    && state.layers_dirty == real.layers_dirty
                    && state.resolved_rules_journal == real.resolved_rules_journal
                    && serde_json::to_value(&state.objects).unwrap() == objects
                    // The serde-skipped fields, compared directly.
                    && state.objects.len() == real.objects.len()
                    && real.objects.iter().all(|(id, before)| {
                        state.objects.get(id).is_some_and(|after| {
                            after.legacy_protection_start_exempt_attachments
                                == before.legacy_protection_start_exempt_attachments
                                && after.protection_start_grants == before.protection_start_grants
                                && after.protection_provenance == before.protection_provenance
                                && after.extra_loyalty_activations_this_turn
                                    == before.extra_loyalty_activations_this_turn
                        })
                    })
            };
            assert!(!offered(&runner, demon));
            assert!(
                unchanged(runner.state()),
                "offer probe left the state unchanged"
            );
            assert_eq!(
                verdict(&runner, demon),
                engine::game::ReservationVerdict::Rejected
            );
            assert!(
                unchanged(runner.state()),
                "verdict left the state unchanged"
            );
            assert!(
                !runner.state().transient_continuous_effects.iter().any(|tce| {
                    tce.modifications.iter().any(|m| {
                        matches!(m,
                            engine::types::ability::ContinuousModification::GrantStaticAbility { definition }
                                if matches!(definition.mode, engine::types::statics::StaticMode::CantPayCost { .. }))
                    })
                }),
                "no reservation grant reached the real state"
            );
            let trail = cast(&mut runner, demon, &ACCEPT_BOTH);
            assert!(trail.refused, "{:?}", trail.prompts);
            assert_eq!((zone(&runner, demon), life(&runner)), (Zone::Hand, 20));
        } else {
            assert!(offered(&runner, demon), "the alternative cast is offered");
            let trail = cast(
                &mut runner,
                demon,
                &Answers {
                    picks: vec![corpses[0], corpses[1], food],
                    ..ACCEPT_BOTH
                },
            );
            assert_cast_completes(&runner, demon, &trail);
            assert_eq!((life(&runner), pool(&runner)), (14, 0));
            assert!(corpses.iter().all(|&c| zone(&runner, c) == Zone::Graveyard));
            // CR 704.5d: the sacrificed Food token ceased to exist.
            assert!(!runner.state().objects.contains_key(&food));
        }
    }
}
/// Everflowing Chalices cast for real with multikicker `kicks`, then Zahid
/// with Defiler of Dreams: one Chalice is tapped for the leg, the other pays
/// the {3}.
fn chalices_then_zahid(kicks: [u32; 2]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = board();
    add_defiler_of_dreams(&mut scenario);
    let chalices: Vec<ObjectId> = kicks
        .iter()
        .map(|_| {
            scenario
                .add_artifact_to_hand_from_oracle(P0, "Everflowing Chalice", EVERFLOWING_CHALICE)
                .with_mana_cost(cost(0, &[]))
                .id()
        })
        .collect();
    let zahid = add_zahid(&mut scenario);
    let mut runner = scenario.build();
    for (&chalice, &kick) in chalices.iter().zip(&kicks) {
        add_pool(&mut runner, &[(ManaType::Colorless, 2 * kick as usize)]);
        let first = cast_action(&runner, chalice);
        runner.act(first).expect("cast the Chalice");
        // CR 702.33c: multikicker is offered again after each payment.
        while let WaitingFor::OptionalCostChoice { times_kicked, .. } = runner.state().waiting_for {
            runner
                .act(GameAction::DecideOptionalCost {
                    index: if times_kicked < kick { 0 } else { 1 },
                })
                .expect("answer multikicker");
        }
        assert_eq!(runner.state().objects[&chalice].zone, Zone::Stack);
        runner.pass_both_players();
        assert_eq!(
            runner.state().objects[&chalice]
                .counters
                .get(&engine::types::counter::CounterType::Generic(
                    "charge".into()
                ))
                .copied(),
            Some(kick),
            "reach: the Chalice entered with its counters"
        );
    }
    (runner, zahid, chalices)
}

/// Charge counters differ, so the Chalices aren't interchangeable: the
/// one-counter Chalice is tapped for the leg, in either cast order.
#[test]
fn chalices_with_different_counters_are_assigned_by_their_output() {
    for kicks in [[1, 3], [3, 1]] {
        let (mut runner, zahid, chalices) = chalices_then_zahid(kicks);
        let small = chalices[usize::from(kicks[0] == 3)];
        assert!(offered(&runner, zahid), "{kicks:?}");

        let trail = cast(
            &mut runner,
            zahid,
            &Answers {
                picks: vec![small],
                ..ACCEPT_BOTH
            },
        );

        assert_cast_completes(&runner, zahid, &trail);
        assert_eq!(life(&runner), 18);
        assert!(chalices.iter().all(|&c| runner.state().objects[&c].tapped));
    }
    let (runner, zahid, _) = chalices_then_zahid([1, 1]);
    assert!(!offered(&runner, zahid), "two one-counter Chalices can't");
}

/// Sephara with `birds` Birds of Paradise (summoning sick when `sick`), `spheres`
/// Spheres of Resistance and `pool` floating.
fn sephara_with_birds(
    birds: usize,
    sick: bool,
    spheres: usize,
    pool: &[(ManaType, usize)],
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = board();
    let birds = (0..birds)
        .map(|_| {
            let mut bird = scenario.add_creature_from_oracle(
                P0,
                "Birds of Paradise",
                0,
                1,
                "Flying\n{T}: Add one mana of any color.",
            );
            if sick {
                bird.with_summoning_sickness();
            }
            bird.id()
        })
        .collect();
    for _ in 0..spheres {
        scenario.add_artifact_from_oracle(
            P0,
            "Sphere of Resistance",
            "Spells cost {1} more to cast.",
        );
    }
    let sephara = add_sephara(&mut scenario);
    scenario.with_mana_pool(P0, mana(pool));
    (scenario.build(), sephara, birds)
}

/// One probe refuses Sephara with no white source; with {W}, the summoning-sick
/// Birds make no mana, so any four are held at once (CR 302.6 limits a
/// creature's own {T} abilities, not tapping it for a cost).
#[test]
fn summoning_sick_flyers_are_held_without_a_search() {
    let (runner, sephara, _) = sephara_with_birds(100, true, 0, &[]);
    assert!(!offered(&runner, sephara));

    let (mut runner, sephara, birds) = sephara_with_birds(100, true, 0, &[(ManaType::White, 1)]);
    let trail = cast(
        &mut runner,
        sephara,
        &Answers {
            picks: birds[..4].to_vec(),
            ..ACCEPT_BOTH
        },
    );
    assert_cast_completes(&runner, sephara, &trail);
    assert_eq!(pool(&runner), 0);
}

/// Twenty active Birds and nineteen Spheres: {19}{W} is payable with nothing
/// held, but every hold of four leaves sixteen mana. C(20, 4) = 4,845
/// assignments exceed the budget, so the alternative stays offered.
#[test]
fn reservation_search_past_its_budget_keeps_the_offer() {
    let (mut runner, sephara, _) = sephara_with_birds(20, false, 19, &[]);
    const { assert!(engine::game::RESERVATION_PROBE_BUDGET < 4_845) };
    assert_eq!(
        verdict(&runner, sephara),
        engine::game::ReservationVerdict::Exhausted
    );
    let first = cast_action(&runner, sephara);
    runner.act(first).expect("the cast starts");
    assert_eq!(runner.waiting_for_kind(), "OptionalCostChoice");
}

/// CR 118.9a: Bringer's own {W}{U}{B}{R}{G} and Fist of Suns' grant are one
/// election, offered once; with As Foretold first in scan order, its {0}
/// follows instead. Each Choice survives a save and reload; declining every
/// one reaches the printed cost: {7}{B} with the Defiler, payable only from
/// {W}{U}{B}{R}{G} and {3}.
#[test]
fn declining_every_alternative_reaches_the_printed_cost_once_each() {
    for foretold in [true, false] {
        let mut scenario = board();
        add_defiler_of_flesh(&mut scenario);
        if foretold {
            let foretold = scenario
                .add_enchantment_from_oracle(P0, "As Foretold", AS_FORETOLD)
                .id();
            scenario.with_counter(foretold, engine::types::counter::CounterType::Time, 9);
        }
        scenario.add_artifact_from_oracle(P0, "Fist of Suns", FIST_OF_SUNS);
        let bringer = add_bringer(&mut scenario);
        let mut pool_kinds = WURG.to_vec();
        if !foretold {
            pool_kinds.extend([(ManaType::Black, 1), (ManaType::Colorless, 3)]);
        }
        scenario.with_mana_pool(P0, mana(&pool_kinds));
        let mut runner = scenario.build();
        let first = cast_action(&runner, bringer);
        runner.act(first).expect("the cast starts");
        runner
            .act(GameAction::DecideOptionalCost { index: 0 })
            .expect("accept the Defiler");

        let mut offers = Vec::new();
        for _ in 0..4 {
            let Some(offer) = offered_alternative(&runner) else {
                break;
            };
            offers.push(offer);
            runner = round_trip(&runner);
            assert_eq!(offered_alternative(&runner), offers.last().cloned());
            if runner
                .act(GameAction::DecideOptionalCost { index: 1 })
                .is_err()
            {
                break;
            }
        }

        if foretold {
            assert_eq!(offers, [wubrg_cost(), ManaCost::zero()]);
            assert_eq!((zone(&runner, bringer), pool(&runner)), (Zone::Hand, 4));
        } else {
            assert_eq!(offers, [wubrg_cost()], "one election, offered once");
            assert_eq!(zone(&runner, bringer), Zone::Stack);
            assert_eq!((life(&runner), pool(&runner)), (18, 0));
        }
    }
}
/// CR 119.4 + CR 601.2h: Enslave targeting Terror of the Peaks costs 3 more
/// life, the Defiler 2 and K'rrik's {B} 2: 7 life. At 5 it can't be cast; at 8
/// it is, for exactly 7.
#[test]
fn imposed_life_and_defiler_life_are_held_from_life_for_mana() {
    for (start, cast_now) in [(5, false), (8, true)] {
        let mut scenario = board();
        scenario.with_life(P0, start);
        add_defiler_of_flesh(&mut scenario);
        add_krrik_grant(&mut scenario);
        let terror = scenario
            .add_creature(P1, "Terror of the Peaks", 5, 4)
            .from_oracle_text_with_keywords(&["Flying"], TERROR_OF_THE_PEAKS)
            .id();
        let enslave = scenario
            .add_spell_to_hand(P0, "Enslave", false)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .with_mana_cost(cost(4, &[ManaCostShard::Black, ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&["Enchant"], ENSLAVE)
            .id();
        scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 4)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            enslave,
            &Answers {
                targets: vec![terror],
                ..ACCEPT_BOTH
            },
        );

        if cast_now {
            assert_cast_completes(&runner, enslave, &trail);
            assert_eq!((life(&runner), pool(&runner)), (1, 0));
        } else {
            // Both Defiler answers are refused as unpayable; nothing is paid.
            assert!(trail.refused, "{:?}", trail.prompts);
            assert_eq!(trail.prompts, ["TargetSelection", "DefilerPayment"]);
            assert!(runner
                .act(GameAction::DecideOptionalCost { index: 1 })
                .is_err());
            assert_eq!((zone(&runner, enslave), life(&runner)), (Zone::Hand, 5));
        }
    }
}

/// K'rrik, Son of Yawgmoth's "For each {B} in a cost, you may pay 2 life
/// rather than pay that mana." on P0's battlefield.
fn add_krrik_grant(scenario: &mut GameScenario) {
    let krrik = parse_oracle_text(KRRIK_GRANT, "K'rrik", &[], &["Creature".into()], &[]);
    let mut grant = scenario.add_creature(P0, "K'rrik, Son of Yawgmoth", 2, 2);
    for s in krrik.statics {
        grant.with_static_definition(s);
    }
}

const PHYREXIAN_SCUTA: &str = "Kicker\u{2014}Pay 3 life. (You may pay 3 life in addition to any other costs as you cast this spell.)\nIf this creature was kicked, it enters with two +1/+1 counters on it.";

/// CR 119.4 + CR 601.2h: Phyrexian Scuta's kicker pays 3 life, and K'rrik pays
/// its {B} with 2 more. At 4 life the kicker isn't offered and the cast pays
/// {3} and 2 life; at 6 it is, and the kicked cast leaves 1.
#[test]
fn kicker_life_is_held_from_life_for_mana() {
    for (start, kicked) in [(4, false), (6, true)] {
        let mut scenario = board();
        scenario.with_life(P0, start);
        add_krrik_grant(&mut scenario);
        let scuta = scenario
            .add_creature_to_hand(P0, "Phyrexian Scuta", 3, 3)
            .with_mana_cost(cost(3, &[ManaCostShard::Black]))
            .with_color(vec![ManaColor::Black])
            .from_oracle_text_with_keywords(&["Kicker"], PHYREXIAN_SCUTA)
            .id();
        scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3)]));
        let mut runner = scenario.build();

        let trail = cast(
            &mut runner,
            scuta,
            &Answers {
                optional: true,
                ..ACCEPT_BOTH
            },
        );

        assert_cast_completes(&runner, scuta, &trail);
        assert_eq!(trail.count("OptionalCostChoice"), usize::from(kicked));
        assert_eq!(life(&runner), if kicked { 1 } else { 2 });
    }
}

// ---------------------------------------------------------------------------
// Alternative costs that are not mana (CR 118.9d)
// ---------------------------------------------------------------------------

const FIELDMIST_BORDERPOST: &str = "You may pay {1} and return a basic land you control to its owner's hand rather than pay this spell's mana cost.\nThis artifact enters tapped.\n{T}: Add {W} or {U}.";

/// CR 118.9 + CR 601.2f: Fieldmist Borderpost (a printed card absent from the
/// integration fixture, built from its Oracle text and printed {1}{W}{U}) cast
/// for its alternative cost pays {1} and a basic land, not its mana cost too.
#[test]
fn composite_alternative_cost_pays_only_its_own_mana() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = crate::support::shared_card_db().expect("the integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plains = scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    let borderpost = scenario
        .add_artifact_to_hand_from_oracle(P0, "Fieldmist Borderpost", FIELDMIST_BORDERPOST)
        .with_mana_cost(cost(1, &[ManaCostShard::White, ManaCostShard::Blue]))
        .id();
    scenario.with_mana_pool(
        P0,
        mana(&[
            (ManaType::Green, 4),
            (ManaType::White, 1),
            (ManaType::Blue, 1),
        ]),
    );
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        borderpost,
        &Answers {
            choice_preferred: true,
            picks: vec![plains],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, borderpost, &trail);
    assert_eq!(trail.prompts, ["OptionalCostChoice", "PayCost"]);
    assert_eq!(zone(&runner, plains), Zone::Hand);
    assert_eq!(pool(&runner), 5);
}

/// CR 118.9d: Thalia, Guardian of Thraben's {1} increase applies to Snuff Out's
/// 4-life alternative cost.
#[test]
fn cost_increase_applies_to_a_life_alternative_cost() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = crate::support::shared_card_db().expect("the integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario.add_real_card(P1, "Thalia, Guardian of Thraben", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let snuff_out = scenario.add_real_card(P0, "Snuff Out", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Green, 4)]));
    let mut runner = scenario.build();

    let trail = cast(
        &mut runner,
        snuff_out,
        &Answers {
            choice_preferred: true,
            targets: vec![bears],
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, snuff_out, &trail);
    assert_eq!(trail.prompts, ["TargetSelection", "OptionalCostChoice"]);
    assert_eq!(life(&runner), 16);
    assert_eq!(pool(&runner), 3);
}

/// Verbatim Oracle text (Scryfall).
const SADISTIC_SKYMARCHER: &str = "As an additional cost to cast this spell, reveal a Vampire card from your hand or pay {1}.\nFlying, lifelink";

/// CR 601.2b + CR 601.2f + CR 601.2h: with no Vampire to reveal, Sadistic
/// Skymarcher's {1} branch is its only payable alternative, and {2}{B} + {1}
/// is affordable from three colorless mana only with Defiler of Flesh's {B}
/// reduction. The castability gate and the declaration judge the branch with
/// the same Defiler-aware authority, so the spell is offered and the cast pays
/// exactly {3} and 2 life.
#[test]
fn choice_branch_affordable_only_with_the_defiler_is_castable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    add_defiler_of_flesh(&mut scenario);
    let not_a_vampire = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let skymarcher = scenario
        .add_creature_to_hand_from_oracle(P0, "Sadistic Skymarcher", 3, 2, SADISTIC_SKYMARCHER)
        .with_mana_cost(cost(2, &[ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .with_subtypes(vec!["Vampire", "Soldier"])
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Colorless, 3)]));
    let mut runner = scenario.build();

    assert!(
        engine::game::casting::can_cast_object_now(runner.state(), P0, skymarcher),
        "the {{1}} branch is affordable with the Defiler's reduction"
    );

    let trail = cast(
        &mut runner,
        skymarcher,
        &Answers {
            defiler: true,
            ..Answers::default()
        },
    );

    assert_cast_completes(&runner, skymarcher, &trail);
    assert_eq!(trail.count("OptionalCostChoice"), 0, "{:?}", trail.prompts);
    assert_eq!(trail.count("DefilerPayment"), 1, "{:?}", trail.prompts);
    assert_eq!(zone(&runner, not_a_vampire), Zone::Hand);
    assert_eq!(life(&runner), 18, "the Defiler's 2 life");
    assert_eq!(pool(&runner), 0, "{{2}}{{B}} + {{1}} less {{B}}");
}
