//! CR 702.78 — Conspire granted by a static ability (Wort, the Raidmother /
//! Rassilon, the War President) must offer its optional additional cost and copy
//! the spell when that cost is paid, exactly like printed Conspire.
//!
//! Conspire was previously synthesized only from a card's *printed* Conspire
//! keyword (`synthesize_conspire`). A spell that gains Conspire from a
//! `StaticMode::CastWithKeyword` static never had the cost offered or the copy
//! produced, because the synthesis path keys off printed keywords and the
//! casting ladder / `process_triggers` had no granted-Conspire seam. This
//! mirrors the working granted-Casualty path (CR 702.153).
//!
//! CR 702.78a (docs/MagicCompRules.txt:4564): "Conspire" = an additional cost
//! ("tap two untapped creatures you control that each share a color with it")
//! plus a reflexive "when you cast this spell, if its conspire cost was paid,
//! copy it" trigger.
//!
//! CARD TEXT: Wort's oracle text below is this engine's authoritative card data
//! (`client/public/card-data.json`): "Each red or green instant or sorcery spell
//! you cast has conspire." The spell under test is a *red* instant, so Wort's
//! static grants it Conspire regardless of the pre-existing parser quirk in the
//! red Or-branch (which drops the instant/sorcery type constraint — out of
//! scope; a red instant matches either way).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Wort, the Raidmother's authoritative oracle text (verified in
/// `client/public/card-data.json`). The conspire-granting clause is the second
/// sentence; the ETB token clause is harmless here (no DB token lookup runs in
/// this scenario, and the cast spell drives the assertions).
const WORT_ORACLE: &str = "When Wort enters, create two 1/1 red and green Goblin \
Warrior creature tokens.\nEach red or green instant or sorcery spell you cast has \
conspire. (As you cast the spell, you may tap two untapped creatures you control \
that share a color with it. When you do, copy it and you may choose new targets \
for the copy.)";

/// Add `count` units of `ty` mana to P0's pool (deterministic payment without
/// modelling lands; mirrors the `add_mana` helper in `chord_of_calling.rs`).
fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Force `id`'s color to red so it (a) matches Wort's "red ... spell you cast"
/// static and (b) shares a color with the red creatures for the conspire tap
/// cost (CR 702.78a). The scenario builder leaves `colors` empty and does not
/// run the load-time color derivation, so set it explicitly.
fn make_red(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().color = vec![ManaColor::Red];
}

/// Count `SpellCopied` events emitted while resolving the stack to empty (each
/// `Effect::CopySpell` iteration emits exactly one, CR 707.10).
fn drain_counting_spell_copies(runner: &mut GameRunner) -> usize {
    let mut copies = 0usize;
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => {
                copies += result
                    .events
                    .iter()
                    .filter(|e| matches!(e, GameEvent::SpellCopied { .. }))
                    .count();
            }
            Err(_) => break,
        }
    }
    copies
}

/// End-to-end: Wort grants Conspire to a red instant; paying the optional
/// tap-two-red-creatures cost copies the spell once (CR 702.78a). Both
/// assertions FAIL on origin/main — no conspire cost arm (CHANGE 2) and no
/// granted-Conspire copy trigger (CHANGE 3).
#[test]
fn wort_grants_conspire_offers_cost_and_copies_when_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Wort, the Raidmother on P0's battlefield — its CastWithKeyword{Conspire}
    // static registers from the parsed oracle text.
    scenario.add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT_ORACLE);

    // Two red creatures P0 controls — the conspire tap targets. They must share
    // a color with the red spell (CR 702.78a).
    let red_a = scenario.add_creature(P0, "Red Bear A", 2, 2).id();
    let red_b = scenario.add_creature(P0, "Red Bear B", 2, 2).id();

    // A red instant that just draws — targetless, so conspire's copy resolves
    // straight through and the copy count is observable via SpellCopied alone.
    let mut builder = scenario.add_spell_to_hand(P0, "Red Draw", true);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 1,
    });
    builder.with_ability(Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    });
    let spell_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;
    make_red(&mut runner, red_a);
    make_red(&mut runner, red_b);
    make_red(&mut runner, spell_id);
    add_mana(&mut runner, ManaType::Colorless, 1); // {1} base cost

    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the red instant under Wort must be accepted");

    // (1) CR 702.78a: the granted conspire cost is offered as an OptionalCostChoice
    //     of TapCreatures { count: 2 }. FAILS on origin/main (no conspire arm).
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalCostChoice { cost, .. } => assert!(
            matches!(
                cost,
                engine::types::ability::AdditionalCost::Optional {
                    cost: engine::types::ability::AbilityCost::TapCreatures { count: 2, .. },
                    repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
                }
            ),
            "granted Conspire must surface an optional TapCreatures{{2}} cost: {cost:?}"
        ),
        other => panic!("expected granted-Conspire OptionalCostChoice, got {other:?}"),
    }

    // (2) Pay the conspire cost by tapping the two red creatures.
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("electing to pay conspire must be accepted");

    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::TapCreatures,
            choices,
            count,
            ..
        } => {
            assert_eq!(count, 2, "conspire taps exactly two creatures");
            assert!(
                choices.contains(&red_a) && choices.contains(&red_b),
                "both red creatures must be eligible conspire tap targets: {choices:?}"
            );
        }
        other => panic!("expected PayCost TapCreatures after paying conspire, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![red_a, red_b],
        })
        .expect("tapping two red creatures for conspire must succeed");

    // (3) Both creatures tapped, the original spell is on the stack.
    assert!(
        runner.state().objects[&red_a].tapped && runner.state().objects[&red_b].tapped,
        "both conspire tap targets must be tapped"
    );
    assert!(
        runner.state().stack.iter().any(|e| e.id == spell_id),
        "the original spell must be on the stack after conspire is paid"
    );

    // (4) CR 702.78a: resolving the cast trigger copies the spell exactly once.
    //     FAILS on origin/main (no granted-Conspire copy trigger).
    let copies = drain_counting_spell_copies(&mut runner);
    assert_eq!(
        copies, 1,
        "granted Conspire paid once must create exactly one copy"
    );
}
