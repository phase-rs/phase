//! CR 601.2c runtime fixture for the mana RECIPIENT + COUNT SOURCE role pair.
//!
//! "The player announces their choice of an appropriate object or player for
//! each target the spell requires." A mana sentence that names a recipient
//! ("target player adds …", CR 106.4) AND a target-derived count ("… for each
//! card in target opponent's hand") declares TWO independent instances of the
//! word "target". Before roles were modeled, both collapsed onto
//! `ability.targets[0]` and the resolver guessed which was which from the
//! production's quantity shape.
//!
//! This drives the REAL cast pipeline — announcement, slot building, positional
//! target selection, resolution — and asserts the two slots stay independent:
//! the mana lands in the RECIPIENT's pool while the COUNT reads the
//! COUNT-SOURCE player's hand. The two players are different AND their hand
//! sizes are different, so any slot mix-up changes both the destination pool and
//! the amount; the test cannot pass vacuously.
//!
//! NOTE ON /card-test's verbatim-Oracle-text rule: this test uses a SYNTHETIC
//! card. No printed card declares both a mana recipient and a target-derived
//! count in one sentence (0 cards in the class today), so verbatim Oracle text
//! is unavailable by construction. CR 601.2c makes the shape legal Magic and it
//! is the class the role model exists to express. The single-role HALVES of the
//! class ARE validated against real verbatim Oracle text by the parser tests for
//! Jetfire ("Target player adds that much {C}" — recipient), Jeska's Will
//! ("Add {R} for each card in target opponent's hand" — count source), and
//! Carpet of Flowers, and at runtime by the in-crate `effects::mana` tests.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::ability::{
    Effect, ManaProduction, ManaTargetRole, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
    ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// The third player in the 3-player fixture; `scenario` exports only P0/P1.
const P2: PlayerId = PlayerId(2);

/// P1 (recipient) holds 2 cards; P2 (count source) holds 5. A slot swap would
/// deposit 2 mana into P2's pool instead of 5 into P1's — both numbers change.
const RECIPIENT_HAND: usize = 2;
const COUNT_SOURCE_HAND: usize = 5;

fn hand_names(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{prefix} {i}")).collect()
}

fn colorless_pool(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool
        .count_color(ManaType::Colorless) as i32
}

fn total_pool(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .mana_pool
        .total() as i32
}

#[test]
fn mana_recipient_and_count_source_resolve_from_their_own_slots() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let recipient_hand = hand_names("Recipient Card", RECIPIENT_HAND);
    let count_hand = hand_names("Count Card", COUNT_SOURCE_HAND);
    scenario.with_cards_in_hand(
        P1,
        &recipient_hand
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    scenario.with_cards_in_hand(
        P2,
        &count_hand.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    // CR 601.2c: two independent player targets — the RECIPIENT whose pool
    // receives the mana, and the COUNT SOURCE whose hand the count reads.
    let spell = scenario
        .add_spell_to_hand(P0, "Role Split Ritual", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Hand,
                    },
                },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(ManaTargetRole::Both {
                recipient: TargetFilter::Player,
                count_source: TargetFilter::Player,
            }),
        })
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the role-split ritual must succeed");

    // Reach guard: the cast must actually surface TWO independent slots. If the
    // role collapsed to one slot, everything below would be vacuous.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                2,
                "CR 601.2c: a recipient AND a count source are two instances of \
                 'target' and must surface two independently announced slots, got {}",
                target_slots.len()
            );
            for (i, slot) in target_slots.iter().enumerate() {
                assert!(
                    slot.legal_targets.contains(&TargetRef::Player(P1))
                        && slot.legal_targets.contains(&TargetRef::Player(P2)),
                    "slot {i} must offer both candidate players so the assignment \
                     below is a real positional choice"
                );
            }
        }
        other => panic!("expected a two-slot TargetSelection prompt, got {other:?}"),
    }

    // Slot 0 = recipient (P1), slot 1 = count source (P2), in declaration order.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1), TargetRef::Player(P2)],
        })
        .expect("positional role targets must be accepted");

    // Resolve the spell off the stack.
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 16,
            "too many prompts; stuck at {:?}",
            runner.state().waiting_for
        );
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // CR 106.4: the mana goes into the RECIPIENT's pool.
    // CR 115.1: the AMOUNT comes from the COUNT SOURCE's hand.
    assert_eq!(
        colorless_pool(&runner, P1),
        COUNT_SOURCE_HAND as i32,
        "the RECIPIENT (P1) must receive COUNT_SOURCE_HAND ({COUNT_SOURCE_HAND}) mana \
         — its own hand size ({RECIPIENT_HAND}) would mean the count read the wrong slot"
    );
    assert_ne!(
        colorless_pool(&runner, P1),
        RECIPIENT_HAND as i32,
        "the count must NOT read the recipient's own hand"
    );
    assert_eq!(
        total_pool(&runner, P2),
        0,
        "the COUNT SOURCE (P2) supplies the amount only — it must receive no mana"
    );
    assert_eq!(
        total_pool(&runner, P0),
        0,
        "the controller (P0) must NOT receive a targeted recipient's mana"
    );
}

/// Paired negative / over-application guard: the SAME production, but with the
/// recipient role dropped (Jeska's Will shape — count source only). The mana
/// must stay with the CONTROLLER, and only ONE slot may be surfaced. A
/// `mana_multi_role` gate that fired on `Effect::Mana { .. }` rather than on
/// "surfaces more than one slot" would fail this.
#[test]
fn count_source_only_deposits_into_the_controller_and_surfaces_one_slot() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let count_hand = hand_names("Count Card", COUNT_SOURCE_HAND);
    scenario.with_cards_in_hand(
        P2,
        &count_hand.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    let spell = scenario
        .add_spell_to_hand(P0, "Count Source Ritual", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Hand,
                    },
                },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(ManaTargetRole::CountSource {
                count_source: TargetFilter::Player,
            }),
        })
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the count-source ritual must succeed");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots.len(),
                1,
                "a single-role mana declares exactly ONE instance of 'target'"
            );
        }
        other => panic!("expected a one-slot TargetSelection prompt, got {other:?}"),
    }

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P2)],
        })
        .expect("the count source must be selectable");

    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 16, "too many prompts");
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // Reach guard: a non-zero controller pool proves the effect actually
    // resolved, so the two zero assertions below cannot pass vacuously.
    assert_eq!(
        colorless_pool(&runner, P0),
        COUNT_SOURCE_HAND as i32,
        "CR 106.4: with no recipient role declared, the CONTROLLER adds the mana, \
         in the amount read from the count source's hand"
    );
    assert_eq!(
        total_pool(&runner, P2),
        0,
        "the count source supplies the amount only — it receives no mana"
    );
    assert_eq!(total_pool(&runner, P1), 0, "the bystander receives no mana");
}

/// CR 608.2b regression: a LEGAL recipient with an ILLEGAL count source must
/// fail to determine the count — it must NOT silently fall back to reading the
/// recipient's hand.
///
/// "The spell or ability … won't do anything to that target … it will fail to
/// determine any such information about an illegal target." The recipient here
/// stays legal, so the effect still resolves and still deposits into the
/// recipient's pool (CR 608.2b affects only the parts naming the illegal
/// target) — but the AMOUNT must come out 0, not the recipient's own hand size.
///
/// Discrimination: the two hand sizes differ (recipient 2, count source 5).
/// Before the fix, an unresolvable count-source slot fell back to the UNSCOPED
/// ability, which still holds the legal recipient at index 0; the shared
/// "first player target" quantity resolution then read the RECIPIENT's hand and
/// produced exactly RECIPIENT_HAND (2) mana. So the assert_ne! below is the
/// revert-failing assertion, and the assert_eq! to 0 pins the CR-correct answer.
#[test]
fn illegal_count_source_fails_to_determine_instead_of_counting_the_recipient() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let recipient_hand = hand_names("Recipient Card", RECIPIENT_HAND);
    let count_hand = hand_names("Count Card", COUNT_SOURCE_HAND);
    scenario.with_cards_in_hand(
        P1,
        &recipient_hand
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    scenario.with_cards_in_hand(
        P2,
        &count_hand.iter().map(String::as_str).collect::<Vec<_>>(),
    );

    let spell = scenario
        .add_spell_to_hand(P0, "Role Split Ritual", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Hand,
                    },
                },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(ManaTargetRole::Both {
                recipient: TargetFilter::Player,
                count_source: TargetFilter::Player,
            }),
        })
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the role-split ritual must succeed");

    // Slot 0 = recipient (P1), slot 1 = count source (P2).
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1), TargetRef::Player(P2)],
        })
        .expect("positional role targets must be accepted");

    // CR 800.4a: the COUNT SOURCE leaves the game after announcement but before
    // resolution, making that target — and only that target — illegal.
    let p2_index = runner
        .state()
        .players
        .iter()
        .position(|p| p.id == P2)
        .expect("P2 is in the game");
    runner.state_mut().players[p2_index].is_eliminated = true;

    // Reach guard: the RECIPIENT must still be legal, or this test would be
    // exercising a whole-spell fizzle (CR 608.2b clause 2) rather than the
    // per-target "fails to determine" clause it is written for.
    let p1_index = runner
        .state()
        .players
        .iter()
        .position(|p| p.id == P1)
        .expect("P1 is in the game");
    assert!(
        !runner.state().players[p1_index].is_eliminated,
        "reach guard: the recipient must remain LEGAL so the effect still resolves"
    );

    let mut resolution_events: Vec<GameEvent> = Vec::new();
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 16,
            "too many prompts; stuck at {:?}",
            runner.state().waiting_for
        );
        match runner.act(GameAction::PassPriority) {
            Ok(result) => resolution_events.extend(result.events),
            Err(_) => break,
        }
    }

    // Reach guard: the spell actually left the stack.
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the spell must have resolved — a spell still on the stack \
         would make the pool assertions vacuous"
    );
    // Reach guard, the load-bearing one: the mana effect RESOLVED. An empty
    // stack alone does not prove that — CR 608.2b's other clause (ALL targets
    // illegal) removes the spell too, and a whole-spell fizzle would satisfy
    // every pool assertion below while testing nothing. Asserting the effect
    // resolved pins that this is the per-target "fails to determine" path.
    assert!(
        resolution_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Mana,
                ..
            }
        )),
        "reach guard: the mana effect must have RESOLVED (recipient still legal), \
         not fizzled — otherwise the zero-mana assertions below are vacuous. \
         events: {resolution_events:?}"
    );

    // THE REGRESSION: the count must fail to determine, not read the recipient.
    assert_ne!(
        colorless_pool(&runner, P1),
        RECIPIENT_HAND as i32,
        "CR 608.2b: an illegal COUNT SOURCE must not fall back to counting the \
         RECIPIENT's hand ({RECIPIENT_HAND}) — that is the unscoped-fallback bug"
    );
    assert_eq!(
        colorless_pool(&runner, P1),
        0,
        "CR 608.2b: the effect fails to determine the illegal count source's hand \
         size, so it produces 0 mana"
    );
    assert_eq!(
        total_pool(&runner, P0),
        0,
        "the controller receives nothing — the recipient role is still declared"
    );
}
