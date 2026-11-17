//! Issue #7862 — Mass Manipulation auto-resolved after one target no matter how
//! much mana was spent on X.
//!
//! Oracle text (verified against Scryfall):
//! "Gain control of X target creatures and/or planeswalkers."
//! Mana cost: `{X}{X}{U}{U}{U}{U}`.
//!
//! CR 115.1d + CR 601.2c: an exact "X target" prefix makes X targets mandatory,
//! so announcing X = 2 must require two targets. The `gain control of ` verb
//! branch only consulted the OPTIONAL prefix combinator, so the exact prefix
//! stayed on the target text and the clause carried no `multi_target`; the
//! engine then offered a single slot and the spell resolved after one pick.
//!
//! Revert-proof: removing the exact-prefix attempt from the gain-control branch
//! leaves `multi_target = None`. The parse assertion then fails, and the cast
//! offers only one slot, leaving the second creature with its original
//! controller.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, MultiTargetSpec, QuantityExpr, QuantityRef, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (Scryfall), including its uppercase `X`.
const MASS_MANIPULATION_ORACLE: &str = "Gain control of X target creatures and/or planeswalkers.";

/// Mass Manipulation's printed `{X}{X}{U}{U}{U}{U}`.
fn mass_manipulation_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::X,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
        ],
        generic: 0,
    }
}

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0]
            .mana_pool
            .add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
    }
}

/// CR 613.1b: control-changing effects are applied in Layer 2, so the resolved
/// `controller` field is the authority.
fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .unwrap_or_else(|| panic!("object {id:?} must still exist"))
        .controller
}

/// SHAPE — the printed text must parse to `GainControl` with an exact X count
/// and the creature/planeswalker disjunction, before any runtime is involved.
#[test]
fn mass_manipulation_parses_exact_x_target_count_from_printed_text() {
    let parsed = parse_oracle_text(
        MASS_MANIPULATION_ORACLE,
        "Mass Manipulation",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    let ability = parsed
        .abilities
        .iter()
        .find(|ability| matches!(&*ability.effect, Effect::GainControl { .. }))
        .expect("Mass Manipulation must parse to GainControl");

    assert_eq!(
        ability.multi_target,
        Some(MultiTargetSpec::exact(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string()
            }
        })),
        "\"X target\" must produce a mandatory exact-X count, got {:?}",
        ability.multi_target
    );

    let Effect::GainControl { target } = &*ability.effect else {
        unreachable!("the ability was selected by that pattern");
    };
    let TargetFilter::Or { filters } = target else {
        panic!("expected the and/or filter to parse as Or, got {target:?}");
    };
    assert_eq!(
        filters.len(),
        2,
        "and/or must keep both creature and planeswalker legs, got {filters:?}"
    );
}

/// RUNTIME — cast Mass Manipulation for X = 2 through the ordinary cast and
/// target-selection pipeline and require both selected creatures to change
/// controller. A single offered slot leaves the second creature with P1.
#[test]
fn mass_manipulation_x_two_requires_and_steals_two_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let spell = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Mass Manipulation",
            false,
            MASS_MANIPULATION_ORACLE,
        );
        builder.with_mana_cost(mass_manipulation_cost());
        builder.id()
    };

    let mut runner = scenario.build();

    // Pre-state guard: both creatures start with the opponent, so the control
    // assertions below cannot pass vacuously.
    assert_eq!(controller_of(&runner, first), P1, "before the cast");
    assert_eq!(controller_of(&runner, second), P1, "before the cast");

    // X = 2 → `{2}{2}{U}{U}{U}{U}` = 4 generic (two X shards) + 4 blue.
    add_mana(&mut runner, ManaType::Colorless, 4);
    add_mana(&mut runner, ManaType::Blue, 4);

    let outcome = runner
        .cast(spell)
        .x(2)
        .target_objects(&[first, second])
        .resolve();

    assert_eq!(
        controller_of(&runner, first),
        P0,
        "the first selected creature must change controller"
    );
    assert_eq!(
        controller_of(&runner, second),
        P0,
        "X=2 must require two targets; a creature still controlled by P1 means only one \
         target slot was offered (issue #7862)"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the spell must resolve completely, got {:?}",
        outcome.final_waiting_for()
    );
}
