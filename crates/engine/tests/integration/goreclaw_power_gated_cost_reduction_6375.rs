//! Reproduction + regression for issue #6375: Goreclaw, Terror of Qal Sisma's
//! power-gated cost reduction.
//!
//! Goreclaw: "Creature spells you cast with power 4 or greater cost {2} less to
//! cast. / Whenever Goreclaw attacks, each creature you control with power 4 or
//! greater gets +1/+1 and gains trample until end of turn."
//!
//! Bug: the cost-reduction static parsed with `spell_filter: null` — the trailing
//! "with power 4 or greater" gate sat after the "spells you cast" infix and blocked
//! the type/subject trims, so the whole "creature ... with power 4+" restriction was
//! dropped and the reduction (mis)applied to EVERY spell. Confirmed reproduction: a
//! power-3 creature spell was wrongly reduced 5→3 at base.
//!
//! CR 208.1 + CR 601.2f: a cost reduction applies only to spells matching the
//! effect's filter, and only creature spells have a power to compare (CR 208.3 — a
//! noncreature card off the battlefield has power only if a P/T is printed on it).
//!
//! Finding #6 (BLOCKING): these tests drive the REAL production cost path
//! (`display_spell_cost`) casting REAL creature card FACES that carry PRINTED power
//! — built through `create_object_from_card_face` / `apply_card_face_to_object` via
//! `add_real_card` — never a hand-constructed object with a force-set `obj.power`.
//! The Goreclaw cost-reduction static is built by parsing the VERBATIM Oracle cost
//! line through `parse_static_line`, so each assertion tests the NEW parser output
//! directly (not a stale card-data fixture).

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle_static::parse_static_line;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Verbatim Goreclaw cost line (issue #6375). Parsed through the real static parser.
const GORECLAW_COST_LINE: &str =
    "Creature spells you cast with power 4 or greater cost {2} less to cast.";

/// Build a board with Goreclaw's parsed cost-reduction static under P0's control,
/// put each named REAL card into P0's hand (printed power comes from the card face),
/// and return the generic mana component the production cost path reports for each.
///
/// Returns `None` when the shared card database is unavailable (keeps the test
/// inert in environments without the fixture, mirroring the repo convention).
fn goreclaw_generics(spell_names: &[&str]) -> Option<Vec<u32>> {
    let db = shared_card_db()?;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 601.2f: Goreclaw's cost-reduction static, parsed from its verbatim Oracle
    // cost line, on a battlefield permanent P0 controls. `affected` carries the
    // "you cast" controller scope so it applies to spells P0 casts.
    let cost_static =
        parse_static_line(GORECLAW_COST_LINE).expect("Goreclaw cost line must parse to a static");
    scenario
        .add_creature(P0, "Goreclaw, Terror of Qal Sisma", 4, 5)
        .with_static_definition(cost_static);

    // Real card faces in hand — power is populated from the printed P/T by the
    // production face-application path, not force-set (Finding #6).
    let ids: Vec<ObjectId> = spell_names
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Hand, db))
        .collect();

    let mut runner = scenario.build();
    // Finalize public state so the layer pipeline runs and the static-presence
    // index reflects Goreclaw's ModifyCost static (mirrors the Cloud Key cost test).
    // Goreclaw is not in the fixture DB, so face-reapply skips it and its parsed
    // static is preserved.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let generics = ids
        .iter()
        .map(|&id| {
            let cost = engine::game::casting::display_spell_cost(runner.state(), P0, id)
                .expect("spell should have a displayable cost");
            match cost {
                ManaCost::Cost { generic, .. } => generic,
                other => panic!("expected ManaCost::Cost, got {other:?}"),
            }
        })
        .collect();
    Some(generics)
}

/// CR 208.1 + CR 601.2f (#6375): a creature spell with power CLEARLY greater than 4
/// is reduced by {2}. Fire Elemental (5/4, {3}{R}{R}) → generic 3 reduced to 1.
///
/// Reach-guard (not vacuous): the observed 3→1 delta proves the reduction actually
/// fired against a real printed-power creature face, not that the assertion is
/// trivially true. Reverting `spell_filter` to `null` keeps this reduced (null
/// reduces everything) — the sibling NOT-reduced tests are the revert-guards.
#[test]
fn power_five_creature_spell_is_reduced_by_two() {
    let Some(generics) = goreclaw_generics(&["Fire Elemental"]) else {
        return;
    };
    assert_eq!(
        generics[0], 1,
        "a power-5 creature spell must be reduced by {{2}} (generic 3 → 1)"
    );
}

/// CR 208.1 + CR 601.2f (#6375): the reported bug. A power-3 creature spell is NOT
/// reduced (power 3 < 4). Hill Giant (3/3, {3}{R}) → generic 3, unchanged.
///
/// Revert-guard: reverting the fix drops the power gate (`spell_filter: null`) and
/// this spell is WRONGLY reduced 3→1 — this assertion then fails (the reported
/// 5→3-at-base bug). Reach-guard: the same board reduces Fire Elemental (power 5)
/// 3→1, proving Goreclaw's static is active and its filter is being evaluated — so
/// Hill Giant's non-reduction is the type/power gate rejecting it, not an inert
/// static.
#[test]
fn power_three_creature_spell_is_not_reduced() {
    let Some(generics) = goreclaw_generics(&["Hill Giant", "Fire Elemental"]) else {
        return;
    };
    assert_eq!(
        generics[0], 3,
        "a power-3 creature spell must NOT be reduced (power 3 < 4); reverting the \
         power gate wrongly reduces it 3→1 (issue #6375)"
    );
    // Reach-guard: the static is live and its filter is evaluated on this board.
    assert_eq!(
        generics[1], 1,
        "reach-guard: a power-5 creature spell must be reduced 3→1 on the same board"
    );
}

/// CR 208.3 + CR 601.2f (#6375): a noncreature spell is NOT reduced — a noncreature
/// card off the battlefield has no power (reads 0), so it fails the creature type
/// gate. Arc Lightning (Sorcery, {2}{R}) → generic 2, unchanged.
///
/// Revert-guard: reverting the fix reduces EVERY spell, so this sorcery would be
/// wrongly reduced 2→0 and the assertion fails. Reach-guard: Fire Elemental (a
/// creature spell) is reduced 3→1 on the same board.
#[test]
fn noncreature_spell_is_not_reduced() {
    let Some(generics) = goreclaw_generics(&["Arc Lightning", "Fire Elemental"]) else {
        return;
    };
    assert_eq!(
        generics[0], 2,
        "a noncreature spell must NOT be reduced (type gate); reverting the filter \
         wrongly reduces it 2→0 (issue #6375)"
    );
    // Reach-guard: the static is live and its filter is evaluated on this board.
    assert_eq!(
        generics[1], 1,
        "reach-guard: a power-5 creature spell must be reduced 3→1 on the same board"
    );
}

/// CR 208.1 + CR 601.2f (#6375): boundary — power EXACTLY 4 satisfies the GE gate
/// and is reduced. Onakke Ogre (4/2, {2}{R}) → generic 2 reduced to 0.
///
/// Reach-guard: the observed 2→0 delta proves the GE-4 gate admits the boundary
/// value against a real printed-power (exactly 4) creature face.
#[test]
fn power_exactly_four_creature_spell_is_reduced_boundary() {
    let Some(generics) = goreclaw_generics(&["Onakke Ogre"]) else {
        return;
    };
    assert_eq!(
        generics[0], 0,
        "a power-4 creature spell must be reduced by {{2}} (GE boundary; generic 2 → 0)"
    );
}
