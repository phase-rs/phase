//! The event-source anaphor lift stops at an exile-until loop and at a chosen
//! stack-object target.
//!
//! CR 608.2c: "The controller of the spell or ability follows its instructions
//! in the order written." A later "that card" / "it" names the object an
//! earlier instruction introduced. In an enters trigger the parser lifts an
//! unbound `ParentTarget` to the trigger's event object, and that lift must
//! stop where an instruction hands a new object to the next one:
//!
//! * an exile-until loop hands on the card it exiled (CR 701.13a) — Breaching
//!   Dragonstorm's "If you don't, put that card into your hand", Demonlord
//!   Belzenlok's "then put that card into your hand";
//! * "counter target spell" hands on the chosen spell (CR 115.1d), so
//!   Transcendent Dragon's "exile it instead" is a CR 614.1a replacement on the
//!   countered spell.
//!
//! Each test drives the real cast → ETB → resolution pipeline with verbatim
//! Oracle text. Trigger sources are set to cost {0} with
//! `CardBuilder::with_mana_cost(ManaCost::zero())` so no mana is needed; the
//! library hit is Divination at its printed {2}{U} (mana value 3).
//!
//! Rows:
//! * R1 / R2: the declined Dragonstorm and Belzenlok put the exiled card into
//!   hand; the trigger source stays on the battlefield.
//! * R3: Transcendent Dragon exiles the spell it countered.
//! * R4: the accepted Dragonstorm cast is untouched (CR 608.2n graveyard).
//! * H1: an exile-until loop with no hit puts nothing into hand (CR 609.3).
//! * H2: a spell that can't be countered is not countered this way (CR 101.2).
//! * R5: known-gap pin — Transcendent Dragon's free cast is not offered, in agreement with the `counter_exile_rider_tail_dropped` coverage gap; the change that makes the cast run inverts it.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BREACHING_DRAGONSTORM: &str = "When this enchantment enters, exile cards from the top of your library until you exile a nonland card. You may cast it without paying its mana cost if that spell's mana value is 8 or less. If you don't, put that card into your hand.\nWhen a Dragon you control enters, return this enchantment to its owner's hand.";
const DEMONLORD_BELZENLOK: &str = "Flying, trample\nWhen Demonlord Belzenlok enters, exile cards from the top of your library until you exile a nonland card, then put that card into your hand. If the card's mana value is 4 or greater, repeat this process. Demonlord Belzenlok deals 1 damage to you for each card put into your hand this way.";
const TRANSCENDENT_DRAGON: &str = "Flash\nFlying\nWhen this creature enters, if you cast it, counter target spell. If that spell is countered this way, exile it instead of putting it into its owner's graveyard, then you may cast it without paying its mana cost.";
const DIVINATION: &str = "Draw two cards.";
const CARNAGE_TYRANT: &str = "This spell can't be countered.\nTrample, hexproof";

fn resolved(events: &[GameEvent], kind: EffectKind) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::EffectResolved { kind: k, .. } if *k == kind))
}

fn resolved_from(events: &[GameEvent], kind: EffectKind, source: ObjectId) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved { kind: k, source_id, .. } if *k == kind && *source_id == source
        )
    })
}

fn countered(events: &[GameEvent], spell: ObjectId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::SpellCountered { object_id, .. } if *object_id == spell))
}

fn moved(events: &[GameEvent], object: ObjectId, from: Zone, to: Zone) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::ZoneChanged { object_id, from: Some(f), to: t, .. }
                if *object_id == object && *f == from && *t == to
        )
    })
}

/// Divination at its printed {2}{U}.
fn divination_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    }
}

/// A library of four fillers, then Divination, then a basic land on top.
/// Returns (Divination, land).
fn library_land_over_divination(scenario: &mut GameScenario, land: &str) -> (ObjectId, ObjectId) {
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), false);
    }
    let divination = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .from_oracle_text(DIVINATION)
        .with_mana_cost(divination_cost())
        .id();
    let land = scenario.add_land_to_library_top(P0, land).id();
    (divination, land)
}

fn add_dragonstorm(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Breaching Dragonstorm", false, BREACHING_DRAGONSTORM)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// R1 — CR 608.2c + CR 701.13a: declining the free cast puts the exiled card,
/// not the entering enchantment, into hand.
#[test]
fn breaching_dragonstorm_decline_puts_the_exiled_card_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (divination, land) = library_land_over_divination(&mut scenario, "Mountain");
    let storm = add_dragonstorm(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(storm).decline_optional().resolve();

    // Reach guard: the loop ran and exiled the land above the hit.
    assert!(resolved(outcome.events(), EffectKind::ExileFromTopUntil));
    assert_eq!(outcome.zone_of(land), Zone::Exile);
    assert_eq!(
        outcome.zone_of(divination),
        Zone::Hand,
        "\"put that card into your hand\" names the exiled card"
    );
    assert_eq!(
        outcome.zone_of(storm),
        Zone::Battlefield,
        "the entering enchantment is not \"that card\""
    );
}

/// R2 — CR 608.2c + CR 701.13a: Belzenlok puts the exiled card into hand and
/// stays on the battlefield; the card's mana value 3 ends the process.
#[test]
fn demonlord_belzenlok_puts_the_exiled_card_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (divination, land) = library_land_over_divination(&mut scenario, "Swamp");
    let demon = scenario
        .add_creature_to_hand_from_oracle(P0, "Demonlord Belzenlok", 6, 6, DEMONLORD_BELZENLOK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(demon).resolve();

    // Reach guard: the loop exiled the land, and the damage link ran.
    assert_eq!(outcome.zone_of(land), Zone::Exile);
    assert_eq!(outcome.life_delta(P0), -1);
    assert_eq!(
        outcome.zone_of(divination),
        Zone::Hand,
        "\"then put that card into your hand\" names the exiled card"
    );
    assert_eq!(
        outcome.zone_of(demon),
        Zone::Battlefield,
        "Belzenlok is not \"that card\""
    );
}

/// R3 — CR 608.2c + CR 614.1a: Transcendent Dragon's "exile it instead" puts the
/// spell it countered into exile, not its owner's graveyard (CR 701.6a).
#[test]
fn transcendent_dragon_exiles_the_spell_it_countered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Transcendent Dragon", 4, 3, TRANSCENDENT_DRAGON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut pending = runner.cast(divination).commit();
    let outcome = pending.cast(dragon).target_objects(&[divination]).resolve();

    // Reach guard: the trigger countered Divination, which never resolved.
    assert!(countered(outcome.events(), divination));
    assert!(!resolved_from(
        outcome.events(),
        EffectKind::Draw,
        divination
    ));
    assert_eq!(
        outcome.zone_of(divination),
        Zone::Exile,
        "the countered spell is exiled instead of put into the graveyard"
    );
    assert_eq!(
        outcome.zone_of(dragon),
        Zone::Battlefield,
        "the entering Dragon is not \"it\""
    );
}

/// R5 — known-gap pin, not a correctness claim. By CR 608.2c the controller may
/// then cast the exiled spell free; the counter rider branch drops the
/// `ContinuationStep` tail that says so (`counter_exile_rider_runs_tail`), and
/// coverage reports the same tail as the `counter_exile_rider_tail_dropped`
/// resolver gap. This pins that the runtime agrees with that gap: with every
/// optional accepted, the cast is NOT offered. The change that makes the cast
/// run inverts this test. Precedent: `devious_cover_up_tail_is_parsed_but_inert`.
#[test]
fn transcendent_dragon_does_not_run_the_cast_coverage_reports_as_a_gap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Transcendent Dragon", 4, 3, TRANSCENDENT_DRAGON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut pending = runner.cast(divination).commit();
    let outcome = pending
        .cast(dragon)
        .target_objects(&[divination])
        .accept_optional()
        .resolve();

    // Reach guard: the counter ran and its exile rider moved Divination.
    assert!(countered(outcome.events(), divination));
    assert_eq!(outcome.zone_of(divination), Zone::Exile);
    assert!(
        !moved(outcome.events(), divination, Zone::Exile, Zone::Stack),
        "the exiled spell is not cast"
    );
    assert!(!resolved(outcome.events(), EffectKind::CastFromZone));
    assert!(!resolved_from(
        outcome.events(),
        EffectKind::Draw,
        divination
    ));
    assert_eq!(outcome.zone_of(dragon), Zone::Battlefield);
}

/// R4 — CR 608.2n: accepting the free cast leaves the enchantment in play and
/// the cast spell resolves into its owner's graveyard.
#[test]
fn breaching_dragonstorm_accept_casts_the_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (divination, _land) = library_land_over_divination(&mut scenario, "Mountain");
    let storm = add_dragonstorm(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(storm).accept_optional().resolve();

    // Reach guard: Divination left exile through the stack.
    assert!(moved(
        outcome.events(),
        divination,
        Zone::Exile,
        Zone::Stack
    ));
    assert_eq!(outcome.zone_of(storm), Zone::Battlefield);
    assert_eq!(outcome.zone_of(divination), Zone::Graveyard);
    assert_eq!(outcome.hand_drawn(P0), 2, "Divination drew two cards");
}

/// H1 — CR 609.3: with no nonland card in the library the loop exiles every
/// card and nothing is put into hand.
#[test]
fn breaching_dragonstorm_without_a_hit_puts_nothing_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_land_to_library_top(P0, "Mountain").id())
        .collect();
    let storm = add_dragonstorm(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(storm).decline_optional().resolve();

    // Reach guard: the loop ran and exiled every land.
    assert!(resolved(outcome.events(), EffectKind::ExileFromTopUntil));
    for land in lands {
        assert_eq!(outcome.zone_of(land), Zone::Exile);
    }
    assert_eq!(outcome.zone_of(storm), Zone::Battlefield);
    assert_eq!(outcome.hand_drawn(P0), 0);
}

/// H2 — CR 101.2: a spell that can't be countered is not countered this way, so
/// the exile rider does not apply and the spell resolves.
#[test]
fn transcendent_dragon_cannot_counter_carnage_tyrant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tyrant = scenario
        .add_creature_to_hand_from_oracle(P0, "Carnage Tyrant", 7, 6, CARNAGE_TYRANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Transcendent Dragon", 4, 3, TRANSCENDENT_DRAGON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut pending = runner.cast(tyrant).commit();
    let outcome = pending.cast(dragon).target_objects(&[tyrant]).resolve();

    // Reach guard: the counter instruction ran.
    assert!(resolved(outcome.events(), EffectKind::Counter));
    assert!(!countered(outcome.events(), tyrant));
    assert_eq!(outcome.zone_of(tyrant), Zone::Battlefield);
    assert_eq!(outcome.zone_of(dragon), Zone::Battlefield);
}
