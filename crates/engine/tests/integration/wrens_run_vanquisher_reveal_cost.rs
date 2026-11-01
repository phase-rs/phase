//! CR 601.2b + CR 701.20a: Wren's Run Vanquisher additional cost choice:
//! "As an additional cost to cast this spell, reveal an Elf card from your hand or pay {3}."
//!
//! Verifies:
//! 1. Choosing to reveal an Elf card prompts `WaitingFor::PayCost { kind: PayCostKind::Reveal { .. } }`,
//!    and selecting an Elf card from hand successfully completes the cast for {1}{G}.
//! 2. Choosing to pay {3} instead charges {4}{G} total ({1}{G} base + {3} additional).
//! 3. Casting with no Elf card in hand falls through directly to the {3} additional cost.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WRENS_RUN_VANQUISHER_ORACLE: &str = "As an additional cost to cast this spell, reveal an Elf card from your hand or pay {3}.\nDeathtouch";
const FOREST_ORACLE: &str = "({T}: Add {G}.)";
const WASTES_ORACLE: &str = "{T}: Add {C}.";

fn add_forest(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Forest", FOREST_ORACLE)
        .with_subtypes(vec!["Forest"])
        .id()
}

fn add_wastes(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Wastes", WASTES_ORACLE)
        .id()
}

#[test]
fn wrens_run_vanquisher_cast_by_revealing_elf() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 601.2h: five lands, so BOTH listed options are payable and the choice
    // is really a choice. With only the base {1}{G} available the pay-{3} option
    // is unpayable, so the announcement is narrowed to the reveal and paid
    // without a prompt — covered by
    // `wrens_run_vanquisher_auto_reveals_when_the_mana_leg_is_unpayable`.
    let forest = add_forest(&mut scenario);
    let wastes = add_wastes(&mut scenario);
    let spare_lands = [
        add_wastes(&mut scenario),
        add_wastes(&mut scenario),
        add_wastes(&mut scenario),
    ];

    // An Elf card in hand to reveal.
    let elf = scenario
        .add_creature_to_hand(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();

    // A non-Elf card in hand — must NOT be an eligible reveal choice.
    scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut builder = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Wren's Run Vanquisher",
        3,
        3,
        WRENS_RUN_VANQUISHER_ORACLE,
    );
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Elf", "Warrior"]);
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Wren's Run Vanquisher must be accepted");

    // 1. Must prompt for the additional cost choice (Reveal vs Pay {3}).
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalCostChoice { cost, .. } => {
            assert!(
                matches!(cost, engine::types::ability::AdditionalCost::Choice(_)),
                "must offer AdditionalCost::Choice"
            );
        }
        other => panic!("expected OptionalCostChoice, got {other:?}"),
    }

    // 2. Choose to pay the preferred cost (reveal an Elf).
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("deciding to reveal must be accepted");

    // 3. Must prompt to select the Elf card from hand.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::Reveal { .. },
            choices,
            count,
            ..
        } => {
            assert_eq!(count, 1, "must reveal 1 card");
            assert_eq!(
                choices,
                vec![elf],
                "only the Elf card in hand must be an eligible choice"
            );
        }
        other => panic!("expected PayCost(Reveal), got {other:?}"),
    }

    // 4. Select the Elf card to reveal.
    let reveal_outcome = runner
        .act(GameAction::SelectCards { cards: vec![elf] })
        .expect("selecting the Elf card to reveal must succeed");

    // 5. Cast completes and moves to stack; priority is open.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "cast must finish after reveal payment"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "spell must be on the stack"
    );
    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Hand,
        "revealed Elf must remain in hand (CR 701.20b)"
    );

    // CR 118.3 & CR 601.2h: only the base {1}{G} was paid — revealing costs no
    // mana, so exactly two of the five lands are tapped and the three spares
    // stay up.
    assert!(
        runner.state().objects[&forest].tapped,
        "Forest must be tapped to pay {{1}}{{G}}"
    );
    assert!(
        runner.state().objects[&wastes].tapped,
        "Wastes must be tapped to pay {{1}}{{G}}"
    );
    for land in spare_lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "revealing an Elf pays no mana, so {land:?} must stay untapped"
        );
    }

    // CR 701.20a: A CardsRevealed event must be emitted for the revealed Elf.
    assert!(
        reveal_outcome.events.iter().any(|e| matches!(
            e,
            engine::types::events::GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&elf)
        )),
        "a CardsRevealed event containing the revealed Elf card must be emitted"
    );

    // 6. Resolve the spell onto the battlefield.
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "Wren's Run Vanquisher must resolve onto the battlefield"
    );
}

#[test]
fn wrens_run_vanquisher_cast_by_paying_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Provide 5 lands for {4}{G} total ({1}{G} base + {3} additional cost).
    let f = add_forest(&mut scenario);
    let w1 = add_wastes(&mut scenario);
    let w2 = add_wastes(&mut scenario);
    let w3 = add_wastes(&mut scenario);
    let w4 = add_wastes(&mut scenario);
    let lands = [f, w1, w2, w3, w4];

    let elf = scenario
        .add_creature_to_hand(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();

    let mut builder = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Wren's Run Vanquisher",
        3,
        3,
        WRENS_RUN_VANQUISHER_ORACLE,
    );
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Elf", "Warrior"]);
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Wren's Run Vanquisher must be accepted");

    // Choose to pay {3} instead of revealing an Elf.
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("deciding to pay {3} must be accepted");

    // Cast completes immediately by paying the full {4}{G}.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "cast must finish after paying mana cost"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "spell must be on the stack"
    );
    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Hand,
        "Elf remains in hand"
    );

    // CR 118.3 & CR 601.2h: Verify all 5 lands were tapped to pay {4}{G} ({1}{G} base + {3} additional).
    for land in lands {
        assert!(
            runner.state().objects[&land].tapped,
            "all 5 lands must be tapped to pay {{4}}{{G}} (land {land:?})"
        );
    }

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Battlefield,
        "Wren's Run Vanquisher must resolve onto the battlefield"
    );
}

/// CR 601.2h: the mana leg can be the unpayable one. With only the base {1}{G}
/// available the pay-{3} option is not announceable, so the reveal — the sole
/// payable option — is paid with no prompt. The mirror of the no-Elf test below,
/// and the only card-visible consequence of narrowing LATER options rather than
/// just the first.
#[test]
fn wrens_run_vanquisher_auto_reveals_when_the_mana_leg_is_unpayable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Exactly the printed {1}{G} — nothing left for the {3}.
    let forest = add_forest(&mut scenario);
    let wastes = add_wastes(&mut scenario);

    let elf = scenario
        .add_creature_to_hand(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();

    let mut builder = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Wren's Run Vanquisher",
        3,
        3,
        WRENS_RUN_VANQUISHER_ORACLE,
    );
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Elf", "Warrior"]);
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Wren's Run Vanquisher must be accepted");

    // No announcement: one payable option leaves nothing to choose.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::Reveal { .. },
            choices,
            ..
        } => assert_eq!(
            choices,
            vec![elf],
            "the reveal is the only payable option, so it is paid directly"
        ),
        other => panic!("expected the reveal payment with no announcement, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![elf] })
        .expect("revealing the Elf must complete the cost");
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "the cast must complete"
    );
    for land in [forest, wastes] {
        assert!(
            runner.state().objects[&land].tapped,
            "only the printed {{1}}{{G}} is paid ({land:?})"
        );
    }
}

#[test]
fn wrens_run_vanquisher_auto_falls_through_when_no_elf_in_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Provide 5 lands for {4}{G}.
    let f = add_forest(&mut scenario);
    let w1 = add_wastes(&mut scenario);
    let w2 = add_wastes(&mut scenario);
    let w3 = add_wastes(&mut scenario);
    let w4 = add_wastes(&mut scenario);
    let lands = [f, w1, w2, w3, w4];

    // Hand has no Elf cards.
    scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut builder = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Wren's Run Vanquisher",
        3,
        3,
        WRENS_RUN_VANQUISHER_ORACLE,
    );
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Elf", "Warrior"]);
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Wren's Run Vanquisher must be accepted");

    // With no Elf in hand, Reveal is not offerable, so it automatically falls through to {3}.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "cast must auto-complete by paying fallback 3 mana cost"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "spell must be on the stack"
    );

    // CR 118.3 & CR 601.2h: Verify all 5 lands were tapped to pay fallback {4}{G}.
    for land in lands {
        assert!(
            runner.state().objects[&land].tapped,
            "all 5 lands must be tapped to pay fallback {{4}}{{G}} (land {land:?})"
        );
    }
}
