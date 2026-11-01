//! Selesnya Loft Gardens (Plane): "Whenever chaos ensues, until end of turn,
//! whenever you tap a land for mana, add one mana of any type that land
//! produced." + Simic Growth Chamber ("{T}: Add {G}{U}").
//!
//! Structural regression for the delayed mana-echo class: a tap event that
//! produces several types at once must offer the resolving player exactly ONE
//! of them (CR 608.2d — the Oracle asks for "one mana"), not deposit every
//! produced type. Before the shared choice-contract fix this test's tap
//! deposited base {G}{U} PLUS bonus {G}{U} with no prompt; now the bonus
//! pauses on a SingleColor choice and deposits exactly the chosen mana.
//!
//! The chaos event is synthesized directly (`ChaosEnsued` keyed by Selesnya's
//! object id) rather than driven through planar-deck setup: no scenario
//! helper places planes or rolls the planar die, and planechase plumbing is
//! not under test. The exercised seam is the CR 311.7/901.9b plane-identity
//! binding plus everything downstream — chaos matching, delayed installation,
//! tap matching, choice, deposit — all through the real pipeline with verbatim
//! Oracle text. Selesnya stays a battlefield land; its chaos trigger fires
//! from the Battlefield member of its parsed trigger zones.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr, TargetFilter,
    TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, ConvokeMode, ManaChoice, ManaChoicePrompt, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

const SELESNYA_ORACLE: &str = "If an effect would create one or more tokens, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent, it puts twice that many of those counters on that permanent instead.\nWhenever chaos ensues, until end of turn, whenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str =
    "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";
const MANA_FLARE_ORACLE: &str =
    "Whenever a player taps a land for mana, that player adds one mana of any type that land produced.";

fn count(runner: &GameRunner, p: PlayerId, color: ManaType) -> usize {
    runner.state().players[p.0 as usize]
        .mana_pool
        .count_color(color)
}

/// Index of the object's first root-`Effect::Mana` activated ability (bounce
/// lands carry an ETB trigger and an enters-tapped replacement alongside the
/// tap ability, so position 0 cannot be assumed).
fn mana_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|def| matches!(*def.effect, Effect::Mana { .. }))
        .unwrap_or_else(|| panic!("object {id:?} has no mana ability"))
}

/// Fire real chaos through the real pipeline and prove Selesnya installed
/// exactly one until-end-of-turn delayed trigger. Shared setup for the rows
/// below; returns the runner with the trigger live.
fn runner_with_selesnya_live() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    // CR 311.7 / CR 901.9b: the chaos event is keyed by the plane whose
    // ability fires; synthesizing it with Selesnya's id exercises the real
    // plane-identity binding without planar-deck setup.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    // If the chaos trigger resolved synchronously the delayed trigger is
    // installed; otherwise it sits on the stack behind one priority pass.
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "chaos must install exactly one Selesnya delayed trigger"
    );
    (runner, chamber, forest)
}

/// CR 608.2d + CR 106.12a: tapping Simic Growth Chamber ({G}{U}) with
/// Selesnya's delayed trigger live must pause on a Green/Blue choice — not
/// silently deposit bonus {G}{U} — and the answer must yield exactly one
/// bonus mana of the chosen type on top of the base {G}{U}.
#[test]
fn selesnya_echo_from_multitype_tap_offers_one_chosen_mana() {
    let (mut runner, chamber, _) = runner_with_selesnya_live();
    let idx = mana_ability_index(&runner, chamber);
    runner
        .act(GameAction::ActivateAbility {
            source_id: chamber,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("tapping the chamber must succeed");

    // The bonus must NOT have resolved silently: base {G}{U} is in the pool
    // and the choice is pending. Immediate resolution (CR 605.4a) pauses on
    // the choice with no stack entry and no priority pass.
    assert_eq!(count(&runner, P0, ManaType::Green), 1);
    assert_eq!(count(&runner, P0, ManaType::Blue), 1);
    assert!(
        runner.state().stack.is_empty(),
        "the Selesnya echo must pause inside immediate resolution, not stack"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(
                *player, P0,
                "the echo choice belongs to the tapping player (= controller here)"
            );
            assert_eq!(
                options.len(),
                2,
                "the choice must offer exactly the two produced types; got {options:?}"
            );
            assert!(
                options.contains(&ManaType::Green) && options.contains(&ManaType::Blue),
                "the choice must offer Green and Blue; got {options:?}"
            );
        }
        other => panic!("expected a SingleColor echo choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must succeed");
    // Base {G}{U} + exactly one bonus {G}: reverting the choice contract
    // yields base {G}{U} + bonus {G}{U} (green 2, blue 2) with no prompt.
    assert_eq!(
        count(&runner, P0, ManaType::Green),
        2,
        "base {{G}} + exactly one chosen bonus {{G}}"
    );
    assert_eq!(
        count(&runner, P0, ManaType::Blue),
        1,
        "no unchosen bonus {{U}} may be deposited"
    );
    // Completion: the answer must return the activation to Priority with no
    // stranded inline-resolution frame and no trigger entry on the stack.
    assert!(
        runner.state().stack.is_empty(),
        "the echo resolves inline and must not add a stack entry"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the echo choice must complete back to Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Sibling control: a single-type tap with the same delayed trigger live
/// resolves inline with no prompt (CR 106.5 — single option auto-picks) and
/// deposits base + exactly one bonus. Guards the common basic-land case
/// against the new prompt machinery.
#[test]
fn selesnya_echo_from_single_type_tap_has_no_prompt_and_adds_one() {
    let (mut runner, _, forest) = runner_with_selesnya_live();
    let idx = mana_ability_index(&runner, forest);
    runner
        .act(GameAction::ActivateAbility {
            source_id: forest,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("tapping the Forest must succeed");
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a single-type echo must not prompt; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        count(&runner, P0, ManaType::Green),
        2,
        "base {{G}} + exactly one bonus {{G}} with no prompt"
    );
}

/// Tapper-routing sibling (Mana Flare class — explicit-recipient echo body):
/// P1 taps P1's own chamber while Mana Flare lives. The choice must go to
/// the TAPPER (P1, via TriggeringPlayer), not the Flare controller, and the
/// answer must land exactly one bonus mana in P1's pool with P0 untouched.
/// Covers the `handle_choose_mana_effect` recipient half of the contract.
#[test]
fn mana_flare_echo_routes_choice_and_bonus_to_tapper() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Mana Flare", MANA_FLARE_ORACLE)
        .id();
    let chamber_p0 = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let chamber_p1 = scenario
        .add_land_from_oracle(P1, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let mut runner = scenario.build();
    let _ = chamber_p0;

    // Hand priority to P1 (active player P0 passes with an empty stack).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passing priority must succeed");
    assert_eq!(
        runner.state().priority_player,
        P1,
        "P1 must hold priority to tap its own land"
    );

    let idx = mana_ability_index(&runner, chamber_p1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: chamber_p1,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 tapping its own chamber must succeed");

    // Base {G}{U} in P1's pool, choice pending FOR P1 with both options.
    // Immediate resolution pauses with no stack entry and no priority pass.
    assert_eq!(count(&runner, P1, ManaType::Green), 1);
    assert_eq!(count(&runner, P1, ManaType::Blue), 1);
    assert!(
        runner.state().stack.is_empty(),
        "the Mana Flare echo must pause inside immediate resolution, not stack"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the echo choice belongs to the tapper P1, not the Flare controller P0"
            );
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a SingleColor echo choice for P1, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("P1 answering Blue must succeed");
    // Reverting the recipient half misroutes the bonus to P0; reverting the
    // count half deposits two Blue. Both flips are caught here.
    assert_eq!(count(&runner, P1, ManaType::Green), 1, "base {{G}} only");
    assert_eq!(
        count(&runner, P1, ManaType::Blue),
        2,
        "base {{U}} + exactly one chosen bonus {{U}}"
    );
    assert_eq!(count(&runner, P0, ManaType::Green), 0, "P0 gains nothing");
    assert_eq!(count(&runner, P0, ManaType::Blue), 0, "P0 gains nothing");
}

const MIRARI_ORACLE: &str = "Creatures you control get +1/+1.\nWhenever you tap a land for mana, add one mana of any type that land produced.";

/// Cost-payment control: auto-tapping Simic Growth Chamber to help pay for a
/// spell while Mirari's Wake lives must pause the payment on the same
/// SingleColor choice — with no stack entry — and resume payment afterwards.
/// CR 605.4a keeps the bonus spendable by the thing it resumes into (the
/// automatic payment finalizer here). Reverting the choice contract deposits
/// bonus {G}{U} with no prompt; the leftover-pool asserts below catch both
/// that and any resumption failure (an unresumed payment never puts the spell
/// on the stack).
#[test]
fn mirari_echo_choice_pauses_cost_payment_without_stack() {
    fn floating_colorless(n: usize) -> Vec<ManaUnit> {
        (0..n)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect()
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    // Three floating colorless plus the chamber's {G}{U} exactly cover
    // {3}{G}{U}; the auto-tapper must take the chamber for the colored half.
    scenario.with_mana_pool(P0, floating_colorless(3));
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("starting the cast must succeed");
    // Mid-payment: the echo paused with no TRIGGERED entry on the stack (the
    // one entry present is the spell being cast itself) and no priority pass.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the spell being cast may be on the stack — the echo adds no entry"
    );
    assert!(
        matches!(
            runner.state().stack.back().map(|e| &e.kind),
            Some(engine::types::game_state::StackEntryKind::Spell { .. })
        ),
        "the single stack entry must be the spell being cast"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 pays, so P0 chooses");
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a SingleColor echo choice mid-payment, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must resume payment");
    // Payment resumed and completed: the spell is on the stack, and exactly
    // the chosen bonus {G} is left over (3 floating + base {G}{U} + bonus {G}
    // minus {3}{G}{U}).
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the paid spell must reach the stack after the choice resumes payment"
    );
    assert_eq!(
        count(&runner, P0, ManaType::Green),
        1,
        "exactly the chosen bonus {{G}} remains"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "nothing else may be left floating"
    );
}

/// Bonus-dependent payment discriminator (CR 605.4a + CR 601.2g-h): with only
/// TWO floating colorless, the chamber's base {G}{U} is insufficient for
/// {3}{G}{U} (2 + 2 = 4 < 5) — exactly one chosen echo bonus is REQUIRED.
/// Before the pre-spend continuation fix this cast failed "Cannot pay mana
/// cost" (the skipping inline loop omitted the bonus from affordability and
/// the pre-spend pool). Now the live payment pauses for the real choice
/// BEFORE spending and resumes this same payment with the bonus.
/// Reverting the pre-spend suspension fails at `CastSpell` (no prompt);
/// reverting the preview accounting fails castability (not offered).
#[test]
fn mirari_echo_bonus_required_for_payment_with_two_floating() {
    fn floating_colorless(n: usize) -> Vec<ManaUnit> {
        (0..n)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect()
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    // Two floating colorless + base {G}{U} = 4 mana; {3}{G}{U} needs 5.
    // The auto-tapper must take the chamber, then pause for the echo choice;
    // the chosen bonus funds the third generic.
    scenario.with_mana_pool(P0, floating_colorless(2));
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast with bonus-dependent payment must pause for choice, not fail Cannot pay");
    // Pre-spend pause: the spell being cast is already on the stack (601.2a),
    // the echo adds no TRIGGERED entry, and the choice is required (no
    // Priority, no auto-completion).
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the spell being cast may be on the stack — the echo adds no entry"
    );
    assert!(
        matches!(
            runner.state().stack.back().map(|e| &e.kind),
            Some(engine::types::game_state::StackEntryKind::Spell { .. })
        ),
        "the single stack entry must be the spell being cast"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 pays, so P0 chooses");
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a required SingleColor echo choice pre-spend, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must resume payment");
    // Payment resumed and completed: the spell is on the stack, the pool is
    // empty (2 floating + base {G}{U} + bonus {G} minus {3}{G}{U} = 0), and
    // the continuation is terminal Priority with no stranded carrier.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the paid spell must reach the stack after the choice resumes payment"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "bonus-dependent payment spends everything (2C + GU + bonus G for 3GU)"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority with no stranded choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_cast.is_none(),
        "the outer payment must complete (no stranded pending cast)"
    );
}

/// CR 605.4a + CR 608.2d: two echo firings on one tap owe two bonuses. Two
/// Mirari's Wakes plus Simic Growth Chamber ({G}{U}), empty pool, casting
/// {2}{G}{U}: base {G}{U} plus one chosen bonus per Wake. A unioned
/// single-type preview rejects this (one virtual mana cannot cover it);
/// per-firing accounting admits it, and the live payment pauses twice in
/// sequence — answering once must leave the second choice pending, not
/// complete the payment.
#[test]
fn two_wake_echoes_fund_cost_requiring_two_bonuses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        generic: 2,
    };
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(cost.clone())
        .id();
    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    // Preview must count one bonus per firing: base {G}{U} + two bonuses funds
    // {2}{G}{U} from an empty pool.
    assert!(
        engine::game::casting::can_pay_cost_after_auto_tap(runner.state(), P0, threat_id, &cost),
        "two echo bonuses plus base {{G}}{{U}} must fund {{2}}{{G}}{{U}}"
    );

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("first echo firing must pause for choice, not fail Cannot pay");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 pays, so P0 chooses");
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected the first SingleColor echo choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must reach the second firing, not complete payment");
    // The second Wake's firing pauses again: still choosing, no Priority yet.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 still chooses");
            assert_eq!(options.len(), 2);
        }
        other => panic!("expected the second SingleColor echo choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering Blue must resume payment");
    // Base {G}{U} + bonus {G} + bonus {U} minus {2}{G}{U}: nothing left.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the paid spell must reach the stack after both choices resume payment"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "two bonuses plus base must exactly fund {{2}}{{G}}{{U}}"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 605.4a + CR 608.2d: the Selesnya delayed-generator echo funds a
/// bonus-dependent payment the same way a printed echo does. Chaos live plus
/// Simic Growth Chamber ({G}{U}), two floating colorless, casting {3}{G}{U}:
/// the preview must count the delayed firing (a battlefield-only scan sees no
/// bonus at all), and the live payment pauses pre-spend for the real choice.
#[test]
fn selesnya_delayed_echo_funds_bonus_dependent_payment() {
    fn floating_colorless(n: usize) -> Vec<ManaUnit> {
        (0..n)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect()
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    // Two floating colorless + base {G}{U} = 4 mana; {3}{G}{U} needs 5, so the
    // delayed echo's one chosen bonus is required.
    scenario.with_mana_pool(P0, floating_colorless(2));
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        generic: 3,
    };
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(cost.clone())
        .id();
    let mut runner = scenario.build();

    // CR 311.7 / CR 901.9b: synthesize chaos keyed by Selesnya's id, as in
    // `runner_with_selesnya_live`.
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "chaos must install exactly one Selesnya delayed trigger"
    );

    // Preview must count the delayed firing: without it the cast looks
    // unpayable (2 floating + base {G}{U} cannot cover {3}{G}{U}).
    assert!(
        engine::game::casting::can_pay_cost_after_auto_tap(runner.state(), P0, threat_id, &cost),
        "the delayed echo bonus plus base {{G}}{{U}} must fund {{3}}{{G}}{{U}}"
    );

    let threat_card = runner.state().objects[&threat_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("delayed-echo payment must pause for choice, not fail Cannot pay");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 pays, so P0 chooses");
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a SingleColor echo choice pre-spend, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering Blue must resume payment");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the paid spell must reach the stack after the choice resumes payment"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "bonus-dependent payment spends everything (2C + GU + bonus U for 3GU)"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority, got {:?}",
        runner.state().waiting_for
    );
}

const MANABARBS_ORACLE: &str =
    "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.";

/// Convergence control (CR 117.5 + CR 605.4a): after the echo choice resumes
/// a settled direct-Priority activation, ordinary deferred observers from the
/// same tap must reach the stack before priority returns. Board: Mirari's
/// Wake + Simic Growth Chamber + Manabarbs (non-mana ordinary observer).
/// P0 taps the chamber, answers the echo choice, and the Manabarbs damage
/// trigger must then sit on the stack; resolving it costs P0 1 life.
/// Reverting the settled-priority convergence leaves the stack empty here
/// (Manabarbs stranded until some later action).
#[test]
fn mirari_echo_choice_drains_ordinary_observer_before_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Manabarbs", MANABARBS_ORACLE);
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let idx = mana_ability_index(&runner, chamber);
    runner
        .act(GameAction::ActivateAbility {
            source_id: chamber,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("tapping the chamber must succeed");
    // Base {G}{U} in the pool; echo choice pending with no triggered entry.
    assert_eq!(count(&runner, P0, ManaType::Green), 1);
    assert_eq!(count(&runner, P0, ManaType::Blue), 1);
    assert!(
        runner.state().stack.is_empty(),
        "the echo choice must not use the stack"
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering Green must succeed");

    // The bonus resolved AND the ordinary observer drained: base {G}{U} +
    // bonus {G} in the pool, Manabarbs' damage trigger on the stack.
    assert_eq!(
        count(&runner, P0, ManaType::Green),
        2,
        "base {{G}} + chosen bonus {{G}}"
    );
    assert_eq!(count(&runner, P0, ManaType::Blue), 1, "base {{U}} only");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Manabarbs must reach the stack before returned priority"
    );

    // Resolve it: both players pass, P0 takes 1 from its own tap.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passing must succeed");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passing must succeed");
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "Manabarbs deals 1 to the tapping player"
    );
}

/// Public-pipeline unscoped proof: a hand-installed UNSCOPED echo trigger
/// (P0-controlled source, subjectless body, no actor scoping — the fixture
/// shape from the `unscoped_echo_*` unit tests, which call the reducer
/// directly) driven through REAL `ActivateAbility` + `ChooseManaColor`
/// actions. P1 taps P1's own chamber; the choice must go to tapper P1 with
/// both options; answering Blue lands exactly one Blue in P1's pool with P0
/// untouched; completion returns to Priority with no stranded state. This
/// covers the reducer's latched-player threading and frame completion that
/// direct-call tests bypass. Reverting the prompt origin to the controller
/// default flips the prompt-player assert; reverting completion flips the
/// pool asserts.
#[test]
fn unscoped_echo_public_pipeline_prompts_and_pays_tapper() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let echo_source = scenario.add_creature(P0, "Echo Source", 1, 1).id();
    let chamber_p1 = scenario
        .add_land_from_oracle(P1, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&echo_source)
        .expect("echo source must exist")
        .trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: ManaProduction::TriggerEventManaType,
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::Typed(TypedFilter::land())),
        );

    // Hand priority to P1 (active player P0 passes with an empty stack).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passing priority must succeed");
    assert_eq!(
        runner.state().priority_player,
        P1,
        "P1 must hold priority to tap its own land"
    );

    let idx = mana_ability_index(&runner, chamber_p1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: chamber_p1,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 tapping its own chamber must succeed");

    // Base {G}{U} in P1's pool; the choice pends for TAPPER P1 (not the
    // P0 source controller) with no stack entry and no priority pass.
    assert_eq!(count(&runner, P1, ManaType::Green), 1);
    assert_eq!(count(&runner, P1, ManaType::Blue), 1);
    assert!(
        runner.state().stack.is_empty(),
        "the echo choice must pause inside immediate resolution, not stack"
    );
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(
                *player, P1,
                "unscoped echo choice belongs to tapper P1, not controller P0"
            );
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a SingleColor echo choice for P1, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("P1 answering Blue must succeed");
    assert_eq!(count(&runner, P1, ManaType::Green), 1, "base {{G}} only");
    assert_eq!(
        count(&runner, P1, ManaType::Blue),
        2,
        "base {{U}} + exactly one chosen bonus {{U}}"
    );
    assert_eq!(count(&runner, P0, ManaType::Green), 0, "P0 gains nothing");
    assert_eq!(count(&runner, P0, ManaType::Blue), 0, "P0 gains nothing");
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority with no stranded state, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 702.51a + CR 605.4a + CR 601.2g-h: a Conclave-Tribunal-shaped
/// ({3}{W}, Convoke) spell whose only feasible payment needs BOTH the Mirari's
/// Wake echo bonus and a white creature's convoke tap must be offered and
/// castable from an empty pool. The chamber's base {G}{U} + one chosen echo
/// bonus is 3 mana; {3}{W} needs 4, so the white creature's convoke tap pays
/// the {W}. The mana-only echo upper bound used to reject this payment before
/// the exact convoke-aware search could see it. The test shell supplies
/// Convoke and a no-target resolution; the card's ETB is not under test.
#[test]
fn mirari_echo_and_convoke_together_fund_bonus_dependent_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ORACLE);
    let chamber = scenario
        .add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE)
        .id();
    let elite = scenario
        .add_creature(P0, "Elite Vanguard", 2, 1)
        .with_color(vec![ManaColor::White])
        .id();
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 3,
    };
    let spell = scenario
        .add_spell_to_hand(P0, "Conclave Tribunal", true)
        .with_mana_cost(cost.clone())
        .with_keyword(Keyword::Convoke)
        .with_ability(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();

    // The regression: empty pool, and neither resource alone can pay {3}{W}.
    // The preview must count the per-firing echo bonus and the convoke
    // contribution together.
    assert!(
        engine::game::casting::can_pay_cost_after_auto_tap(runner.state(), P0, spell, &cost),
        "base {{G}}{{U}} + one echo bonus + the white creature's convoke tap must fund {{3}}{{W}}"
    );

    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the convoke-plus-echo cast must be offered and start");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ),
        "the spell must reach the interactive convoke payment step; got {:?}",
        runner.state().waiting_for
    );

    // Convoke pays the white shard with the white creature.
    runner
        .act(GameAction::TapForConvoke {
            object_id: elite,
            mana_type: ManaType::White,
        })
        .expect("the white creature must pay the white shard");
    assert!(runner.state().objects[&elite].tapped);

    // Base {G}{U} from the chamber pauses on the Mirari echo; answer it.
    let idx = mana_ability_index(&runner, chamber);
    runner
        .act(GameAction::ActivateAbility {
            source_id: chamber,
            ability_index: idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the chamber must produce the base {{G}}{{U}}");
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "P0 pays, so P0 chooses");
            assert_eq!(options.len(), 2);
            assert!(options.contains(&ManaType::Green) && options.contains(&ManaType::Blue));
        }
        other => panic!("expected a SingleColor echo choice during convoke payment, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the echo must resume the convoke payment");

    // Commit the interactive payment.
    runner
        .act(GameAction::PassPriority)
        .expect("the convoke-plus-echo payment must commit");
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            engine::types::game_state::StackEntryKind::Spell { .. }
        )),
        "the paid spell must reach the stack"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion returns to Priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "3 mana + one convoke unit exactly pays {{3}}{{W}}"
    );
}

/// CR 605.4a + CR 608.2d: a single-type delayed echo owes no type choice, so
/// it resolves inline during payment. The affordability preview must count it
/// before the auto-payment decision: one Forest's base {G} alone cannot pay
/// {1}{G}, but base {G} + the delayed echo's {G} can.
#[test]
fn selesnya_single_type_delayed_echo_funds_auto_payment_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selesnya = scenario
        .add_land_from_oracle(P0, "Selesnya Loft Gardens", SELESNYA_ORACLE)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    let spell = scenario
        .add_spell_to_hand(P0, "Single-Type Echo Spell", true)
        .with_mana_cost(cost.clone())
        .with_ability(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();

    // Install Selesnya's until-end-of-turn delayed echo through the real chaos
    // pipeline (same seam as `runner_with_selesnya_live`).
    let events = vec![GameEvent::ChaosEnsued { plane_id: selesnya }];
    process_triggers(runner.state_mut(), &events);
    while runner.state().delayed_triggers.is_empty() && !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the chaos trigger must succeed");
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "chaos must install exactly one Selesnya delayed trigger"
    );

    assert!(
        engine::game::casting::can_pay_cost_after_auto_tap(runner.state(), P0, spell, &cost),
        "one Forest plus the single-type delayed echo must fund {{1}}{{G}}"
    );
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the single-type delayed echo must fund the auto-payment");
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            engine::types::game_state::StackEntryKind::Spell { .. }
        )),
        "the paid spell must reach the stack"
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a single-type echo owes no choice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "base {{G}} + one delayed {{G}} exactly pays {{1}}{{G}}"
    );
}
