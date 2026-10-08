//! Regression (#9213): a cast grant followed in its own sentence by ", and mana
//! of any type can be spent to cast …" / ", and you may spend mana as though it
//! were mana of any color to cast …" lost that concession. The conjunct is a
//! payment concession on the grant it follows (CR 609.4b; CR 118.14 for "any
//! type"), but it stayed glued to the grant's sentence, where either nothing
//! claimed the sentence (the whole of it became the standalone concession gap,
//! the grant with it) or the grant's grammar claimed it and dropped the
//! conjunct — as an `Unrecognized` part of a "for as long as" condition, or by
//! never reading past its own head.
//!
//! Fix: the clause splitter cuts the conjunct off the grant
//! (`starts_mana_spend_rider_conjunct`), also after a leading duration or
//! condition, so the chunk loop folds it onto the grant like the rider that
//! arrives as its own sentence. The tests drive the real resolution
//! (`GameScenario` / `GameRunner` / `GameAction`) and then cast the granted
//! {G} sorcery with a Swamp.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, ManaSpendPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

/// Mnemonic Betrayal: nothing claimed the sentence, so the grant was lost with
/// the concession.
const MNEMONIC_BETRAYAL: &str = "Exile all opponents' graveyards. You may cast spells from among \
those cards this turn, and mana of any type can be spent to cast them. At the beginning of the \
next end step, if any of those cards remain exiled, return them to their owners' graveyards.\n\
Exile Mnemonic Betrayal.";

/// Petty Larceny: the "for as long as" grammar read the conjunct as part of
/// its condition.
const PETTY_LARCENY: &str = "Freerunning {1}{B} (You may cast this spell for its freerunning cost \
if you dealt combat damage to a player this turn with an Assassin or commander.)\n\
Look at the top two cards of target opponent's library and exile those cards face down. You may \
play those cards for as long as they remain exiled, and mana of any type can be spent to cast \
them. Create a Treasure token.";

/// Lidless Gaze: behind a leading duration, the grant's grammar never read
/// past its own head.
const LIDLESS_GAZE: &str = "Exile the top card of each player's library. Until the end of your \
next turn, you may play those cards, and mana of any type can be spent to cast those spells.\n\
Flashback {2}{B}{R} (You may cast this card from your graveyard for its flashback cost. Then \
exile it.)";

/// Gonti, Lord of Luxury: the sentence used to be read whole by its own
/// recognizer (grant target `Any`); it now lowers like its rider-less
/// siblings (grant target `TrackedSet`).
const GONTI_LORD_OF_LUXURY: &str = "Deathtouch\n\
When Gonti enters, look at the top four cards of target opponent's library, exile one of them face \
down, then put the rest on the bottom of that library in a random order. You may cast that card \
for as long as it remains exiled, and mana of any type can be spent to cast that spell.";

fn green_sorcery_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    }
}

/// Answer the prompts a resolving grant calls for until the stack is empty:
/// target P1, keep `keep` from a dig. Any other prompt fails the test by name.
/// Returns the prompts answered, in order — the reach guard.
fn settle(runner: &mut GameRunner, keep: ObjectId) -> Vec<&'static str> {
    let mut prompts = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                prompts.push("TargetSelection");
                assert!(
                    target_slots[selection.current_slot]
                        .legal_targets
                        .contains(&TargetRef::Player(P1)),
                    "P1 is a legal target"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::DigChoice { cards, .. } => {
                prompts.push("DigChoice");
                assert!(cards.contains(&keep), "the dig offers the {{G}} sorcery");
                runner
                    .act(GameAction::SelectCards { cards: vec![keep] })
                    .expect("SelectCards accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return prompts;
                }
                prompts.push("Priority");
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while resolving the grant: {other:?}"),
        }
    }
    panic!("the grant never resolved back to an empty stack");
}

/// Cast `spell` from P0's hand for free and resolve it.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId, keep: ObjectId) -> Vec<&'static str> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    settle(runner, keep)
}

/// The concession every permission recorded on `card` carries. Exact: a
/// permission without one reads `None`, so a half-stamped card fails, and a
/// card with no permission at all yields an empty list.
fn recorded_concessions(runner: &GameRunner, card: ObjectId) -> Vec<Option<ManaSpendPermission>> {
    runner.state().objects[&card]
        .casting_permissions
        .iter()
        .map(|permission| match permission {
            CastingPermission::ExileWithAltCost {
                mana_spend_permission,
                ..
            }
            | CastingPermission::PlayFromExile {
                mana_spend_permission,
                ..
            } => *mana_spend_permission,
            other => panic!("unexpected permission recorded on the granted card: {other:?}"),
        })
        .collect()
}

/// Cast the granted {G} `card` from exile with P0's Swamps and let it resolve.
/// Returns how many of `swamps` ended tapped.
fn cast_granted_card(runner: &mut GameRunner, card: ObjectId, swamps: &[ObjectId]) -> usize {
    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card)
        ),
        "the granted card must be offered as a legal cast with Swamps"
    );
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the granted card is cast with Swamps");
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while the granted card resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "the granted sorcery resolved and went to its owner's graveyard"
    );
    swamps
        .iter()
        .filter(|swamp| runner.state().objects[swamp].tapped)
        .count()
}

/// Every permission on the exiled `green` carries the any-type concession, and
/// `green` is cast with exactly one Swamp.
fn assert_granted_with_any_type_mana(
    runner: &mut GameRunner,
    green: ObjectId,
    swamps: &[ObjectId],
) {
    assert_eq!(runner.state().objects[&green].zone, Zone::Exile);
    let concessions = recorded_concessions(runner, green);
    assert!(
        !concessions.is_empty()
            && concessions
                .iter()
                .all(|c| *c == Some(ManaSpendPermission::AnyTypeOrColor)),
        "every permission on the exiled card carries the any-type concession: {concessions:?}"
    );
    assert_eq!(
        cast_granted_card(runner, green, swamps),
        1,
        "exactly one Swamp paid for {{G}}"
    );
}

fn two_swamps(scenario: &mut GameScenario) -> [ObjectId; 2] {
    [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ]
}

/// First shape: the sentence no grant grammar claims — "You may cast spells
/// from among those cards this turn, and mana of any type can be spent to cast
/// them." Pre-fix the whole sentence was the standalone concession gap: the
/// exiled card carried no permission at all.
#[test]
fn unclaimed_grant_sentence_keeps_its_grant_and_its_concession() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let green = {
        let mut b = scenario.add_spell_to_graveyard(P1, "Green Sorcery", false);
        b.with_mana_cost(green_sorcery_cost());
        b.id()
    };
    let swamps = two_swamps(&mut scenario);
    let betrayal = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Mnemonic Betrayal",
            false,
            MNEMONIC_BETRAYAL,
        );
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let prompts = cast_and_resolve(&mut runner, betrayal, green);
    assert_eq!(
        prompts,
        vec!["Priority", "Priority"],
        "resolving asks nothing"
    );
    assert_granted_with_any_type_mana(&mut runner, green, &swamps);
}

/// Second shape, trailing duration: "You may play those cards for as long as
/// they remain exiled, and mana of any type can be spent to cast them." Pre-fix
/// the "for as long as" grammar read the conjunct as part of its condition and
/// the grant carried no concession.
#[test]
fn trailing_duration_does_not_swallow_the_concession() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "Filler");
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(green_sorcery_cost());
        b.id()
    };
    let swamps = two_swamps(&mut scenario);
    let larceny = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Petty Larceny", false, PETTY_LARCENY);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let prompts = cast_and_resolve(&mut runner, larceny, green);
    assert_eq!(
        prompts,
        vec!["Priority", "Priority"],
        "resolving asks nothing (the one opponent is the only legal target)"
    );
    assert!(runner.state().objects[&green].face_down, "exiled face down");
    assert_granted_with_any_type_mana(&mut runner, green, &swamps);
}

/// Second shape, leading duration: "Until the end of your next turn, you may
/// play those cards, and mana of any type can be spent to cast those spells."
/// Pre-fix the grant's grammar never read past its own head and the grant
/// carried no concession.
#[test]
fn leading_duration_grant_keeps_its_concession() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Own Filler");
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(green_sorcery_cost());
        b.id()
    };
    let swamps = two_swamps(&mut scenario);
    let gaze = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Lidless Gaze", false, LIDLESS_GAZE);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let prompts = cast_and_resolve(&mut runner, gaze, green);
    assert_eq!(
        prompts,
        vec!["Priority", "Priority"],
        "resolving asks nothing"
    );
    assert_granted_with_any_type_mana(&mut runner, green, &swamps);
}

/// "You may cast that card for as long as it remains exiled, and mana of any
/// type can be spent to cast that spell." after a dig that exiles one card.
/// A guard, not a regression drive (it passes on main too): the grant's
/// target changed shape (`Any` → `TrackedSet`), and the permission must still
/// reach the exiled card and not Gonti.
#[test]
fn dig_exiled_card_receives_the_grant_and_its_concession() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for filler in ["Filler A", "Filler B", "Filler C"] {
        scenario.add_card_to_library_top(P1, filler);
    }
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(green_sorcery_cost());
        b.id()
    };
    let swamps = two_swamps(&mut scenario);
    let gonti = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Gonti, Lord of Luxury",
            2,
            3,
            GONTI_LORD_OF_LUXURY,
        );
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let prompts = cast_and_resolve(&mut runner, gonti, green);
    assert_eq!(
        prompts.last(),
        Some(&"DigChoice"),
        "the enters trigger dug P1's library: {prompts:?}"
    );
    assert!(runner.state().objects[&green].face_down, "exiled face down");
    assert!(
        runner.state().objects[&gonti]
            .casting_permissions
            .is_empty(),
        "Gonti itself receives no permission"
    );
    assert_granted_with_any_type_mana(&mut runner, green, &swamps);
}
