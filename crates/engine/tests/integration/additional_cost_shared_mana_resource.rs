//! CR 601.2f-h + CR 118.3 + CR 118.10: a spell's additional cost and the costs
//! of the mana abilities that pay for it draw on the same finite cards. A hand
//! or graveyard card exiled or discarded for the additional cost can't also pay
//! a mana ability's cost, so a cast whose mana is only reachable by spending
//! those same cards isn't offered, and one that leaves enough of them is.
//!
//! Covered class: hand and graveyard legs. Battlefield legs (a sacrifice beside
//! a self-sacrifice mana source) follow the deferred-sacrifice model and are
//! pinned by `issue_5252_additional_sacrifice_after_mana_abilities.rs`.
//!
//! Oracle text below is verbatim from MTGJSON `AtomicCards.json`.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AdditionalCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOARING_STONEGLIDER: &str = "As an additional cost to cast this spell, exile two cards from your graveyard or pay {1}{W}.\nFlying, vigilance";
const RUBBLE_ROUSER: &str = "When this creature enters, you may discard a card. If you do, draw a card.\n{T}, Exile a card from your graveyard: Add {R}. When you do, this creature deals 1 damage to each opponent.";
const ABHORRENT_OCULUS: &str = "As an additional cost to cast this spell, exile six cards from your graveyard.\nFlying\nAt the beginning of each opponent's upkeep, manifest dread. (Look at the top two cards of your library. Put one onto the battlefield face down as a 2/2 creature and the other into your graveyard. Turn it face up any time for its mana cost if it's a creature card.)";
const THRILL_OF_POSSIBILITY: &str =
    "As an additional cost to cast this spell, discard a card.\nDraw two cards.";
const SKIRGE_FAMILIAR: &str = "Flying\nDiscard a card: Add {B}.";
const REDEMPTOR_DREADNOUGHT: &str = "Fallen Warrior \u{2014} As an additional cost to cast this spell, you may exile a creature card from your graveyard.\nTrample\nPlasma Incinerator \u{2014} Whenever this creature attacks, if a card is exiled with it, it gets +X/+X until end of turn, where X is the power of the exiled card.";
const STITCHED_DRAKE: &str =
    "As an additional cost to cast this spell, exile a creature card from your graveyard.\nFlying";

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: shards.to_vec(),
    }
}

fn scenario() -> GameScenario {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    // Thrill of Possibility draws on resolution; the casts
    // below stop on the stack, but a library keeps the board ordinary.
    sc.with_library_top(P0, &["Card A", "Card B"]);
    sc
}

fn add_rubble_rouser(sc: &mut GameScenario) -> ObjectId {
    sc.add_creature_from_oracle(P0, "Rubble Rouser", 1, 4, RUBBLE_ROUSER)
        .with_mana_cost(cost(2, &[ManaCostShard::Red]))
        .id()
}

fn add_stoneglider(sc: &mut GameScenario) -> ObjectId {
    let mut card = sc.add_creature_to_hand(P0, "Soaring Stoneglider", 4, 3);
    card.from_oracle_text_with_keywords(&["Flying", "Vigilance"], SOARING_STONEGLIDER)
        .with_mana_cost(cost(2, &[ManaCostShard::White]));
    card.id()
}

/// `count` cards in P0's graveyard, none of them creature cards.
fn add_graveyard_cards(sc: &mut GameScenario, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|i| {
            sc.add_spell_to_graveyard(P0, &format!("Graveyard Card {i}"), true)
                .id()
        })
        .collect()
}

fn cast_action(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

/// Offer: the cast is in the legal actions.
fn offered(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == spell))
}

/// Predicate: the castability authority the AI filter accepts on.
fn castable(runner: &GameRunner, spell: ObjectId) -> bool {
    can_cast_object_now(runner.state(), P0, spell)
}

/// How a cast's prompts are answered.
#[derive(Default)]
struct Answers {
    /// Whether an optional additional cost is paid. A Choice always
    /// takes its first branch, the object leg.
    optional: bool,
    /// Objects preferred when a cost asks for a selection.
    picks: Vec<ObjectId>,
}

struct Trail {
    prompts: Vec<&'static str>,
    refused: bool,
}

/// Dispatch the cast and answer every prompt until priority. During a mana
/// payment each mana ability on offer is activated once, then each untapped
/// land is tapped, then the payment is finished.
fn drive(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> Trail {
    let mut trail = Trail {
        prompts: Vec::new(),
        refused: false,
    };
    if runner.act(cast_action(runner, spell)).is_err() {
        trail.refused = true;
        return trail;
    }
    let mut used: Vec<ObjectId> = Vec::new();
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => {
                let pay = match cost {
                    AdditionalCost::Choice(..) => true,
                    _ => answers.optional,
                };
                GameAction::DecideOptionalCost {
                    index: if pay { 0 } else { 1 },
                }
            }
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
            WaitingFor::ManaSourceSelection { options, .. } => {
                let Some(selection) = options.first().cloned() else {
                    break;
                };
                GameAction::ActivateManaSource { selection }
            }
            WaitingFor::ManaPayment { .. } => {
                let (flat, _, grouped) = legal_actions_full(runner.state());
                let ability = flat.iter().find(|a| {
                    matches!(a, GameAction::ActivateAbility { source_id, .. }
                        if !used.contains(source_id))
                });
                let land = grouped.values().flatten().find(|a| {
                    matches!(a, GameAction::TapLandForMana { selection }
                        if !used.contains(&selection.source.object_id))
                });
                match ability.or(land) {
                    Some(action) => {
                        used.extend(action.related_object_ids());
                        action.clone()
                    }
                    None => GameAction::PassPriority,
                }
            }
            _ => break,
        };
        trail.prompts.push(runner.waiting_for_kind());
        if runner.act(action).is_err() {
            trail.refused = true;
            break;
        }
    }
    trail
}

/// The cast's completion boundary: nothing refused, the spell on the stack and
/// P0 holding priority (CR 601.2i). Rubble Rouser's reflexive trigger may share
/// the stack, so the spell's own zone is read, not the stack's length.
fn assert_cast_completes(runner: &GameRunner, spell: ObjectId, trail: &Trail, row: &str) {
    assert!(
        !trail.refused,
        "{row}: the cast must not be refused: {:?} then {:?}",
        trail.prompts,
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{row}: the cast must end with P0 holding priority, got {:?} after {:?}",
        runner.state().waiting_for,
        trail.prompts
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "{row}: the spell is on the stack after {:?}",
        trail.prompts
    );
}

/// The negative rows: no offer, and a directly dispatched cast is refused.
fn assert_not_castable(runner: &mut GameRunner, spell: ObjectId, row: &str) {
    assert!(
        !offered(runner, spell),
        "{row}: the cast must not be offered"
    );
    assert!(
        runner.act(cast_action(runner, spell)).is_err(),
        "{row}: a directly dispatched cast must be refused (CR 601.2), got {:?}",
        runner.state().waiting_for
    );
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

struct StonegliderBoard {
    runner: GameRunner,
    spell: ObjectId,
    rouser: ObjectId,
    lands: Vec<ObjectId>,
    graveyard: Vec<ObjectId>,
}

/// P0: Soaring Stoneglider in hand, Rubble Rouser, Plains and Mountain, plus
/// `extra_plains` more Plains and `graveyard` cards in the graveyard.
fn stoneglider_board(graveyard: usize, extra_plains: usize) -> StonegliderBoard {
    let mut sc = scenario();
    let spell = add_stoneglider(&mut sc);
    let rouser = add_rubble_rouser(&mut sc);
    let mut lands = vec![
        sc.add_basic_land(P0, ManaColor::White),
        sc.add_basic_land(P0, ManaColor::Red),
    ];
    lands.extend((0..extra_plains).map(|_| sc.add_basic_land(P0, ManaColor::White)));
    let graveyard = add_graveyard_cards(&mut sc, graveyard);
    StonegliderBoard {
        runner: sc.build(),
        spell,
        rouser,
        lands,
        graveyard,
    }
}

/// T1. CR 601.2h + CR 118.10: the two graveyard cards pay the exile branch, so
/// Rubble Rouser has nothing to exile and only {W}{R} can be made for {2}{W};
/// the mana branch totals {3}{W}{W}. Neither branch is payable.
#[test]
fn stoneglider_is_not_offered_when_the_exile_takes_the_rousers_fodder() {
    let mut b = stoneglider_board(2, 0);
    assert!(
        !castable(&b.runner, b.spell),
        "no branch of the additional cost is payable beside the mana"
    );
    assert_not_castable(&mut b.runner, b.spell, "two graveyard cards");
}

/// T2, the reach guard of T1 on the same path: a third graveyard card is left
/// for Rubble Rouser, so the cast is offered and pays in full.
#[test]
fn stoneglider_is_cast_when_a_third_card_is_left_for_the_rouser() {
    let mut b = stoneglider_board(3, 0);
    assert!(castable(&b.runner, b.spell));
    assert!(offered(&b.runner, b.spell));

    let trail = drive(&mut b.runner, b.spell, &Answers::default());

    assert_cast_completes(&b.runner, b.spell, &trail, "three graveyard cards");
    for card in &b.graveyard {
        assert_eq!(
            zone(&b.runner, *card),
            Zone::Exile,
            "two cards are exiled for the cost and the third by Rubble Rouser: {:?}",
            trail.prompts
        );
    }
    assert!(
        tapped(&b.runner, b.rouser),
        "Rubble Rouser made the third mana"
    );
    for land in &b.lands {
        assert!(tapped(&b.runner, *land), "both lands are tapped");
    }
}

/// T3. The hold is not over-broad: with a third land the mana needs no fodder,
/// so both graveyard cards pay the exile branch.
#[test]
fn stoneglider_is_cast_with_two_cards_when_lands_pay_the_mana() {
    let mut b = stoneglider_board(2, 1);
    assert!(castable(&b.runner, b.spell));
    assert!(offered(&b.runner, b.spell));

    let trail = drive(&mut b.runner, b.spell, &Answers::default());

    assert_cast_completes(&b.runner, b.spell, &trail, "three lands");
    for card in &b.graveyard {
        assert_eq!(zone(&b.runner, *card), Zone::Exile);
    }
}

/// T7 (hostile). A graveyard-exile mana ability that can't be activated now (a
/// tapped or summoning-sick Rubble Rouser) must not block a cast the lands pay.
#[test]
fn an_unusable_rouser_does_not_block_the_cast() {
    for summoning_sick in [false, true] {
        let row = if summoning_sick {
            "summoning-sick Rouser"
        } else {
            "tapped Rouser"
        };
        let mut b = stoneglider_board(2, 1);
        {
            let turn = b.runner.state().turn_number;
            let rouser = b.runner.state_mut().objects.get_mut(&b.rouser).unwrap();
            if summoning_sick {
                rouser.entered_battlefield_turn = Some(turn);
                rouser.summoning_sick = true;
            } else {
                rouser.tapped = true;
            }
        }
        assert!(castable(&b.runner, b.spell), "{row}");
        assert!(offered(&b.runner, b.spell), "{row}");

        let trail = drive(&mut b.runner, b.spell, &Answers::default());

        assert_cast_completes(&b.runner, b.spell, &trail, row);
        for card in &b.graveyard {
            assert_eq!(zone(&b.runner, *card), Zone::Exile, "{row}");
        }
    }
}

struct Board {
    runner: GameRunner,
    spell: ObjectId,
}

/// P0: Abhorrent Oculus in hand, two Islands, Rubble Rouser (its {R} pays the
/// generic mana) and `graveyard` cards in the graveyard.
fn oculus_board(graveyard: usize) -> (Board, Vec<ObjectId>) {
    let mut sc = scenario();
    let spell = sc
        .add_creature_to_hand_from_oracle(P0, "Abhorrent Oculus", 5, 5, ABHORRENT_OCULUS)
        .with_mana_cost(cost(2, &[ManaCostShard::Blue]))
        .id();
    add_rubble_rouser(&mut sc);
    sc.add_basic_land(P0, ManaColor::Blue);
    sc.add_basic_land(P0, ManaColor::Blue);
    let cards = add_graveyard_cards(&mut sc, graveyard);
    (
        Board {
            runner: sc.build(),
            spell,
        },
        cards,
    )
}

/// T4. A required graveyard-exile cost: six cards for the cost leave none for
/// Rubble Rouser; a seventh pays for its mana.
#[test]
fn oculus_needs_a_seventh_card_for_the_rouser() {
    let (mut six, _) = oculus_board(6);
    assert!(
        !castable(&six.runner, six.spell),
        "all six cards are exiled for the cost"
    );
    assert_not_castable(&mut six.runner, six.spell, "six graveyard cards");

    let (mut seven, cards) = oculus_board(7);
    assert!(castable(&seven.runner, seven.spell));
    assert!(offered(&seven.runner, seven.spell));
    let trail = drive(&mut seven.runner, seven.spell, &Answers::default());
    assert_cast_completes(&seven.runner, seven.spell, &trail, "seven graveyard cards");
    for card in &cards {
        assert_eq!(zone(&seven.runner, *card), Zone::Exile);
    }
}

/// P0: Thrill of Possibility and `others` more cards in hand, a Mountain and
/// Skirge Familiar.
fn thrill_board(others: usize) -> Board {
    let mut sc = scenario();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Thrill of Possibility", true, THRILL_OF_POSSIBILITY)
        .with_mana_cost(cost(1, &[ManaCostShard::Red]))
        .id();
    sc.add_creature_from_oracle(P0, "Skirge Familiar", 3, 2, SKIRGE_FAMILIAR)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]));
    sc.add_basic_land(P0, ManaColor::Red);
    for i in 0..others {
        sc.add_spell_to_hand(P0, &format!("Other Card {i}"), true)
            .with_mana_cost(cost(5, &[ManaCostShard::Red, ManaCostShard::Red]));
    }
    Board {
        runner: sc.build(),
        spell,
    }
}

/// T5. A hand leg against a discard mana ability: the one other card is
/// discarded for the cost, so Skirge Familiar has none; a second card pays for
/// its mana. The predicate is asserted only on the positive board: before
/// announcement the spell itself is still hand fodder (CR 601.2a).
#[test]
fn thrill_needs_a_second_card_for_the_familiar() {
    let mut one = thrill_board(1);
    assert_not_castable(&mut one.runner, one.spell, "one other card");

    let mut two = thrill_board(2);
    assert!(castable(&two.runner, two.spell));
    assert!(offered(&two.runner, two.spell));
    let trail = drive(&mut two.runner, two.spell, &Answers::default());
    assert_cast_completes(&two.runner, two.spell, &trail, "two other cards");
    assert!(
        two.runner.state().players[0].hand.is_empty(),
        "one card is discarded for the cost and one for Skirge Familiar's mana: {:?}",
        trail.prompts
    );
}

/// P0: Redemptor Dreadnought in hand, four Mountains, Rubble Rouser, a creature
/// card in the graveyard, and a noncreature card there when `spare`.
fn dreadnought_board(spare: bool) -> (Board, ObjectId, Option<ObjectId>) {
    let mut sc = scenario();
    let spell = sc
        .add_creature_to_hand_from_oracle(P0, "Redemptor Dreadnought", 4, 4, REDEMPTOR_DREADNOUGHT)
        .with_mana_cost(cost(5, &[]))
        .id();
    add_rubble_rouser(&mut sc);
    for _ in 0..4 {
        sc.add_basic_land(P0, ManaColor::Red);
    }
    let creature = sc
        .add_creature_to_graveyard(P0, "Graveyard Creature", 1, 1)
        .id();
    let other = spare.then(|| add_graveyard_cards(&mut sc, 1)[0]);
    (
        Board {
            runner: sc.build(),
            spell,
        },
        creature,
        other,
    )
}

/// T8. An optional graveyard-exile cost: paying it would exile the one card
/// Rubble Rouser needs for the fifth mana of {5}, so the option isn't offered
/// and the cast completes without it. With a second graveyard card the option
/// is offered and paid.
#[test]
fn redemptor_dreadnought_exile_is_offered_only_with_a_card_to_spare() {
    let (mut bare, creature, _) = dreadnought_board(false);
    assert!(offered(&bare.runner, bare.spell));
    let trail = drive(
        &mut bare.runner,
        bare.spell,
        &Answers {
            optional: true,
            ..Answers::default()
        },
    );
    assert!(
        !trail.prompts.contains(&"OptionalCostChoice"),
        "the exile can't be paid beside the mana, so it isn't offered: {:?}",
        trail.prompts
    );
    assert_cast_completes(&bare.runner, bare.spell, &trail, "cost not paid");
    assert_eq!(
        zone(&bare.runner, creature),
        Zone::Exile,
        "the creature card is exiled for Rubble Rouser's mana"
    );

    let (mut spare, creature, other) = dreadnought_board(true);
    let other = other.unwrap();
    let trail = drive(
        &mut spare.runner,
        spare.spell,
        &Answers {
            optional: true,
            // The mana ability's exile takes the noncreature card if asked first.
            picks: vec![other],
        },
    );
    assert!(
        trail.prompts.contains(&"OptionalCostChoice"),
        "reach: with a card to spare the exile is offered: {:?}",
        trail.prompts
    );
    assert_cast_completes(&spare.runner, spare.spell, &trail, "cost paid");
    assert_eq!(
        zone(&spare.runner, creature),
        Zone::Exile,
        "the creature card is exiled for the cost"
    );
    assert_eq!(
        zone(&spare.runner, other),
        Zone::Exile,
        "the other card is exiled for Rubble Rouser's mana"
    );
}

/// P0: Stitched Drake in hand, two Islands, Rubble Rouser, a creature card in
/// the graveyard, and a noncreature card there when `noncreature`.
fn drake_board(noncreature: bool) -> (Board, ObjectId, Option<ObjectId>) {
    let mut sc = scenario();
    let spell = sc
        .add_creature_to_hand_from_oracle(P0, "Stitched Drake", 3, 4, STITCHED_DRAKE)
        .with_mana_cost(cost(1, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .id();
    add_rubble_rouser(&mut sc);
    sc.add_basic_land(P0, ManaColor::Blue);
    sc.add_basic_land(P0, ManaColor::Blue);
    let creature = sc
        .add_creature_to_graveyard(P0, "Graveyard Creature", 1, 1)
        .id();
    let other = noncreature.then(|| add_graveyard_cards(&mut sc, 1)[0]);
    (
        Board {
            runner: sc.build(),
            spell,
        },
        creature,
        other,
    )
}

/// T9 (hostile, asymmetric). The cost's candidates are a strict subset of the
/// mana ability's fodder: the creature card must pay Stitched Drake's cost, and
/// Rubble Rouser may exile either card. With only the creature card the cast
/// isn't offered; with a noncreature card beside it, it is.
#[test]
fn stitched_drake_needs_a_card_besides_the_creature_card() {
    let (mut only, _, _) = drake_board(false);
    assert!(
        !castable(&only.runner, only.spell),
        "the only graveyard card is exiled for the cost"
    );
    assert_not_castable(&mut only.runner, only.spell, "creature card only");

    let (mut both, creature, other) = drake_board(true);
    let other = other.unwrap();
    assert!(castable(&both.runner, both.spell));
    assert!(offered(&both.runner, both.spell));
    let trail = drive(
        &mut both.runner,
        both.spell,
        &Answers {
            // The mana ability's exile takes the noncreature card if asked first.
            picks: vec![other],
            ..Answers::default()
        },
    );
    assert_cast_completes(&both.runner, both.spell, &trail, "creature and noncreature");
    assert_eq!(zone(&both.runner, creature), Zone::Exile);
    assert_eq!(zone(&both.runner, other), Zone::Exile);
}
