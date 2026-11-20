//! Regression for issue #11132: in "target creature … gets +X/…, where X is
//! its power", "its" names the target, not the spell or ability source.
//!
//! https://github.com/phase-rs/phase/issues/11132
//!
//! CR 608.2c: "… read the whole text and apply the rules of English to the
//! text."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ObjectScope, QuantityExpr, QuantityRef};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const RUSH_OF_BLOOD: &str = "Target creature gets +X/+0 until end of turn, where X is its power.";
const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";
const BEFUDDLE: &str = "Target creature gets -4/-0 until end of turn.\nDraw a card.";
const WINE_OF_BLOOD_AND_IRON: &str = "{4}: Target creature gets +X/+0 until end of turn, where X is its power. Sacrifice this artifact at the beginning of the next end step.";
const BERSERK: &str = "Cast this spell only before the combat damage step.\nTarget creature gains trample and gets +X/+0 until end of turn, where X is its power. At the beginning of the next end step, destroy that creature if it attacked this turn.";
const FATAL_FRENZY: &str = "Until end of turn, target creature you control gains trample and gets +X/+0, where X is its power. Sacrifice it at the beginning of the next end step.";
const WINGED_TEMPLE_OF_ORAZCA: &str = "(Transforms from Hadana's Climb.)\n{T}: Add one mana of any color.\n{1}{G}{U}, {T}: Target creature you control gains flying and gets +X/+X until end of turn, where X is its power.";
const AURIOK_BLADEWARDEN: &str =
    "{T}: Target creature gets +X/+X until end of turn, where X is this creature's power.";
const RAVENOUS_GIGANTOTHERIUM: &str = "Devour 3 (As this creature enters, you may sacrifice any number of creatures. It enters with three times that many +1/+1 counters on it.)\nWhen this creature enters, it deals X damage divided as you choose among up to X target creatures, where X is its power. Each of those creatures deals damage equal to its power to this creature.";

fn mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// Casts `text` at `phase` on a P0 creature of the given P/T; returns the
/// creature's P/T and whether it has `keyword` afterwards.
fn spell_on(
    name: &str,
    text: &str,
    phase: Phase,
    pt: (i32, i32),
    keyword: Keyword,
) -> ((i32, i32), bool) {
    let mut s = GameScenario::new();
    s.at_phase(phase);
    let bear = s.add_creature(P0, "Bear", pt.0, pt.1).id();
    let spell = s.add_spell_to_hand_from_oracle(P0, name, true, text).id();
    let mut r = s.build();
    let out = r.cast(spell).target_object(bear).resolve();
    (
        out.power_toughness(bear),
        out.state().objects[&bear].has_keyword(&keyword),
    )
}

/// Rush of Blood ruling: "The value of X is determined when Rush of Blood
/// resolves. The bonus won't change later in the turn if the creature's power
/// changes." (CR 608.2h)
#[test]
fn rush_of_blood_adds_the_targets_power_once() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let bear = s.add_creature(P0, "Bear", 3, 2).id();
    let rush = s
        .add_spell_to_hand_from_oracle(P0, "Rush of Blood", true, RUSH_OF_BLOOD)
        .id();
    let growth = s
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH)
        .id();
    let mut r = s.build();
    let out = r.cast(rush).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (6, 2));
    let out = r.cast(growth).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (7, 3));
}

/// A noncreature artifact source targeting an opponent's creature: X is the
/// target's power, not the artifact's.
#[test]
fn wine_of_blood_and_iron_adds_the_targets_power() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(P0, mana(4, ManaType::Colorless));
    let wine = s
        .add_artifact_from_oracle(P0, "Wine of Blood and Iron", WINE_OF_BLOOD_AND_IRON)
        .id();
    let bear = s.add_creature(P1, "Bear", 3, 2).id();
    let mut r = s.build();
    let out = r.activate(wine, 0).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (6, 2));
}

#[test]
fn berserk_adds_the_targets_power_and_trample() {
    assert_eq!(
        spell_on(
            "Berserk",
            BERSERK,
            Phase::BeginCombat,
            (3, 2),
            Keyword::Trample
        ),
        ((6, 2), true)
    );
}

#[test]
fn fatal_frenzy_adds_the_targets_power_and_trample() {
    assert_eq!(
        spell_on(
            "Fatal Frenzy",
            FATAL_FRENZY,
            Phase::PreCombatMain,
            (3, 2),
            Keyword::Trample
        ),
        ((6, 2), true)
    );
}

/// "+X/+X" reads the target's power for both axes, before the pump applies.
#[test]
fn winged_temple_of_orazca_adds_the_targets_power_to_both_axes() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mut pool = mana(1, ManaType::Colorless);
    pool.extend(mana(1, ManaType::Green));
    pool.extend(mana(1, ManaType::Blue));
    s.with_mana_pool(P0, pool);
    let temple = s
        .add_land_from_oracle(P0, "Winged Temple of Orazca", WINGED_TEMPLE_OF_ORAZCA)
        .id();
    let bear = s.add_creature(P0, "Bear", 3, 2).id();
    let mut r = s.build();
    let index = r.state().objects[&temple].abilities.len() - 1;
    let out = r.activate(temple, index).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (6, 5));
    assert!(out.state().objects[&bear].has_keyword(&Keyword::Flying));
}

/// "this creature's power" is an explicit self-reference, not the pronoun: X
/// stays the source's power.
#[test]
fn auriok_bladewarden_keeps_its_own_power() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let auriok = s
        .add_creature_from_oracle(P0, "Auriok Bladewarden", 1, 1, AURIOK_BLADEWARDEN)
        .id();
    let bear = s.add_creature(P0, "Bear", 3, 2).id();
    let mut r = s.build();
    let out = r.activate(auriok, 0).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (4, 3));
}

/// "its power" in a damage clause names the source: only a pump's subject is
/// the pronoun's antecedent.
#[test]
fn ravenous_gigantotherium_damage_keeps_its_own_power() {
    let parsed = parse_oracle_text(
        RAVENOUS_GIGANTOTHERIUM,
        "Ravenous Gigantotherium",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let amounts: Vec<&QuantityExpr> = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .filter_map(|def| match def.effect.as_ref() {
            Effect::DealDamage { amount, .. } => Some(amount),
            _ => None,
        })
        .collect();
    assert_eq!(
        amounts,
        vec![&QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Source
            }
        }]
    );
}

/// CR 107.1b composes with the rebind: a negative target power adds +0/+0.
#[test]
fn rush_of_blood_negative_target_power_adds_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Card A"]);
    let bear = s.add_creature(P0, "Bear", 3, 3).id();
    let befuddle = s
        .add_spell_to_hand_from_oracle(P0, "Befuddle", true, BEFUDDLE)
        .id();
    let rush = s
        .add_spell_to_hand_from_oracle(P0, "Rush of Blood", true, RUSH_OF_BLOOD)
        .id();
    let mut r = s.build();
    let out = r.cast(befuddle).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (-1, 3));
    let out = r.cast(rush).target_object(bear).resolve();
    assert_eq!(out.power_toughness(bear), (-1, 3));
}
