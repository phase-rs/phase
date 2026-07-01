//! Runtime proof that Escape works on INSTANTS, not just permanents.
//!
//! Cluster 62 (WHO misparse) registered the `escape—` em-dash branch in
//! `parse_keyword_from_oracle` so the generic keyword-cost guards extract Escape
//! uniformly for every card type. Before that fix, the escape line on an
//! instant/sorcery fell through to `Effect::Unimplemented` and the card was
//! never offered as an escape cast from the graveyard.
//!
//! These tests build Run for Your Life (an instant) from its REAL Oracle text via
//! `from_oracle_text`, so the parsed `Keyword::Escape` keyword drives the runtime
//! end-to-end. The permanent path is covered by `issue_3281_uro_escape`; this is
//! the previously-unreachable instant path.
//!
//! CR 702.138a: "Escape [cost]" means "You may cast this card from your graveyard
//! by paying [cost] rather than paying its mana cost."

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::keywords::has_haste;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, PayCostKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::{ExileCostSourceZone, Zone};

const RUN_FOR_YOUR_LIFE: &str = "One or two target creatures each gain haste until end of turn. \
They can't be blocked this turn except by creatures with haste.\n\
Escape\u{2014}{2}{U}{R}, Exile four other cards from your graveyard. \
(You may cast this card from your graveyard for its escape cost.)";

fn mana(color: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// Build Run for Your Life in P0's graveyard from its real Oracle text and return
/// its object id. The parser must extract `Keyword::Escape` onto the object —
/// that is the whole point of the cluster-62 fix.
fn add_run_for_your_life_to_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_graveyard(P0, "Run for Your Life", true)
        .from_oracle_text(RUN_FOR_YOUR_LIFE)
        .id()
}

#[test]
fn run_for_your_life_escape_castable_from_graveyard_with_four_other_cards() {
    // CR 702.138a: escape needs four OTHER cards in the graveyard to exile.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for idx in 0..4 {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1)
            .id();
    }
    let spell = add_run_for_your_life_to_graveyard(&mut scenario);

    let runner = scenario.build();
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&spell),
        "Run for Your Life (an INSTANT) must be escape-castable from the graveyard \
         with four other cards; castable={castable:?}"
    );
}

#[test]
fn run_for_your_life_escape_not_castable_with_only_three_other_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for idx in 0..3 {
        scenario
            .add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1)
            .id();
    }
    let spell = add_run_for_your_life_to_graveyard(&mut scenario);

    let runner = scenario.build();
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !castable.contains(&spell),
        "Run for Your Life must not be escape-castable with only three other graveyard cards"
    );
}

#[test]
fn run_for_your_life_escape_cast_pays_exile_targets_and_resolves() {
    // End-to-end runtime proof of the instant escape cast: declare a target,
    // pay the four-card exile additional cost, resolve, and observe the haste
    // grant + the exile delta.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(ManaType::Colorless, 2)
            .into_iter()
            .chain(mana(ManaType::Blue, 1))
            .chain(mana(ManaType::Red, 1))
            .collect(),
    );

    let filler: Vec<ObjectId> = (0..4)
        .map(|idx| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1)
                .id()
        })
        .collect();
    // A creature for the spell to target (CR 601.2c targets precede cost payment).
    let target_creature = scenario.add_creature(P0, "Runner", 2, 2).id();
    let spell = add_run_for_your_life_to_graveyard(&mut scenario);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Run for Your Life escape cast must enter the pipeline");

    let mut paid_exile = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let already: Vec<TargetRef> =
                    selection.selected_slots.iter().flatten().cloned().collect();
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| !already.contains(t))
                    .cloned();
                let choice = match choice {
                    Some(c) => Some(c),
                    None if slot.optional => None,
                    None => panic!("required target slot has no legal target: {slot:?}"),
                };
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget must be accepted");
            }
            WaitingFor::PayCost {
                kind:
                    PayCostKind::ExileFromZone {
                        zone: ExileCostSourceZone::Graveyard,
                    },
                count,
                choices,
                ..
            } => {
                assert_eq!(count, 4, "escape must exile four other graveyard cards");
                assert!(
                    !choices.contains(&spell),
                    "the escaping card itself must not be exilable to pay its own cost"
                );
                let stack_variant =
                    runner
                        .state()
                        .stack
                        .get(0)
                        .and_then(|entry| match &entry.kind {
                            StackEntryKind::Spell {
                                casting_variant, ..
                            } => Some(*casting_variant),
                            _ => None,
                        });
                assert_eq!(
                    stack_variant,
                    Some(CastingVariant::Escape),
                    "the spell must be on the stack as an Escape cast while paying the cost"
                );
                let pay: Vec<ObjectId> = choices
                    .into_iter()
                    .filter(|c| filler.contains(c))
                    .take(4)
                    .collect();
                assert_eq!(pay.len(), 4, "all four filler cards must be exile-eligible");
                runner
                    .act(GameAction::SelectCards { cards: pay })
                    .expect("paying the four-card exile cost must be accepted");
                paid_exile = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the escape mana payment from pool must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner.advance_until_stack_empty();
                break;
            }
            other => panic!("unexpected pipeline state during escape cast: {other:?}"),
        }
    }

    assert!(paid_exile, "the escape exile cost must have been paid");

    // CR 702.10a: the chosen creature gained haste from the resolved spell.
    let runner_obj = &runner.state().objects[&target_creature];
    assert!(
        has_haste(runner_obj),
        "the targeted creature must have haste after Run for Your Life resolves; keywords={:?}",
        runner_obj.keywords
    );

    // Exactly the four filler cards moved graveyard -> exile.
    for id in &filler {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "each paid filler card must be exiled"
        );
    }

    // CR 608.2n: as the final part of an instant's resolution it goes to its
    // owner's graveyard.
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "Run for Your Life (instant) must return to the graveyard after resolving"
    );
}
