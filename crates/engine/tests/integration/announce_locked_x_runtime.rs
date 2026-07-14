//! t99 — RUNTIME WITNESSES for the ANNOUNCE-LOCKED X channel.
//!
//! The population is every face whose Oracle text reads
//!
//!     "where X is <count> as you cast this spell"        (CR 601.2a-b)
//!     "where X is <count> as you activate this ability"  (CR 602.2b -> 601.2b-i)
//!
//! The printed qualifier is LOAD-BEARING. CR 107.3c makes a text-defined X a LIVE value
//! by default — "Note that the value of X may change while that spell or ability is on
//! the stack" — and the qualifier is the card text that OVERRIDES that default, pinning
//! the count to the announcement step. So the ONLY question that matters is:
//!
//!     does the value LOCK at announcement, or is it re-read at resolution?
//!
//! A test that casts and resolves against a STATIC board cannot answer that: a locked
//! snapshot and a live re-read produce the same number. Every witness below therefore
//! CHANGES THE BOARD while the spell/ability is on the stack (`commit()` ->
//! `state_mut()` -> `resolve()`) and asserts the announce-time count survived. Without
//! that mid-stack mutation these tests would be vacuous, and the bug they pin —
//! resolution-time re-evaluation — would pass them.
//!
//! HARNESS NOTE (inherited from t96, learned the hard way): `add_card_to_hand` builds a
//! name-only object "without rules text" — a probe built on it reads 0 for everything,
//! which looks exactly like a fabrication. Every card below is synthesized from its
//! VERBATIM Oracle text (pool export) via the `*_from_oracle` builders.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Exile a battlefield permanent by id, straight out of the state. Used to shrink the
/// counted population WHILE the announce-locked spell is on the stack.
fn remove_from_battlefield(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    state.battlefield.retain(|o| *o != id);
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.zone = Zone::Exile;
    }
}

fn damage_on(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .map_or(0, |o| o.damage_marked)
}

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 1 — Jaws of Stone. THE witness named in the charter.
// ─────────────────────────────────────────────────────────────────────────────

/// Jaws of Stone `{4}{R}` (sorcery):
/// "Jaws of Stone deals X damage divided as you choose among any number of targets,
///  where X is the number of Mountains you control as you cast this spell."
///
/// Cast with **3** Mountains, then EXILE one while the spell is on the stack. At
/// resolution the board shows **2** Mountains.
///
///   * announce-locked (CORRECT, CR 601.2b) -> 3 damage
///   * resolution-time re-read (the bug)    -> 2 damage
///   * unbound `Variable("X")`               -> 0 damage (the pre-fix honest red)
///
/// Three distinguishable outcomes, so this assertion cannot pass by accident.
#[test]
fn jaws_of_stone_locks_the_mountain_count_at_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // `add_basic_land` stamps the CR 205.4 land subtype ("Mountain"), which is what
    // `ObjectCount{ Mountain }` actually filters on — a name-only land would count 0.
    let mountains: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let victim = scenario
        .add_creature_from_oracle(P1, "Target Dummy", 0, 20, "")
        .id();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Jaws of Stone",
            false,
            "Jaws of Stone deals X damage divided as you choose among any number of targets, \
             where X is the number of Mountains you control as you cast this spell.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::Red], 4));
        b.id()
    };
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 6);

    // ---- announce with 3 Mountains, all damage onto the single victim
    let mut committed = runner
        .cast(spell)
        .target_object(victim)
        .distribute_among(&[(TargetRef::Object(victim), 3)])
        .commit();

    // The announced value must already be on the stack object (CR 601.2b), BEFORE
    // anything resolves. This is the direct observation of the lock.
    let announced = committed
        .state()
        .stack
        .last()
        .and_then(|e| e.ability())
        .and_then(|a| a.chosen_x);
    assert_eq!(
        announced,
        Some(3),
        "CR 601.2b: X must be ANNOUNCED (locked) while the spell is on the stack. \
         MEASURED chosen_x = {announced:?}"
    );

    // ---- the board changes UNDER the spell: one Mountain leaves.
    remove_from_battlefield(committed.state_mut(), mountains[0]);
    let mountains_at_resolution = committed
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            committed
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.name.starts_with("Mountain"))
        })
        .count();
    assert_eq!(
        mountains_at_resolution, 2,
        "NON-VACUITY: the mid-stack mutation must actually shrink the counted population, \
         otherwise this test cannot tell a lock from a live re-read. MEASURED: \
         {mountains_at_resolution}"
    );

    committed.resolve();

    let dealt = damage_on(&runner, victim);
    assert_eq!(
        dealt, 3,
        "CR 601.2b overrides CR 107.3c: 'as you cast this spell' LOCKS the Mountain count at \
         announcement. 3 Mountains at cast => 3 damage, even though only 2 remain at \
         resolution. A 2 here means the count is being re-read at resolution (the rules-wrong \
         behaviour this channel exists to prevent); a 0 means X never bound at all. \
         MEASURED: {dealt}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 2 — Monstrous Onslaught. A LYING GREEN on main: it bound X to a
// resolution-time Aggregate and dropped the lock entirely.
// ─────────────────────────────────────────────────────────────────────────────

/// Monstrous Onslaught `{4}{R}` (sorcery):
/// "Monstrous Onslaught deals X damage divided as you choose among any number of target
///  creatures, where X is the greatest power among creatures you control as you cast
///  this spell."
///
/// Before this unit, this face carried NO `Unimplemented` and NO bare `Variable("X")` —
/// it rendered as 100% supported while binding X to `Aggregate{Max, Power, creatures you
/// control}`, re-read at resolution. Kill the big creature in response and the damage
/// shrank. That is the manufactured-lying-green class, and it was shipped.
///
/// Cast with a 5/5 out, then EXILE it while the spell is on the stack, leaving a 1/1.
///   * announce-locked (CORRECT) -> 5 damage
///   * resolution-time re-read   -> 1 damage   <- what main did
#[test]
fn monstrous_onslaught_locks_the_greatest_power_at_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let big = scenario
        .add_creature_from_oracle(P0, "Big Friend", 5, 5, "")
        .id();
    scenario.add_creature_from_oracle(P0, "Small Friend", 1, 1, "");
    let victim = scenario
        .add_creature_from_oracle(P1, "Target Dummy", 0, 20, "")
        .id();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Monstrous Onslaught",
            false,
            "Monstrous Onslaught deals X damage divided as you choose among any number of \
             target creatures, where X is the greatest power among creatures you control as \
             you cast this spell.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::Red], 4));
        b.id()
    };
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 6);

    let mut committed = runner
        .cast(spell)
        .target_object(victim)
        .distribute_among(&[(TargetRef::Object(victim), 5)])
        .commit();

    let announced = committed
        .state()
        .stack
        .last()
        .and_then(|e| e.ability())
        .and_then(|a| a.chosen_x);
    assert_eq!(
        announced,
        Some(5),
        "CR 601.2b: greatest power among creatures you control = 5, locked at announcement. \
         MEASURED chosen_x = {announced:?}"
    );

    // The 5/5 dies in response; only the 1/1 remains at resolution.
    remove_from_battlefield(committed.state_mut(), big);
    committed.resolve();

    let dealt = damage_on(&runner, victim);
    assert_eq!(
        dealt, 5,
        "The greatest power is LOCKED at cast (5), not re-read at resolution (1). A 1 here is \
         the lying-green behaviour main shipped: the 'as you cast this spell' qualifier was \
         silently dropped and X bound to a resolution-time Aggregate. MEASURED: {dealt}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 3 — the ACTIVATED-ABILITY surface (CR 602.2b). One channel, two surfaces.
// ─────────────────────────────────────────────────────────────────────────────

/// Endurance Bobblehead `{3}, {T}`:
/// "Up to X target creatures you control get +1/+0 and gain indestructible until end of
///  turn, where X is the number of Bobbleheads you control as you activate this ability."
///
/// CR 602.2b: "The remainder of the process for activating an ability is identical to the
/// process for casting a spell listed in rules 601.2b-i." So the announce-time lock is the
/// SAME rule on this surface, and it must be the SAME code path — this witness is what
/// proves the channel is not spell-only.
///
/// X lives in `multi_target.max`, which is consumed at target selection (announcement).
/// Activate with 3 Bobbleheads out and 3 friendly creatures; all 3 must become legal
/// targets. If X were unbound this would offer "up to 0 targets" and pump nobody.
#[test]
fn endurance_bobblehead_locks_the_bobblehead_count_at_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bobbleheads: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_from_oracle(P0, &format!("Bobblehead {i}"), 1, 1, "")
                .with_subtypes(vec!["Bobblehead"])
                .id()
        })
        .collect();
    let friends: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_from_oracle(P0, &format!("Friend {i}"), 2, 2, "")
                .id()
        })
        .collect();
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Endurance Bobblehead",
            0,
            0,
            "{T}: Add one mana of any color.\n{3}, {T}: Up to X target creatures you control \
             get +1/+0 and gain indestructible until end of turn, where X is the number of \
             Bobbleheads you control as you activate this ability. Activate only as a sorcery.",
        )
        .with_subtypes(vec!["Bobblehead"])
        .id();
    let _ = &bobbleheads;
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 4);

    // 4 Bobbleheads on the battlefield (3 + the source itself) => X = 4, but only
    // 3 friendly creatures exist, so all 3 are pumpable.
    let powers_before: Vec<i32> = friends
        .iter()
        .map(|id| runner.state().objects[id].power.unwrap_or(0))
        .collect();
    assert_eq!(
        powers_before,
        vec![2, 2, 2],
        "NON-VACUITY: the friends must start at 2 power, else the +1/+0 assertion below \
         cannot discriminate. MEASURED: {powers_before:?}"
    );

    runner
        .activate(source, 1)
        .target_objects(&friends)
        .resolve();

    let powers_after: Vec<i32> = friends
        .iter()
        .map(|id| runner.state().objects[id].power.unwrap_or(0))
        .collect();
    assert_eq!(
        powers_after,
        vec![3, 3, 3],
        "CR 602.2b -> CR 601.2b: 'as you activate this ability' announces X exactly as a \
         spell announces it, so `multi_target.max` = the Bobblehead count and all 3 friendly \
         creatures are legal targets. An unbound X offers 'up to 0 targets' and pumps nobody \
         (all still 2/2). MEASURED: {powers_after:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// NEGATIVE CONTROL — CR 107.3k independence must NOT regress (#96's pin).
// ─────────────────────────────────────────────────────────────────────────────

/// CR 107.3c: a text-defined X with **no** announce-lock qualifier is a LIVE value —
/// "the value of X may change while that spell or ability is on the stack". The channel
/// this unit adds must therefore be INERT on such a face: it publishes `chosen_x` only
/// when `announced_x.is_some()`, and a card with no lock qualifier parses to
/// `announced_x = None`.
///
/// This is the control that proves the fix is SCOPED. If the announce-lock recognizer
/// over-matched (e.g. by treating any "where X is …" tail as locked), this face's count
/// would freeze at announcement and the assertion below would fail — so the control
/// cannot pass vacuously.
#[test]
fn control_unlocked_where_x_stays_live_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblins: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_from_oracle(P0, &format!("Goblin {i}"), 1, 1, "")
                .id()
        })
        .collect();
    let victim = scenario
        .add_creature_from_oracle(P1, "Target Dummy", 0, 20, "")
        .id();
    // Same SHAPE as Jaws of Stone, minus the lock qualifier: X is defined by the text but
    // NOT pinned to announcement, so CR 107.3c's default applies and it stays live.
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Unlocked Bolt",
            false,
            "Unlocked Bolt deals X damage to target creature, where X is the number of \
             creatures you control.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::Red], 1));
        b.id()
    };
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 3);

    let mut committed = runner.cast(spell).target_object(victim).commit();

    // No announce-lock => nothing is published onto the stack object.
    let announced = committed
        .state()
        .stack
        .last()
        .and_then(|e| e.ability())
        .and_then(|a| a.chosen_x);
    assert_eq!(
        announced, None,
        "CR 107.3c: an UNLOCKED text-defined X must NOT be announced/frozen. The announce-lock \
         channel must be inert here. MEASURED chosen_x = {announced:?}"
    );

    // A goblin dies in response. With no lock, resolution re-reads the board (CR 107.3c).
    remove_from_battlefield(committed.state_mut(), goblins[0]);
    committed.resolve();

    let dealt = damage_on(&runner, victim);
    assert_eq!(
        dealt, 2,
        "CR 107.3c: with NO 'as you cast this spell' qualifier the count is LIVE and must be \
         re-read at resolution — 2 creatures remain, so 2 damage. A 3 here means the \
         announce-lock recognizer OVER-MATCHED and froze a value the rules say may change. \
         MEASURED: {dealt}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// SAVE-COMPAT — `announced_x` is new PERSISTED state (it rides ResolvedAbility on
// the stack, and AbilityDefinition in card data). Both directions are exercised.
// ─────────────────────────────────────────────────────────────────────────────

/// `#[serde(default, skip_serializing_if = "Option::is_none")]` on both carriers, checked
/// rather than asserted:
///
///   * FORWARD (old save -> new binary): a `None` carrier omits the key entirely, so a
///     fresh save is byte-identical to a pre-field save on this axis — and re-reading that
///     shape IS the old-save path. It must default to `None`, not fail.
///   * BACKWARD (new save -> old binary): neither struct sets `deny_unknown_fields`, so an
///     older binary reading a save that carries the key ignores it rather than erroring.
///   * LIVE: a populated announce-locked expression round-trips intact.
#[test]
fn announced_x_is_save_compatible() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, ResolvedAbility,
        TargetFilter,
    };

    // ---- FORWARD: None is omitted from the wire entirely.
    let plain = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let plain_json = serde_json::to_string(&plain).expect("serialize");
    assert!(
        !plain_json.contains("announced_x"),
        "a None announced_x must NOT be written, so a fresh save stays byte-identical to a \
         pre-field save on this axis. GOT: {plain_json}"
    );
    let back: AbilityDefinition =
        serde_json::from_str(&plain_json).expect("a save with no announced_x key must load");
    assert_eq!(
        back.announced_x, None,
        "the old-save shape (key absent) must default to None, not fail"
    );

    // ---- LIVE: a populated announce-locked count round-trips intact.
    let mut locked = plain.clone();
    locked.announced_x = Some(QuantityExpr::Ref {
        qty: QuantityRef::CostXPaid,
    });
    let locked_json = serde_json::to_string(&locked).expect("serialize");
    assert!(
        locked_json.contains("announced_x"),
        "a live value must be written"
    );
    let locked_back: AbilityDefinition = serde_json::from_str(&locked_json).expect("round-trip");
    assert_eq!(
        locked_back.announced_x,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid
        }),
        "the announce-locked expression must survive a save/load round-trip"
    );

    // ---- ResolvedAbility (the STACK-persisted carrier) — same two directions.
    let mut resolved = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(1),
        P0,
    );
    let resolved_json = serde_json::to_string(&resolved).expect("serialize");
    assert!(
        !resolved_json.contains("announced_x"),
        "a None announced_x must be omitted from a stack entry too. GOT: {resolved_json}"
    );
    let resolved_back: ResolvedAbility =
        serde_json::from_str(&resolved_json).expect("pre-field stack entry must load");
    assert_eq!(resolved_back.announced_x, None);

    resolved.announced_x = Some(QuantityExpr::Fixed { value: 7 });
    let live_json = serde_json::to_string(&resolved).expect("serialize");
    let live_back: ResolvedAbility = serde_json::from_str(&live_json).expect("round-trip");
    assert_eq!(
        live_back.announced_x,
        Some(QuantityExpr::Fixed { value: 7 }),
        "a mid-announcement pause must round-trip the locked X definition"
    );
}
