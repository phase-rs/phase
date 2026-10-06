//! A declared prevention recipient ("...dealt to target creature...") shields
//! exactly the chosen object or player, and an untargeted population
//! ("...dealt to creatures this turn") mints no target slot and is matched as
//! each damage event happens.
//!
//! CR 115.1 + CR 601.2c / CR 602.2b: a declared recipient is chosen when the
//! spell is cast or the ability activated.
//! CR 115.10a: a clause that does not use the word "target" does not target.
//! CR 611.2c + CR 615.1: a prevention effect applies to the population as it
//! stands when each damage event happens.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, EffectKind, EffectScope};
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHIELDED_PASSAGE: &str =
    "Prevent all damage that would be dealt to target creature this turn.";
const GODTOUCHER: &str = "{1}{W}, {T}: Prevent all damage that would be dealt to target creature with power 5 or greater this turn.";
const REDEEM: &str =
    "Prevent all damage that would be dealt this turn to up to two target creatures.";
const NOBLE_VESTIGE: &str = "Flying\n{T}: Prevent the next 1 damage that would be dealt to target player or planeswalker this turn.";
const BLINDING_FOG: &str = "Prevent all damage that would be dealt to creatures this turn. Creatures you control gain hexproof until end of turn. (They can't be the targets of spells or abilities your opponents control.)";
const DEFEND_THE_HEARTH: &str =
    "Prevent all combat damage that would be dealt to players this turn.";
const FYLGJA: &str = "Enchant creature\nThis Aura enters with four healing counters on it.\nRemove a healing counter from this Aura: Prevent the next 1 damage that would be dealt to enchanted creature this turn.\n{2}{W}: Put a healing counter on this Aura.";
const PYROCLASM: &str = "Pyroclasm deals 2 damage to each creature.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

/// Cast a spell that declares no target, asserting the cast never surfaces a
/// target prompt (CR 115.10a), then let it resolve. Returns every event emitted
/// from the cast through resolution.
fn cast_untargeted(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("an untargeted cast is accepted");
    assert!(
        !matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 115.10a: a clause without the word \"target\" must not prompt for a target, got {:?}",
        result.waiting_for
    );
    let mut events = std::mem::take(&mut result.events);
    for _ in 0..6 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the untargeted spell resolved"
    );
    events
}

fn prevention_resolved(events: &[GameEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::PreventDamage,
                ..
            }
        )
    })
}

/// T1. Shielded Passage on A, then Pyroclasm: only A is shielded.
#[test]
fn shielded_passage_shields_only_the_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P1, "C", 2, 2).id();
    let passage = scenario
        .add_spell_to_hand_from_oracle(P0, "Shielded Passage", true, SHIELDED_PASSAGE)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    runner.cast(passage).target_objects(&[a]).resolve();
    let outcome = runner.cast(pyroclasm).resolve();

    assert_eq!(
        outcome.zone_of(a),
        Zone::Battlefield,
        "reach guard: A is shielded"
    );
    assert_eq!(
        outcome.zone_of(b),
        Zone::Graveyard,
        "a same-controller creature that was not chosen is not shielded"
    );
    assert_eq!(
        outcome.zone_of(c),
        Zone::Graveyard,
        "an opposing creature that was not chosen is not shielded"
    );
}

/// T2. The same scoping through an activated ability, with a richer filter.
#[test]
fn godtoucher_shields_only_the_chosen_creature_with_power_five_or_greater() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let godtoucher = scenario
        .add_creature_from_oracle(P0, "Godtoucher", 2, 2, GODTOUCHER)
        .id();
    let a = scenario.add_creature(P0, "A", 5, 5).id();
    let b = scenario.add_creature(P0, "B", 5, 5).id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    runner.activate(godtoucher, 0).target_object(a).resolve();
    let outcome = runner.cast(pyroclasm).resolve();

    assert_eq!(outcome.damage_marked(a), 0, "reach guard: A is shielded");
    assert_eq!(
        outcome.damage_marked(b),
        2,
        "a creature with power 5 or greater that was not chosen is not shielded"
    );
}

/// T3. Redeem declares two creatures: both are shielded and a third is not.
#[test]
fn redeem_shields_exactly_the_two_chosen_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P1, "C", 2, 2).id();
    let redeem = scenario
        .add_spell_to_hand_from_oracle(P0, "Redeem", true, REDEEM)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    runner.cast(redeem).target_objects(&[a, b]).resolve();
    let outcome = runner.cast(pyroclasm).resolve();

    assert_eq!(
        outcome.zone_of(a),
        Zone::Battlefield,
        "reach guard: A is shielded"
    );
    assert_eq!(
        outcome.zone_of(b),
        Zone::Battlefield,
        "reach guard: B is shielded"
    );
    assert_eq!(
        outcome.zone_of(c),
        Zone::Graveyard,
        "the creature that was not chosen is not shielded"
    );
}

/// T3b. CR 115.6: "up to two target creatures" may be cast with zero targets,
/// which leaves nothing to shield.
#[test]
fn redeem_with_no_chosen_creature_shields_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P1, "C", 2, 2).id();
    let redeem = scenario
        .add_spell_to_hand_from_oracle(P0, "Redeem", true, REDEEM)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    let cast = runner.cast(redeem).resolve();
    assert_eq!(
        cast.zone_of(redeem),
        Zone::Graveyard,
        "reach guard: the zero-target Redeem resolved"
    );
    assert!(matches!(
        cast.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));

    let outcome = runner.cast(pyroclasm).resolve();
    for creature in [a, b, c] {
        assert_eq!(
            outcome.zone_of(creature),
            Zone::Graveyard,
            "a zero-target declared prevention must not shield any creature"
        );
    }
}

/// T4. Noble Vestige on a player shields that player only, and on a
/// planeswalker shields only the chosen planeswalker.
#[test]
fn noble_vestige_shields_only_the_chosen_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vestige = scenario
        .add_creature_from_oracle(P0, "Noble Vestige", 1, 2, NOBLE_VESTIGE)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let other_bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.activate(vestige, 0).target_player(P0).resolve();
    let other_hit = runner.cast(other_bolt).target_player(P1).resolve();
    assert_eq!(
        other_hit.life_delta(P1),
        -3,
        "the unchosen player takes full damage"
    );
    let outcome = runner.cast(bolt).target_player(P0).resolve();

    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "the chosen player has one of the three damage prevented"
    );
}

fn vestige_planeswalker_loyalty(bolt_the_chosen: bool) -> (u32, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vestige = scenario
        .add_creature_from_oracle(P0, "Noble Vestige", 1, 2, NOBLE_VESTIGE)
        .id();
    let chosen = scenario
        .add_planeswalker_from_oracle(P0, "Jace Beleren", "Jace", 5, "")
        .id();
    let other = scenario
        .add_planeswalker_from_oracle(P0, "Chandra Nalaar", "Chandra", 5, "")
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.activate(vestige, 0).target_object(chosen).resolve();
    let hit = if bolt_the_chosen { chosen } else { other };
    let outcome = runner.cast(bolt).target_object(hit).resolve();
    (
        outcome.counters(chosen, CounterType::Loyalty),
        outcome.counters(other, CounterType::Loyalty),
    )
}

#[test]
fn noble_vestige_shields_only_the_chosen_planeswalker() {
    let (chosen_loyalty, other_loyalty) = vestige_planeswalker_loyalty(true);
    assert_eq!(
        chosen_loyalty, 3,
        "reach guard: one damage to the chosen planeswalker is prevented"
    );
    assert_eq!(other_loyalty, 5);

    let (chosen_loyalty, other_loyalty) = vestige_planeswalker_loyalty(false);
    assert_eq!(chosen_loyalty, 5);
    assert_eq!(
        other_loyalty, 2,
        "a planeswalker that was not chosen is not shielded"
    );
}

/// T5. Blinding Fog names "creatures", not "target creature": no prompt, and
/// every creature is shielded while players are not.
#[test]
fn blinding_fog_mints_no_target_slot_and_shields_all_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P1, "B", 2, 2).id();
    let fog = scenario
        .add_spell_to_hand_from_oracle(P0, "Blinding Fog", true, BLINDING_FOG)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    cast_untargeted(&mut runner, fog);
    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(outcome.zone_of(a), Zone::Battlefield, "A is shielded");
    assert_eq!(outcome.zone_of(b), Zone::Battlefield, "B is shielded");

    // CR 109.1: players are not creatures, so Blinding Fog does not shield them.
    let outcome = runner.cast(bolt).target_player(P0).resolve();
    assert_eq!(outcome.life_delta(P0), -3);
}

/// T5b. CR 611.2c + CR 615.1: the population is read as each damage event
/// happens. A creature that enters after Blinding Fog resolves is shielded and
/// the shield does not depend on any one creature staying on the battlefield.
#[test]
fn blinding_fog_shields_a_creature_that_enters_after_it_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P1, "B", 2, 2).id();
    let bears = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let fog = scenario
        .add_spell_to_hand_from_oracle(P0, "Blinding Fog", true, BLINDING_FOG)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    cast_untargeted(&mut runner, fog);
    runner.cast(unsummon).target_objects(&[a]).resolve();
    let entered = runner.cast(bears).resolve();
    assert_eq!(
        entered.zone_of(bears),
        Zone::Battlefield,
        "reach guard: the creature entered after Blinding Fog resolved"
    );
    assert_eq!(entered.zone_of(a), Zone::Hand, "reach guard: A left");

    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(outcome.zone_of(b), Zone::Battlefield);
    assert_eq!(outcome.zone_of(bears), Zone::Battlefield);
}

/// T5c. CR 115.10a: with no creature anywhere there is still no target to
/// choose, so the cast is legal and the prevention resolves.
#[test]
fn blinding_fog_is_castable_with_no_creature_on_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fog = scenario
        .add_spell_to_hand_from_oracle(P0, "Blinding Fog", true, BLINDING_FOG)
        .id();
    let mut runner = scenario.build();

    let events = cast_untargeted(&mut runner, fog);

    assert!(
        prevention_resolved(&events),
        "the prevention effect resolved"
    );
    assert_eq!(runner.state().objects[&fog].zone, Zone::Graveyard);
}

/// T6. Defend the Hearth names "players": no prompt, every player is shielded
/// from combat damage and a blocker still takes damage (CR 510.2).
#[test]
fn defend_the_hearth_mints_no_target_slot_and_shields_players() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let x = scenario.add_creature(P0, "X", 2, 2).id();
    let y = scenario.add_creature(P0, "Y", 2, 2).id();
    let z = scenario.add_creature(P1, "Z", 2, 2).id();
    let defend = scenario
        .add_spell_to_hand_from_oracle(P0, "Defend the Hearth", true, DEFEND_THE_HEARTH)
        .id();
    let mut runner = scenario.build();

    cast_untargeted(&mut runner, defend);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (x, engine::game::combat::AttackTarget::Player(P1)),
            (y, engine::game::combat::AttackTarget::Player(P1)),
        ])
        .expect("attack");
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    runner.declare_blockers(&[(z, y)]).expect("block");
    let life_before = runner.state().players[1].life;
    let outcome = runner.combat_damage();

    assert!(
        outcome.state().objects[&z].damage_marked > 0,
        "reach guard: combat damage to a creature is not prevented"
    );
    assert_eq!(
        outcome.state().players[1].life,
        life_before,
        "combat damage to the player is prevented"
    );
}

/// Put Fylgja on `a`, with four healing counters.
fn fylgja_on(scenario: &mut GameScenario) -> ObjectId {
    let fylgja = scenario
        .add_enchantment_from_oracle(P0, "Fylgja", FYLGJA)
        .with_subtypes(vec!["Aura"])
        .id();
    scenario.with_counter(fylgja, parse_counter_type("healing"), 4);
    fylgja
}

fn attach(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(aura);
    reindex_object_triggers(runner.state_mut(), aura);
}

/// T10. Preservation row, non-discriminating at base: Fylgja prevents the next
/// damage to the creature it enchants and to no other creature.
#[test]
fn fylgja_shields_only_the_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let fylgja = fylgja_on(&mut scenario);
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();
    attach(&mut runner, fylgja, a);

    runner.activate(fylgja, 0).resolve();
    let outcome = runner.cast(pyroclasm).resolve();

    assert_eq!(
        outcome.zone_of(a),
        Zone::Battlefield,
        "reach guard: A survives"
    );
    assert_eq!(
        outcome.damage_marked(a),
        1,
        "one of the two damage is prevented"
    );
    assert_eq!(outcome.zone_of(b), Zone::Graveyard);
}

/// T10b. A second Aura on another creature does not make that creature
/// "enchanted creature" for Fylgja's shield.
#[test]
fn fylgja_does_not_shield_a_creature_wearing_a_different_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 4, 4).id();
    let fylgja = fylgja_on(&mut scenario);
    let other_aura = scenario
        .add_enchantment_from_oracle(P0, "Other Aura", "")
        .with_subtypes(vec!["Aura"])
        .id();
    let shock_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let shock_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let mut runner = scenario.build();
    attach(&mut runner, fylgja, a);
    attach(&mut runner, other_aura, b);

    runner.activate(fylgja, 0).resolve();
    let outcome = runner.cast(shock_b).target_object(b).resolve();
    assert_eq!(
        outcome.damage_marked(b),
        2,
        "a creature enchanted by some other Aura is not Fylgja's enchanted creature"
    );

    let outcome = runner.cast(shock_a).target_object(a).resolve();
    assert_eq!(
        outcome.damage_marked(a),
        1,
        "reach guard: Fylgja's shield still protects its enchanted creature"
    );
}

/// Unsupported source placements must never install a recipient shield
/// (CR 615.2). Both callers use the exact MTGJSON Oracle text.
fn assert_unsupported_source_installs_no_recipient_shield(
    name: &str,
    oracle: &str,
    power: i32,
    toughness: i32,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, name, power, toughness, oracle)
        .id();
    let victim = scenario.add_creature(P0, "Unprotected", 4, 4).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    let activation = runner.activate(source, 0).target_object(victim).resolve();
    assert!(
        activation
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::AbilityActivated { .. })),
        "reach guard: the ability was activated"
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: activation resolved"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(victim, engine::game::combat::AttackTarget::Player(P1))])
        .expect("attack");
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    runner
        .declare_blockers(&[(blocker, victim)])
        .expect("block");
    let outcome = runner.combat_damage();
    // CR 615.2: the unsupported source clause cannot shield its selected source as a recipient.
    assert_eq!(
        outcome.state().objects[&victim].damage_marked,
        2,
        "{name}: no wrong recipient shield"
    );
    assert_eq!(
        outcome.state().objects[&blocker].damage_marked,
        4,
        "reach guard: combat damage happened"
    );
}

#[test]
fn stonewise_fortifier_installs_no_wrong_recipient_shield() {
    assert_unsupported_source_installs_no_recipient_shield(
        "Stonewise Fortifier",
        "{4}{W}: Prevent all damage that would be dealt to this creature by target creature this turn.",
        2, 2,
    );
}

#[test]
fn resistance_fighter_installs_no_wrong_recipient_shield() {
    assert_unsupported_source_installs_no_recipient_shield(
        "Resistance Fighter",
        "Sacrifice this creature: Prevent all combat damage target creature would deal this turn.",
        1,
        1,
    );
}

/// Synthetic hostile fixture: Djeru's Resolve's targeting instruction followed
/// by Blinding Fog's mass recipient. A floating shield must survive the
/// inherited target leaving the battlefield (CR 611.2c + CR 400.7).
#[test]
fn mass_recipient_after_a_targeting_clause_shields_every_creature() {
    let oracle =
        "Untap target creature. Prevent all damage that would be dealt to creatures this turn.";
    let parsed = parse_oracle_text(
        oracle,
        "Mass prevention probe",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = &parsed.abilities[0];
    assert!(matches!(*ability.effect, Effect::SetTapState { .. }));
    assert!(matches!(
        *ability
            .sub_ability
            .as_ref()
            .expect("mass prevention clause")
            .effect,
        Effect::PreventDamage {
            recipient_scope: EffectScope::All,
            ..
        }
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P1, "B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mass prevention probe", true, oracle)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();
    let cast = runner.cast(spell).target_object(a).resolve();
    assert!(
        prevention_resolved(cast.events()),
        "reach guard: mass prevention resolved"
    );
    let bounced = runner.cast(unsummon).target_object(a).resolve();
    assert_eq!(
        bounced.zone_of(a),
        Zone::Hand,
        "reach guard: inherited target left"
    );
    let hit = runner.cast(pyroclasm).resolve();
    assert_eq!(hit.zone_of(b), Zone::Battlefield);
    assert_eq!(hit.damage_marked(b), 0);
}

#[test]
fn djerus_resolve_shields_only_the_untapped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P1, "B", 2, 2).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Djeru's Resolve", true,
        "Untap target creature. Prevent all damage that would be dealt to it this turn.\nCycling {2} ({2}, Discard this card: Draw a card.)").id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(a).resolve();
    let hit = runner.cast(pyroclasm).resolve();
    assert_eq!(hit.zone_of(a), Zone::Battlefield);
    assert_eq!(hit.damage_marked(a), 0);
    assert_eq!(hit.zone_of(b), Zone::Graveyard);
}

/// CR 115.1a + CR 615.1: "another target" still binds one eligible recipient.
#[test]
fn kurbis_shields_only_the_chosen_countered_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kurbis = scenario.add_creature_from_oracle(
        P0, "Kurbis, Harvest Celebrant", 0, 0,
        "Kurbis enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\nRemove a +1/+1 counter from Kurbis: Prevent all damage that would be dealt this turn to another target creature with a +1/+1 counter on it.",
    ).id();
    let chosen = scenario.add_creature(P0, "Chosen", 4, 4).id();
    let unchosen = scenario.add_creature(P0, "Unchosen", 4, 4).id();
    scenario.with_counter(kurbis, CounterType::Plus1Plus1, 3);
    scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
    scenario.with_counter(unchosen, CounterType::Plus1Plus1, 1);
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();
    let activation = runner.activate(kurbis, 0).target_object(chosen).resolve();
    assert!(
        activation
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::AbilityActivated { .. })),
        "reach guard: Kurbis activated through its counter cost"
    );
    let damage = runner.cast(pyroclasm).resolve();
    assert_eq!(
        damage.damage_marked(chosen),
        0,
        "the chosen countered creature is shielded"
    );
    assert_eq!(
        damage.damage_marked(unchosen),
        2,
        "reach guard: an eligible unchosen creature takes damage"
    );
}
