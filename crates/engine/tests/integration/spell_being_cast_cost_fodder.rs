//! CR 601.2a: a spell being cast has moved to the stack, so it can't pay a
//! cost from the zone it was cast from: not a mana ability's exile or discard
//! cost activated while paying for it, and not its own collect-evidence cost.
//! Its object keeps its origin zone (and its place in that zone's card list)
//! until the cast finalizes, so every hand/graveyard cost-fodder enumeration
//! consults `casting::is_spell_being_cast` (phase-rs/phase#9812 follow-on).
//!
//! Oracle text verbatim from Scryfall `cards/named` where a card is named.

use engine::ai_support::{candidate_actions, legal_actions};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp,
    ManaContribution, ManaProduction, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{FlashbackCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn auto_cast(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}
/// "Exile a card from your hand: Add {B}." (a choice-bearing, non-tap mana
/// ability; Auto can't activate it).
fn exile_from_hand_mana() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Black],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Hand),
        filter: Some(TargetFilter::Typed(
            TypedFilter::card()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
        )),
    })
}

struct HandExileBoard {
    runner: GameRunner,
    spell: ObjectId,
    source: ObjectId,
    victim: ObjectId,
}

/// P0: a creature with "Exile a card from your hand: Add {B}", no lands, and
/// one {B} spell in hand ("Destroy target creature." or "Draw a card."). With
/// `fodder`, one more card in hand the source can exile.
fn hand_exile_board(targeted: bool, fodder: bool) -> HandExileBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let source = sc
        .add_creature(P0, "Hand-Exile Mana Source", 0, 3)
        .with_ability_definition(exile_from_hand_mana())
        .id();
    let victim = sc.add_creature(P1, "Victim", 2, 2).id();
    sc.with_library_top(P0, &["Card A", "Card B"]);
    let (name, text) = if targeted {
        ("Targeted Black Spell", "Destroy target creature.")
    } else {
        ("Untargeted Black Spell", "Draw a card.")
    };
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
        .id();
    if fodder {
        sc.add_spell_to_hand_from_oracle(P0, "Hand Fodder", true, "Draw a card.")
            .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 5));
    }
    HandExileBoard {
        runner: sc.build(),
        spell,
        source,
        victim,
    }
}

/// CR 601.2h: the reducer refuses the payment — at announcement for an
/// untargeted cast, once the target is chosen for a targeted one (after the
/// reach guard that it stopped at target selection, the post-target seam).
fn assert_reducer_refuses_payment(state: &GameState, action: GameAction, victim: Option<ObjectId>) {
    let mut runner = GameRunner::from_state(state.clone());
    let refused = match victim {
        None => runner.act(action),
        Some(victim) => {
            runner.act(action).expect("the targeted cast is announced");
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::TargetSelection { .. }
                ),
                "reach: the targeted cast stops at target selection"
            );
            runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(victim)],
            })
        }
    };
    assert!(
        refused.is_err(),
        "the spell being cast can't pay for itself: {:?}",
        runner.state().waiting_for
    );
}

fn announce_and_target(runner: &mut GameRunner, action: GameAction, victim: Option<ObjectId>) {
    runner.act(action).expect("the cast is announced");
    if let Some(victim) = victim {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "reach: the targeted cast stops at target selection, the post-target offer seam"
        );
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(victim)],
            })
            .expect("the target is accepted");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at the mana-payment step: {:?}",
        runner.state().waiting_for
    );
}

/// Hostile (CR 601.2a + CR 601.2g): before the cast is announced the spell in
/// hand is fodder for "Exile a card from your hand: Add {B}", so the broad
/// castability gate generates the cast. Once announced, the spell is on the
/// stack: the source has nothing to exile, the cast can't be paid, and it is
/// not offered, targeted or not. (Untargeted, this was offered before the fix,
/// and the source could exile the spell being cast.)
#[test]
fn spell_that_is_its_own_only_exile_fodder_is_not_offered() {
    for targeted in [false, true] {
        let b = hand_exile_board(targeted, false);
        let action = auto_cast(&b.runner, b.spell);
        assert!(
            candidate_actions(b.runner.state())
                .iter()
                .any(|candidate| candidate.action == action),
            "reach (targeted={targeted}): before announcement the spell is its own fodder"
        );
        assert!(
            !legal_actions(b.runner.state()).contains(&action),
            "targeted={targeted}: nothing is left to exile once the spell is on the stack"
        );
        assert_reducer_refuses_payment(b.runner.state(), action, targeted.then_some(b.victim));
    }
}

/// Positive control for the self-fodder board: one other card in hand makes the
/// untargeted cast payable through the exile source, so it is offered, and the
/// offered Auto action completes with that card (the only choice).
#[test]
fn spell_with_other_exile_fodder_is_offered_and_completes() {
    let mut b = hand_exile_board(false, true);
    let action = auto_cast(&b.runner, b.spell);
    assert!(legal_actions(b.runner.state()).contains(&action));
    announce_and_target(&mut b.runner, action, None);
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the exile source activates during payment");
    let WaitingFor::PayCost { choices, .. } = b.runner.state().waiting_for.clone() else {
        panic!(
            "the exile cost asks for a card: {:?}",
            b.runner.state().waiting_for
        )
    };
    assert!(
        !choices.contains(&b.spell),
        "CR 601.2a: the spell being cast is not in hand"
    );
    assert_eq!(choices.len(), 1, "only the other card remains in hand");
    b.runner
        .act(GameAction::SelectCards { cards: choices })
        .expect("the other card is exiled");
    b.runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the payment");
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().objects[&b.spell].zone,
        Zone::Graveyard,
        "the spell resolved"
    );
}

const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";

/// P0: Skirge Familiar (Oracle text verbatim from Scryfall), no lands, and a
/// {B} spell in hand, plus one other card when `other_card`.
fn skirge_board(targeted: bool, other_card: bool) -> HandExileBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let source = sc
        .add_creature(P0, "Skirge Familiar", 3, 2)
        .from_oracle_text_with_keywords(&["Flying"], SKIRGE_FAMILIAR)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 4))
        .id();
    let victim = sc.add_creature(P1, "Victim", 2, 2).id();
    sc.with_library_top(P0, &["Card A", "Card B"]);
    let (name, text) = if targeted {
        ("Targeted Black Spell", "Destroy target creature.")
    } else {
        ("Untargeted Black Spell", "Draw a card.")
    };
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
        .id();
    if other_card {
        sc.add_spell_to_hand_from_oracle(P0, "Other Card", true, "Draw a card.")
            .with_mana_cost(cost(vec![ManaCostShard::Red, ManaCostShard::Red], 5));
    }
    HandExileBoard {
        runner: sc.build(),
        spell,
        source,
        victim,
    }
}

/// CR 601.2a + CR 601.2g: Skirge Familiar can't discard the spell being cast
/// to pay for it. With that spell as the only card in hand, the cast isn't
/// offered, targeted or untargeted. (Before the fix the untargeted cast was
/// offered, and discarding the spell being cast paid for it.)
#[test]
fn skirge_familiar_cannot_discard_the_spell_being_cast() {
    for targeted in [false, true] {
        let b = skirge_board(targeted, false);
        let action = auto_cast(&b.runner, b.spell);
        assert!(
            candidate_actions(b.runner.state())
                .iter()
                .any(|candidate| candidate.action == action),
            "reach (targeted={targeted}): before announcement the spell is the Skirge's fodder"
        );
        assert!(
            !legal_actions(b.runner.state()).contains(&action),
            "targeted={targeted}: no card is left to discard once the spell is on the stack"
        );
        assert_reducer_refuses_payment(b.runner.state(), action, targeted.then_some(b.victim));
    }
}

/// Positive control: with one other card in hand, Skirge Familiar pays for the
/// untargeted spell by discarding that card, the only choice.
#[test]
fn skirge_familiar_pays_by_discarding_another_card() {
    let mut b = skirge_board(false, true);
    let action = auto_cast(&b.runner, b.spell);
    assert!(legal_actions(b.runner.state()).contains(&action));
    announce_and_target(&mut b.runner, action, None);
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Skirge Familiar activates during payment");
    let WaitingFor::PayCost { choices, .. } = b.runner.state().waiting_for.clone() else {
        panic!(
            "the discard cost asks for a card: {:?}",
            b.runner.state().waiting_for
        )
    };
    assert_eq!(choices.len(), 1, "only the other card can be discarded");
    assert!(!choices.contains(&b.spell));
    b.runner
        .act(GameAction::SelectCards { cards: choices })
        .expect("the other card is discarded");
    b.runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the payment");
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().objects[&b.spell].zone,
        Zone::Graveyard,
        "the spell resolved"
    );
}

/// "Exile a card from your graveyard: Add {B}."
fn exile_from_graveyard_mana() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Black],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Graveyard),
        filter: Some(TargetFilter::Typed(
            TypedFilter::card()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }]),
        )),
    })
}

/// P0: a creature with "Exile a card from your graveyard: Add {B}", no lands,
/// and a {B} flashback spell in the graveyard ("Destroy target creature." or
/// "Draw a card."), plus one other card there when `other_card`.
fn graveyard_exile_board(targeted: bool, other_card: bool) -> HandExileBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let source = sc
        .add_creature(P0, "Graveyard-Exile Mana Source", 0, 3)
        .with_ability_definition(exile_from_graveyard_mana())
        .id();
    let victim = sc.add_creature(P1, "Victim", 2, 2).id();
    sc.with_library_top(P0, &["Card A", "Card B"]);
    let text = if targeted {
        "Destroy target creature."
    } else {
        "Draw a card."
    };
    let black = cost(vec![ManaCostShard::Black], 0);
    let spell = sc
        .add_spell_to_graveyard(P0, "Flashback Black Spell", true)
        .from_oracle_text(text)
        .with_mana_cost(black.clone())
        .with_keyword(Keyword::Flashback(FlashbackCost::Mana(black)))
        .id();
    if other_card {
        sc.add_spell_to_graveyard(P0, "Other Graveyard Card", true);
    }
    HandExileBoard {
        runner: sc.build(),
        spell,
        source,
        victim,
    }
}

fn flashback_cast(runner: &GameRunner, spell: ObjectId) -> Option<GameAction> {
    candidate_actions(runner.state())
        .into_iter()
        .map(|candidate| candidate.action)
        .find(|action| {
            matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell)
        })
}

/// CR 601.2a + CR 702.34a: a flashback spell cast from the graveyard is on the
/// stack, so it can't be exiled from the graveyard to pay for itself.
#[test]
fn flashback_spell_cannot_exile_itself_from_the_graveyard_to_pay() {
    let b = graveyard_exile_board(true, false);
    let action = flashback_cast(&b.runner, b.spell)
        .expect("reach: before announcement the spell is its own graveyard fodder");
    assert!(
        !legal_actions(b.runner.state()).contains(&action),
        "nothing is left to exile once the spell is on the stack"
    );
    assert_reducer_refuses_payment(b.runner.state(), action, Some(b.victim));
}

/// Positive control: another graveyard card pays for the untargeted flashback
/// cast.
#[test]
fn flashback_spell_pays_by_exiling_another_graveyard_card() {
    let mut b = graveyard_exile_board(false, true);
    let action = flashback_cast(&b.runner, b.spell).expect("the flashback cast is generated");
    assert!(legal_actions(b.runner.state()).contains(&action));
    announce_and_target(&mut b.runner, action, None);
    b.runner
        .act(GameAction::ActivateAbility {
            source_id: b.source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the exile source activates during payment");
    let WaitingFor::PayCost { choices, .. } = b.runner.state().waiting_for.clone() else {
        panic!(
            "the exile cost asks for a card: {:?}",
            b.runner.state().waiting_for
        )
    };
    assert_eq!(choices.len(), 1, "only the other card can be exiled");
    assert!(!choices.contains(&b.spell));
    b.runner
        .act(GameAction::SelectCards { cards: choices })
        .expect("the other card is exiled");
    b.runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the payment");
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().objects[&b.spell].zone,
        Zone::Exile,
        "CR 702.34a: the flashback spell resolved and was exiled as it left the stack"
    );
}

const EVIDENCE_SPELL: &str =
    "As an additional cost to cast this spell, collect evidence 3.\nDraw a card.";

/// P0: a sorcery with "collect evidence 3" and flashback {0} in the graveyard
/// (mana value 3), plus, when `fuel`, another mana value 3 card there.
fn evidence_board(fuel: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.with_library_top(P0, &["Card A", "Card B"]);
    let spell = sc
        .add_spell_to_graveyard(P0, "Evidence Flashback Spell", false)
        .from_oracle_text(EVIDENCE_SPELL)
        .with_mana_cost(ManaCost::generic(3))
        .with_keyword(Keyword::Flashback(FlashbackCost::Mana(ManaCost::zero())))
        .id();
    let fuel = fuel.then(|| {
        sc.add_spell_to_graveyard(P0, "Evidence Fuel", false)
            .with_mana_cost(ManaCost::generic(3))
            .id()
    });
    (sc.build(), spell, fuel)
}

/// CR 601.2a + CR 701.59a: a collect-evidence spell cast from the graveyard is
/// on the stack, so it can't exile itself as its own evidence. With no other
/// card there, the reducer refuses the cast. (Whether such a cast is offered
/// at all is a separate pre-existing gap: the castability gate doesn't check a
/// required collect-evidence cost, even with an empty graveyard. Not asserted.)
#[test]
fn collect_evidence_spell_is_not_its_own_evidence() {
    // Reach guard: with other evidence the same flashback cast is generated
    // and reaches the evidence choice (see the positive control below).
    let (fueled, fueled_spell, _) = evidence_board(true);
    assert!(flashback_cast(&fueled, fueled_spell).is_some());

    let (runner, spell, _) = evidence_board(false);
    let action = GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    let mut announced = GameRunner::from_state(runner.state().clone());
    assert!(
        announced.act(action).is_err(),
        "the reducer refuses a cast whose only evidence is itself"
    );
}

/// Positive control: another mana value 3 card is the only evidence offered,
/// and the cast resolves.
#[test]
fn collect_evidence_from_graveyard_uses_other_cards() {
    let (mut runner, spell, fuel) = evidence_board(true);
    let fuel = fuel.expect("fuel card");
    let action = flashback_cast(&runner, spell).expect("the flashback cast is generated");
    assert!(legal_actions(runner.state()).contains(&action));
    runner.act(action).expect("the cast is announced");
    let WaitingFor::CollectEvidenceChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the cast asks for evidence: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        cards,
        vec![fuel],
        "CR 601.2a: the spell being cast isn't evidence"
    );
    runner
        .act(GameAction::SelectCards { cards })
        .expect("the evidence is collected");
    let hand_before = runner.state().players[0].hand.len();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
}
