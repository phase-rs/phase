//! Regression for GitHub issue #4960 — Nova Flame deals no damage.
//!
//! Oracle: "Put X +1/+1 counters on target creature you control. It deals
//! damage equal to its power to each other creature."
//!
//! This is the "counter-then-sweep" sibling of the Chandra's Ignition class
//! ("target creature you control deals damage equal to its power to each
//! other creature and each opponent") and the Ambuscade one-sided-fight class
//! ("Target creature you control gets +1/+1... It deals damage equal to its
//! power to target creature an opponent controls"). The AST for Nova Flame
//! (dumped via a throwaway `parse_oracle_text` probe) is:
//!
//!   PutCounter { counter_type: P1P1, count: Variable("X"), target: Typed(Creature, You) }
//!   sub_ability:
//!     DamageAll { amount: Power(Anaphoric), target: Typed(Creature, [Another]) }
//!       <- `damage_source` is ABSENT (None)
//!
//! Root cause: the "It deals damage..." clause's damage recipient ("each
//! other creature") is a `TargetFilter::Typed` carrying `FilterProp::Another`
//! (CR 115.10a's "other"/"another" exclusion), but the parser's anaphoric
//! damage-subject binder only recognized two recipient shapes as "distinct
//! from the anaphoric subject": a fresh opponent-controlled target
//! (`target_filter_is_fresh_opponent_typed`, the Ambuscade class) and a
//! self-reference recipient (the Karplusan Yeti fight-back class). An
//! `Another`-tagged board-sweep recipient (the Chandra's Ignition /
//! Nova Flame class) matched neither, so `bind_anaphoric_damage_subject_keep_
//! recipient` declined to bind `damage_source = Some(DamageSource::Target)`,
//! and `replace_target_with_parent`'s own `Another` guard correctly declined
//! to clobber the recipient but also left `damage_source` untouched. With
//! `damage_source` staying `None`, the runtime one-sided-fight fallback in
//! `game/quantity.rs` (gated on `damage_source == Some(DamageSource::Target)`)
//! never fires, so `Power{Anaphoric}` had no live referent and resolved to 0
//! (`crates/engine/src/game/quantity.rs`, `ObjectScope::Anaphoric` arm,
//! final `.unwrap_or(0)`) — Nova Flame dealt 0 damage regardless of X.
//!
//! Fix: `target_filter_is_distinct_recipient` (nee
//! `target_filter_is_fresh_opponent_typed`,
//! `crates/engine/src/parser/oracle_effect/mod.rs`) now also recognizes an
//! `Another`-tagged typed recipient as structurally distinct from the
//! anaphoric subject, so `bind_anaphoric_damage_subject_keep_recipient` binds
//! `damage_source = Some(Target)` for this class too — the boosted/counter'd
//! creature (parent target) becomes the damage source, its live (post-counter)
//! power feeds `Power{Anaphoric}` via the existing one-sided-fight runtime
//! fallback (CR 608.2h: power is read after the earlier same-effect
//! instruction — the +1/+1 counters — has been applied), and the "each other
//! creature" recipient is preserved verbatim (never the boosted creature
//! itself).
//!
//! This test drives the real `parse_oracle_text` + cast/resolve pipeline (not
//! a synthetic AST), so it fails end-to-end on a revert of the fix.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const NOVA_FLAME_ORACLE: &str = "Put X +1/+1 counters on target creature you control. \
It deals damage equal to its power to each other creature.";

fn red_pool(runner: &mut engine::game::scenario::GameRunner, amount: usize) {
    for _ in 0..amount {
        let unit = ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// CR 107.3 + CR 120.1 + CR 208.1 + CR 608.2c/h: cast Nova Flame for X=6 onto
/// P0's own 2/2. The boosted creature must end up an 8/8 (2 base + 6 counters,
/// CR 122.1) and, per CR 608.2h, the "It deals damage equal to its power"
/// clause must read that POST-counter power (8) — not the pre-resolution
/// power (2), and not 0 (the pre-fix bug).
///
/// "Each other creature" (CR 115.10a) sweeps every OTHER creature regardless
/// of controller: P0's own bystander AND P1's creature both take 8 damage,
/// while the boosted creature itself takes none (it is the damage source, per
/// CR 120.1, attributed to the parent target by `damage_source =
/// Some(DamageSource::Target)`).
#[test]
fn nova_flame_deals_boosted_power_to_each_other_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Nova Flame", false, NOVA_FLAME_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();

    // The creature Nova Flame is cast onto: 2/2 -> 8/8 after 6 +1/+1 counters.
    let boosted = scenario.add_creature(P0, "Invisible Woman", 2, 2).id();
    // P0's own OTHER creature — high toughness so it survives to let us read
    // `damage_marked` directly instead of racing SBA cleanup.
    let own_bystander = scenario.add_creature(P0, "Camp Cook", 1, 9).id();
    // P1's creature — also high toughness for the same reason.
    let foe = scenario.add_creature(P1, "Stone Ogre", 3, 9).id();

    let mut runner = scenario.build();
    red_pool(&mut runner, 7);

    let outcome = runner.cast(spell).x(6).target_object(boosted).resolve();
    let state = outcome.state();

    let counters = state
        .objects
        .get(&boosted)
        .and_then(|o| {
            o.counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
        })
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 6,
        "X=6 must put 6 +1/+1 counters on the targeted creature; got {counters}"
    );

    let boosted_power = state.objects[&boosted].power.unwrap_or(0);
    assert_eq!(
        boosted_power, 8,
        "the counter'd creature's power must be 8 (2 base + 6 counters); got {boosted_power}"
    );

    let boosted_damage = state.objects[&boosted].damage_marked;
    assert_eq!(
        boosted_damage, 0,
        "the counter'd creature is the damage SOURCE (CR 120.1), not a recipient \
         of its own sweep — it must take 0; got {boosted_damage}"
    );

    let bystander_damage = state.objects[&own_bystander].damage_marked;
    assert_eq!(
        bystander_damage, 8,
        "#4960: P0's OTHER creature must take the boosted creature's POST-counter \
         power (8), not 0 (the pre-fix bug: `damage_source` never bound, so \
         Power{{Anaphoric}} had no referent) and not 2 (pre-counter power, the \
         CR 608.2h ordering bug); got {bystander_damage}"
    );

    let foe_damage = state.objects[&foe].damage_marked;
    assert_eq!(
        foe_damage, 8,
        "#4960: the opponent's creature must also take 8 damage from the \
         'each other creature' sweep (CR 115.10a — every other creature \
         regardless of controller); got {foe_damage}"
    );
}
