//! An additional cost of two " and "-joined costs requires paying both.
//!
//! CR 601.2f + CR 601.2h: every additional cost is part of the total cost, and
//! the total cost is paid in full. Ruthless Disposal: "discard a card and
//! sacrifice a creature".

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, Effect, ReplacementDefinition,
    ReplacementMode, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;
use engine::types::TargetRef;

const RUTHLESS_DISPOSAL: &str = "As an additional cost to cast this spell, discard a card and sacrifice a creature.\nTwo target creatures each get -13/-13 until end of turn.";

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    fodder: ObjectId,
    targets: [ObjectId; 2],
}

/// Ruthless Disposal and one other card in P0's hand, two 2/2s for P1, and
/// {B}{B}{B}{B}{B} floating; `with_own_creature` adds P0's own 2/2.
fn ruthless_board(with_own_creature: bool) -> (Board, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ruthless Disposal", false, RUTHLESS_DISPOSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Fodder");
    let own = with_own_creature.then(|| scenario.add_vanilla(P0, 2, 2));
    let targets = [
        scenario.add_vanilla(P1, 2, 2),
        scenario.add_vanilla(P1, 2, 2),
    ];
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    (
        Board {
            runner: scenario.build(),
            spell,
            fodder,
            targets,
        },
        own,
    )
}

#[test]
fn ruthless_disposal_pays_the_discard_and_the_sacrifice() {
    let (mut board, own) = ruthless_board(true);
    let bear = own.expect("own creature");

    let outcome = board
        .runner
        .cast(board.spell)
        .target_objects(&board.targets)
        .pay_cost_with(&[board.fodder, bear])
        .resolve();

    outcome.assert_zone(&[board.fodder, bear], Zone::Graveyard);
    outcome.assert_zone(&board.targets, Zone::Graveyard);
    outcome.assert_zone(&[board.spell], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// Ruling: Ruthless Disposal can't be cast without a creature to sacrifice, so
/// the cast is reversed (CR 733.1) rather than completed on the discard alone.
#[test]
fn ruthless_disposal_without_a_creature_is_not_cast() {
    let (mut board, _) = ruthless_board(false);
    let runner = &mut board.runner;
    let card_id = runner.state().objects[&board.spell].card_id;
    let Some(AdditionalCost::Required(AbilityCost::Composite { costs })) =
        &runner.state().objects[&board.spell].additional_cost
    else {
        panic!("Ruthless Disposal parses to a required composite cost");
    };
    assert!(costs
        .iter()
        .any(|cost| matches!(cost, AbilityCost::Discard { .. })));
    assert!(costs
        .iter()
        .any(|cost| matches!(cost, AbilityCost::Sacrifice(_))));

    runner
        .act(GameAction::CastSpell {
            object_id: board.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast is offered");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the cast reaches target selection, got {:?}",
        runner.state().waiting_for
    );
    let result = runner.act(GameAction::SelectTargets {
        targets: board
            .targets
            .iter()
            .map(|&t| TargetRef::Object(t))
            .collect(),
    });
    let result = result.expect("the unpayable sacrifice reverses the cast");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "the cast is reversed, not continued to a discard prompt: {:?}",
        result.waiting_for
    );
    assert!(result.events.is_empty());

    let state = runner.state();
    assert_eq!(state.objects[&board.fodder].zone, Zone::Hand);
    assert_eq!(state.objects[&board.spell].zone, Zone::Hand);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

const KAERVEKS_SPITE: &str = "As an additional cost to cast this spell, sacrifice all permanents you control and discard your hand.\nTarget player loses 5 life.";

#[test]
fn kaerveks_spite_sacrifices_everything_and_discards_the_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kaervek's Spite", true, KAERVEKS_SPITE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black; 3],
            generic: 0,
        })
        .id();
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
    ];
    let swamps: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .pay_cost_with(&hand)
        .resolve();

    // CR 601.2g + CR 601.2h: the Swamps tapped for {B}{B}{B} are still
    // controlled when the cost is paid, so they are sacrificed too.
    outcome.assert_zone(&swamps, Zone::Graveyard);
    outcome.assert_zone(&[bear], Zone::Graveyard);
    outcome.assert_zone(&hand, Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(outcome.life_delta(P1), -5);
    outcome.assert_zone(&[theirs], Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";

fn add_kaerveks_spite(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Kaervek's Spite", true, KAERVEKS_SPITE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black; 3],
            generic: 0,
        })
        .id()
}

fn add_skirge_familiar(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Skirge Familiar", 3, 2)
        .from_oracle_text_with_keywords(&["Flying"], SKIRGE_FAMILIAR)
        .id()
}

/// Casts `spell` with `payment_mode`, answering every target prompt with P1.
fn cast_at_p1(runner: &mut GameRunner, spell: ObjectId, payment_mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode,
        })
        .expect("the cast is offered");
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("P1 is a legal target");
    }
}

fn cast_manually(runner: &mut GameRunner, spell: ObjectId) {
    cast_at_p1(runner, spell, CastPaymentMode::Manual);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "CR 601.2g: the mana window opens before the hand is discarded, got {:?}",
        runner.state().waiting_for
    );
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the mana ability activates during payment");
}

fn discard_to_familiar(runner: &mut GameRunner, familiar: ObjectId, card: ObjectId) {
    activate(runner, familiar);
    runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("the card is discarded for {B}");
}

/// Answers the "discard your hand" prompt with `cards`, in the order given.
fn discard_rest_of_hand(runner: &mut GameRunner, cards: &[ObjectId]) {
    let WaitingFor::PayCost {
        kind: PayCostKind::Discard,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the hand is discarded at payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((choices.len(), *count), (cards.len(), cards.len()));
    runner
        .act(GameAction::SelectCards {
            cards: cards.to_vec(),
        })
        .expect("the whole hand is discarded");
}

/// CR 601.2g + CR 601.2h: the hand is discarded when the cost is paid, after
/// mana abilities, so a card discarded to Skirge Familiar in the mana window
/// funds the spell and "discard your hand" then discards what is left.
#[test]
fn kaerveks_spite_hand_funds_skirge_familiar_before_the_hand_is_discarded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_kaerveks_spite(&mut scenario);
    let familiar = add_skirge_familiar(&mut scenario);
    let swamps: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
        scenario.add_card_to_hand(P0, "H3"),
    ];
    let theirs = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    cast_manually(&mut runner, spell);
    for &swamp in &swamps {
        activate(&mut runner, swamp);
    }
    discard_to_familiar(&mut runner, familiar, hand[0]);
    runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the mana payment");
    discard_rest_of_hand(&mut runner, &[hand[1], hand[2]]);

    let state = runner.state();
    assert_eq!(state.stack.back().map(|e| e.id), Some(spell));
    assert!(state.players[0].hand.is_empty());
    for id in hand.iter().chain(&swamps).chain([&familiar]) {
        assert_eq!(state.objects[id].zone, Zone::Graveyard);
    }
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), life_before - 5);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Battlefield);
}

/// CR 601.2g: every {B} comes from discarding the whole hand to three Skirge
/// Familiars in the mana window; "discard your hand" then has nothing left.
#[test]
fn kaerveks_spite_paid_entirely_by_discarding_the_hand_to_skirge_familiars() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_kaerveks_spite(&mut scenario);
    let familiars: Vec<ObjectId> = (0..3).map(|_| add_skirge_familiar(&mut scenario)).collect();
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
        scenario.add_card_to_hand(P0, "H3"),
    ];
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    cast_manually(&mut runner, spell);
    for (&familiar, &card) in familiars.iter().zip(&hand) {
        discard_to_familiar(&mut runner, familiar, card);
    }
    runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the payment");

    let state = runner.state();
    assert_eq!(state.stack.back().map(|e| e.id), Some(spell));
    for id in hand.iter().chain(&familiars) {
        assert_eq!(state.objects[id].zone, Zone::Graveyard);
    }
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), life_before - 5);
}

/// The count-0 arm: an empty hand discards nothing and the cast completes.
#[test]
fn kaerveks_spite_with_an_empty_hand_still_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_kaerveks_spite(&mut scenario);
    let swamps: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    outcome.assert_zone(&swamps, Zone::Graveyard);
    assert_eq!(outcome.life_delta(P1), -5);
}

/// A chosen-count discard ("discard a card") is still selected before the mana
/// window; a different card can then be discarded to Skirge Familiar in it.
#[test]
fn ruthless_disposal_discard_is_chosen_before_the_mana_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ruthless Disposal", false, RUTHLESS_DISPOSAL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .id();
    let familiar = add_skirge_familiar(&mut scenario);
    let swamps: Vec<ObjectId> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let a = scenario.add_card_to_hand(P0, "A");
    let b = scenario.add_card_to_hand(P0, "B");
    let bear = scenario.add_vanilla(P0, 2, 2);
    let targets = [
        scenario.add_vanilla(P1, 2, 2),
        scenario.add_vanilla(P1, 2, 2),
    ];
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the cast is offered");
    runner
        .act(GameAction::SelectTargets {
            targets: targets.iter().map(|&t| TargetRef::Object(t)).collect(),
        })
        .expect("two P1 creatures are legal targets");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards { cards: vec![b] })
        .expect("B is discarded for the cost");
    runner
        .act(GameAction::SelectCards { cards: vec![bear] })
        .expect("the bear is sacrificed for the cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    discard_to_familiar(&mut runner, familiar, a);
    for &swamp in &swamps {
        activate(&mut runner, swamp);
    }
    runner
        .act(GameAction::PassPriority)
        .expect("passing finishes the payment");

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.back().map(|e| e.id), Some(spell));
    for id in [a, b, bear] {
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);
    }
}

/// An optional graveyard-to-exile redirect for every card but `spell`.
fn optional_exile_instead_except(spell: ObjectId) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .valid_card(TargetFilter::Not {
            filter: Box::new(TargetFilter::SpecificObject { id: spell }),
        })
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

/// CR 601.2h: each card of the discarded hand can pause the payment
/// on a replacement choice, and the payment resumes until the whole hand is gone.
#[test]
fn kaerveks_spite_hand_discard_resumes_after_each_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_kaerveks_spite(&mut scenario);
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
        scenario.add_card_to_hand(P0, "H3"),
    ];
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_exile_instead_except(spell));
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast is offered");
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("P1 is a legal target");
    }
    discard_rest_of_hand(&mut runner, &hand);
    assert!(
        runner.state().pending_discard_for_cost.is_some(),
        "reach guard: the hand discard parks on a cost resume, got {:?}",
        runner.state().waiting_for
    );
    let mut choices = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for {
        let accept = candidates
            .iter()
            .position(|c| c.description == "Accept")
            .expect("an Accept option");
        runner
            .act(GameAction::ChooseReplacement { index: accept })
            .expect("accepting the redirect resumes the payment");
        choices += 1;
    }

    assert_eq!(choices, 3);
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.back().map(|e| e.id), Some(spell));
    assert_eq!(state.objects[&spell].mana_spent_to_cast_amount, 3);
    assert_eq!(state.objects[&spell].colors_spent_to_cast.black, 3);
    assert!(state.players[0].hand.is_empty());
    for id in &hand {
        assert_eq!(state.objects[id].zone, Zone::Exile);
    }
}

/// CR 404.3 + CR 701.9b: "discard your hand" has no random element, so the
/// owner orders the discarded cards (CR 404.1: each lands on top) and no
/// outcome draw is spent.
#[test]
fn kaerveks_spite_owner_orders_the_discarded_hand_without_an_outcome_draw() {
    for owner_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_kaerveks_spite(&mut scenario);
        let bears = scenario
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .id();
        let giant = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
        let swamps: Vec<ObjectId> = (0..3)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
            .collect();
        let mut runner = scenario.build();
        let draws_before = runner.state().rng.outcome_draws();
        let order = if owner_first {
            [giant, bears]
        } else {
            [bears, giant]
        };

        cast_at_p1(&mut runner, spell, CastPaymentMode::Auto);
        discard_rest_of_hand(&mut runner, &order);

        let state = runner.state();
        assert_eq!(state.stack.back().map(|e| e.id), Some(spell));
        for id in &swamps {
            assert_eq!(state.objects[id].zone, Zone::Graveyard);
        }
        let graveyard_order: Vec<ObjectId> = state.players[0]
            .graveyard
            .iter()
            .copied()
            .filter(|id| order.contains(id))
            .collect();
        assert_eq!(graveyard_order, order);
        assert_eq!(state.rng.outcome_draws(), draws_before);
    }
}

const LAVINIA: &str = "Each opponent can't cast noncreature spells with mana value greater than the number of lands that player controls.\nWhenever an opponent casts a spell, if no mana was spent to cast it, counter that spell.";

/// CR 601.2h: discarding the hand at payment keeps the record of the {B}{B}{B}
/// paid, so Lavinia's "if no mana was spent to cast it" doesn't counter the spell.
#[test]
fn kaerveks_spite_keeps_its_spent_mana_through_the_hand_discard() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_kaerveks_spite(&mut scenario);
        let hand = [
            scenario.add_card_to_hand(P0, "H1"),
            scenario.add_card_to_hand(P0, "H2"),
        ];
        let swamps: Vec<ObjectId> = (0..3)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
            .collect();
        scenario.add_creature_from_oracle(P1, "Lavinia, Azorius Renegade", 2, 2, LAVINIA);
        let mut runner = scenario.build();
        let life_before = runner.life(P1);

        if mode == CastPaymentMode::Manual {
            cast_manually(&mut runner, spell);
            for &swamp in &swamps {
                activate(&mut runner, swamp);
            }
            runner
                .act(GameAction::PassPriority)
                .expect("passing finishes the mana payment");
        } else {
            cast_at_p1(&mut runner, spell, mode);
        }
        discard_rest_of_hand(&mut runner, &hand);

        let cast = &runner.state().objects[&spell];
        assert_eq!(cast.mana_spent_to_cast_amount, 3, "{mode:?}");
        assert_eq!(cast.colors_spent_to_cast.black, 3, "{mode:?}");
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P1), life_before - 5, "{mode:?}");
    }
}

/// CR 733.1: cancelling at the hand-discard prompt reverses the whole cast,
/// including the permanents already sacrificed for it.
#[test]
fn kaerveks_spite_cancelled_at_the_hand_discard_restores_board_and_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_kaerveks_spite(&mut scenario);
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
    ];
    let swamps: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();

    cast_at_p1(&mut runner, spell, CastPaymentMode::Auto);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            }
        ),
        "reach guard: the hand discard prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "reach guard: the sacrifice was committed before the prompt"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be cancelled at the prompt");

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&spell].zone, Zone::Hand);
    for id in &hand {
        assert_eq!(state.objects[id].zone, Zone::Hand);
    }
    for id in swamps.iter().chain([&bear]) {
        assert_eq!(state.objects[id].zone, Zone::Battlefield);
    }
}

const VEXING_BAUBLE: &str = "Whenever a player casts a spell, if no mana was spent to cast it, counter that spell.\n{1}, {T}, Sacrifice this artifact: Draw a card.";

/// CR 601.2h: with nothing to sacrifice and {B}{B}{B} floating, the hand
/// discard still keeps the spent-mana record, so Vexing Bauble doesn't counter.
#[test]
fn kaerveks_spite_paid_from_floating_mana_keeps_its_spent_mana_through_the_hand_discard() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_kaerveks_spite(&mut scenario);
        let hand = [
            scenario.add_card_to_hand(P0, "H1"),
            scenario.add_card_to_hand(P0, "H2"),
        ];
        scenario.add_artifact_from_oracle(P1, "Vexing Bauble", VEXING_BAUBLE);
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let life_before = runner.life(P1);

        if mode == CastPaymentMode::Manual {
            cast_manually(&mut runner, spell);
            runner
                .act(GameAction::PassPriority)
                .expect("passing pays from the pool");
        } else {
            cast_at_p1(&mut runner, spell, mode);
        }
        discard_rest_of_hand(&mut runner, &hand);

        let state = runner.state();
        assert_eq!(
            state.objects[&spell].mana_spent_to_cast_amount, 3,
            "{mode:?}"
        );
        assert_eq!(
            state.objects[&spell].colors_spent_to_cast.black, 3,
            "{mode:?}"
        );
        assert_eq!(state.stack.back().map(|e| e.id), Some(spell), "{mode:?}");
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P1), life_before - 5, "{mode:?}");
    }
}
