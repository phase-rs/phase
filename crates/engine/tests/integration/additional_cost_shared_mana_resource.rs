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
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AdditionalCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
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
const AS_FORETOLD: &str = "At the beginning of your upkeep, put a time counter on this enchantment.\nOnce each turn, you may pay {0} rather than pay the mana cost for a spell you cast with mana value X or less, where X is the number of time counters on this enchantment.";
const CONSPIRACY_UNRAVELER: &str = "Flying\nYou may collect evidence 10 rather than pay the mana cost for spells you cast. (To collect evidence 10, exile cards with total mana value 10 or greater from your graveyard.)";
const STITCHED_DRAKE: &str =
    "As an additional cost to cast this spell, exile a creature card from your graveyard.\nFlying";
const CORPSE_EXPLOSION: &str = "As an additional cost to cast this spell, exile a creature card from your graveyard.\nCorpse Explosion deals damage equal to the exiled card's power to each creature and each planeswalker.";
const UNEXPECTED_WINDFALL: &str = "As an additional cost to cast this spell, discard a card.\nDraw two cards and create two Treasure tokens. (They're artifacts with \"{T}, Sacrifice this token: Add one mana of any color.\")";
const METALWORKER: &str = "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way.";

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

/// Whether an optional additional cost is paid when it is offered.
#[derive(Clone, Copy, Default)]
enum CostElection {
    Pay,
    #[default]
    Decline,
}

/// How a cast's prompts are answered.
#[derive(Default)]
struct Answers {
    /// The election for an optional additional cost. A Choice always takes its
    /// first branch: a granted alternative cost when one is offered, otherwise
    /// the additional cost's object leg.
    optional: CostElection,
    /// Objects preferred when a cost asks for a selection.
    picks: Vec<ObjectId>,
    /// The cards exiled when evidence is collected.
    evidence: Vec<ObjectId>,
}

struct Trail {
    prompts: Vec<&'static str>,
    /// The error of the action the engine refused, if one was.
    refused: Option<EngineError>,
}

/// Dispatch the cast and answer every prompt until priority. During a mana
/// payment each mana ability on offer is activated once, then each untapped
/// land is tapped, then the payment is finished.
fn drive(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> Trail {
    let mut trail = Trail {
        prompts: Vec::new(),
        refused: None,
    };
    if let Err(error) = runner.act(cast_action(runner, spell)) {
        trail.refused = Some(error);
        return trail;
    }
    let mut used: Vec<ObjectId> = Vec::new();
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => {
                let election = match cost {
                    AdditionalCost::Choice(..) => CostElection::Pay,
                    AdditionalCost::Optional { .. } | AdditionalCost::Kicker { .. } => {
                        answers.optional
                    }
                    AdditionalCost::Required(cost) => {
                        panic!("a required cost isn't an election: {cost:?}")
                    }
                };
                GameAction::DecideOptionalCost {
                    index: match election {
                        CostElection::Pay => 0,
                        CostElection::Decline => 1,
                    },
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
            WaitingFor::CollectEvidenceChoice { .. } => GameAction::SelectCards {
                cards: answers.evidence.clone(),
            },
            WaitingFor::ManaSourceSelection { options, .. } => {
                let Some(selection) = options.first().cloned() else {
                    panic!(
                        "a mana source selection offers nothing after {:?}",
                        trail.prompts
                    );
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
            // CR 601.2i: the cast is complete once its caster has priority again.
            WaitingFor::Priority { .. } => break,
            unsupported => panic!(
                "the driver has no answer for {unsupported:?} after {:?}",
                trail.prompts
            ),
        };
        trail.prompts.push(runner.waiting_for_kind());
        if let Err(error) = runner.act(action) {
            trail.refused = Some(error);
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
        trail.refused.is_none(),
        "{row}: the cast must not be refused: {:?} after {:?}, then {:?}",
        trail.refused,
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
            optional: CostElection::Pay,
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
            optional: CostElection::Pay,
            // The mana ability's exile takes the noncreature card if asked first.
            picks: vec![other],
            ..Answers::default()
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

/// P0: Thrill of Possibility and an Island in hand, Skirge Familiar and no red
/// source; with `foretold`, As Foretold with two time counters.
fn foretold_board(foretold: bool) -> (Board, ObjectId, Option<ObjectId>) {
    let mut sc = scenario();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Thrill of Possibility", true, THRILL_OF_POSSIBILITY)
        .with_mana_cost(cost(1, &[ManaCostShard::Red]))
        .id();
    sc.add_creature_from_oracle(P0, "Skirge Familiar", 3, 2, SKIRGE_FAMILIAR)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]));
    let island = sc.add_land_to_hand(P0, "Island").id();
    let enchantment = foretold.then(|| {
        let enchantment = sc
            .add_enchantment_from_oracle(P0, "As Foretold", AS_FORETOLD)
            .with_mana_cost(cost(2, &[ManaCostShard::Blue]))
            .id();
        sc.with_counter(enchantment, CounterType::Time, 2);
        enchantment
    });
    (
        Board {
            runner: sc.build(),
            spell,
        },
        island,
        enchantment,
    )
}

/// T10. CR 118.9d: additional costs apply to an alternative cost, which
/// replaces the mana cost. Skirge Familiar's discard contends with Thrill of
/// Possibility's, and {1}{R} can't be paid without a red source, but As
/// Foretold's {0} can: the cast is offered and discards the Island. Without As
/// Foretold it stays refused.
#[test]
fn a_granted_alternative_cost_keeps_a_contended_cast_offered() {
    let (mut bare, _, _) = foretold_board(false);
    assert!(!castable(&bare.runner, bare.spell));
    assert_not_castable(&mut bare.runner, bare.spell, "no red source");

    let (mut board, island, foretold) = foretold_board(true);
    let foretold = foretold.unwrap();
    assert!(castable(&board.runner, board.spell));
    assert!(offered(&board.runner, board.spell));
    let trail = drive(&mut board.runner, board.spell, &Answers::default());

    assert_cast_completes(&board.runner, board.spell, &trail, "As Foretold");
    assert!(
        board
            .runner
            .state()
            .alt_cost_grant_permissions_used
            .contains(&foretold),
        "As Foretold's {{0}} is the cost paid: {:?}",
        trail.prompts
    );
    assert_eq!(
        zone(&board.runner, island),
        Zone::Graveyard,
        "the Island is discarded for the additional cost: {:?}",
        trail.prompts
    );
}

/// P0: Soaring Stoneglider in hand, As Foretold with three time counters,
/// `graveyard` cards in the graveyard and, with `board`, T1's Plains, Mountain
/// and Rubble Rouser.
fn foretold_stoneglider(graveyard: usize, board: bool) -> (Board, Vec<ObjectId>, ObjectId) {
    let mut sc = scenario();
    let spell = add_stoneglider(&mut sc);
    if board {
        add_rubble_rouser(&mut sc);
        sc.add_basic_land(P0, ManaColor::White);
        sc.add_basic_land(P0, ManaColor::Red);
    }
    let cards = add_graveyard_cards(&mut sc, graveyard);
    let foretold = sc
        .add_enchantment_from_oracle(P0, "As Foretold", AS_FORETOLD)
        .with_mana_cost(cost(2, &[ManaCostShard::Blue]))
        .id();
    sc.with_counter(foretold, CounterType::Time, 3);
    (
        Board {
            runner: sc.build(),
            spell,
        },
        cards,
        foretold,
    )
}

/// T11. CR 118.9d, the Choice twin of T10: on T1's board neither branch of
/// Soaring Stoneglider's cost is payable beside {2}{W}, but As Foretold's {0}
/// needs no mana, so the cast is offered and both graveyard cards pay the
/// exile branch.
#[test]
fn a_granted_alternative_cost_keeps_a_contended_choice_cast_offered() {
    let (mut board, graveyard, foretold) = foretold_stoneglider(2, true);
    assert!(castable(&board.runner, board.spell));
    assert!(offered(&board.runner, board.spell));
    let trail = drive(&mut board.runner, board.spell, &Answers::default());

    assert_cast_completes(&board.runner, board.spell, &trail, "As Foretold");
    for card in &graveyard {
        assert_eq!(
            zone(&board.runner, *card),
            Zone::Exile,
            "both cards are exiled for the cost: {:?}",
            trail.prompts
        );
    }
    assert!(
        board
            .runner
            .state()
            .alt_cost_grant_permissions_used
            .contains(&foretold),
        "As Foretold's {{0}} is the cost paid: {:?}",
        trail.prompts
    );
}

/// T12. CR 118.9d + CR 601.2h: the additional cost still applies to As
/// Foretold's {0}. With no mana source and one graveyard card neither branch
/// can be paid beside it, so the cast isn't offered; with two the exile branch
/// is paid and the cast completes. Only the offer is asserted on the refused
/// board: the handler's alternative-cost arm accepts a directly dispatched
/// cast before it judges the choice, as it did before this module.
#[test]
fn a_granted_alternative_cost_does_not_waive_the_choice() {
    let (bare, _, _) = foretold_stoneglider(1, false);
    assert!(!castable(&bare.runner, bare.spell));
    assert!(!offered(&bare.runner, bare.spell));

    let (mut board, graveyard, _) = foretold_stoneglider(2, false);
    assert!(castable(&board.runner, board.spell));
    assert!(offered(&board.runner, board.spell));
    let trail = drive(&mut board.runner, board.spell, &Answers::default());
    assert_cast_completes(&board.runner, board.spell, &trail, "two graveyard cards");
    for card in &graveyard {
        assert_eq!(zone(&board.runner, *card), Zone::Exile);
    }
}

/// T13. CR 118.3: Conspiracy Unraveler's collect evidence 10 takes both
/// graveyard cards (mana value 5 each), leaving none for Soaring Stoneglider's
/// exile branch, and no mana pays {1}{W}. An alternative cost with a non-mana
/// leg isn't judged together with the choice, so it doesn't open the cast.
#[test]
fn an_alternative_cost_with_an_object_leg_does_not_open_a_contended_choice() {
    let mut sc = scenario();
    let spell = add_stoneglider(&mut sc);
    sc.add_creature_from_oracle(P0, "Conspiracy Unraveler", 6, 6, CONSPIRACY_UNRAVELER)
        .with_mana_cost(cost(5, &[ManaCostShard::Blue, ManaCostShard::Blue]));
    for i in 0..2 {
        sc.add_spell_to_graveyard(P0, &format!("Graveyard Card {i}"), true)
            .with_mana_cost(cost(5, &[]));
    }
    let runner = sc.build();

    assert!(!castable(&runner, spell));
    assert!(!offered(&runner, spell));
}

/// P0: Corpse Explosion in hand, Conspiracy Unraveler and Rubble Rouser on the
/// battlefield and no land; Desolation Twin in the graveyard and, with `spare`,
/// a second creature card beside it.
fn corpse_explosion_board(spare: bool) -> (Board, ObjectId, Option<ObjectId>) {
    let mut sc = scenario();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Corpse Explosion", false, CORPSE_EXPLOSION)
        .with_mana_cost(cost(1, &[ManaCostShard::Black, ManaCostShard::Red]))
        .id();
    sc.add_creature_from_oracle(P0, "Conspiracy Unraveler", 6, 6, CONSPIRACY_UNRAVELER)
        .with_mana_cost(cost(5, &[ManaCostShard::Blue, ManaCostShard::Blue]));
    add_rubble_rouser(&mut sc);
    let twin = sc
        .add_creature_to_graveyard(P0, "Desolation Twin", 10, 10)
        .with_mana_cost(cost(10, &[]))
        .id();
    let other = spare.then(|| {
        sc.add_creature_to_graveyard(P0, "Graveyard Creature", 1, 1)
            .with_mana_cost(cost(1, &[ManaCostShard::Green]))
            .id()
    });
    (
        Board {
            runner: sc.build(),
            spell,
        },
        twin,
        other,
    )
}

/// T13. CR 118.9d + CR 118.10, the Required twin of T12: {1}{B}{R} can't be
/// paid, and Conspiracy Unraveler's collect evidence 10 is paid with graveyard
/// cards, as Corpse Explosion's own cost is. One card pays one cost, so the
/// sole Desolation Twin can't be exiled for both and the cast isn't offered.
/// With a second creature card the Twin is the evidence and the other card
/// pays the additional cost.
#[test]
fn an_alternative_cost_and_a_required_cost_need_distinct_cards() {
    let (sole, _, _) = corpse_explosion_board(false);
    assert!(
        !castable(&sole.runner, sole.spell),
        "one card can't be exiled for both costs"
    );
    assert!(
        !offered(&sole.runner, sole.spell),
        "one card can't be exiled for both costs"
    );

    let (mut both, twin, other) = corpse_explosion_board(true);
    let other = other.unwrap();
    assert!(castable(&both.runner, both.spell));
    assert!(offered(&both.runner, both.spell));
    let trail = drive(
        &mut both.runner,
        both.spell,
        &Answers {
            picks: vec![other],
            evidence: vec![twin],
            ..Answers::default()
        },
    );
    assert_cast_completes(&both.runner, both.spell, &trail, "a second creature card");
    assert_eq!(zone(&both.runner, twin), Zone::Exile);
    assert_eq!(zone(&both.runner, other), Zone::Exile);
}

/// P0: Unexpected Windfall and `other` in hand, Skirge Familiar, two Mountains
/// and `source` on the battlefield.
fn windfall_board(source: WindfallSource, artifact_in_hand: bool) -> Board {
    let mut sc = scenario();
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, "Unexpected Windfall", true, UNEXPECTED_WINDFALL)
        .with_mana_cost(cost(2, &[ManaCostShard::Red, ManaCostShard::Red]))
        .id();
    sc.add_creature_from_oracle(P0, "Skirge Familiar", 3, 2, SKIRGE_FAMILIAR)
        .with_mana_cost(cost(4, &[ManaCostShard::Black]));
    sc.add_basic_land(P0, ManaColor::Red);
    sc.add_basic_land(P0, ManaColor::Red);
    match source {
        WindfallSource::Metalworker => {
            sc.add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER)
                .as_artifact_creature()
                .with_mana_cost(cost(3, &[]));
        }
        WindfallSource::Mountain => {
            sc.add_basic_land(P0, ManaColor::Red);
        }
    }
    if artifact_in_hand {
        sc.add_creature_to_hand(P0, "Ornithopter", 0, 2)
            .as_artifact_creature()
            .with_mana_cost(cost(0, &[]));
    } else {
        sc.add_land_to_hand(P0, "Island");
    }
    Board {
        runner: sc.build(),
        spell,
    }
}

/// What makes Unexpected Windfall's last two mana beside the two Mountains.
enum WindfallSource {
    Metalworker,
    Mountain,
}

/// T14. CR 601.2g: mana abilities are activated before costs are paid, so the
/// Ornithopter held for Unexpected Windfall's discard is still in hand when
/// Metalworker reveals it for {C}{C}: reading a card doesn't use it up, and the
/// cast is offered. Skirge Familiar's discard does use it up: with a third
/// Mountain in Metalworker's place the fourth mana needs that discard, and with
/// an Island in hand Metalworker has no artifact to reveal. Neither is offered.
///
/// Only the offer is asserted. The engine pays a chosen discard before its
/// mana-ability window, so the offered cast can't be completed yet. The
/// predicate is asserted only on the positive board, as in T5.
#[test]
fn a_held_card_is_still_revealed_for_mana() {
    let revealed = windfall_board(WindfallSource::Metalworker, true);
    assert!(castable(&revealed.runner, revealed.spell));
    assert!(offered(&revealed.runner, revealed.spell));

    let mut discarded = windfall_board(WindfallSource::Mountain, true);
    assert_not_castable(&mut discarded.runner, discarded.spell, "Skirge Familiar");

    let mut no_artifact = windfall_board(WindfallSource::Metalworker, false);
    assert_not_castable(&mut no_artifact.runner, no_artifact.spell, "an Island");
}
