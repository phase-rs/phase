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
use engine::types::ability::{Effect, EffectKind, EffectScope, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHIELDED_PASSAGE: &str =
    "Prevent all damage that would be dealt to target creature this turn.";
const GODTOUCHER: &str = "{1}{W}, {T}: Prevent all damage that would be dealt to target creature with power 5 or greater this turn.";
const REDEEM: &str =
    "Prevent all damage that would be dealt this turn to up to two target creatures.";
const NOBLE_VESTIGE: &str = "Flying\n{T}: Prevent the next 1 damage that would be dealt to target player or planeswalker this turn.";
const AVEN_REDEEMER: &str =
    "Flying\n{T}: Prevent the next 2 damage that would be dealt to any target this turn.";
const ELVISH_HEALER: &str = "{T}: Prevent the next 1 damage that would be dealt to any target this turn. If it's a green creature, prevent the next 2 damage instead.";
const BARBED_WIRE: &str =
    "{2}: Prevent the next 1 damage that would be dealt by this artifact this turn.";
const BARBED_WIRE_DAMAGE: &str = "{T}: This artifact deals 1 damage to target player.";
const BLINDING_FOG: &str = "Prevent all damage that would be dealt to creatures this turn. Creatures you control gain hexproof until end of turn. (They can't be the targets of spells or abilities your opponents control.)";
const DEFEND_THE_HEARTH: &str =
    "Prevent all combat damage that would be dealt to players this turn.";
const FYLGJA: &str = "Enchant creature\nThis Aura enters with four healing counters on it.\nRemove a healing counter from this Aura: Prevent the next 1 damage that would be dealt to enchanted creature this turn.\n{2}{W}: Put a healing counter on this Aura.";
const PYROCLASM: &str = "Pyroclasm deals 2 damage to each creature.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const REVERED_ELDER: &str =
    "{1}: Prevent the next 1 damage that would be dealt to this creature this turn.";
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

/// CR 115.1c + CR 115.4 + CR 615.7: Aven Redeemer ("Prevent the next 2 damage that would
/// be dealt to any target this turn") must prompt for a target when activated (work item fix),
/// and the shield must protect only the chosen creature.
#[test]
fn aven_redeemer_prompts_for_target_when_activated_and_shields_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = scenario
        .add_creature_from_oracle(P0, "Aven Redeemer", 2, 2, AVEN_REDEEMER)
        .id();
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let artifact = scenario.add_artifact_from_oracle(P0, "Bauble", "").id();
    let battle = scenario.add_enchantment_from_oracle(P1, "Battle", "").id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&battle).unwrap();
        object.card_types.core_types = vec![engine::types::card_type::CoreType::Battle];
        object.base_card_types = object.card_types.clone();
    }

    // Work item assertion: activating Aven Redeemer must ask for a target.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: aven,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Aven Redeemer");
    assert!(
        matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 115.1c + CR 115.4: Aven Redeemer must prompt for a target when activated, got {:?}",
        result.waiting_for
    );
    let WaitingFor::TargetSelection { target_slots, .. } = &result.waiting_for else {
        panic!("Aven Redeemer target prompt must expose its legal target slot");
    };
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(battle)),
        "CR 115.4: battles are legal any-targets"
    );
    assert!(
        !legal.contains(&TargetRef::Object(land)),
        "CR 115.4: lands are not legal any-targets"
    );
    assert!(
        !legal.contains(&TargetRef::Object(artifact)),
        "CR 115.4: noncreature artifacts are not legal any-targets"
    );

    // Select creature A as the target.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("choose target creature A");

    // Pass priority until the ability resolves.
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    // Pyroclasm deals 2 damage to each creature.
    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(
        outcome.zone_of(a),
        Zone::Battlefield,
        "chosen creature A is shielded against 2 damage"
    );
    assert_eq!(
        outcome.damage_marked(a),
        0,
        "all 2 damage dealt to A was prevented"
    );
    assert_eq!(
        outcome.zone_of(b),
        Zone::Graveyard,
        "unchosen creature B is not shielded and dies"
    );
}

/// CR 115.4: "any target" allows choosing a player; 2 damage dealt to the chosen
/// player is prevented.
#[test]
fn aven_redeemer_prompts_for_target_and_shields_chosen_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = scenario
        .add_creature_from_oracle(P0, "Aven Redeemer", 2, 2, AVEN_REDEEMER)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let other_bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    runner.activate(aven, 0).target_player(P0).resolve();

    let other_hit = runner.cast(other_bolt).target_player(P1).resolve();
    assert_eq!(
        other_hit.life_delta(P1),
        -3,
        "the unchosen player takes full damage"
    );

    let outcome = runner.cast(bolt).target_player(P0).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        -1,
        "the chosen player has 2 of the 3 damage prevented"
    );
}

/// CR 615.7: Aven Redeemer's "next 2 damage" shield depletes after preventing 2 damage.
#[test]
fn aven_redeemer_shields_up_to_two_damage_and_depletes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = scenario
        .add_creature_from_oracle(P0, "Aven Redeemer", 2, 2, AVEN_REDEEMER)
        .id();
    let a = scenario.add_creature(P0, "A", 4, 4).id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let mut runner = scenario.build();

    runner.activate(aven, 0).target_object(a).resolve();

    // Bolt deals 3 damage: 2 prevented, 1 marked. Shield is now depleted.
    let outcome1 = runner.cast(bolt).target_object(a).resolve();
    assert_eq!(
        outcome1.damage_marked(a),
        1,
        "2 damage was prevented by the shield, 1 damage marked"
    );

    // Shock deals 2 damage: shield is depleted, so all 2 damage is marked (total 3).
    let outcome2 = runner.cast(shock).target_object(a).resolve();
    assert_eq!(
        outcome2.damage_marked(a),
        3,
        "shield is depleted so full damage is marked"
    );
}

/// CR 115.1c + CR 115.10a + CR 608.2c: Revered Elder ("{1}: Prevent the next 1 damage that
/// would be dealt to this creature this turn.") is a self-only prevention ability without
/// the "target" keyword. Activating it must NOT prompt for a target, and the resulting shield
/// must protect only Revered Elder (the source), leaving other creatures unshielded.
#[test]
fn revered_elder_raises_no_target_prompt_when_activated_and_shields_only_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elder = scenario
        .add_creature_from_oracle(P0, "Revered Elder", 1, 2, REVERED_ELDER)
        .id();
    let other = scenario.add_creature(P0, "Other", 2, 2).id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", true, PYROCLASM)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    // Activating Revered Elder must NOT prompt for a target (CR 115.1c, CR 115.10a).
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: elder,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Revered Elder");
    assert!(
        !matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 115.10a: Revered Elder must not prompt for a target when activated, got {:?}",
        result.waiting_for
    );

    // Pay mana if in ManaPayment window.
    if matches!(result.waiting_for, WaitingFor::ManaPayment { .. }) {
        let _ = runner
            .act(GameAction::PassPriority)
            .expect("pay activation mana");
    }

    // Pass priority until the activated ability resolves on the stack.
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    // Pyroclasm deals 2 damage to each creature.
    // Revered Elder (1/2) has 1 damage prevented, taking 1 damage, and survives.
    // Other creature (2/2) has no damage prevented, taking 2 damage, and dies.
    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(
        outcome.damage_marked(elder),
        1,
        "CR 608.2c + CR 615.1: 1 of 2 damage to Revered Elder was prevented by its self-shield"
    );
    assert_eq!(
        outcome.zone_of(elder),
        Zone::Battlefield,
        "Revered Elder survives with 1 damage marked on 2 toughness"
    );
    assert_eq!(
        outcome.zone_of(other),
        Zone::Graveyard,
        "Other creature received no prevention and died from 2 damage"
    );
}

/// CR 115.10a: Barbed Wire's "dealt by this artifact" names a damage source, not a
/// recipient, so activating it must not prompt for a target.
#[test]
fn barbed_wire_raises_no_target_prompt_when_activated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The second ability is a fixture that lets the artifact be a damage
    // source; the prevention ability uses Barbed Wire's exact Oracle text.
    let wire = scenario
        .add_artifact_from_oracle(
            P0,
            "Barbed Wire",
            &format!("{BARBED_WIRE}\n{BARBED_WIRE_DAMAGE}"),
        )
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", false, SHOCK)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    assert!(
        matches!(
            runner.state().objects[&wire].abilities[0].effect.as_ref(),
            Effect::PreventDamage {
                damage_source_filter: Some(_),
                ..
            }
        ),
        "Barbed Wire must retain its printed source restriction: {:#?}",
        runner.state().objects[&wire].abilities[0].effect
    );

    let mut result = runner
        .act(GameAction::ActivateAbility {
            source_id: wire,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Barbed Wire");
    assert!(
        !matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "CR 115.10a: Barbed Wire must not prompt for a target, got {:?}",
        result.waiting_for
    );
    let mut events = std::mem::take(&mut result.events);
    if matches!(result.waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay activation mana")
                .events,
        );
    }
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Barbed Wire")
                .events,
        );
    }
    assert!(
        prevention_resolved(&events),
        "reach guard: Barbed Wire installed a PreventDamage effect"
    );

    let outcome = runner.cast(shock).target_player(P1).resolve();
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "damage from a different source must not consume Barbed Wire's shield"
    );

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: wire,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Barbed Wire's test damage ability");
    assert!(
        matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "the fixture ability should target a player"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("target the opponent with Barbed Wire");
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Barbed Wire damage")
                .events,
        );
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { amount: 1, .. })),
        "Barbed Wire's own damage should be prevented"
    );
}

/// CR 608.2k + CR 120.1: a passive damage trigger's bare "it" refers to the
/// damaged object captured from the trigger condition throughout parsing and
/// lowering.
#[test]
fn passive_damage_trigger_prevention_bare_it_binds_the_damaged_object() {
    let parsed = parse_oracle_text(
        "Whenever a creature is dealt damage, prevent the next 1 damage that would be dealt to it this turn.",
        "Damage Recipient Preventer",
        &[],
        &[],
        &[],
    );
    let trigger = parsed.triggers.first().expect("damage trigger parses");
    let execute = trigger.execute.as_deref().expect("trigger has a body");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::PreventDamage {
                target: engine::types::ability::TargetFilter::EventTarget,
                ..
            }
        ),
        "the prevention recipient must remain the damaged object through the production parser: {:#?}",
        execute.effect
    );
}

/// CR 608.2k + CR 120.1 + CR 615.1: a passive damage trigger's bare "it"
/// installs a shield on the damaged creature itself, not on its controller.
#[test]
fn passive_damage_trigger_prevention_shields_damaged_creature_not_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let preventer_oracle =
        "Whenever a creature is dealt damage, prevent the next 1 damage that would be dealt to it this turn.";
    let shock_oracle = "Shock deals 2 damage to any target.";
    scenario.add_enchantment_from_oracle(P0, "Damage Shield", preventer_oracle);
    let creature = scenario.add_creature(P0, "Shielded Creature", 2, 4).id();
    let shock1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 1", false, shock_oracle)
        .id();
    let shock2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 2", false, shock_oracle)
        .id();
    let shock3 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 3", false, shock_oracle)
        .id();
    let mut runner = scenario.build();

    // Deal 2 damage to the creature.
    let outcome = runner.cast(shock1).target_object(creature).resolve();
    assert_eq!(outcome.damage_marked(creature), 2);

    // The trigger went onto the stack. Resolve it.
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve trigger");
    }

    // Now Creature should have a 1-damage prevention shield.
    // Cast Shock 2 on the creature: 2 damage - 1 prevented = 1 dealt (total marked: 3).
    let outcome = runner.cast(shock2).target_object(creature).resolve();
    assert_eq!(
        outcome.damage_marked(creature),
        3,
        "1 of the 2 damage to the creature should have been prevented by the shield"
    );

    // Cast Shock 3 on the creature's controller (P0): controller must take full 2 damage (shield was on creature, not player).
    let outcome = runner.cast(shock3).target_player(P0).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "creature's prevention shield must not protect controller"
    );
}

/// CR 400.7 + CR 608.2b: when a creature is destroyed by the triggering damage,
/// the prevention trigger resolving for bare "it" does not install a floating shield
/// that protects other creatures or players.
#[test]
fn passive_damage_trigger_prevention_on_lethal_damage_does_not_install_floating_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let preventer_oracle =
        "Whenever a creature is dealt damage, prevent the next 1 damage that would be dealt to it this turn.";
    let shock_oracle = "Shock deals 2 damage to any target.";
    scenario.add_enchantment_from_oracle(P0, "Damage Shield", preventer_oracle);
    let dead_creature = scenario.add_creature(P0, "Dead Creature", 2, 2).id();
    let survivor = scenario.add_creature(P0, "Survivor", 2, 4).id();
    let shock1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 1", false, shock_oracle)
        .id();
    let shock2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 2", false, shock_oracle)
        .id();
    let shock3 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock 3", false, shock_oracle)
        .id();
    let mut runner = scenario.build();

    // Deal lethal damage to the 2/2 creature.
    let outcome = runner.cast(shock1).target_object(dead_creature).resolve();
    assert_eq!(outcome.zone_of(dead_creature), Zone::Graveyard);

    // The trigger went onto the stack. Resolve it.
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve trigger");
    }

    // Shock survivor: must take full 2 damage (no floating shield was created).
    let outcome = runner.cast(shock2).target_object(survivor).resolve();
    assert_eq!(
        outcome.damage_marked(survivor),
        2,
        "survivor must not be shielded by the dead creature's prevention trigger"
    );

    // Shock P0: must take full 2 damage.
    let outcome = runner.cast(shock3).target_player(P0).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        -2,
        "controller must not be shielded by the dead creature's prevention trigger"
    );
}

fn resolve_top_stack_entry(runner: &mut GameRunner) {
    let top_id = runner
        .state()
        .stack
        .last()
        .expect("expected a stack object to resolve")
        .id;
    for _ in 0..8 {
        if !runner.state().stack.iter().any(|entry| entry.id == top_id) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve top stack entry");
    }
    panic!("top stack entry did not resolve");
}

fn ping_creature_leaving_triggers_pending(
    runner: &mut GameRunner,
    pinger: ObjectId,
    creature: ObjectId,
) {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: pinger,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate pinger");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            })
            .expect("target creature with pinger");
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay 0 activation mana");
    }
    resolve_top_stack_entry(runner);
}

/// CR 400.7 + CR 608.2k: when a damage-triggered prevention shield is left
/// pending on the stack and its recipient is blinked by full-Oracle Ephemerate
/// before the trigger resolves, the returned creature is a new object with a
/// new incarnation and must NOT receive the original event's shield.
#[test]
fn passive_damage_trigger_pending_shield_does_not_install_after_ephemerate_blink() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let preventer_oracle =
        "Whenever a creature is dealt damage, prevent the next 1 damage that would be dealt to it this turn.";
    let ephemerate_oracle =
        "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
    let pinger_oracle = "{0}: This creature deals 1 damage to any target.";
    let shock_oracle = "Shock deals 2 damage to any target.";

    scenario.add_enchantment_from_oracle(P0, "Damage Shield", preventer_oracle);
    let pinger = scenario
        .add_creature_from_oracle(P0, "Pinger", 1, 1, pinger_oracle)
        .id();
    let creature = scenario.add_creature(P0, "Shielded Creature", 3, 3).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, ephemerate_oracle)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", false, shock_oracle)
        .id();
    let mut runner = scenario.build();

    let incarnation_before = runner.state().objects[&creature].incarnation;

    // Activate Pinger targeting the creature and resolve only the ping ability,
    // leaving the triggered Damage Shield ability pending on the stack.
    ping_creature_leaving_triggers_pending(&mut runner, pinger, creature);
    assert_eq!(runner.state().objects[&creature].damage_marked, 1);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the passive damage-prevention trigger is pending on the stack"
    );

    // Blink the recipient with full-Oracle Ephemerate while the prevention
    // trigger is still pending on the stack.
    runner.cast(ephemerate).target_object(creature).commit();
    assert_eq!(runner.state().stack.len(), 2);
    resolve_top_stack_entry(&mut runner);

    let incarnation_after_blink = runner.state().objects[&creature].incarnation;
    assert_ne!(
        incarnation_after_blink, incarnation_before,
        "reach guard: Ephemerate returned the creature as a new incarnation (CR 400.7)"
    );
    assert_eq!(runner.state().objects[&creature].damage_marked, 0);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the original damage-prevention trigger is still pending"
    );

    // Resolve the original trigger: it must NOT install a shield on the
    // returned creature's new incarnation.
    resolve_top_stack_entry(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().objects[&creature]
            .replacement_definitions
            .is_empty(),
        "CR 400.7 + CR 608.2k: the original trigger must not install a shield on the blinked incarnation"
    );

    // Shock deals 2 damage to the returned creature; none of it is prevented.
    let outcome = runner.cast(shock).target_object(creature).resolve();
    assert_eq!(
        outcome.damage_marked(creature),
        2,
        "returned incarnation takes full 2 damage from Shock"
    );
}

/// CR 400.7 + CR 608.2k + CR 120.1: if the damage-triggered prevention shield
/// is left pending, its recipient is blinked by full-Oracle Ephemerate, and the
/// SAME unchanged dealer deals an equal amount of damage to the returned
/// creature before the original trigger resolves, the original trigger remains
/// bound to its own pre-blink occurrence and must not install a second shield
/// onto the returned incarnation.
#[test]
fn passive_damage_trigger_pending_shield_ignores_second_equal_damage_from_same_dealer_after_blink()
{
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let preventer_oracle =
        "Whenever a creature is dealt damage, prevent the next 1 damage that would be dealt to it this turn.";
    let ephemerate_oracle =
        "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
    let pinger_oracle = "{0}: This creature deals 1 damage to any target.";
    let shock_oracle = "Shock deals 2 damage to any target.";

    scenario.add_enchantment_from_oracle(P0, "Damage Shield", preventer_oracle);
    let pinger = scenario
        .add_creature_from_oracle(P0, "Pinger", 1, 1, pinger_oracle)
        .id();
    let creature = scenario.add_creature(P0, "Shielded Creature", 3, 4).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, ephemerate_oracle)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", false, shock_oracle)
        .id();
    let mut runner = scenario.build();

    let pinger_incarnation = runner.state().objects[&pinger].incarnation;
    let incarnation_before = runner.state().objects[&creature].incarnation;

    // 1. First ping: deals 1 damage to `creature` (incarnation_before) and puts
    //    Trigger #1 on the stack.
    ping_creature_leaving_triggers_pending(&mut runner, pinger, creature);
    assert_eq!(runner.state().stack.len(), 1);

    // 2. Blink `creature` with full-Oracle Ephemerate while Trigger #1 is
    //    still pending on the stack.
    runner.cast(ephemerate).target_object(creature).commit();
    resolve_top_stack_entry(&mut runner);
    let incarnation_after_blink = runner.state().objects[&creature].incarnation;
    assert_ne!(incarnation_after_blink, incarnation_before);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: Trigger #1 is still pending on the stack"
    );

    // 3. Before Trigger #1 resolves, the same unchanged dealer (`pinger`) deals
    //    an equal 1 damage to the returned `creature`, putting Trigger #2 on
    //    top of the stack above Trigger #1.
    assert_eq!(
        runner.state().objects[&pinger].incarnation,
        pinger_incarnation,
        "reach guard: dealer incarnation is unchanged"
    );
    ping_creature_leaving_triggers_pending(&mut runner, pinger, creature);
    assert_eq!(runner.state().objects[&creature].damage_marked, 1);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: Trigger #2 sits above Trigger #1 on the stack"
    );

    // 4. Resolve Trigger #2 (for the post-blink occurrence): installs 1 shield
    //    on the returned creature.
    resolve_top_stack_entry(&mut runner);
    assert_eq!(
        runner.state().objects[&creature]
            .replacement_definitions
            .len(),
        1,
        "reach guard: Trigger #2 installs one shield on the current incarnation"
    );

    // 5. Resolve Trigger #1 (for the pre-blink occurrence): must NOT install a
    //    second shield on the returned creature despite the equal later ledger
    //    row from the same dealer.
    resolve_top_stack_entry(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&creature]
            .replacement_definitions
            .len(),
        1,
        "CR 400.7 + CR 608.2k: Trigger #1 must remain bound to the pre-blink occurrence and install nothing"
    );

    // 6. Shock deals 2 damage: only Trigger #2's single 1-damage shield applies
    //    (1 from second ping + 1 unprevented from Shock = 2 damage marked).
    let outcome = runner.cast(shock).target_object(creature).resolve();
    assert_eq!(
        outcome.damage_marked(creature),
        2,
        "only one 1-damage shield may apply to the returned incarnation"
    );
}

/// CR 115.1c + CR 608.2c: Elvish Healer declares one "any target" recipient; the
/// "prevent the next 2 damage instead" shorthand reuses it and adds no second prompt.
#[test]
fn elvish_healer_prompts_once_and_shields_only_the_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let healer = scenario
        .add_creature_from_oracle(P0, "Elvish Healer", 1, 2, ELVISH_HEALER)
        .id();
    let a = scenario
        .add_creature(P0, "A", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    assert!(
        matches!(
            runner.state().objects[&healer].abilities[0]
                .sub_ability
                .as_deref()
                .and_then(|branch| branch.condition.as_ref()),
            Some(engine::types::ability::AbilityCondition::ConditionInstead { inner })
                if matches!(inner.as_ref(), engine::types::ability::AbilityCondition::TargetMatchesFilter { .. })
        ),
        "Elvish Healer must parse its green-creature instead rider as a target condition: {:#?}",
        runner.state().objects[&healer].abilities[0]
    );

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: healer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Elvish Healer");
    assert!(
        matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "Elvish Healer must prompt for its any-target recipient, got {:?}",
        result.waiting_for
    );
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("choose target creature A");
    assert!(
        !matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
        "the instead-shorthand must not prompt for a second target, got {:?}",
        result.waiting_for
    );
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(
        outcome.damage_marked(a),
        0,
        "CR 608.2c: the green target receives the 2-damage instead shield"
    );
    assert_eq!(outcome.zone_of(a), Zone::Battlefield);
    assert_eq!(
        outcome.zone_of(b),
        Zone::Graveyard,
        "unchosen B is unshielded"
    );
}

/// Exile `id` and return it to the battlefield: the same ObjectId, but a new
/// object with a bumped incarnation (CR 400.7).
fn blink(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Battlefield, &mut events);
}

/// CR 400.7 + CR 615.1: an object-hosted prevention shield belongs to its
/// host's incarnation at install time. Aven Redeemer shields creature A, then
/// A blinks (same id, new incarnation). The returned A is a new object with no
/// memory of the departed shield, so damage dealt to it must NOT be prevented.
#[test]
fn aven_redeemer_shield_does_not_protect_a_blinked_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = scenario
        .add_creature_from_oracle(P0, "Aven Redeemer", 2, 2, AVEN_REDEEMER)
        .id();
    let a = scenario.add_creature(P0, "A", 3, 3).id();
    let b = scenario.add_creature(P0, "B", 3, 3).id();
    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM)
        .id();
    let mut runner = scenario.build();

    // Shield creature A.
    runner.activate(aven, 0).target_object(a).resolve();
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    let incarnation_before = runner.state().objects[&a].incarnation;

    // Blink A: same ObjectId, new incarnation (CR 400.7).
    blink(&mut runner, a);
    assert_ne!(
        runner.state().objects[&a].incarnation,
        incarnation_before,
        "the blinked creature returned as a new object"
    );

    // Pyroclasm deals 2 damage to each creature. The shield was installed on
    // A's prior incarnation, so it must NOT protect the returned A.
    let outcome = runner.cast(pyroclasm).resolve();
    assert_eq!(
        outcome.damage_marked(a),
        2,
        "the shield does not protect a blinked creature's new incarnation"
    );
    assert_eq!(
        outcome.zone_of(a),
        Zone::Battlefield,
        "3-toughness A survives 2 un-prevented damage"
    );
    assert_eq!(
        outcome.damage_marked(b),
        2,
        "unshielded B takes full damage"
    );
}
