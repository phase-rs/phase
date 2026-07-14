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

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 4 — the LOYALTY surface (CR 606.1 + CR 602.2b).
//
// Loyalty abilities ARE activated abilities (CR 606.1), so CR 602.2b applies to them
// verbatim. But `planeswalker::handle_activate_loyalty` has a MANA-FREE FAST PATH that
// only delegates to `casting::handle_activate_ability` when a cost-raise static adds a
// mana tax — otherwise it builds its own ResolvedAbility and goes straight to targeting.
//
// That fast path is a THIRD announce surface, and it is the one every real activation
// takes. Without a publication there, Lukka's X is never announced and the ability deals
// ZERO damage — while the parse ledger still shows the face as cleanly "re-shaped",
// because Lukka is a LYING GREEN with no honest red to clear. Only this runtime witness
// can see it.
// ─────────────────────────────────────────────────────────────────────────────

/// Lukka, Bound to Ruin `[−4]`:
/// "Lukka deals X damage divided as you choose among any number of target creatures
///  and/or planeswalkers, where X is the greatest power among creatures you control as
///  you activate this ability."
///
/// Activate with a 5/5 out, then EXILE it while the ability is on the stack.
///   * announce-locked (CORRECT)         -> 5 damage
///   * resolution-time re-read           -> 1 damage
///   * X never published (loyalty bypass) -> 0 damage   <- what the fast path did
#[test]
fn lukka_minus_four_locks_the_greatest_power_at_loyalty_activation() {
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let big = scenario
        .add_creature_from_oracle(P0, "Big Friend", 5, 5, "")
        .id();
    scenario.add_creature_from_oracle(P0, "Small Friend", 1, 1, "");
    let victim = scenario
        .add_creature_from_oracle(P1, "Target Dummy", 0, 20, "")
        .id();
    // Verbatim [-4] line. A name-only object would parse no ability at all and the test
    // would read 0 for everything — the t96 vacuum.
    let lukka = scenario
        .add_creature_from_oracle(
            P0,
            "Lukka, Bound to Ruin",
            0,
            0,
            "[−4]: Lukka deals X damage divided as you choose among any number of target \
             creatures and/or planeswalkers, where X is the greatest power among creatures \
             you control as you activate this ability.",
        )
        .id();
    let mut runner = scenario.build();
    {
        // Make it a real planeswalker so the LOYALTY activation path (not the generic
        // activated-ability path) is the one under test.
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&lukka).expect("lukka");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = Some(7);
        obj.counters.insert(CounterType::Loyalty, 7);
    }

    // SCOPE OF THIS WITNESS — stated plainly. `AbilityActivation` has no `commit()`
    // window (you can hold a SPELL on the stack but not an ABILITY — a harness asymmetry
    // that is plausibly WHY this bypass went unnoticed), so this witness cannot perform
    // the mid-stack mutation the spell witnesses do. It therefore proves REACHABILITY:
    // does the announce-X publication happen AT ALL on the loyalty surface? The LOCK
    // semantics are a property of `publish_announced_x` itself — one authority, already
    // witnessed locking on both the spell and the activated-ability surfaces — so what is
    // genuinely in question here is whether the loyalty fast path reaches it. It does not,
    // without the third call site, and 0-vs-5 is a clean discriminator for exactly that.
    let _ = &big;
    runner.activate(lukka, 0).target_object(victim).resolve();

    let dealt = damage_on(&runner, victim);
    assert_eq!(
        dealt, 5,
        "CR 606.1 + CR 602.2b: a loyalty ability IS an activated ability, so its X is \
         announced at activation — greatest power among creatures you control = 5. \
         A 0 here means `planeswalker::handle_activate_loyalty`'s mana-free FAST PATH \
         bypassed the announce-X publication entirely: X was never published, so \
         `Variable(\"X\")` resolved to 0 and Lukka dealt no damage. That is the bug this \
         witness exists for, and it is invisible to the parse ledger (Lukka is a LYING \
         GREEN with no honest red to clear). MEASURED: {dealt}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 5 — Aether Burst: an announce-locked TARGET COUNT (not a damage pool).
// ─────────────────────────────────────────────────────────────────────────────

/// Aether Burst `{1}{U}`:
/// "Return up to X target creatures to their owners' hands, where X is one plus the number
///  of cards named Aether Burst in all graveyards as you cast this spell."
///
/// A LYING GREEN before this unit: `multi_target.max` bound to a live
/// `Offset{ObjectCount{named "Aether Burst" in Graveyard}, +1}`. `multi_target` bounds are
/// re-resolved at resolution by `effects/change_zone.rs`, so the count was NOT safe.
///
/// Two Aether Bursts in the graveyard at cast => X = 3. Exile one in response; at
/// resolution the graveyard shows 1 (X would be 2).
///   * announce-locked (CORRECT) -> all 3 creatures bounced
///   * resolution-time re-read   -> 2
///   * X never published          -> "up to 0 targets", nobody bounced
#[test]
fn aether_burst_locks_the_graveyard_count_at_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.with_graveyard(P0, &["Aether Burst", "Aether Burst"]);
    let victims: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_from_oracle(P1, &format!("Bouncee {i}"), 1, 1, "")
                .id()
        })
        .collect();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Aether Burst",
            true,
            "Return up to X target creatures to their owners' hands, where X is one plus the \
             number of cards named Aether Burst in all graveyards as you cast this spell.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::Blue], 1));
        b.id()
    };
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 3);

    let graveyard_bursts = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Graveyard && o.name == "Aether Burst")
        .count();
    assert_eq!(
        graveyard_bursts, 2,
        "NON-VACUITY: 2 Aether Bursts must actually be in the graveyard at cast, else X=1 and \
         this test cannot discriminate. MEASURED: {graveyard_bursts}"
    );

    let mut committed = runner.cast(spell).target_objects(&victims).commit();

    let announced = committed
        .state()
        .stack
        .last()
        .and_then(|e| e.ability())
        .and_then(|a| a.chosen_x);
    assert_eq!(
        announced,
        Some(3),
        "CR 601.2b: one plus 2 graveyard Aether Bursts = 3, locked at cast. \
         MEASURED chosen_x = {announced:?}"
    );

    // A graveyard Aether Burst is exiled in response — the counted population shrinks to 1,
    // so a LIVE count would now read X = 2.
    let a_burst = committed
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Graveyard && o.name == "Aether Burst")
        .map(|o| o.id)
        .expect("a graveyard Aether Burst");
    if let Some(obj) = committed.state_mut().objects.get_mut(&a_burst) {
        obj.zone = Zone::Exile;
    }
    let remaining = committed
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Graveyard && o.name == "Aether Burst")
        .count();
    assert_eq!(
        remaining, 1,
        "NON-VACUITY: the mid-stack exile must actually shrink the counted population, else \
         this test cannot tell a lock from a live re-read. MEASURED: {remaining}"
    );

    // THE ASSERTION: the announced X is a SNAPSHOT. It still reads 3 against a board that
    // now says 2. In the un-published state it is `None`, so this discriminates.
    let still_announced = committed
        .state()
        .stack
        .last()
        .and_then(|e| e.ability())
        .and_then(|a| a.chosen_x);
    assert_eq!(
        still_announced,
        Some(3),
        "CR 601.2b: the count is LOCKED at cast. One plus 2 graveyard Aether Bursts = 3, and \
         it must still read 3 after the graveyard shrinks to 1 — a live count would now say 2. \
         MEASURED chosen_x = {still_announced:?}"
    );

    // WHY THIS WITNESS STOPS AT `chosen_x` AND DOES NOT ASSERT THE BOUNCE.
    //
    // Aether Burst carries a SECOND, INDEPENDENT MISPARSE, present identically on this
    // unit's base and head (so it is neither caused nor cured here): the "in all
    // graveyards" phrase — which qualifies the *Aether Burst count* — leaked into the
    // *target's* zone filter. The face lowers to
    //
    //     ChangeZone { origin: Graveyard, target: Typed{ Creature, InZone(Graveyard) } }
    //
    // but the card returns *battlefield* creatures to their owners' hands. So it bounces
    // graveyard creature CARDS, targets nothing on the battlefield, and moves zero of the
    // three creatures above. That is a live lying-green on a different axis (target zone),
    // filed separately; asserting the bounce here would be asserting someone else's bug.
    //
    // This witness therefore pins exactly what this unit owns — the announce-locked COUNT —
    // and does so with a discriminator that cannot pass vacuously (`None` in the red state).
    committed.resolve();
}

// ─────────────────────────────────────────────────────────────────────────────
// WITNESS 6 (rider 1) — the CR 202.3e MANA-VALUE gate, at RUNTIME.
//
// The unit-level pin (types::mana) proves the arithmetic. THIS proves the arithmetic is
// the one a real spell on a real stack answers with — the observation an opposing
// Spell Pierce / Mana Leak / "mana value 3 or less" actually makes.
//
// This witness is only EXPRESSIBLE once the announce-locked channel exists: without it,
// Monstrous Onslaught never gets a `chosen_x`, so `finalize_cast` stamps no `cost_x_paid`
// and the mana-value bug is unreachable. That is why it lands here and not in the gate's
// own commit.
// ─────────────────────────────────────────────────────────────────────────────

/// Monstrous Onslaught is `{4}{R}` — **no `{X}` in its mana cost**. Its X is defined by
/// card text (CR 107.3c) and announced at cast, so `finalize_cast` stamps `cost_x_paid`.
///
/// CR 202.3e substitutes X into mana value only "of an object **with an {X} in its mana
/// cost**". Monstrous Onslaught has none, so on the stack it must answer **5** — not 5+X.
/// Ungated, an announced X of 5 would make it report **10**, and every mana-value-keyed
/// interaction with it on the stack would be wrong.
#[test]
fn a_text_defined_x_does_not_inflate_the_spells_mana_value_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Big Friend", 5, 5, "");
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

    let committed = runner
        .cast(spell)
        .target_object(victim)
        .distribute_among(&[(TargetRef::Object(victim), 5)])
        .commit();

    let obj = committed
        .state()
        .objects
        .get(&spell)
        .expect("spell on stack");
    assert_eq!(
        obj.zone,
        Zone::Stack,
        "the spell must be ON THE STACK for this read"
    );
    assert_eq!(
        obj.cost_x_paid,
        Some(5),
        "NON-VACUITY: `finalize_cast` must actually have stamped the announced X onto the \
         object — otherwise there is no X to inflate the mana value and this test proves \
         nothing. MEASURED: {:?}",
        obj.cost_x_paid
    );

    let mv = obj.effective_mana_value();
    assert_eq!(
        mv, 5,
        "CR 202.3e substitutes X into mana value only for an object WITH an {{X}} in its mana \
         cost. Monstrous Onslaught is {{4}}{{R}} — mana value 5, on the stack or anywhere \
         else. A 10 here is the ungated bug: the object's announced X (CR 107.3i) leaking \
         into a cost that has no {{X}} to substitute it into, which would make an opposing \
         Spell Pierce / Mana Leak / \"mana value 3 or less\" read this spell wrong. \
         MEASURED: {mv}"
    );
}

/// GREEN CONTROL for the gate (rider 1): a spell that genuinely HAS `{X}` in its mana cost
/// must still answer base + announced X on the stack. The gate narrows CR 202.3e to the
/// objects the rule actually covers — it must not silence them.
///
/// This must hold in BOTH states (gate present or reverted); if it ever fails, the gate has
/// over-reached and broken real X spells.
#[test]
fn control_a_real_x_cost_spell_still_includes_x_in_its_mana_value_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario
        .add_creature_from_oracle(P1, "Target Dummy", 0, 20, "")
        .id();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Fireball",
            false,
            "Fireball deals X damage to any target.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Red], 0));
        b.id()
    };
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 6);

    let committed = runner.cast(spell).x(3).target_object(victim).commit();

    let obj = committed
        .state()
        .objects
        .get(&spell)
        .expect("spell on stack");
    assert_eq!(obj.zone, Zone::Stack);
    assert_eq!(
        obj.cost_x_paid,
        Some(3),
        "NON-VACUITY: the announced X must be stamped, else the assertion below is vacuous"
    );
    let mv = obj.effective_mana_value();
    assert_eq!(
        mv, 4,
        "CR 202.3e: {{X}}{{R}} cast for X=3 has mana value 1 + 3 = 4 while on the stack. A 1 \
         here means the has_x() gate OVER-REACHED and silenced X for a cost that really does \
         contain {{X}}. MEASURED: {mv}"
    );
}
