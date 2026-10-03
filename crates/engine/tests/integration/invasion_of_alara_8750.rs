//! Issue #8750 — Invasion of Alara's enters trigger.
//!
//! Oracle (front face):
//!   When this Siege enters, exile cards from the top of your library until you
//!   exile two nonland cards with mana value 4 or less. You may cast one of
//!   those two cards without paying its mana cost. Put one of them into your
//!   hand. Then put the other cards exiled this way on the bottom of your
//!   library in a random order.
//!
//! The rulings settle the two edges this file pins: if neither card is cast,
//! the one not put into hand stays in exile (it is not one of "the other
//! cards"), and with only one card found, a declined cast puts that card into
//! hand.
//!
//! The trigger rides a creature here: the Siege's battle permanent adds a
//! protector choice that is not part of this ability.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, QuantityExpr, UntilCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ALARA_TRIGGER: &str = "When this creature enters, exile cards from the top of your \
library until you exile two nonland cards with mana value 4 or less. You may cast one of \
those two cards without paying its mana cost. Put one of them into your hand. Then put the \
other cards exiled this way on the bottom of your library in a random order.";

struct Board {
    runner: GameRunner,
    siege: ObjectId,
    /// Library cards, top first.
    land_one: ObjectId,
    hit_one: ObjectId,
    too_big: ObjectId,
    land_two: ObjectId,
    hit_two: Option<ObjectId>,
    unreached: ObjectId,
}

/// Library, top first: land, hit (MV 2), miss (MV 5), land, then either a
/// second hit (MV 1) above an unreached card (MV 1), or a last land, so the
/// loop runs to the bottom with one hit.
fn board(second_hit: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let siege = scenario
        .add_creature_to_hand_from_oracle(P0, "Alara Siege", 1, 1, ALARA_TRIGGER)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    // `add_*_library_top` puts each new card on top, so build bottom-up.
    let unreached = if second_hit {
        scenario
            .add_spell_to_library_top(P0, "Unreached", false)
            .with_mana_cost(ManaCost::generic(1))
            .from_oracle_text("You gain 1 life.")
            .id()
    } else {
        scenario.add_land_to_library_top(P0, "Bottom Land").id()
    };
    let hit_two = second_hit.then(|| {
        scenario
            .add_spell_to_library_top(P0, "Second Hit", true)
            .with_mana_cost(ManaCost::generic(1))
            .from_oracle_text("You gain 1 life.")
            .id()
    });
    let land_two = scenario.add_land_to_library_top(P0, "Land Two").id();
    let too_big = scenario
        .add_spell_to_library_top(P0, "Too Big", false)
        .with_mana_cost(ManaCost::generic(5))
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit_one = scenario
        .add_spell_to_library_top(P0, "First Hit", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let land_one = scenario.add_land_to_library_top(P0, "Land One").id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&siege].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: siege,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the creature must succeed");
    Board {
        runner,
        siege,
        land_one,
        hit_one,
        too_big,
        land_two,
        hit_two,
        unreached,
    }
}

/// Pass priority until the trigger asks something other than priority.
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving the trigger");
            }
            _ => return,
        }
    }
    panic!("the trigger never reached a choice");
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 608.2c: the head is the counted exile loop, so the parser has to keep
/// the "two" (a one-card head stops at the first hit).
#[test]
fn the_head_exiles_until_two_cards_match() {
    let b = board(true);
    let def = b.runner.state().objects[&b.siege].trigger_definitions[0]
        .definition
        .execute
        .clone()
        .expect("enters trigger body");
    let Effect::ExileFromTopUntil {
        until: UntilCondition::NextMatches { count, .. },
        ..
    } = def.effect.as_ref()
    else {
        panic!("expected a counted exile loop, got {:?}", def.effect);
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
}

/// CR 608.2g: one of the two found cards is cast as the trigger resolves; the
/// other goes to hand, the misses go to the bottom, the unreached card stays.
#[test]
fn casting_one_hit_puts_the_other_into_hand_and_bottoms_the_misses() {
    let mut b = board(true);
    let hit_two = b.hit_two.unwrap();
    advance_to_choice(&mut b.runner);
    match b.runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player: P0,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
        } => {
            assert_eq!(candidates, vec![b.hit_one, hit_two]);
            assert_eq!(remaining_casts, Some(1));
        }
        other => panic!("expected the one-card free-cast window, got {other:?}"),
    }
    b.runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(b.hit_one),
        })
        .expect("casting the first hit must succeed");
    assert_eq!(zone(&b.runner, b.hit_one), Zone::Stack);
    // The cast card left exile, so the other hit is the only card offered.
    match b.runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => assert_eq!(cards, vec![hit_two]),
        other => panic!("expected the choice of the card for hand, got {other:?}"),
    }
    b.runner
        .act(GameAction::SelectCards {
            cards: vec![hit_two],
        })
        .expect("choosing the other hit must succeed");
    b.runner.advance_until_stack_empty();

    assert_eq!(zone(&b.runner, hit_two), Zone::Hand);
    assert_eq!(zone(&b.runner, b.hit_one), Zone::Graveyard);
    assert_eq!(
        zone(&b.runner, b.siege),
        Zone::Battlefield,
        "\"one of them\" names a found card, never the permanent"
    );
    let library: Vec<ObjectId> = b.runner.state().players[0]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library.first(),
        Some(&b.unreached),
        "the unreached card stays on top"
    );
    for miss in [b.land_one, b.too_big, b.land_two] {
        assert_eq!(zone(&b.runner, miss), Zone::Library);
        assert!(
            library[1..].contains(&miss),
            "a miss goes under the unreached card"
        );
    }
}

/// Ruling: with two cards found and none cast, the one not put into hand
/// stays in exile.
#[test]
fn declining_the_cast_puts_one_hit_into_hand_and_leaves_the_other_in_exile() {
    let mut b = board(true);
    let hit_two = b.hit_two.unwrap();
    advance_to_choice(&mut b.runner);
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { .. },
            ..
        }
    ));
    b.runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the cast must succeed");
    advance_to_choice(&mut b.runner);
    match b.runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => {
            assert_eq!(cards, vec![b.hit_one, hit_two]);
        }
        other => panic!("expected the choice of the card for hand, got {other:?}"),
    }
    b.runner
        .act(GameAction::SelectCards {
            cards: vec![hit_two],
        })
        .expect("choosing the second hit must succeed");
    b.runner.advance_until_stack_empty();

    assert_eq!(zone(&b.runner, hit_two), Zone::Hand);
    assert_eq!(
        zone(&b.runner, b.hit_one),
        Zone::Exile,
        "the other hit stays in exile"
    );
    assert_eq!(zone(&b.runner, b.siege), Zone::Battlefield);
    for miss in [b.land_one, b.too_big, b.land_two] {
        assert_eq!(zone(&b.runner, miss), Zone::Library);
    }
}

/// Ruling: with only one card found, a declined cast puts it into hand.
#[test]
fn a_single_hit_that_is_not_cast_goes_to_hand() {
    let mut b = board(false);
    advance_to_choice(&mut b.runner);
    b.runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the cast must succeed");
    advance_to_choice(&mut b.runner);
    match b.runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => assert_eq!(cards, vec![b.hit_one]),
        other => panic!("expected the choice of the card for hand, got {other:?}"),
    }
    b.runner
        .act(GameAction::SelectCards {
            cards: vec![b.hit_one],
        })
        .expect("the only hit must be choosable");
    b.runner.advance_until_stack_empty();

    assert_eq!(zone(&b.runner, b.hit_one), Zone::Hand);
    for miss in [b.land_one, b.too_big, b.land_two, b.unreached] {
        assert_eq!(zone(&b.runner, miss), Zone::Library);
    }
}

/// With only one card found and that card cast, no card is left for "put one
/// of them into your hand"; the misses still go to the bottom.
#[test]
fn a_single_hit_that_is_cast_leaves_nothing_for_hand() {
    let mut b = board(false);
    advance_to_choice(&mut b.runner);
    b.runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(b.hit_one),
        })
        .expect("casting the only hit must succeed");
    assert_eq!(zone(&b.runner, b.hit_one), Zone::Stack);
    assert!(
        matches!(b.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no card is left to choose for hand: {:?}",
        b.runner.state().waiting_for
    );
    b.runner.advance_until_stack_empty();

    assert_eq!(zone(&b.runner, b.hit_one), Zone::Graveyard);
    for miss in [b.land_one, b.too_big, b.land_two, b.unreached] {
        assert_eq!(zone(&b.runner, miss), Zone::Library);
    }
}

/// CR 400.7j + CR 608.2c: a triggered "put the rest on the bottom" after an
/// exile-until loop bottoms the cards that loop exiled. The trigger's
/// linked-exile snapshot is taken when it triggers, before the loop exiled
/// anything; the loop's own batch is the authority. Jodah, the Unifier's
/// trigger has this shape; a creature's enters trigger stands in for its
/// spell-cast trigger.
#[test]
fn a_triggered_rest_cleanup_bottoms_the_cards_its_own_loop_exiled() {
    const REST_TRIGGER: &str = "When this creature enters, exile cards from the top of your \
library until you exile a nonland card. You may cast that card without paying its mana cost. \
Put the rest on the bottom of your library in a random order.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Rest Source", 1, 1, REST_TRIGGER)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Hit", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let miss = scenario.add_land_to_library_top(P0, "Miss").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&source].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: source,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the creature must succeed");
    advance_to_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the cast offer, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the cast must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library,
        vec![unreached, miss],
        "the miss goes to the bottom"
    );
    assert_eq!(
        zone(&runner, hit),
        Zone::Exile,
        "a declined hit stays in exile"
    );
}

/// CR 400.7j + CR 608.2c: Gríma, Saruman's Footman (#7758) — the same batch
/// reaches a triggered cleanup over the damaged player's library. A declined
/// hit "wasn't cast this way", so it goes to the bottom with the misses.
#[test]
fn grima_bottoms_the_exiled_cards_that_were_not_cast() {
    const GRIMA: &str = "Gríma can't be blocked.\nWhenever Gríma deals combat damage to a \
player, that player exiles cards from the top of their library until they exile an instant or \
sorcery card. You may cast that card without paying its mana cost. Then that player puts the \
exiled cards that weren't cast this way on the bottom of their library in a random order.";
    let p1 = engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grima = scenario
        .add_creature_from_oracle(P0, "Gríma, Saruman's Footman", 2, 2, GRIMA)
        .id();
    let unreached = scenario
        .add_spell_to_library_top(p1, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit = scenario
        .add_spell_to_library_top(p1, "Hit", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let miss_one = scenario.add_land_to_library_top(p1, "Miss One").id();
    let miss_two = scenario.add_land_to_library_top(p1, "Miss Two").id();
    let mut runner = scenario.build();
    super::rules::run_combat(&mut runner, vec![grima], vec![]);
    advance_to_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the cast offer, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the cast must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[1].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&unreached),
        "the unreached card stays on top"
    );
    for card in [hit, miss_one, miss_two] {
        assert_eq!(zone(&runner, card), Zone::Library);
        assert!(
            library[1..].contains(&card),
            "goes under the unreached card"
        );
    }
}

/// CR 608.2d: "put one of them into your hand" names the found cards, not the
/// ones the cast window could offer. A found card with no legal target is not
/// castable, so the window leaves it out, yet it is still offered for hand.
#[test]
fn an_uncastable_found_card_is_still_offered_for_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let siege = scenario
        .add_creature_to_hand_from_oracle(P0, "Alara Siege", 1, 1, ALARA_TRIGGER)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let no_target = scenario
        .add_spell_to_library_top(P0, "No Target", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Destroy target artifact.")
        .id();
    let castable = scenario
        .add_spell_to_library_top(P0, "Castable", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&siege].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: siege,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the creature must succeed");
    advance_to_choice(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => assert_eq!(candidates, vec![castable]),
        other => panic!("expected the free-cast window, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the cast must succeed");
    advance_to_choice(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, .. } => {
            assert_eq!(cards, vec![castable, no_target]);
        }
        other => panic!("expected the choice of the card for hand, got {other:?}"),
    }
}

const CODIE: &str = "You can't cast permanent spells.\n{4}, {T}: Add {W}{U}{B}{R}{G}. When \
you next cast a spell this turn, exile cards from the top of your library until you exile an \
instant or sorcery card with lesser mana value. Until end of turn, you may cast that card \
without paying its mana cost. Put each other card exiled this way on the bottom of your library \
in a random order.";

/// CR 603.7a + CR 608.2c: Codie's loop sits in a delayed trigger, and its
/// filter compares against the spell that triggered it. "Each other card
/// exiled this way" is still everything but the found card: an instant whose
/// mana value is too high is a miss and goes to the bottom with the lands.
#[test]
fn codie_bottoms_every_card_but_the_found_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let codie = scenario
        .add_creature_from_oracle(P0, "Codie", 1, 4, CODIE)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Three Drop", false)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Small Instant", true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let too_big = scenario
        .add_spell_to_library_top(P0, "Big Instant", true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let land = scenario.add_land_to_library_top(P0, "Miss Land").id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: codie,
            ability_index: 0,
        })
        .expect("activating Codie must succeed");
    runner.advance_until_stack_empty();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell with Codie's mana must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone(&runner, hit),
        Zone::Exile,
        "the found card stays in exile"
    );
    assert!(
        !runner.state().objects[&hit].casting_permissions.is_empty(),
        "reach guard: the found card carries its cast permission"
    );
    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(library.first(), Some(&unreached));
    for miss in [land, too_big] {
        assert!(library[1..].contains(&miss), "a miss goes to the bottom");
    }
}

const POSSIBILITY_STORM: &str = "Whenever a player casts a spell from their hand, that player \
exiles it, then exiles cards from the top of their library until they exile a card that shares \
a card type with it. That player may cast that card without paying its mana cost. Then they put \
all cards exiled with this enchantment on the bottom of their library in a random order.";

/// CR 607.2a: "all cards exiled with this enchantment" includes the spell the
/// trigger exiled before its loop, not only the cards the loop exiled.
#[test]
fn possibility_storm_bottoms_the_exiled_spell_too() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    scenario.add_enchantment_from_oracle(P0, "Possibility Storm", POSSIBILITY_STORM);
    let spell = scenario
        .add_spell_to_hand(P0, "Cast Sorcery", false)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Hit Sorcery", false)
        .from_oracle_text("You gain 1 life.")
        .id();
    let miss = scenario.add_land_to_library_top(P0, "Miss Land").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must succeed");
    advance_to_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the cast offer, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the cast must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&unreached),
        "the unreached card stays on top"
    );
    for card in [spell, hit, miss] {
        assert!(library[1..].contains(&card), "goes to the bottom");
    }
}

/// CR 608.2c: Ryan Sinclair's "the exiled cards not cast this way" includes the
/// found card when it is not cast.
#[test]
fn ryan_sinclair_bottoms_a_found_card_that_was_not_cast() {
    const RYAN: &str = "Whenever Ryan attacks, exile cards from the top of your library until \
you exile a nonland card. You may cast the exiled card without paying its mana cost if it's a \
spell with mana value less than or equal to Ryan's power. Put the exiled cards not cast this way \
on the bottom of your library in a random order.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ryan = scenario
        .add_creature_from_oracle(P0, "Ryan Sinclair", 1, 1, RYAN)
        .id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Hit", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let miss = scenario.add_land_to_library_top(P0, "Miss Land").id();
    let mut runner = scenario.build();
    super::rules::run_combat(&mut runner, vec![ryan], vec![]);
    advance_to_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: the cast offer, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the cast must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&unreached),
        "the unreached card stays on top"
    );
    for card in [hit, miss] {
        assert!(library[1..].contains(&card), "goes to the bottom");
    }
}

const COLLECTED_CONJURING: &str = "Exile the top six cards of your library. You may cast up to \
two sorcery spells with mana value 3 or less from among them without paying their mana costs. \
Put the exiled cards not cast this way on the bottom of your library in a random order.";

/// Collected Conjuring with its top six cards exiled; the sixth (top) card is
/// a castable sorcery.
fn collected_conjuring() -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Collected Conjuring", false, COLLECTED_CONJURING)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let mut lands = Vec::new();
    for index in 0..5 {
        lands.push(
            scenario
                .add_land_to_library_top(P0, &format!("Land {index}"))
                .id(),
        );
    }
    let sorcery = scenario
        .add_spell_to_library_top(P0, "Free Sorcery", false)
        .from_oracle_text("You gain 1 life.")
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Collected Conjuring must succeed");
    advance_to_choice(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "reach guard: the cast window, got {:?}",
        runner.state().waiting_for
    );
    (runner, unreached, lands, sorcery)
}

/// CR 608.2c: "the exiled cards not cast this way" is every card the spell
/// exiled that was not cast. On main its tracked-set reading moved none.
#[test]
fn collected_conjuring_bottoms_the_exiled_cards_not_cast() {
    let (mut runner, unreached, lands, sorcery) = collected_conjuring();
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the cast must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(library.first(), Some(&unreached));
    for card in lands.iter().copied().chain([sorcery]) {
        assert!(library[1..].contains(&card), "goes to the bottom");
    }
}

/// The cast sorcery is not "not cast this way": it resolves into the graveyard
/// while the rest goes to the bottom.
#[test]
fn collected_conjuring_keeps_the_cast_sorcery_out_of_the_library() {
    let (mut runner, unreached, lands, sorcery) = collected_conjuring();
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(sorcery),
        })
        .expect("casting the free sorcery must succeed");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(library.first(), Some(&unreached));
    for land in lands {
        assert!(library[1..].contains(&land), "a land goes to the bottom");
    }
    assert_eq!(zone(&runner, sorcery), Zone::Graveyard);
}

/// CR 607.2a: with no card sharing a type, the loop exiles the whole library
/// and the cleanup still bottoms everything, the exiled spell included.
#[test]
fn possibility_storm_without_a_match_bottoms_the_exiled_spell_too() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let storm = scenario
        .add_enchantment_from_oracle(P0, "Possibility Storm", POSSIBILITY_STORM)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Cast Sorcery", false)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let instant = scenario
        .add_spell_to_library_top(P0, "No Shared Type", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let land = scenario.add_land_to_library_top(P0, "Miss Land").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must succeed");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == storm),
        "reach guard: the Possibility Storm trigger is on the stack"
    );
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    for card in [spell, instant, land] {
        assert!(library.contains(&card), "goes to the bottom");
    }
}

const X_LOOP: &str =
    "{X}, {T}: Exile cards from the top of your library until you exile X nonland cards.";

/// Library, top first: land, two nonland cards, an unreached nonland card.
/// Activates the X loop with `x` announced and returns each card's zone.
fn x_loop(x: u32) -> [Zone; 4] {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..x)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let source = scenario.add_artifact_from_oracle(P0, "X Loop", X_LOOP).id();
    let unreached = scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First", true)
        .from_oracle_text("You gain 1 life.")
        .id();
    let land = scenario.add_land_to_library_top(P0, "Miss Land").id();
    let mut runner = scenario.build();
    let waiting = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activating the X loop must succeed");
    assert!(
        matches!(waiting.waiting_for, WaitingFor::ChooseXValue { .. }),
        "reach guard: X is announced: {:?}",
        waiting.waiting_for
    );
    runner
        .act(GameAction::ChooseX { value: x })
        .expect("announcing X must succeed");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("paying X must succeed");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the activation is on the stack"
    );
    runner.advance_until_stack_empty();
    [land, first, second, unreached].map(|id| zone(&runner, id))
}

/// CR 107.3a + CR 608.2c: "until you exile X nonland cards" counts the X the
/// controller announced while activating the ability.
#[test]
fn an_announced_x_sets_how_many_matches_end_the_loop() {
    assert_eq!(
        x_loop(2),
        [Zone::Exile, Zone::Exile, Zone::Exile, Zone::Library]
    );
}

/// CR 107.3a + CR 608.2c: X = 0 is met before the first card moves.
#[test]
fn an_announced_x_of_zero_exiles_nothing() {
    assert_eq!(x_loop(0), [Zone::Library; 4]);
}
