//! Runtime cast-pipeline coverage for issue #9886, Dralnu's Pet:
//!
//! > Kicker—{2}{B}, Discard a creature card.
//! > If this creature was kicked, it enters with flying and with X +1/+1
//! > counters on it, where X is the discarded card's mana value.
//!
//! CR 614.1c makes the second line ONE replacement: the permanent enters with
//! flying AND with X +1/+1 counters. CR 107.3 + CR 107.3i: X is defined by the
//! "where X is" clause — the mana value (CR 202.3) of the creature card
//! discarded to pay the kicker (CR 702.33a, CR 608.2k), which an ability of
//! the permanent may reference (CR 400.7d).
//!
//! Three seams carry that value from the cost to the counters:
//!
//! * PARSER (2a) — the keyword-FIRST rider "enters with flying and with X
//!   +1/+1 counters" was an unsupported gap; the leading-rider splitter in
//!   `parser/oracle_replacement.rs` (`split_enters_with_leading_keyword_riders`)
//!   now reads it.
//! * DISCARD STAMP (2b-1) — `handle_discard_for_cost` (and its replacement-pause
//!   resume) publishes the discarded card onto the SPELL object's
//!   `cast_cost_paid_object` carrier (`stamp_spell_cast_cost_paid_object`), so
//!   the `CastLinkSnapshot` restores it onto the permanent.
//! * CONTINUATION STAMP (2b-2) — `engine_replacement::apply_post_replacement_effect`
//!   stamps that carrier onto the enters-with continuation of a battlefield
//!   `Moved` replacement whose chain reads the cost-paid object.
//! * FOLD GUARD (2b-3) — `EventModifiers::is_event_modifier_effect` keeps a
//!   self `PutCounter` whose count reads the cost-paid object off the event
//!   fold (which resolves counts with no `ResolvedAbility` and would read 0).
//!
//! Every test names its revert discriminator in its doc comment. All casts run
//! through the real pipeline (`CastSpell` → `DecideOptionalCost` → the discard
//! `PayCost` prompt → `PassPriority`), with the card's verbatim Oracle text —
//! Kicker line included — parsed by the production parser.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityKind, AdditionalCost, AdditionalCostRepeatability, CardSelectionMode,
    DiscardSelfScope, Effect, QuantityExpr, ReplacementDefinition, ReplacementMode, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Dralnu's Pet {1}{U}{U} 2/2, verbatim Oracle text (MTGJSON AtomicCards).
const DRALNUS_PET: &str = "Kicker—{2}{B}, Discard a creature card. (You may pay {2}{B} and discard a creature card in addition to any other costs as you cast this spell.)\nIf this creature was kicked, it enters with flying and with X +1/+1 counters on it, where X is the discarded card's mana value.";

/// Synthetic rider-less twin: the same kicker and the same where-X count, with
/// no keyword rider, so the counter placement is the WHOLE replacement chain —
/// the shape the event fold would otherwise claim.
const RIDERLESS_PET: &str = "Kicker—{2}{B}, Discard a creature card.\nIf this creature was kicked, it enters with X +1/+1 counters on it, where X is the discarded card's mana value.";

/// Synthetic twin with an activated discard cost, for the activation-gate test.
const PET_WITH_DISCARD_ABILITY: &str = "Kicker—{2}{B}, Discard a creature card.\nIf this creature was kicked, it enters with flying and with X +1/+1 counters on it, where X is the discarded card's mana value.\nDiscard a card: You gain 1 life.";

/// The board for one cast: the pet, its discard candidates, and the lands.
struct PetBoard {
    runner: GameRunner,
    pet: ObjectId,
    /// Creature card, mana value 2 (listed first among the candidates).
    small: ObjectId,
    /// Creature card, mana value 5.
    big: ObjectId,
    /// Noncreature card, mana value 3 — never a legal kicker discard.
    instant: ObjectId,
}

/// Seed P0's hand with `oracle` (as a {1}{U}{U} 2/2) plus two creature cards of
/// different mana values and one noncreature card, and enough lands to pay the
/// kicked cost. `with_discard_replacement` installs an optional discard
/// replacement so the cost's discard pauses for a replacement choice.
fn pet_board(name: &str, oracle: &str, with_discard_replacement: bool) -> PetBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let pet = scenario
        .add_creature_to_hand(P0, name, 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Kicker"], oracle)
        .id();
    let small = scenario
        .add_creature_to_hand(P0, "Small Discard Fodder", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let big = scenario
        .add_creature_to_hand(P0, "Big Discard Fodder", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let instant = scenario
        .add_spell_to_hand(P0, "Noncreature Discard Fodder", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        })
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    if with_discard_replacement {
        scenario
            .add_creature(P0, "Discard Replacement Source", 1, 1)
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::Discard)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("optional discard replacement".to_string()),
            );
    }

    PetBoard {
        runner: scenario.build(),
        pet,
        small,
        big,
        instant,
    }
}

/// CR 702.33a: the parsed kicker option is exactly {2}{B} plus discarding one
/// chosen creature card from hand — the production kicker parser built it.
fn assert_dralnu_kicker(board: &PetBoard) {
    assert_eq!(
        board.runner.state().objects[&board.pet].additional_cost,
        Some(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::Black],
                            generic: 2,
                        },
                    },
                    AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: Some(TargetFilter::Typed(TypedFilter::creature())),
                        selection: CardSelectionMode::Chosen,
                        self_scope: DiscardSelfScope::FromHand,
                    },
                ],
            }],
            repeatability: AdditionalCostRepeatability::Once,
        }),
        "the kicker must parse from the verbatim Oracle line"
    );
}

/// Reach guard: the card parsed with zero `Effect::Unimplemented`, so a
/// negative assertion cannot be satisfied by an upstream parse failure.
fn assert_parsed_cleanly(board: &PetBoard) {
    let pet = &board.runner.state().objects[&board.pet];
    assert!(
        !pet.abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "the card must parse with zero Effect::Unimplemented, got {:?}",
        pet.abilities
    );
    assert_eq!(
        pet.replacement_definitions.len(),
        1,
        "the enters-with line must be one replacement, got {:?}",
        pet.replacement_definitions
    );
}

/// Cast the pet, decide the kicker, answer the discard prompt with `discard`
/// (when kicked), and resolve until the pet is on the battlefield. Every
/// optional discard-replacement prompt is answered with its first option.
/// Returns how many replacement-choice prompts were answered.
fn cast_and_resolve(board: &mut PetBoard, discard: Option<ObjectId>) -> usize {
    let pet = board.pet;
    let runner = &mut board.runner;
    let card = runner.state().objects[&pet].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: pet,
            card_id: card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the pet");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "the engine must offer the kicker choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost {
            pay: discard.is_some(),
        })
        .expect("P0 decides the kicker");

    let mut replacement_choices = 0;
    let mut discarded = false;
    while runner.state().objects.get(&pet).map(|o| o.zone) != Some(Zone::Battlefield) {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                choices,
                ..
            } => {
                let chosen = discard.expect("an unkicked cast must not prompt a discard");
                assert!(!discarded, "the kicker discards exactly once");
                assert!(
                    choices.contains(&chosen),
                    "the chosen card must be a legal discard: {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("P0 discards the chosen creature card");
                discarded = true;
            }
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("P0 answers the discard replacement");
                replacement_choices += 1;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the pet");
            }
            other => panic!("unexpected waiting state while casting the pet: {other:?}"),
        }
    }
    assert_eq!(
        discarded,
        discard.is_some(),
        "the discard prompt is reached exactly when kicked"
    );
    replacement_choices
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// PRIMARY. Kicked, discarding the mana-value-5 creature card while a
/// mana-value-2 creature card (listed first) and a noncreature card are also in
/// hand: the pet enters with flying AND exactly five +1/+1 counters — the
/// CHOSEN card's mana value, not the pet's own (3), not the first candidate's
/// (2), and not the `{X}` paid (there is none).
///
/// REVERT DISCRIMINATORS: revert the leading splitter (2a) and the line is an
/// `Unimplemented` gap — `assert_parsed_cleanly` fails and neither flying nor
/// counters appear. Revert the discard stamp (2b-1) or the continuation stamp
/// (2b-2) and X reads 0, so the five-counter assertion fails while flying (the
/// reach guard that the replacement applied) still holds.
#[test]
fn dralnus_pet_kicked_enters_with_flying_and_the_chosen_cards_mana_value_in_counters() {
    let mut board = pet_board("Dralnu's Pet", DRALNUS_PET, false);
    assert_parsed_cleanly(&board);
    assert_dralnu_kicker(&board);

    let (big, small, instant) = (board.big, board.small, board.instant);
    cast_and_resolve(&mut board, Some(big));
    let runner = &board.runner;
    let pet = &runner.state().objects[&board.pet];

    assert!(
        pet.has_keyword(&Keyword::Flying),
        "CR 614.1c: the flying rider is part of the kicked replacement"
    );
    assert_eq!(
        plus_one_counters(runner, board.pet),
        5,
        "CR 107.3 + CR 202.3: X is the discarded card's mana value (5)"
    );
    assert_eq!(
        runner.state().objects[&big].zone,
        Zone::Graveyard,
        "the chosen creature card paid the kicker"
    );
    assert_eq!(
        runner.state().objects[&small].zone,
        Zone::Hand,
        "the other creature card was not discarded"
    );
    assert_eq!(runner.state().objects[&instant].zone, Zone::Hand);
}

/// Second mana value: discarding the mana-value-2 creature card gives exactly
/// two counters, so X follows the discarded card rather than a constant.
///
/// REVERT DISCRIMINATOR: as the primary test — revert 2b-1 or 2b-2 and the
/// count is 0.
#[test]
fn dralnus_pet_counter_count_follows_the_discarded_cards_mana_value() {
    let mut board = pet_board("Dralnu's Pet", DRALNUS_PET, false);
    let small = board.small;
    cast_and_resolve(&mut board, Some(small));
    let runner = &board.runner;

    assert!(
        runner.state().objects[&board.pet].has_keyword(&Keyword::Flying),
        "reach guard: the kicked replacement applied"
    );
    assert_eq!(plus_one_counters(runner, board.pet), 2);
    assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
}

/// Not kicked: neither half of the gated replacement applies (CR 702.33d).
/// Reach guard: the pet resolved onto the battlefield and every candidate
/// card stayed in hand, so the absence of flying and counters is a gate
/// decision, not a failed cast.
///
/// REVERT DISCRIMINATOR: removing the kicker gate shared by the whole
/// replacement grants flying here. The zero-counter assertion alone does not
/// establish the gate: with no kicker discard there is no cost-paid referent,
/// so an ungated placement would also resolve X to zero.
#[test]
fn dralnus_pet_unkicked_enters_without_flying_or_counters() {
    let mut board = pet_board("Dralnu's Pet", DRALNUS_PET, false);
    assert_parsed_cleanly(&board);
    cast_and_resolve(&mut board, None);
    let runner = &board.runner;

    assert_eq!(
        runner.state().objects[&board.pet].zone,
        Zone::Battlefield,
        "reach guard: the unkicked pet resolved"
    );
    assert!(!runner.state().objects[&board.pet].has_keyword(&Keyword::Flying));
    assert_eq!(plus_one_counters(runner, board.pet), 0);
    for card in [board.small, board.big, board.instant] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    }
}

/// Rider-less twin: with no keyword rider the counter placement is the whole
/// replacement chain, the shape the event fold would claim. It must still read
/// the discarded card's mana value.
///
/// REVERT DISCRIMINATOR: revert the fold guard (2b-3) and the placement folds
/// into the entry event, where the count resolves without the cost-paid
/// referent and reads 0 — the five-counter assertion fails. The discarded card
/// in the graveyard is the reach guard that the kicker was paid.
#[test]
fn riderless_cost_paid_counter_count_is_not_folded_to_zero() {
    let mut board = pet_board("Synthetic Riderless Pet", RIDERLESS_PET, false);
    assert_parsed_cleanly(&board);
    let big = board.big;
    cast_and_resolve(&mut board, Some(big));
    let runner = &board.runner;

    assert_eq!(runner.state().objects[&big].zone, Zone::Graveyard);
    assert_eq!(
        plus_one_counters(runner, board.pet),
        5,
        "the rider-less placement must read the discarded card's mana value"
    );
    assert!(!runner.state().objects[&board.pet].has_keyword(&Keyword::Flying));
}

/// The discard pauses for a replacement choice (CR 616.1) before it completes,
/// so `handle_discard_for_cost` returns early and the cost finishes in
/// `resume_interrupted_cost_payment`. The discarded card still reaches the
/// permanent.
///
/// REVERT DISCRIMINATOR: remove the stamp from the resumed discard suffix
/// (`resume_interrupted_cost_payment`) and the spell carrier is never stamped
/// on this path, so X reads 0 and the five-counter assertion fails. The
/// replacement-choice count is the reach guard that the pause path was taken.
#[test]
fn dralnus_pet_discard_paused_for_a_replacement_choice_still_sets_x() {
    let mut board = pet_board("Dralnu's Pet", DRALNUS_PET, true);
    let big = board.big;
    let choices = cast_and_resolve(&mut board, Some(big));
    let runner = &board.runner;

    assert!(
        choices >= 1,
        "reach guard: the discard must pause for the replacement choice"
    );
    assert_eq!(runner.state().objects[&big].zone, Zone::Graveyard);
    assert!(runner.state().objects[&board.pet].has_keyword(&Keyword::Flying));
    assert_eq!(plus_one_counters(runner, board.pet), 5);
}

/// CR 400.7d: the carrier records how the PERMANENT was cast. An activated
/// ability's discard cost runs through the same discard handler, but its
/// `object_id` is the source permanent, so it must not overwrite that record.
/// The original carrier is established positively by a real kicked cast (five
/// counters), then a completed activation (life gained, card discarded) leaves
/// it naming the kicker's discarded card.
///
/// REVERT DISCRIMINATOR: drop the spell-casts-only gate in
/// `stamp_spell_cast_cost_paid_object` and the activation's discarded card
/// overwrites the carrier, so the final carrier assertion fails.
#[test]
fn activated_discard_cost_does_not_overwrite_the_cast_carrier() {
    let mut board = pet_board("Synthetic Discard Pet", PET_WITH_DISCARD_ABILITY, false);
    assert_parsed_cleanly(&board);
    let (big, small) = (board.big, board.small);
    cast_and_resolve(&mut board, Some(big));
    let pet = board.pet;
    let runner = &mut board.runner;

    assert_eq!(
        plus_one_counters(runner, pet),
        5,
        "reach guard: the cast carrier was established by the kicked discard"
    );
    let carrier_before = runner.state().objects[&pet]
        .cast_cost_paid_object
        .as_ref()
        .map(|snapshot| snapshot.object_id);
    assert_eq!(carrier_before, Some(big));

    let ability_index = runner.state().objects[&pet]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("the discard ability is an activated ability");
    let life_before = runner.state().players[0].life;
    runner
        .act(GameAction::ActivateAbility {
            source_id: pet,
            ability_index,
        })
        .expect("P0 activates the discard ability");
    let mut discarded = false;
    while runner.state().players[0].life == life_before {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                choices,
                ..
            } => {
                assert!(choices.contains(&small), "{choices:?}");
                runner
                    .act(GameAction::SelectCards { cards: vec![small] })
                    .expect("P0 discards for the activation");
                discarded = true;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the activation");
            }
            other => panic!("unexpected waiting state while activating: {other:?}"),
        }
    }
    assert!(
        discarded,
        "reach guard: the activation paid its discard cost"
    );
    assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&pet]
            .cast_cost_paid_object
            .as_ref()
            .map(|snapshot| snapshot.object_id),
        Some(big),
        "the activation's discard must not overwrite the permanent's cast carrier"
    );
}
