//! Issue #9334 (rest), #9018 — "you may cast a <type> spell from <a graveyard>
//! this turn": a resolution-created permission for ONE cast.
//!
//! Chainer, Nightmare Adept; The Tomb of Aclazotz; Whispersteel Dagger. Each
//! lowered to an `Effect::CastFromZone` naming a class and no card, which
//! `cast_from_zone::resolve` cannot cast, so the ability granted nothing.
//!
//! CR 601.3: "A player can begin to cast a spell only if a rule or effect
//! allows that player to cast it and no rule or effect prohibits that player
//! from casting it." CR 611.2c: the permission modifies no object's
//! characteristics, so it also covers cards that reach the graveyard after it
//! began. The article ("a creature spell") makes it one cast.
//!
//! Cards are built from their verbatim Oracle text
//! (`client/public/card-data.json`); CI has no card database.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{AlternativeCastKeyword, CastPaymentMode, WaitingFor};
use engine::types::keywords::{BlitzCost, Keyword};
use engine::types::mana::{ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHAINER: &str = "Discard a card: You may cast a creature spell from your graveyard this turn. Activate only once each turn.\nWhenever a nontoken creature you control enters, if you didn't cast it from your hand, it gains haste until your next turn.";

const TOMB_OF_ACLAZOTZ: &str = "(Transforms from Tarrian's Journal.)\n{T}: Add {B}.\n{T}: You may cast a creature spell from your graveyard this turn. If you do, it enters with a finality counter on it and is a Vampire in addition to its other types. (If a creature with a finality counter on it would die, exile it instead.)";

const WHISPERSTEEL_DAGGER: &str = "Equipped creature gets +2/+0.\nWhenever equipped creature deals combat damage to a player, you may cast a creature spell from that player's graveyard this turn, and you may spend mana as though it were mana of any color to cast that spell.\nEquip {3}";

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    (0..count)
        .map(|_| ManaUnit::new(kind, dummy, false, vec![]))
        .collect()
}

fn colorless(count: usize) -> Vec<ManaUnit> {
    mana(ManaType::Colorless, count)
}

fn black(count: usize) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black; count],
        generic: 0,
    }
}

fn one_generic() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 1,
    }
}

/// Chainer's grant admits a creature card in its controller's graveyard, and
/// only one: after the first cast the second creature card is no longer
/// castable. A noncreature card and a creature card in the opponent's
/// graveyard are never admitted.
#[test]
fn chainer_grants_one_creature_cast_from_own_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chainer = scenario
        .add_creature(P0, "Chainer, Nightmare Adept", 3, 2)
        .from_oracle_text(CHAINER)
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Fodder");
    let first = scenario
        .add_creature_to_graveyard(P0, "First Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Second Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    let own_instant = scenario
        .add_spell_to_graveyard(P0, "Grave Instant", true)
        .with_mana_cost(one_generic())
        .id();
    let opponents_creature = scenario
        .add_creature_to_graveyard(P1, "Opponent's Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    scenario.with_mana_pool(P0, colorless(3));
    let mut runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, first),
        "no graveyard creature is castable before Chainer's ability resolves"
    );

    runner.activate(chainer, 0).pay_with(&[fodder]).resolve();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&first) && castable.contains(&second),
        "both creature cards in P0's graveyard are offered: {castable:?}"
    );
    assert!(
        !castable.contains(&own_instant),
        "a noncreature card is not a creature spell"
    );
    assert!(
        !castable.contains(&opponents_creature),
        "\"your graveyard\" is P0's own (CR 404.1)"
    );

    let outcome = runner.cast(first).resolve();
    assert_eq!(outcome.zone_of(first), Zone::Battlefield);
    assert!(
        !can_cast_object_now(runner.state(), P0, second),
        "\"a creature spell\" is one cast: the grant is used up"
    );
}

/// The Tomb of Aclazotz: the creature cast through the grant enters with a
/// finality counter and is a Vampire in addition to its other types; the grant
/// allows one cast.
#[test]
fn tomb_of_aclazotz_cast_enters_with_finality_as_a_vampire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let first = scenario
        .add_creature_to_graveyard(P0, "First Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Second Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();

    runner.activate(tomb, 1).resolve();
    assert!(
        can_cast_object_now(runner.state(), P0, first),
        "the Tomb's grant admits a creature card in P0's graveyard"
    );

    let outcome = runner.cast(first).resolve();
    assert_eq!(outcome.zone_of(first), Zone::Battlefield);
    assert_eq!(
        outcome.counters(first, CounterType::Finality),
        1,
        "\"it enters with a finality counter on it\""
    );
    assert!(
        runner.state().objects[&first]
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Vampire"),
        "\"is a Vampire in addition to its other types\""
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, second),
        "the grant is one cast"
    );
}

/// CR 611.2f: the Tomb's "is a Vampire in addition to its other types" applies
/// while the spell is cast, so mana spendable only on Vampire creature spells
/// (Cavern of Souls naming Vampire) pays for a non-Vampire creature cast
/// through the grant, also pinned in the interactive payment step; the same
/// card in hand, not cast through it, can't use it.
#[test]
fn tomb_rider_makes_the_spell_a_vampire_for_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let from_graveyard = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .with_mana_cost(black(1))
        .id();
    let in_hand = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(black(1))
        .id();
    let cavern_mana = ManaUnit::new(
        ManaType::Black,
        engine::types::identifiers::ObjectId(0),
        false,
        vec![ManaRestriction::OnlyForCreatureType("Vampire".to_string())],
    );
    scenario.with_mana_pool(P0, vec![cavern_mana]);
    let mut runner = scenario.build();

    runner.activate(tomb, 1).resolve();
    assert!(
        !can_cast_object_now(runner.state(), P0, in_hand),
        "Vampire-only mana can't pay for a non-Vampire spell"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, from_graveyard),
        "cast through the Tomb, the spell is a Vampire as it is paid for"
    );
    let card_id = runner.state().objects[&from_graveyard].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: from_graveyard,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the graveyard cast starts");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "manual payment opens the payment step, got {:?}",
        runner.state().waiting_for
    );
    let pip_id = runner.state().players[0]
        .mana_pool
        .unit_at(0)
        .unwrap()
        .pip_id;
    runner
        .act(GameAction::SpendPoolMana { pip_id })
        .expect("Vampire-only mana may be pinned to the Vampire spell");
    runner
        .act(GameAction::PassPriority)
        .expect("Vampire-only mana pays for the Vampire spell");
    assert_eq!(runner.state().objects[&from_graveyard].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// CR 611.2f + CR 601.2f: the spell is already a Vampire as its total cost is
/// determined, so "Vampire spells you cast cost {1} less to cast" reduces a
/// non-Vampire creature card cast through the Tomb; the same card in hand pays
/// full price.
#[test]
fn tomb_rider_makes_the_spell_a_vampire_for_cost_reductions() {
    let two_drop = ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    scenario.add_artifact_from_oracle(
        P0,
        "Vampire Reducer",
        "Vampire spells you cast cost {1} less to cast.",
    );
    let from_graveyard = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .with_mana_cost(two_drop.clone())
        .id();
    let in_hand = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(two_drop)
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let mut runner = scenario.build();

    runner.activate(tomb, 1).resolve();
    assert!(
        !can_cast_object_now(runner.state(), P0, in_hand),
        "a non-Vampire spell costs its full {{1}}{{B}}"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, from_graveyard),
        "cast through the Tomb, the Vampire spell costs {{B}}"
    );
    let outcome = runner.cast(from_graveyard).resolve();
    assert_eq!(outcome.zone_of(from_graveyard), Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// A creature cast from hand while the Tomb's grant stands is neither a
/// Vampire nor given a finality counter: the rider rides the grant, so only the
/// spell cast through it — the graveyard creature cast next — receives it.
#[test]
fn tomb_rider_does_not_reach_a_cast_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let from_hand = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    let from_graveyard = scenario
        .add_creature_to_graveyard(P0, "Grave Bear", 2, 2)
        .with_mana_cost(one_generic())
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();
    let is_vampire = |runner: &engine::game::scenario::GameRunner, id| {
        runner.state().objects[&id]
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Vampire")
    };

    runner.activate(tomb, 1).resolve();
    let outcome = runner.cast(from_hand).resolve();
    assert_eq!(outcome.zone_of(from_hand), Zone::Battlefield);
    assert_eq!(outcome.counters(from_hand, CounterType::Finality), 0);
    assert!(!is_vampire(&runner, from_hand));

    let outcome = runner.cast(from_graveyard).resolve();
    assert_eq!(outcome.zone_of(from_graveyard), Zone::Battlefield);
    assert_eq!(outcome.counters(from_graveyard, CounterType::Finality), 1);
    assert!(is_vampire(&runner, from_graveyard));
}

/// The board after an attacker carrying Whispersteel Dagger dealt combat damage
/// to P1, in P0's postcombat main phase with three red mana — enough to pay for
/// each {B} card in P1's graveyard under the concession, so mana never limits
/// how many are cast; whether a grant still stands decides it.
struct DaggerHit {
    runner: engine::game::scenario::GameRunner,
    /// Creature cards ({B}) in P1's graveyard.
    theirs: [engine::types::identifiers::ObjectId; 3],
    /// A creature card ({B}) in P0's own graveyard.
    mine: engine::types::identifiers::ObjectId,
    /// A creature card ({B}) in P0's hand.
    in_hand: engine::types::identifiers::ObjectId,
}

fn dagger_hits_p1(double_strike: bool) -> DaggerHit {
    let black_one_drop = black(1);
    let mut scenario = GameScenario::new();
    let theirs = ["Their Bear", "Their Second Bear", "Their Third Bear"].map(|name| {
        scenario
            .add_creature_to_graveyard(P1, name, 2, 2)
            .with_mana_cost(black_one_drop.clone())
            .id()
    });
    let mine = scenario
        .add_creature_to_graveyard(P0, "My Bear", 2, 2)
        .with_mana_cost(black_one_drop.clone())
        .id();
    let in_hand = scenario
        .add_creature_to_hand(P0, "Hand Bear", 2, 2)
        .with_mana_cost(black_one_drop)
        .id();
    let mut runner = dagger_strikes_p1(scenario, double_strike);
    for unit in mana(ManaType::Red, 3) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }
    DaggerHit {
        runner,
        theirs,
        mine,
        in_hand,
    }
}

/// Adds an attacker carrying Whispersteel Dagger to `scenario`, has it deal
/// combat damage to P1, and stops in P0's postcombat main phase with an empty
/// mana pool.
fn dagger_strikes_p1(
    mut scenario: GameScenario,
    double_strike: bool,
) -> engine::game::scenario::GameRunner {
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = {
        let mut builder = scenario.add_creature(P0, "Attacker", 1, 1);
        if double_strike {
            builder.double_strike();
        }
        builder.id()
    };
    let dagger = scenario
        .add_artifact_from_oracle(P0, "Whispersteel Dagger", WHISPERSTEEL_DAGGER)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), dagger, attacker);
    assert_eq!(
        runner.state().objects[&dagger].attached_to,
        Some(AttachTarget::Object(attacker)),
        "the Dagger is attached to the attacker"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    runner.combat_damage();
    runner.advance_to_phase(Phase::PostCombatMain);
    runner.state_mut().players[0].mana_pool.clear();
    runner
}

/// Whispersteel Dagger: after the equipped creature deals combat damage to P1,
/// one creature card in P1's graveyard is castable, paid with off-color mana;
/// P0's own creature card is not, and nor is a second card after the cast.
#[test]
fn whispersteel_dagger_grants_one_cast_from_the_damaged_players_graveyard() {
    let DaggerHit {
        mut runner,
        theirs,
        mine,
        ..
    } = dagger_hits_p1(false);
    assert_eq!(runner.state().players[1].life, 17, "3 combat damage to P1");

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&theirs[0]),
        "a creature card in the damaged player's graveyard is castable: {castable:?}"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, theirs[0]),
        "red mana prices {{B}} under the grant's concession"
    );
    assert!(
        !castable.contains(&mine),
        "\"that player's graveyard\" is P1's (CR 400.3), not P0's own"
    );

    assert!(
        can_cast_object_now(runner.state(), P0, theirs[1]),
        "before the cast, a second creature card is castable too"
    );

    // CR 609.4b: red mana pays the {B} of the spell cast through the grant.
    let outcome = runner.cast(theirs[0]).resolve();
    assert_eq!(outcome.zone_of(theirs[0]), Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&theirs[0]].controller,
        P0,
        "the caster controls the spell and the permanent it becomes"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, theirs[1]),
        "\"a creature spell\" is one cast"
    );
}

/// The any-color concession belongs to the grant: with the same red mana, the
/// {B} card in P1's graveyard is castable and the {B} card in P0's hand is not,
/// until P0 has {B}.
#[test]
fn whispersteel_mana_concession_does_not_reach_a_cast_from_hand() {
    let DaggerHit {
        mut runner,
        theirs,
        in_hand,
        ..
    } = dagger_hits_p1(false);
    assert!(
        can_cast_object_now(runner.state(), P0, theirs[0]),
        "red mana pays {{B}} for the spell cast through the grant"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, in_hand),
        "red mana cannot pay {{B}} for a spell not cast through the grant"
    );
    for unit in mana(ManaType::Black, 1) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }
    assert!(
        can_cast_object_now(runner.state(), P0, in_hand),
        "with {{B}} in the pool the hand card is castable"
    );
}

/// Double strike deals combat damage twice, so the ability triggers twice and
/// each resolution grants its own cast: two creature cards, not three.
#[test]
fn each_dagger_trigger_grants_its_own_cast() {
    let DaggerHit {
        mut runner, theirs, ..
    } = dagger_hits_p1(true);
    assert_eq!(runner.state().players[1].life, 14, "two hits of 3");

    // CR 601.2a: two grants admit the card, so the cast announces which one it
    // uses; each is its own permission.
    let card_id = runner.state().objects[&theirs[0]].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: theirs[0],
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the graveyard cast starts")
        .waiting_for;
    let WaitingFor::CastingVariantChoice { options, .. } = waiting else {
        panic!("two grants ask which one the cast uses, got {waiting:?}");
    };
    assert_eq!(options.len(), 2, "one option per trigger's grant");
    runner
        .act(GameAction::ChooseCastingVariant { index: 0 })
        .expect("choosing an offered grant is legal");
    runner.resolve_top();
    assert_eq!(runner.state().objects[&theirs[0]].zone, Zone::Battlefield);

    let outcome = runner.cast(theirs[1]).resolve();
    assert_eq!(
        outcome.zone_of(theirs[1]),
        Zone::Battlefield,
        "the second grant still stands"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "one red mana is left for a third {{B}} card"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, theirs[2]),
        "two triggers grant two casts"
    );
}

/// CR 514.2: an unused grant ends with the turn ("this turn").
#[test]
fn unused_chainer_grant_ends_with_the_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chainer = scenario
        .add_creature(P0, "Chainer, Nightmare Adept", 3, 2)
        .from_oracle_text(CHAINER)
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Fodder");
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest"]);
    scenario
        .add_creature_to_graveyard(P0, "Grave Bear", 2, 2)
        .with_mana_cost(one_generic());
    let mut runner = scenario.build();

    runner.activate(chainer, 0).pay_with(&[fodder]).resolve();
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| effect.source_id == chainer),
        "the grant stands this turn"
    );
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    let turn = runner.state().turn_number;
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().turn_number, turn + 1, "the next turn began");
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| effect.source_id == chainer),
        "the grant ended at cleanup"
    );
}

/// The Dagger's cast paid through the interactive mana-payment step (rather
/// than at announcement) is priced under the grant's concession too.
#[test]
fn whispersteel_concession_holds_for_manual_payment() {
    let DaggerHit {
        mut runner, theirs, ..
    } = dagger_hits_p1(false);
    let card_id = runner.state().objects[&theirs[0]].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: theirs[0],
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the graveyard cast starts");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "manual payment opens the payment step, got {:?}",
        runner.state().waiting_for
    );
    // CR 118.3a: pinning a pool unit is judged while the cast is still pending.
    let pip_id = runner.state().players[0]
        .mana_pool
        .unit_at(0)
        .unwrap()
        .pip_id;
    runner
        .act(GameAction::SpendPoolMana { pip_id })
        .expect("red mana may be pinned to {B} under the grant");
    runner
        .act(GameAction::PassPriority)
        .expect("red mana pays {B} under the grant");
    assert_eq!(runner.state().objects[&theirs[0]].zone, Zone::Stack);
}

/// The grant's concession prices the card's own alternative cost too: with
/// three red mana, a card in P1's graveyard whose printed cost is out of reach
/// is cast for its Blitz {B}{B} (CR 702.152a), and the same card in P0's hand,
/// not cast through the grant, is not castable.
#[test]
fn whispersteel_concession_prices_a_graveyard_blitz() {
    let blitz_card = |builder: &mut engine::game::scenario::CardBuilder<'_>| {
        builder
            .with_mana_cost(black(9))
            .with_keyword(Keyword::Blitz(BlitzCost::Mana(black(2))));
    };
    let mut scenario = GameScenario::new();
    let theirs = {
        let mut builder = scenario.add_creature_to_graveyard(P1, "Their Blitzer", 2, 2);
        blitz_card(&mut builder);
        builder.id()
    };
    let in_hand = {
        let mut builder = scenario.add_creature_to_hand(P0, "Hand Blitzer", 2, 2);
        blitz_card(&mut builder);
        builder.id()
    };
    let mut runner = dagger_strikes_p1(scenario, false);
    for unit in mana(ManaType::Red, 3) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    assert!(
        !can_cast_object_now(runner.state(), P0, in_hand),
        "red mana cannot pay blitz {{B}}{{B}} for a spell not cast through the grant"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, theirs),
        "red mana prices blitz {{B}}{{B}} under the grant's concession"
    );
    let card_id = runner.state().objects[&theirs].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: theirs,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the blitz cast is accepted as offered");
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "blitz {{B}}{{B}} = 2 of the 3 red mana is spent"
    );
}

/// CR 118.9b: alternative costs are optional. When the concession makes both
/// the printed cost ({1}{R}) and Blitz ({2}{R}) payable with three blue mana,
/// the player chooses (Riveteers Requisitioner), and the printed cost is paid
/// under the concession.
#[test]
fn whispersteel_concession_keeps_the_printed_cost_choice() {
    let red = |generic: u32| ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic,
    };
    let mut scenario = GameScenario::new();
    let theirs = scenario
        .add_creature_to_graveyard(P1, "Their Requisitioner", 2, 1)
        .with_mana_cost(red(1))
        .with_keyword(Keyword::Blitz(BlitzCost::Mana(red(2))))
        .id();
    let mut runner = dagger_strikes_p1(scenario, false);
    for unit in mana(ManaType::Blue, 3) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    let card_id = runner.state().objects[&theirs].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: theirs,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the graveyard cast starts");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Blitz,
                ..
            }
        ),
        "both costs are payable, so the player chooses, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("the printed cost is chosen");
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "the printed {{1}}{{R}} = 2 of the 3 blue mana is spent"
    );
}

/// CR 118.3 + CR 601.2f-h: a mandatory choice of additional costs is judged
/// under the concession too. Bayou Groff ({1}{G}; "As an additional cost to
/// cast this spell, sacrifice a creature or pay {3}.") in P1's graveyard is
/// castable with five red mana, offers both branches and is cast paying {3};
/// the same card in P0's hand is not castable.
#[test]
fn whispersteel_concession_prices_an_additional_cost_choice() {
    // The additional cost as `client/public/card-data.json` parses Bayou Groff.
    let groff_cost: AdditionalCost = serde_json::from_str(
        r#"{"type":"Choice","data":[{"type":"Sacrifice","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"count":1},{"type":"Mana","cost":{"type":"Cost","shards":[],"generic":3}}]}"#,
    )
    .expect("Bayou Groff's additional cost");
    assert!(matches!(
        &groff_cost,
        AdditionalCost::Choice(AbilityCost::Sacrifice(_), AbilityCost::Mana { .. })
    ));
    let groff_mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    let mut scenario = GameScenario::new();
    let theirs = scenario
        .add_creature_to_graveyard(P1, "Their Bayou Groff", 5, 4)
        .with_mana_cost(groff_mana.clone())
        .with_additional_cost(groff_cost.clone())
        .id();
    let in_hand = scenario
        .add_creature_to_hand(P0, "Hand Bayou Groff", 5, 4)
        .with_mana_cost(groff_mana)
        .with_additional_cost(groff_cost)
        .id();
    let mut runner = dagger_strikes_p1(scenario, false);
    for unit in mana(ManaType::Red, 5) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    assert!(
        !can_cast_object_now(runner.state(), P0, in_hand),
        "red mana pays neither branch with {{1}}{{G}} outside the grant"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, theirs),
        "under the concession red mana pays {{1}}{{G}} with either branch"
    );
    let card_id = runner.state().objects[&theirs].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: theirs,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the offered cast is accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice {
                cost: AdditionalCost::Choice(..),
                ..
            }
        ),
        "both branches are payable, so the player chooses, got {:?}",
        runner.state().waiting_for
    );
    // CR 601.2b: declining the preferred sacrifice picks "pay {3}".
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("red mana pays {1}{G} plus {3} under the concession");
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{1}}{{G}} + {{3}} = all five red mana is spent"
    );
}

/// The payment mode is chosen under the concession: when a sacrificial {R}
/// source is the only payer, it keeps its explicit choice
/// (`AutoExceptSacrificialMana`) for a {B} card the concession lets it pay,
/// exactly as for an {R} card it pays anyway.
#[test]
fn whispersteel_concession_keeps_the_sacrificial_source_choice() {
    let mut scenario = GameScenario::new();
    let off_color = scenario
        .add_creature_to_graveyard(P1, "Their Black Bear", 2, 2)
        .with_mana_cost(black(1))
        .id();
    let on_color = scenario
        .add_creature_to_graveyard(P1, "Their Red Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.add_artifact_from_oracle(P0, "Red Petal", "{T}, Sacrifice this artifact: Add {R}.");
    let runner = dagger_strikes_p1(scenario, false);

    let offered_modes = |object: engine::types::identifiers::ObjectId| -> Vec<CastPaymentMode> {
        legal_actions(runner.state())
            .into_iter()
            .filter_map(|action| match action {
                GameAction::CastSpell {
                    object_id,
                    payment_mode,
                    ..
                } if object_id == object => Some(payment_mode),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        offered_modes(on_color),
        vec![CastPaymentMode::AutoExceptSacrificialMana],
        "the sacrificial source is the only payer of {{R}}"
    );
    assert_eq!(
        offered_modes(off_color),
        vec![CastPaymentMode::AutoExceptSacrificialMana],
        "under the concession it is the only payer of {{B}} too"
    );
}

/// A printed static permission carrying `PermissionCastLimit::Single` has no
/// granting effect to end, so it is refused (fail closed); the same static as
/// `Unlimited` is offered.
#[test]
fn printed_single_cast_static_is_refused() {
    use engine::types::ability::CardPlayMode;
    use engine::types::ability::{
        ControllerRef, FilterProp, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::statics::{
        CastFrequency, GraveyardPermissionPool, PermissionCastLimit, StaticMode,
    };

    for (cast_limit, offered) in [
        (PermissionCastLimit::Unlimited, true),
        (PermissionCastLimit::Single, false),
    ] {
        let permission = StaticDefinition::new(StaticMode::GraveyardCastPermission {
            frequency: CastFrequency::Unlimited,
            play_mode: CardPlayMode::Cast,
            graveyard_destination_replacement: None,
            extra_cost: None,
            enters_with_counter: None,
            required_cast_keyword: None,
            pool: GraveyardPermissionPool::OwnGraveyard,
            cast_limit,
            mana_spend_permission: None,
            enters_modifications: Default::default(),
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }]),
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Permission Holder", 1, 1)
            .with_static_definition(permission);
        let bear = scenario
            .add_creature_to_graveyard(P0, "Grave Bear", 2, 2)
            .with_mana_cost(one_generic())
            .id();
        scenario.with_mana_pool(P0, colorless(1));
        let runner = scenario.build();
        assert_eq!(
            can_cast_object_now(runner.state(), P0, bear),
            offered,
            "{cast_limit:?}"
        );
    }
}

const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";

const PATH_OF_ANCESTRY: &str = "This land enters tapped.\n{T}: Add one mana of any color in your commander's color identity. When that mana is spent to cast a creature spell that shares a creature type with your commander, scry 1. (Look at the top card of your library. You may put that card on the bottom.)";

fn is_vampire(subtypes: &[String]) -> bool {
    subtypes.iter().any(|subtype| subtype == "Vampire")
}

/// Walking Corpse ({1}{B} Zombie) in P0's graveyard.
fn walking_corpse(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id()
}

/// CR 601.3: the cast the Tomb allowed ends its grant, and that retirement is
/// journaled like every other continuous-effect edit, so replaying the recorded
/// commands from before the cast reaches the live state: the grant is gone in
/// both, and a later installation's recorded position still holds.
#[test]
fn used_tomb_grant_retirement_replays() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let corpse = walking_corpse(&mut scenario);
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = scenario.build();

    runner.activate(tomb, 1).resolve();
    let grant_ids: Vec<u64> = runner
        .state()
        .transient_continuous_effects
        .iter()
        .map(|effect| effect.id)
        .collect();
    assert_eq!(
        grant_ids.len(),
        1,
        "the Tomb's grant is one installed effect"
    );
    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    runner.cast(corpse).resolve();
    let outcome = runner.cast(growth).target_object(corpse).resolve();
    let state = outcome.state();
    assert_eq!(
        state.objects[&corpse].power,
        Some(5),
        "the later Giant Growth installed and applied"
    );
    assert!(
        state
            .transient_continuous_effects
            .iter()
            .all(|effect| effect.id != grant_ids[0]),
        "the used grant is gone live"
    );

    let mut replay = pre_state;
    for entry in state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
    {
        if let Some(engine::types::resolved_commands::ResolvedRulesCommand::ContinuousEffect(
            edit,
        )) = &entry.command
        {
            replay
                .apply_resolved_continuous_effect_edit(edit)
                .expect("each recorded continuous-effect edit replays in order");
        }
    }
    assert_eq!(
        replay.transient_continuous_effects, state.transient_continuous_effects,
        "replay reaches the live continuous effects, the used grant gone in both"
    );
}

/// CR 611.2f: the spell is a Vampire from the moment it is put on the stack,
/// so the cast-history record taken as it is cast says so, and a cast reader
/// (Edgar Markov's eminence, "Whenever you cast another Vampire spell")
/// sees it. Walking Corpse is a Zombie in the graveyard.
#[test]
fn tomb_rider_reaches_the_cast_record() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let corpse = walking_corpse(&mut scenario);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = scenario.build();

    assert!(
        !is_vampire(&runner.state().objects[&corpse].card_types.subtypes),
        "Walking Corpse is not a Vampire in the graveyard"
    );
    runner.activate(tomb, 1).resolve();
    let card_id = runner.state().objects[&corpse].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: corpse,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Tomb cast is accepted");
    assert_eq!(runner.state().objects[&corpse].zone, Zone::Stack);
    let record = runner.state().spells_cast_this_turn_by_player[&P0]
        .last()
        .expect("the cast is recorded")
        .clone();
    assert!(
        is_vampire(&record.subtypes),
        "the cast record carries the Tomb's Vampire, got {:?}",
        record.subtypes
    );
}

/// CR 611.2f + CR 106.6: Path of Ancestry's "When that mana is spent to cast a
/// creature spell that shares a creature type with your commander, scry 1"
/// sees the Tomb's Vampire on Walking Corpse, with Edgar Markov (a Vampire
/// Knight) as commander.
#[test]
fn tomb_rider_reaches_a_spend_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    let path = scenario
        .add_land_from_oracle(P0, "Path of Ancestry", PATH_OF_ANCESTRY)
        .id();
    let edgar = scenario
        .add_creature(P0, "Edgar Markov", 4, 4)
        .with_subtypes(vec!["Vampire", "Knight"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Red,
                ManaCostShard::White,
                ManaCostShard::Black,
            ],
            generic: 3,
        })
        .id();
    scenario.with_commander(edgar);
    let corpse = walking_corpse(&mut scenario);
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();

    assert!(
        !is_vampire(&runner.state().objects[&corpse].card_types.subtypes),
        "Walking Corpse is not a Vampire in the graveyard"
    );
    runner.activate(tomb, 1).resolve();
    let card_id = runner.state().objects[&corpse].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: corpse,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Tomb cast is accepted");
    assert_eq!(runner.state().objects[&corpse].zone, Zone::Stack);
    assert!(
        runner.state().objects[&path].tapped,
        "Path of Ancestry's mana paid the {{B}}"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == path),
        "Path's scry trigger is on the stack, got {:?}",
        runner.state().stack
    );
}

/// CR 611.2f + CR 118.9d: a reduction applies to the alternative cost being
/// paid, read against the spell as the Tomb makes it. Girder Goons (an Ogre
/// Warrior, {4}{B}, Blitz {3}{B}) through the Tomb, with "Vampire spells you
/// cast cost {2} less to cast", blitzes for {1}{B}.
#[test]
fn tomb_rider_reduces_a_graveyard_blitz() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_land_from_oracle(P0, "The Tomb of Aclazotz", TOMB_OF_ACLAZOTZ)
        .id();
    scenario.add_artifact_from_oracle(
        P0,
        "Vampire Reducer",
        "Vampire spells you cast cost {2} less to cast.",
    );
    let goons = scenario
        .add_creature_to_graveyard(P0, "Girder Goons", 4, 3)
        .with_subtypes(vec!["Ogre", "Warrior"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 4,
        })
        .with_keyword(Keyword::Blitz(BlitzCost::Mana(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })))
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = scenario.build();

    runner.activate(tomb, 1).resolve();
    assert!(
        can_cast_object_now(runner.state(), P0, goons),
        "blitz {{3}}{{B}} less {{2}} is payable with two black mana"
    );
    let card_id = runner.state().objects[&goons].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: goons,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Tomb cast is accepted");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("blitz is chosen");
    }
    assert_eq!(runner.state().objects[&goons].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
