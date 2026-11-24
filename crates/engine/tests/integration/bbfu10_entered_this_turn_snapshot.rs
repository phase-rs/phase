//! BB-FU10 PROBE — `parse_entered_this_turn_ref` emits the live-board
//! `QuantityRef::EnteredThisTurn`, which under-counts a permanent that entered
//! the battlefield this turn and has since LEFT.
//!
//! CR 608.2i (look-back): "Some effects look back in time ... they don't need to
//! be currently in the zone they were in at the time of that previous game state
//! or action ... as long as they did so at the specified time."
//!
//! Scryfall ruling, Hobgoblin Bandit Lord (card 09e9dc36-f2d8-4384-98cb-e44c00b02433):
//! "Hobgoblin Bandit Lord's activated ability only counts the number of Goblins
//! that entered the battlefield this turn. It doesn't matter if those Goblins are
//! still on the battlefield as it resolves."
//!
//! Contrast Tromell, Seymour's Butler ("the number of nontoken creatures you
//! control that entered this turn"), whose ruling says to "look at the nontoken
//! creatures you control and count each one that entered this turn" — a LIVE set
//! (CR 608.2h). That form must NOT migrate.

use engine::game::engine::apply;
use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::quantity::resolve_quantity;
use engine::game::restrictions::{check_activation_restrictions, record_battlefield_entry};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ActivationRestriction, Comparator, ControllerRef, Effect, FilterProp,
    ParsedCondition, PlayerScope, QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    EntryTokenStatus, GameState, PersistedGameState, PersistedRestoreError,
    PersistedRestoreFinalization, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HOBGOBLIN: &str = "Other Goblins you control get +1/+1.\n{R}, {T}: This creature deals \
damage equal to the number of Goblins that entered the battlefield under your control this turn \
to any target.";

// --- BB-FU10 verbatim Oracle text (MTGJSON `data/mtgjson/AtomicCards.json`) ---

const CLOUDSPIRE: &str = "When this creature enters, scry 2.\n{T}: Create X 1/1 colorless Pilot \
creature tokens, where X is the number of Mounts and/or Vehicles that entered the battlefield \
under your control this turn. The tokens have \"This token saddles Mounts and crews Vehicles as \
though its power were 2 greater.\"";

const GERALF: &str = "Whenever you cast a spell during your turn other than your first spell that \
turn, create a 2/2 blue and black Zombie Rogue creature token.\nWhenever a Zombie you control \
enters, put a +1/+1 counter on it for each other Zombie that entered the battlefield under your \
control this turn.";

const KINBINDING: &str = "Creatures you control get +X/+X, where X is the number of creatures \
that entered the battlefield under your control this turn.\nAt the beginning of combat on your \
turn, create a 1/1 green and white Kithkin creature token.";

const BIOENGINEERED_FUTURE: &str = "When this enchantment enters, create a Lander token. (It's an \
artifact with \"{2}, {T}, Sacrifice this token: Search your library for a basic land card, put it \
onto the battlefield tapped, then shuffle.\")\nEach creature you control enters with an \
additional +1/+1 counter on it for each land that entered the battlefield under your control this \
turn.";

const TROMELL: &str = "Each other nontoken creature you control enters with an additional +1/+1 \
counter on it.\n{1}, {T}: Proliferate X times, where X is the number of nontoken creatures you \
control that entered this turn. (To proliferate, choose any number of permanents and/or players, \
then give each another counter of each kind already there.)";

const OCELOT_PRIDE: &str =
    "First strike, lifelink\nAscend (If you control ten or more permanents, \
you get the city's blessing for the rest of the game.)\nAt the beginning of your end step, if you \
gained life this turn, create a 1/1 white Cat creature token. Then if you have the city's \
blessing, for each token you control that entered this turn, create a token that's a copy of it.";

const OAKHOLLOW_VILLAGE: &str = "{T}: Add {C}.\n{T}: Add {G}. Spend this mana only to cast a \
creature spell.\n{G}, {T}: Put a +1/+1 counter on each Frog, Rabbit, Raccoon, or Squirrel you \
control that entered the battlefield this turn.";

const NOVIJEN: &str =
    "{T}: Add {C}.\n{G}{U}, {T}: Put a +1/+1 counter on each creature that entered this turn.";

const LILYPAD_VILLAGE: &str = "{T}: Add {C}.\n{T}: Add {U}. Spend this mana only to cast a \
creature spell.\n{U}, {T}: Surveil 2. Activate only if a Bird, Frog, Otter, or Rat entered the \
battlefield under your control this turn.";

/// Synthetic T12 fixture: the CONTROLLER-ON-THE-SUBJECT-NOUN reading that still
/// carries the substring "the battlefield". The discriminator is the attachment
/// site, not that substring, so this must stay on the live variant.
const NOUN_CONTROLLER_SYNTHETIC: &str = "{T}: This creature deals damage equal to the number of \
creatures you control that entered the battlefield this turn to any target.";

/// Synthetic T13 fixture: the opponent entry-event surface no printed card uses.
const OPPONENT_SURFACE_SYNTHETIC: &str = "{T}: This creature deals damage equal to the number of \
creatures that entered the battlefield under an opponent's control this turn to any target.";

/// Synthetic T13 positive reach-guard: the SAME card shape with "under your
/// control", which must parse to the ledger variant. Without this the T13
/// `Unimplemented` assertion would pass for any parse failure whatsoever.
const OWN_SURFACE_SYNTHETIC: &str = "{T}: This creature deals damage equal to the number of \
creatures that entered the battlefield under your control this turn to any target.";

/// Synthetic T20 fixture: the same shape whose subject noun carries a
/// `FilterProp` the entry-record matcher cannot answer (`Tapped`).
const NONEVALUABLE_PROP_SYNTHETIC: &str = "{T}: This creature deals damage equal to the number of \
tapped creatures that entered the battlefield under your control this turn to any target.";

/// Synthetic T20 prop-discrimination reach-guard: same shape, a prop the matcher
/// DOES answer (`HasColor`). Proves the guard screens *which* prop rather than
/// rejecting every propertied filter.
const EVALUABLE_PROP_SYNTHETIC: &str = "{T}: This creature deals damage equal to the number of \
green creatures that entered the battlefield under your control this turn to any target.";

/// Walk an ability chain and return the first `DealDamage` amount.
fn find_damage_amount(def: &AbilityDefinition) -> Option<QuantityExpr> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Effect::DealDamage { amount, .. } = &*d.effect {
            return Some(amount.clone());
        }
        cur = d.sub_ability.as_deref();
    }
    None
}

fn hobgoblin_damage_amount() -> QuantityExpr {
    let parsed = parse_oracle_text(
        HOBGOBLIN,
        "Hobgoblin Bandit Lord",
        &[],
        &["Creature".to_string()],
        &["Goblin".to_string()],
    );
    parsed
        .abilities
        .iter()
        .find_map(find_damage_amount)
        .expect("Hobgoblin Bandit Lord must parse a DealDamage activated ability")
}

#[test]
fn bbfu10_probe_departed_goblin_still_counts() {
    let amount = hobgoblin_damage_amount();
    eprintln!("PROBE parsed amount = {amount:?}");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P0, "Hobgoblin Bandit Lord", 3, 3, HOBGOBLIN)
        .id();
    let goblin = scenario
        .add_creature(P0, "Departed Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;

    // Production entry snapshot taken while the Goblin is still on the battlefield.
    record_battlefield_entry(runner.state_mut(), goblin);
    {
        let obj = runner.state_mut().objects.get_mut(&goblin).unwrap();
        obj.entered_battlefield_turn = Some(turn);
        obj.zone = Zone::Graveyard;
    }

    let n = resolve_quantity(runner.state(), &amount, P0, lord);
    eprintln!("PROBE departed-goblin count = {n}");
    assert_eq!(
        n, 1,
        "CR 608.2i + Scryfall ruling: a Goblin that entered under your control this turn and has \
         since left the battlefield must STILL count"
    );
}

#[test]
fn bbfu10_probe_present_goblin_counts_control() {
    // Positive control: the same setup without the departure must count 1 under
    // both the pre-fix live-board read and the post-fix ledger read. If this
    // fails, the probe scaffolding itself is broken, not the engine.
    let amount = hobgoblin_damage_amount();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P0, "Hobgoblin Bandit Lord", 3, 3, HOBGOBLIN)
        .id();
    let goblin = scenario
        .add_creature(P0, "Fresh Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    record_battlefield_entry(runner.state_mut(), goblin);
    runner
        .state_mut()
        .objects
        .get_mut(&goblin)
        .unwrap()
        .entered_battlefield_turn = Some(turn);

    let n = resolve_quantity(runner.state(), &amount, P0, lord);
    eprintln!("PROBE present-goblin count = {n}");
    assert_eq!(n, 1, "a Goblin that entered this turn and stayed counts");
}

// ===========================================================================
// BB-FU10 shared helpers
// ===========================================================================

fn parse_card(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &[], &types, &subtypes)
}

/// The single ledger shape this migration produces: `PlayerScope::Controller`
/// carries "under whose control", so the filter is BARE (CR 608.2i — the runtime
/// keys on `record.controller`, not on a filter controller).
fn ledger_typed(type_filters: Vec<TypeFilter>, properties: Vec<FilterProp>) -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::BattlefieldEntriesThisTurn {
            player: PlayerScope::Controller,
            filter: TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: None,
                properties,
            }),
        },
    }
}

/// Stamp `id` into the production battlefield-entry ledger for the current turn,
/// exactly as `record_zone_change` does in a real game.
fn record_entry_now(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    record_battlefield_entry(runner.state_mut(), id);
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .entered_battlefield_turn = Some(turn);
}

/// Move an already-recorded permanent off the battlefield. CR 608.2i: its ledger
/// record must survive.
fn depart_to_graveyard(runner: &mut GameRunner, id: ObjectId) {
    let st = runner.state_mut();
    st.battlefield.retain(|&b| b != id);
    st.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
}

// ===========================================================================
// T1 — the entry-event surface emits the CR 608.2i ledger variant
// ===========================================================================

/// T1. Hobgoblin Bandit Lord's "…Goblins that entered the battlefield UNDER YOUR
/// CONTROL this turn" binds its controller to the past entry EVENT (CR 608.2i),
/// so it must lower to `BattlefieldEntriesThisTurn` with a BARE filter.
///
/// REVERT-PROBE: restoring the `EnteredThisTurn` + `inject_controller` arm makes
/// the amount `EnteredThisTurn{Typed{[Subtype("Goblin")], controller: Some(You)}}`
/// and this `assert_eq!` panics.
#[test]
fn bbfu10_hobgoblin_count_form_migrated_to_ledger() {
    let amount = hobgoblin_damage_amount();
    assert_eq!(
        amount,
        ledger_typed(vec![TypeFilter::Subtype("Goblin".to_string())], vec![]),
        "CR 608.2i: entry-event binding lowers to the look-back ledger with the \
         controller on the PlayerScope, never injected into the filter"
    );
}

// ===========================================================================
// T4 — the tally is scoped by `PlayerScope::Controller`, not a filter controller
// ===========================================================================

/// T4 (hostile, multi-authority). Two candidate controllers are on the board and
/// BOTH have a Goblin entry on the ledger; only the ability controller's may
/// count (CR 608.2i tallies `record.controller`).
///
/// NON-VACUITY: assertion (b) is an in-test positive reach-guard on the SAME
/// state — recording one own Goblin flips 0 → 1, so (a)'s zero cannot be an
/// "everything reads zero" scaffolding failure.
///
/// REVERT-PROBE: change Step 1's `PlayerScope::Controller` to
/// `AllPlayers { aggregate: Sum }` → (a) becomes 1 and FAILS.
#[test]
fn bbfu10_opponent_goblin_does_not_count() {
    let amount = hobgoblin_damage_amount();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P0, "Hobgoblin Bandit Lord", 3, 3, HOBGOBLIN)
        .id();
    let opp_goblin = scenario
        .add_creature(P1, "Opponent Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let own_goblin = scenario
        .add_creature(P0, "Own Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let mut runner = scenario.build();

    record_entry_now(&mut runner, opp_goblin);
    assert_eq!(
        resolve_quantity(runner.state(), &amount, P0, lord),
        0,
        "(a) an OPPONENT's Goblin entry must not count under PlayerScope::Controller"
    );

    record_entry_now(&mut runner, own_goblin);
    assert_eq!(
        resolve_quantity(runner.state(), &amount, P0, lord),
        1,
        "(b) positive reach-guard on the same state: the controller's own Goblin \
         entry DOES count, so (a) is a scope result and not a dead fixture"
    );
}

// ===========================================================================
// T5 — composite (`Or`) filters match the ledger record
// ===========================================================================

/// T5 (hostile, composite filter). Cloudspire Coordinator's ledger filter is a
/// `TargetFilter::Or`, which `battlefield_entry_matches_filter` failed closed on
/// before Step 2 (measured: constant 0 on records that make `Typed(Mount)` = 1).
///
/// REVERT-PROBE: delete Step 2's `TargetFilter::Or` arm → the count drops to 0.
#[test]
fn bbfu10_cloudspire_or_filter_counts_mount_and_vehicle() {
    let parsed = parse_card(
        CLOUDSPIRE,
        "Cloudspire Coordinator",
        &["Creature"],
        &["Human", "Pilot"],
    );
    let count = parsed
        .abilities
        .iter()
        .find_map(|a| match &*a.effect {
            Effect::Token { count, .. } => Some(count.clone()),
            _ => None,
        })
        .expect("Cloudspire must parse a Token-creating activated ability");

    // Reach-guard: the exact composite shape this test exists to exercise.
    let QuantityExpr::Ref {
        qty:
            QuantityRef::BattlefieldEntriesThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Or { ref filters },
            },
    } = count
    else {
        panic!("reach-guard: expected a ledger ref over TargetFilter::Or, got {count:?}");
    };
    assert_eq!(filters.len(), 2, "Or[Typed(Mount), Typed(Vehicle)]");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coordinator = scenario
        .add_creature_from_oracle(P0, "Cloudspire Coordinator", 2, 2, CLOUDSPIRE)
        .id();
    let mount = scenario
        .add_creature(P0, "Departed Mount", 2, 2)
        .with_subtypes(vec!["Mount"])
        .id();
    let vehicle = scenario
        .add_creature(P0, "Live Vehicle", 3, 3)
        .with_subtypes(vec!["Vehicle"])
        .id();
    let mut runner = scenario.build();

    record_entry_now(&mut runner, mount);
    record_entry_now(&mut runner, vehicle);
    depart_to_graveyard(&mut runner, mount);

    assert_eq!(
        resolve_quantity(runner.state(), &count, P0, coordinator),
        2,
        "CR 608.2i: BOTH disjuncts must match the entry ledger — the departed \
         Mount still counts and the live Vehicle counts too"
    );
}

// ===========================================================================
// T6 — `FilterProp::Another` still excludes the ability source
// ===========================================================================

/// T6 (hostile, two candidate authorities). Geralf's "each OTHER Zombie" must
/// exclude the ability source's own ledger record. Both the source and another
/// Zombie are recorded, so a dropped `source_id` would read 2.
///
/// REVERT-PROBE: pass `None` for `source_id` on the ledger resolution path
/// (`game/quantity.rs`, the `BattlefieldEntriesThisTurn` arm) → `Another` can
/// exclude nothing, the count becomes 2, FAIL.
#[test]
fn bbfu10_geralf_another_excludes_source() {
    let parsed = parse_card(
        GERALF,
        "Geralf, the Fleshwright",
        &["Creature"],
        &["Human", "Warlock"],
    );
    let count = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(|d| match &*d.effect {
            Effect::PutCounter { count, .. } => Some(count.clone()),
            _ => None,
        })
        .expect("Geralf must parse a PutCounter trigger");
    assert_eq!(
        count,
        ledger_typed(
            vec![TypeFilter::Subtype("Zombie".to_string())],
            vec![FilterProp::Another]
        ),
        "reach-guard: the migrated shape carries FilterProp::Another on a bare filter"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The source is itself a Zombie, so `Another` is load-bearing rather than
    // trivially satisfied.
    let geralf = scenario
        .add_creature_from_oracle(P0, "Geralf, the Fleshwright", 3, 3, GERALF)
        .with_subtypes(vec!["Zombie"])
        .id();
    let other = scenario
        .add_creature(P0, "Other Zombie", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let mut runner = scenario.build();

    record_entry_now(&mut runner, geralf);
    record_entry_now(&mut runner, other);
    depart_to_graveyard(&mut runner, other);

    assert_eq!(
        runner.state().battlefield_entries_this_turn.len(),
        2,
        "reach-guard: BOTH Zombie entries are on the ledger, so a count of 1 is \
         the `Another` exclusion and not a missing record"
    );
    assert_eq!(
        resolve_quantity(runner.state(), &count, P0, geralf),
        1,
        "CR 109.1: the source's own entry is excluded; the departed OTHER Zombie \
         still counts (CR 608.2i)"
    );
}

// ===========================================================================
// T7 — a token entry re-derives PRE-EXISTING recipients, controller-scoped only
// ===========================================================================

/// T7 (Step 0a/0b). Kinbinding's Layer 7c magnitude is the ledger tally. When its
/// OWN begin-combat trigger creates a Kithkin token, the incremental-flush
/// escalation scan must fire, or every PRE-EXISTING creature keeps the stale
/// magnitude (CR 611.3a: a continuous effect from a static ability is never
/// locked in).
///
/// The fixture pair is **(1) + (4)**: (1) is the multiplayer-scope discriminator
/// and (4) is what makes it load-bearing — an OPPONENT entry really is on the
/// ledger, so a wrong `PlayerScope` would fold it into the anthem magnitude.
/// Assertion (2) is an affected-set guard, NOT a scope probe: Kinbinding's
/// `affected` is `Typed{[Creature], controller: You}`, so P1's creature is never
/// anthem-affected and cannot flip under a scope mutation.
///
/// REVERT-PROBES, both measured:
/// - Step 0a/0b → P0 bystander reads 2/2 and `layers_incremental == 1` /
///   `layers_escalated == 0`; (1) and (5) FAIL.
/// - Step 1's `PlayerScope::Controller` → `AllPlayers { aggregate: Sum }` → the
///   tally becomes 2 (Kithkin + P1's entry) and the bystander reads 4/4; (1) FAILS.
#[test]
fn bbfu10_kinbinding_token_entry_updates_existing_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Kinbinding", 0, 0, KINBINDING)
        .as_enchantment();
    let bystander = scenario.add_creature(P0, "P0 Bystander", 2, 2).id();
    let opponent_creature = scenario.add_creature(P1, "P1 Entrant", 3, 3).id();
    let mut runner = scenario.build();

    // `GameScenario` does not push a `BattlefieldEntryRecord`, so the opponent's
    // entry must be stamped through the production authority by hand.
    record_entry_now(&mut runner, opponent_creature);

    engine::game::perf_counters::reset();
    runner.advance_to_combat();
    runner.advance_until_stack_empty();
    let perf = engine::game::perf_counters::snapshot();

    // (3) reach-guard — the trigger really fired and a token really entered.
    let kithkin: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].name == "Kithkin")
        .collect();
    assert_eq!(
        kithkin.len(),
        1,
        "(3) reach-guard: one Kithkin token entered"
    );

    // (4) reach-guard — the OPPONENT's entry IS on the ledger. This is what makes
    // (1) a real multiplayer-scope discriminator.
    assert_eq!(
        runner.state().battlefield_entries_this_turn.len(),
        2,
        "(4) reach-guard: the ledger holds P1's stamped entry AND the Kithkin's"
    );

    // (1) THE DISCRIMINATOR — a PRE-EXISTING recipient sees the new magnitude,
    // and it is the CONTROLLER-scoped tally (1), not the all-players tally (2).
    let by = &runner.state().objects[&bystander];
    assert_eq!(
        (by.power, by.toughness),
        (Some(3), Some(3)),
        "(1) CR 611.3a: the pre-existing bystander must be 2/2 + X where X = 1 \
         (only P0's Kithkin entry counts)"
    );

    // (2) affected-set guard — the anthem is `controller: You`, so P1 is untouched.
    let opp = &runner.state().objects[&opponent_creature];
    assert_eq!(
        (opp.power, opp.toughness),
        (Some(3), Some(3)),
        "(2) affected-set guard: Kinbinding's anthem must not spray across controllers"
    );

    // (5) path guard — the entry took the ESCALATED flush, not the incremental one.
    assert_eq!(
        (perf.layers_escalated, perf.layers_incremental),
        (1, 0),
        "(5) the token entry must force `active_effects_force_incremental_escalation`"
    );
}

// ===========================================================================
// T8 — the enters-with-counters replacement path reads the ledger
// ===========================================================================

/// T8. Bioengineered Future resolves its magnitude on demand inside a
/// `ReplacementDefinition`, a path that never goes through the layer system.
/// (a) is the positive reach-guard (land still on the battlefield); (b) is the
/// CR 608.2i claim (the same land departed).
///
/// REVERT-PROBE: revert Step 1 → (b) enters 2/2 with no counters, FAIL.
/// (Measured pre-fix: (a) 3/3 with `Plus1Plus1 = 1`, (b) 2/2 with none.)
#[test]
fn bbfu10_bioengineered_future_departed_land_counts() {
    for depart in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature_from_oracle(P0, "Bioengineered Future", 0, 0, BIOENGINEERED_FUTURE)
            .as_enchantment();
        let land = scenario.add_land_from_oracle(P0, "Recorded Land", "").id();
        let creature = scenario
            .add_creature_to_hand(P0, "Entering Creature", 2, 2)
            .id();
        let mut runner = scenario.build();

        record_entry_now(&mut runner, land);
        if depart {
            depart_to_graveyard(&mut runner, land);
        }

        runner.cast(creature).free_cast().resolve();

        let obj = &runner.state().objects[&creature];
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "reach-guard (depart={depart}): the creature must actually resolve onto \
             the battlefield"
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "CR 608.2i (depart={depart}): the land that entered this turn counts \
             whether or not it is still on the battlefield"
        );
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(3), Some(3)),
            "(depart={depart}) the +1/+1 counter must be reflected in P/T"
        );
    }
}

// ===========================================================================
// T9 — BOUNDARY LOCK: Tromell's subject-noun form does NOT migrate
// ===========================================================================

/// T9. "nontoken creatures YOU CONTROL that entered this turn" binds the
/// controller to the SUBJECT NOUN, which by CR 109.2 names a battlefield
/// permanent — a CR 608.2h live read, not a CR 608.2i look-back.
///
/// REVERT-PROBE: route the `SubjectNoun` arm to the ledger → (a) panics and (b)'s
/// first assertion reads 1 instead of 0.
#[test]
fn bbfu10_tromell_live_form_not_over_migrated() {
    let parsed = parse_card(
        TROMELL,
        "Tromell, Seymour's Butler",
        &["Creature"],
        &["Elf", "Advisor"],
    );
    let repeat = parsed
        .abilities
        .iter()
        .find_map(|a| a.repeat_for.clone())
        .expect("Tromell must parse a `Proliferate X times` repeat quantity");

    // (a) shape lock — the LIVE variant with the controller ON THE FILTER.
    assert_eq!(
        repeat,
        QuantityExpr::Ref {
            qty: QuantityRef::EnteredThisTurn {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::NonToken],
                }),
            },
        },
        "(a) CR 608.2h: subject-noun binding stays on the live-board variant"
    );

    // (b) runtime, with an in-test positive reach-guard.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tromell = scenario
        .add_creature_from_oracle(P0, "Tromell, Seymour's Butler", 2, 3, TROMELL)
        .id();
    let departed_own = scenario.add_creature(P0, "Departed Own", 1, 1).id();
    let live_opponent = scenario.add_creature(P1, "Live Opponent", 1, 1).id();
    let live_own = scenario.add_creature(P0, "Live Own", 1, 1).id();
    let mut runner = scenario.build();

    record_entry_now(&mut runner, departed_own);
    record_entry_now(&mut runner, live_opponent);
    depart_to_graveyard(&mut runner, departed_own);

    assert_eq!(
        resolve_quantity(runner.state(), &repeat, P0, tromell),
        0,
        "(b) a departed own creature and a LIVE opponent creature both entered \
         this turn, and the live-board reading counts neither"
    );

    record_entry_now(&mut runner, live_own);
    assert_eq!(
        resolve_quantity(runner.state(), &repeat, P0, tromell),
        1,
        "(b) positive reach-guard on the same state: a live own nontoken creature \
         stamped as entered-this-turn DOES count"
    );
}

// ===========================================================================
// T10 / T11 — SET-PATH and AFFECTED-SET locks (Step 4 does nothing)
// ===========================================================================

/// T10. The for-each-copy set path lowers "token you control that entered this
/// turn" to `FilterProp::EnteredThisTurn` inside a `source_filter`, an entirely
/// different enum surface from `QuantityRef`. It must be untouched.
///
/// REVERT-PROBE: add a ledger arm to
/// `parse_for_each_object_filter_clause_with_context` that swallows the clause,
/// or migrate the `SubjectNoun` arm → the filter disappears and this FAILS.
#[test]
fn bbfu10_ocelot_pride_for_each_copy_set_stays_live() {
    let parsed = parse_card(OCELOT_PRIDE, "Ocelot Pride", &["Creature"], &["Cat"]);
    let mut found = false;
    for trigger in &parsed.triggers {
        let mut cur = trigger.execute.as_deref();
        while let Some(def) = cur {
            if let Effect::CopyTokenOf { source_filter, .. } = &*def.effect {
                let Some(TargetFilter::Typed(tf)) = source_filter else {
                    panic!("expected a Typed source_filter, got {source_filter:?}");
                };
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Token));
                assert!(tf.properties.contains(&FilterProp::EnteredThisTurn));
                found = true;
            }
            cur = def.sub_ability.as_deref();
        }
    }
    assert!(
        found,
        "reach-guard: Ocelot Pride must still parse a CopyTokenOf with an \
         EnteredThisTurn source_filter — a `None` here is the swallowed state"
    );
}

/// T11. Oakhollow Village and Novijen route "entered the battlefield this turn"
/// through the TARGET path (`PutCounterAll` + `FilterProp::EnteredThisTurn`), not
/// through `QuantityRef` at all. This is the non-interference lock for Step 4.
///
/// REVERT-PROBE: any change routing the target path through the ledger → FAIL.
#[test]
fn bbfu10_oakhollow_novijen_affected_set_unchanged() {
    let oak = parse_card(OAKHOLLOW_VILLAGE, "Oakhollow Village", &["Land"], &[]);
    let oak_target = oak
        .abilities
        .iter()
        .find_map(|a| match &*a.effect {
            Effect::PutCounterAll { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Oakhollow must parse a PutCounterAll");
    let TargetFilter::Or { filters } = &oak_target else {
        panic!("Oakhollow's affected set is Or[4x Typed], got {oak_target:?}");
    };
    assert_eq!(filters.len(), 4, "Frog / Rabbit / Raccoon / Squirrel");
    for f in filters {
        let TargetFilter::Typed(tf) = f else {
            panic!("expected Typed disjunct, got {f:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::EnteredThisTurn));
    }

    let nov = parse_card(NOVIJEN, "Novijen, Heart of Progress", &["Land"], &[]);
    let nov_target = nov
        .abilities
        .iter()
        .find_map(|a| match &*a.effect {
            Effect::PutCounterAll { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Novijen must parse a PutCounterAll");
    assert_eq!(
        nov_target,
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::EnteredThisTurn],
        }),
        "Novijen's bare affected set stays a live FilterProp, not a ledger ref"
    );
}

// ===========================================================================
// T12 — DISCRIMINATOR LOCK: the predicate is controller attachment
// ===========================================================================

/// T12. "creatures YOU CONTROL that entered THE BATTLEFIELD this turn" carries
/// the substring "the battlefield" while binding the controller to the noun. A
/// substring-based discriminator would over-migrate it; the attachment-site
/// discriminator must not.
///
/// REVERT-PROBE: implement "`the battlefield` present ⇒ migrate" → this becomes
/// `BattlefieldEntriesThisTurn` and FAILS.
#[test]
fn bbfu10_the_battlefield_arm_with_noun_controller_stays_live() {
    let parsed = parse_card(
        NOUN_CONTROLLER_SYNTHETIC,
        "Bbfu10 Discriminator Probe",
        &["Creature"],
        &["Elemental"],
    );
    let amount = parsed
        .abilities
        .iter()
        .find_map(find_damage_amount)
        .expect("the synthetic must parse a DealDamage activated ability");
    assert_eq!(
        amount,
        QuantityExpr::Ref {
            qty: QuantityRef::EnteredThisTurn {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                }),
            },
        },
        "CR 608.2h: the controller came from the SUBJECT NOUN, so this stays live \
         even though the clause says \"the battlefield\""
    );
}

// ===========================================================================
// T13 — FUTURE-ADDITION LOCK: the opponent surface stays honestly unsupported
// ===========================================================================

/// T13. No printed card templates "…under an opponent's control this turn" in a
/// quantity context (measured: 0/34 corpus cards), so the parser must fall to an
/// honest `Effect::Unimplemented` rather than guess a scope.
///
/// The `OWN_SURFACE_SYNTHETIC` half is the positive reach-guard: without it the
/// `Unimplemented` assertion would pass for ANY parse failure on that card shape.
///
/// This is a LOCK, not a revert-probe: adding a guessed opponent arm with no
/// printed card behind it makes it FAIL.
#[test]
fn bbfu10_opponent_entry_surface_stays_unimplemented() {
    let opponent = parse_card(
        OPPONENT_SURFACE_SYNTHETIC,
        "Bbfu10 Opponent Probe",
        &["Creature"],
        &["Elemental"],
    );
    assert!(
        opponent
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "the opponent entry-event surface must stay honestly unimplemented, got {:?}",
        opponent
            .abilities
            .iter()
            .map(|a| &a.effect)
            .collect::<Vec<_>>()
    );

    let own = parse_card(
        OWN_SURFACE_SYNTHETIC,
        "Bbfu10 Own Probe",
        &["Creature"],
        &["Elemental"],
    );
    let amount = own.abilities.iter().find_map(find_damage_amount).expect(
        "positive reach-guard: the SAME card shape with \"under your \
                 control\" must parse",
    );
    assert_eq!(
        amount,
        ledger_typed(vec![TypeFilter::Creature], vec![]),
        "positive reach-guard: the sibling surface reaches the ledger variant, so \
         the Unimplemented above is about the OPPONENT scope specifically"
    );
}

// ===========================================================================
// T18 / T19 — Step 2 on a SHIPPED `Or` ledger card (Lilypad Village)
// ===========================================================================

/// Parse Lilypad Village and return `(ability_index, RequiresCondition list)`.
///
/// The index is found by SCANNING for the `RequiresCondition`, never hardcoded:
/// Lilypad Village's gated ability is `abilities[2]` (`[0]`/`[1]` are the two
/// mana abilities), so the BB-FU1 precedent helper's hardcoded `abilities[0]`
/// would silently gate on `{T}: Add {C}` and pass vacuously.
fn lilypad_gated_ability() -> (usize, Vec<ActivationRestriction>) {
    let parsed = parse_card(LILYPAD_VILLAGE, "Lilypad Village", &["Land"], &[]);
    let hits: Vec<(usize, Vec<ActivationRestriction>)> = parsed
        .abilities
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let req: Vec<ActivationRestriction> = a
                .activation_restrictions
                .iter()
                .filter(|r| matches!(r, ActivationRestriction::RequiresCondition { .. }))
                .cloned()
                .collect();
            (!req.is_empty()).then_some((i, req))
        })
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "reach-guard: exactly ONE Lilypad Village ability carries a \
         RequiresCondition (found indices {:?})",
        hits.iter().map(|(i, _)| *i).collect::<Vec<_>>()
    );
    hits.into_iter().next().unwrap()
}

/// Assert the isolated condition's `lhs` really is the composite ledger read, and
/// return nothing. Pins `Some(condition)` — a `condition: None` is permissively
/// TRUE at the production gate, so without this a reverted parse would look legal.
fn assert_lilypad_condition_shape(req: &[ActivationRestriction]) {
    let condition = req
        .iter()
        .find_map(|r| match r {
            ActivationRestriction::RequiresCondition {
                condition: Some(c), ..
            } => Some(c),
            _ => None,
        })
        .expect("condition: None is the reverted/permissive state, not a pass");
    let ParsedCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    } = condition
    else {
        panic!("expected a QuantityComparison, got {condition:?}");
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    let QuantityExpr::Ref {
        qty:
            QuantityRef::BattlefieldEntriesThisTurn {
                player: PlayerScope::Controller,
                filter: TargetFilter::Or { filters },
            },
    } = lhs
    else {
        panic!("expected a ledger ref over TargetFilter::Or, got {lhs:?}");
    };
    assert_eq!(filters.len(), 4, "Bird / Frog / Otter / Rat");
}

/// T18 (M-1 deliverable). Lilypad Village is a SHIPPED card carrying the ledger
/// variant with a `TargetFilter::Or` filter, which `battlefield_entry_matches_filter`
/// failed closed on ⇒ a constant 0. Step 2 makes it count for real. This asserts
/// on the QUANTITY only; T19 covers the activation layer.
///
/// REVERT-PROBE: delete Step 2's `TargetFilter::Or` arm → (b) reads 0, FAIL.
#[test]
fn bbfu10_shipped_or_ledger_card_counts_after_composite_fix() {
    let (_idx, req) = lilypad_gated_ability();
    assert_lilypad_condition_shape(&req); // (a) reach-guard
    let ActivationRestriction::RequiresCondition {
        condition: Some(ParsedCondition::QuantityComparison { lhs, .. }),
        ..
    } = &req[0]
    else {
        unreachable!("shape asserted above")
    };

    // (b) one MATCHING entry (a Frog) ⇒ the ledger reads at least 1.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let village = scenario
        .add_land_from_oracle(P0, "Lilypad Village", LILYPAD_VILLAGE)
        .id();
    let frog = scenario
        .add_creature(P0, "Qualifying Frog", 1, 1)
        .with_subtypes(vec!["Frog"])
        .id();
    let human = scenario
        .add_creature(P0, "Non-Qualifying Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();
    let mut runner = scenario.build();

    record_entry_now(&mut runner, human);
    // (c) negative sibling FIRST, on the same board: a non-matching entry alone
    // must read 0, so (b) cannot pass on a mere "any record present" bug.
    assert_eq!(
        resolve_quantity(runner.state(), lhs, P0, village),
        0,
        "(c) a Human entry matches none of Bird/Frog/Otter/Rat"
    );

    record_entry_now(&mut runner, frog);
    assert_eq!(
        resolve_quantity(runner.state(), lhs, P0, village),
        1,
        "(b) CR 608.2i: the Frog disjunct must match the entry record — this reads \
         a fail-closed 0 without Step 2's `TargetFilter::Or` arm"
    );
}

/// T19 (F7 deliverable). The `Or` fail-closed 0 vs `GE 1` makes Lilypad Village's
/// `{U}, {T}: Surveil 2.` **un-activatable, always** on the shipped card. Step 2
/// is therefore a user-visible UNLOCK, and the repo's non-vacuous-test rule does
/// not let the PR claim an unlock it never drives.
///
/// Depends on Step 2 ONLY (not on Step 1): Lilypad Village already carries the
/// ledger variant on the shipped tip.
///
/// REVERT-PROBE: delete Step 2's `TargetFilter::Or` arm → the tally is 0,
/// `0 >= 1` is false, and (3) becomes ILLEGAL: FAIL. That flip IS the unlock.
/// (4) is ILLEGAL in BOTH builds and is the anti-vacuity control.
#[test]
fn bbfu10_lilypad_village_activation_unlocked_by_composite_fix() {
    let (idx, req) = lilypad_gated_ability(); // (1) index reach-guard
    assert_eq!(
        idx, 2,
        "(1) the gated ability is abilities[2]; [0]/[1] are the mana abilities"
    );
    assert_lilypad_condition_shape(&req); // (2) condition shape reach-guard

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let village = scenario
        .add_land_from_oracle(P0, "Lilypad Village", LILYPAD_VILLAGE)
        .id();
    let human = scenario
        .add_creature(P0, "Non-Qualifying Human", 1, 1)
        .with_subtypes(vec!["Human"])
        .id();
    let frog = scenario
        .add_creature(P0, "Qualifying Frog", 1, 1)
        .with_subtypes(vec!["Frog"])
        .id();
    let mut runner = scenario.build();

    // (4) NEGATIVE CONTROL — a non-matching entry leaves the ability illegal, so a
    // fixture that silently failed to push a record cannot fake (3).
    record_entry_now(&mut runner, human);
    assert!(
        check_activation_restrictions(runner.state(), P0, village, idx, &req).is_err(),
        "(4) with only a Human entry the condition is false ⇒ still ILLEGAL"
    );

    // (3) THE UNLOCK — one qualifying entry flips the production gate to LEGAL.
    record_entry_now(&mut runner, frog);
    assert!(
        check_activation_restrictions(runner.state(), P0, village, idx, &req).is_ok(),
        "(3) CR 608.2i: with a Frog entry recorded the RequiresCondition is \
         satisfied and `{{U}}, {{T}}: Surveil 2.` becomes activatable — this is \
         ILLEGAL without Step 2's `TargetFilter::Or` arm"
    );
}

// ===========================================================================
// T20 / T21 — Step 7: the entry-record matcher's fail-closed `FilterProp` hole
// ===========================================================================

/// T20 (Step 7b). `battlefield_entry_matches_filter` fails closed on
/// unsupported `FilterProp` variants, so a ledger tally over an
/// unanswerable prop resolves to a silent constant 0. On the QUANTITY path a
/// refused clause becomes an honest `Effect::Unimplemented`, so the guard refuses
/// rather than mis-lowering.
///
/// (Deliberately NOT mirrored at the condition-side emitters, where an
/// unparseable clause is silently dropped and would over-permit instead.)
///
/// REVERT-PROBE: delete the `if !ledger_filter_is_evaluable(..)` block in
/// `parse_entered_this_turn_ref` → (a) lowers to
/// `DealDamage{amount: Ref(BattlefieldEntriesThisTurn{…[Tapped]})}` and FAILS.
/// (b) and (c) pass in both builds and are the vacuity controls.
#[test]
fn bbfu10_nonevaluable_entry_filter_stays_unimplemented() {
    // (a) THE DISCRIMINATOR — `Tapped` is not answerable from the entry record.
    let refused = parse_card(
        NONEVALUABLE_PROP_SYNTHETIC,
        "Bbfu10 Nonevaluable Prop Probe",
        &["Creature"],
        &["Elemental"],
    );
    let serialized = serde_json::to_string(&refused.abilities).expect("abilities serialize");
    assert!(
        !serialized.contains("BattlefieldEntriesThisTurn"),
        "(a) a filter the entry-record matcher cannot evaluate must NOT reach the \
         ledger variant (would resolve a silent constant 0), got {serialized}"
    );
    assert!(
        refused
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "(a) the refused clause must surface as an honest Effect::Unimplemented, got {:?}",
        refused
            .abilities
            .iter()
            .map(|a| &a.effect)
            .collect::<Vec<_>>()
    );

    // (b) PROP-DISCRIMINATION reach-guard — `HasColor` IS answerable
    // (game/restrictions.rs:504), so the same shape must still lower.
    let evaluable = parse_card(
        EVALUABLE_PROP_SYNTHETIC,
        "Bbfu10 Evaluable Prop Probe",
        &["Creature"],
        &["Elemental"],
    );
    assert_eq!(
        evaluable
            .abilities
            .iter()
            .find_map(find_damage_amount)
            .expect("(b) the green-creature surface must still parse a DealDamage"),
        ledger_typed(
            vec![TypeFilter::Creature],
            vec![FilterProp::HasColor {
                color: ManaColor::Green
            }]
        ),
        "(b) the guard screens WHICH prop, not whether any prop is present"
    );

    // (c) BARE reach-guard — an empty property list is trivially evaluable.
    let bare = parse_card(
        OWN_SURFACE_SYNTHETIC,
        "Bbfu10 Bare Reach Guard",
        &["Creature"],
        &["Elemental"],
    );
    assert_eq!(
        bare.abilities
            .iter()
            .find_map(find_damage_amount)
            .expect("(c) the bare surface must parse a DealDamage"),
        ledger_typed(vec![TypeFilter::Creature], vec![]),
        "(c) the unpropertied ledger read is untouched by the guard"
    );
}

/// T21. A tapped entrant proves that the historical matcher fails closed on
/// properties it cannot evaluate from the snapshot.
#[test]
fn bbfu10_nonevaluable_ledger_prop_reads_constant_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Bbfu10 Ledger Source", 1, 1).id();
    let entrant = scenario
        .add_creature(P0, "Legendary Tapped Entrant", 2, 2)
        .as_legendary()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&entrant).unwrap().tapped = true;
    record_entry_now(&mut runner, entrant);

    let read = |properties: Vec<FilterProp>| {
        resolve_quantity(
            runner.state(),
            &ledger_typed(vec![TypeFilter::Creature], properties),
            P0,
            source,
        )
    };

    // Positive reach-guard: the fixture really pushed a record.
    assert_eq!(read(vec![]), 1, "the bare ledger read sees the entry");
    assert_eq!(read(vec![FilterProp::NonToken]), 1, "nontoken entry");
    assert_eq!(
        read(vec![FilterProp::HasSupertype {
            value: Supertype::Legendary
        }]),
        0,
        "HasSupertype fails closed even though the record snapshots supertypes"
    );
    assert_eq!(read(vec![FilterProp::Tapped]), 0, "Tapped fails closed");
}

const GYOME: &str = "Trample\nAt the beginning of your end step, create a number of Food tokens equal to the number of nontoken creatures you had enter the battlefield under your control this turn.\n{1}, Sacrifice a Food: Target creature gains indestructible until end of turn. Tap it.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

fn assert_priority(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn assert_gyome_food_delta(runner: &mut GameRunner, gyome: ObjectId, expected: usize) {
    let food_count = |runner: &GameRunner| {
        runner
            .state()
            .objects
            .values()
            .filter(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == P0
                    && obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Food")
            })
            .count()
    };
    assert_priority(runner);
    let before = food_count(runner);
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "expected one end-step trigger"
    );
    assert!(matches!(
        &runner.state().stack[0].kind,
        StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == gyome
    ));
    runner.resolve_top();
    assert_priority(runner);
    assert_eq!(food_count(runner), before + expected);
}

#[test]
fn issue_634_gyome_counts_departed_and_present_nontokens_only() {
    for qualifying_entrants in [0, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gyome = scenario
            .add_creature(P0, "Gyome, Master Chef", 5, 3)
            .from_oracle_text_with_keywords(&["Trample"], GYOME)
            .with_subtypes(vec!["Troll", "Warlock"])
            .as_legendary()
            .id();
        let prior = scenario.add_creature(P0, "Prior-turn creature", 2, 2).id();
        let first = scenario
            .add_creature_to_hand(P0, "First entrant", 2, 2)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let second = scenario
            .add_creature_to_hand(P0, "Second entrant", 2, 2)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let soldiers = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Raise the Alarm",
                true,
                "Create two 1/1 white Soldier creature tokens.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id();
        let murder = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black; 2],
                generic: 1,
            })
            .id();
        let viper = scenario
            .add_creature_to_hand(P1, "Ambush Viper", 2, 1)
            .from_oracle_text_with_keywords(&["Flash", "Deathtouch"], "Flash\nDeathtouch")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![
                mana(ManaType::White),
                mana(ManaType::Black),
                mana(ManaType::Black),
                mana(ManaType::Colorless),
                mana(ManaType::Colorless),
            ],
        );
        scenario.with_mana_pool(P1, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);
        let mut runner = scenario.build();
        assert!(runner.state().battlefield_entries_this_turn.is_empty());
        assert_eq!(runner.state().objects[&prior].zone, Zone::Battlefield);

        if qualifying_entrants == 2 {
            runner
                .cast(first)
                .resolve()
                .assert_zone(&[first], Zone::Battlefield);
            runner
                .cast(second)
                .resolve()
                .assert_zone(&[second], Zone::Battlefield);
        }
        runner
            .cast(soldiers)
            .resolve()
            .assert_zone(&[soldiers], Zone::Graveyard);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|obj| {
                    obj.zone == Zone::Battlefield
                        && obj.controller == P0
                        && obj.is_token
                        && obj.card_types.core_types.contains(&CoreType::Creature)
                })
                .count(),
            2
        );
        if qualifying_entrants == 2 {
            runner
                .cast(murder)
                .target_objects(&[first])
                .resolve()
                .assert_zone(&[first], Zone::Graveyard);
            assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
        }

        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        runner
            .cast(viper)
            .resolve()
            .assert_zone(&[viper], Zone::Battlefield);
        assert_eq!(runner.state().objects[&viper].controller, P1);
        assert_gyome_food_delta(&mut runner, gyome, qualifying_entrants);
    }
}

#[test]
fn issue_634_gyome_counts_its_own_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature_to_hand(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .with_subtypes(vec!["Troll", "Warlock"])
        .as_legendary()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Green],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Black),
            mana(ManaType::Green),
            mana(ManaType::Colorless),
            mana(ManaType::Colorless),
        ],
    );
    let mut runner = scenario.build();
    runner
        .cast(gyome)
        .resolve()
        .assert_zone(&[gyome], Zone::Battlefield);
    assert_gyome_food_delta(&mut runner, gyome, 1);
}

// CR 701.40a: the continuous face-down effect makes the land a creature at entry.
#[test]
fn issue_634_manifested_land_is_a_nontoken_creature_at_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let forest = scenario
        .add_land_to_library_top(P0, "Forest")
        .with_subtypes(vec!["Forest"])
        .id();
    let summons = scenario.add_spell_to_hand_from_oracle(
        P0, "Soul Summons", false,
        "Manifest the top card of your library. (Put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)",
    ).with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 1 }).id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::White), mana(ManaType::Colorless)]);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&forest].card_types.core_types,
        vec![CoreType::Land]
    );
    runner
        .cast(summons)
        .resolve()
        .assert_zone(&[forest], Zone::Battlefield);
    assert_priority(&runner);
    let entry = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .find(|entry| entry.object_id == forest)
        .unwrap();
    assert_eq!(entry.core_types, vec![CoreType::Creature]);
    assert_eq!(entry.token_status, Some(EntryTokenStatus::Nontoken));
    assert_gyome_food_delta(&mut runner, gyome, 1);
}

#[test]
fn issue_634_gyome_counts_forest_animated_as_it_enters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let revolt = scenario
        .add_enchantment_from_oracle(
            P0,
            "Nature's Revolt",
            "All lands are 2/2 creatures that are still lands.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 3,
        })
        .id();
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .with_subtypes(vec!["Forest"])
        .id();
    let mut runner = scenario.build();
    let previous_turn = runner.state().turn_number - 1;
    runner
        .state_mut()
        .objects
        .get_mut(&revolt)
        .unwrap()
        .entered_battlefield_turn = Some(previous_turn);
    assert!(runner.state().battlefield_entries_this_turn.is_empty());
    play_land(&mut runner, forest);

    // CR 611.3c: Nature's Revolt makes the land a creature as it enters.
    assert_gyome_food_delta(&mut runner, gyome, 1);
}

fn play_land(runner: &mut GameRunner, land: ObjectId) {
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert_priority(runner);
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
}

fn legacy_save_without_entry_token_status(state: &GameState) -> PersistedGameState {
    let mut save = serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap();
    assert!(save["state"]["battlefield_entries_this_turn"][0]
        .as_object_mut()
        .unwrap()
        .remove("token_status")
        .is_some());
    serde_json::from_value(save).unwrap()
}

fn legacy_payment_checkpoint_save() -> (serde_json::Value, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let pet = scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_enchantment_from_oracle(
        P1,
        "Rest in Peace",
        "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.",
    );
    scenario.add_enchantment_from_oracle(
        P1,
        "Leyline of the Void",
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.",
    );
    let zoraline = scenario
        .add_creature_to_hand(P0, "Zoraline, Cosmos Caller", 3, 3)
        .as_legendary()
        .with_subtypes(vec!["Bat", "Cleric"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Black],
            generic: 1,
        })
        .from_oracle_text_with_keywords(
            &["Flying", "Vigilance"],
            "Flying, vigilance\nWhenever a Bat you control attacks, you gain 1 life.\nWhenever Zoraline enters or attacks, you may pay {W}{B} and 2 life. When you do, return target nonland permanent card with mana value 3 or less from your graveyard to the battlefield with a finality counter on it.",
        )
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::White),
            mana(ManaType::Black),
            mana(ManaType::Colorless),
        ],
    );
    let mut runner = scenario.build();
    runner
        .cast(zoraline)
        .commit()
        .stop_at_optional()
        .resolve()
        .assert_zone(&[zoraline], Zone::Battlefield);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    // CR 605.3a + CR 616.1: paying with Blood Pet pauses at the exile replacements.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            player: P0,
            candidate_count: 2,
            ..
        }
    ));
    // CR 104.3a: an uninvolved player may concede during this prompt.
    let departing = PlayerId(2);
    apply(
        runner.state_mut(),
        departing,
        GameAction::Concede {
            player_id: departing,
        },
    )
    .unwrap();
    let mut save =
        serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    let state = &mut save["state"];
    remove_saved_entry_token_status(&mut state["battlefield_entries_this_turn"][0], zoraline);
    remove_saved_entry_token_status(
        &mut state["payment_transaction"]["recovered"]["state"]["battlefield_entries_this_turn"][0],
        zoraline,
    );
    (save, gyome, zoraline, pet)
}

fn remove_saved_entry_token_status(entry: &mut serde_json::Value, entrant: ObjectId) {
    assert_eq!(entry["object_id"], serde_json::to_value(entrant).unwrap());
    assert!(entry
        .as_object_mut()
        .unwrap()
        .remove("token_status")
        .is_some());
}

#[test]
fn legacy_payment_checkpoint_repairs_entry_token_status_before_settlement() {
    let (save, gyome, zoraline, pet) = legacy_payment_checkpoint_save();
    let restored = serde_json::from_value::<PersistedGameState>(save)
        .unwrap()
        .into_game_state()
        .unwrap();
    let checkpoint = restored
        .payment_transaction
        .as_deref()
        .and_then(|transaction| transaction.recovered.as_deref())
        .expect("restore preserves the recovery checkpoint");
    let entry = &checkpoint.state.battlefield_entries_this_turn[0];
    assert_eq!(entry.object_id, zoraline);
    assert_eq!(entry.token_status, Some(EntryTokenStatus::Nontoken));

    let mut runner = GameRunner::from_state(restored);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let life_before = runner.life(P0);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert!(runner.state().payment_transaction.is_none());
    assert_eq!(runner.state().objects[&pet].zone, Zone::Exile);
    assert_eq!(runner.life(P0), life_before - 2);
    // CR 608.2i: Gyome looks back at Zoraline's entry in the settled game.
    assert_gyome_food_delta(&mut runner, gyome, 1);
}

#[test]
fn legacy_payment_checkpoint_refuses_unrecoverable_entry_token_status() {
    let (mut save, _, _, _) = legacy_payment_checkpoint_save();
    let missing = ObjectId(9_999);
    // Only the checkpoint ledger refers to this absent entrant. Its object and
    // entry zone-change record are both unavailable as repair evidence.
    save["state"]["payment_transaction"]["recovered"]["state"]["battlefield_entries_this_turn"]
        [0]["object_id"] = serde_json::to_value(missing).unwrap();
    let result = serde_json::from_value::<PersistedGameState>(save)
        .unwrap()
        .prepare_for_restore(PersistedRestoreFinalization::Immediate);
    assert!(matches!(
        result,
        Err(PersistedRestoreError::UnrecoverableBattlefieldEntryTokenStatus { object_id })
            if object_id == missing
    ));
}

fn legacy_bears_entry() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);
    let mut runner = scenario.build();
    runner
        .cast(bears)
        .resolve()
        .assert_zone(&[bears], Zone::Battlefield);
    assert_priority(&runner);
    (runner, gyome, bears)
}

#[test]
fn legacy_entry_token_status_is_recovered_for_same_incarnation() {
    let (runner, gyome, _) = legacy_bears_entry();
    let restored = legacy_save_without_entry_token_status(runner.state())
        .into_game_state()
        .unwrap();
    assert_eq!(
        restored.battlefield_entries_this_turn[0].token_status,
        Some(EntryTokenStatus::Nontoken)
    );
    assert_gyome_food_delta(&mut GameRunner::from_state(restored), gyome, 1);
}

#[test]
fn legacy_entry_token_status_is_recovered_from_entrant_object_only() {
    let (mut runner, gyome, bears) = legacy_bears_entry();
    let state = runner.state_mut();
    assert!(state
        .zone_changes_this_turn
        .iter()
        .any(|change| { change.object_id == bears && change.to_zone == Zone::Battlefield }));
    state.zone_changes_this_turn.clear();
    state.instruction_boundary_marks.clear();
    state.battlefield_entry_observations = Default::default();

    let legacy = legacy_save_without_entry_token_status(state);
    let restored = legacy.into_game_state().unwrap();
    assert_eq!(restored.objects[&bears].zone, Zone::Battlefield);
    let entry = &restored.battlefield_entries_this_turn[0];
    assert_eq!(entry.object_id, bears);
    assert_eq!(entry.token_status, Some(EntryTokenStatus::Nontoken));
    assert_gyome_food_delta(&mut GameRunner::from_state(restored), gyome, 1);
}

#[test]
fn legacy_entry_token_status_is_recovered_after_the_entrant_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);
    let mut runner = scenario.build();
    runner
        .cast(bears)
        .resolve()
        .assert_zone(&[bears], Zone::Battlefield);
    assert_priority(&runner);
    let entered_incarnation = runner.state().objects[&bears].incarnation;

    let mut events = Vec::new();
    let state = runner.state_mut();
    move_to_zone(state, bears, Zone::Hand, &mut events);
    assert_eq!(state.objects[&bears].zone, Zone::Hand);
    assert_ne!(state.objects[&bears].incarnation, entered_incarnation);

    let restored = legacy_save_without_entry_token_status(state)
        .into_game_state()
        .unwrap();
    assert_eq!(
        restored.battlefield_entries_this_turn[0].token_status,
        Some(EntryTokenStatus::Nontoken)
    );
    assert_gyome_food_delta(&mut GameRunner::from_state(restored), gyome, 1);
}

#[test]
fn legacy_entry_token_status_is_recovered_from_record_after_token_ceases_to_exist() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gyome = scenario
        .add_creature(P0, "Gyome, Master Chef", 5, 3)
        .from_oracle_text_with_keywords(&["Trample"], GYOME)
        .id();
    let soldiers = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(soldiers)
        .resolve()
        .assert_zone(&[soldiers], Zone::Graveyard);
    let entry = &runner.state().battlefield_entries_this_turn[0];
    let token = entry.object_id;
    assert_eq!(entry.token_status, Some(EntryTokenStatus::Token));
    assert!(entry.core_types.contains(&CoreType::Creature));
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);

    runner.cast(murder).target_objects(&[token]).resolve();
    assert_priority(&runner);
    let state = runner.state();
    assert!(state.zone_changes_this_turn.iter().any(|change| {
        change.object_id == token
            && change.from_zone == Some(Zone::Battlefield)
            && change.to_zone == Zone::Graveyard
    }));
    // CR 111.7 + CR 704.5d: the token ceases to exist in the graveyard.
    assert!(!state.objects.contains_key(&token));

    let restored = legacy_save_without_entry_token_status(state)
        .into_game_state()
        .unwrap();
    let entry = &restored.battlefield_entries_this_turn[0];
    assert_eq!(entry.object_id, token);
    assert_eq!(entry.token_status, Some(EntryTokenStatus::Token));
    assert_gyome_food_delta(&mut GameRunner::from_state(restored), gyome, 0);
}

#[test]
fn legacy_entry_token_status_refuses_a_merged_entrant_without_its_record() {
    let (mut runner, component, bears) = legacy_bears_entry();
    let state = runner.state_mut();
    merge_object_onto(state, component, bears, MergeSide::Bottom, &mut Vec::new());
    state.battlefield.retain(|&id| id != component);
    assert_eq!(state.objects[&bears].merged_components, [bears, component]);
    state.zone_changes_this_turn.clear();
    state.instruction_boundary_marks.clear();
    state.battlefield_entry_observations = Default::default();

    let result = legacy_save_without_entry_token_status(state)
        .prepare_for_restore(PersistedRestoreFinalization::Immediate);
    assert!(matches!(
        result,
        Err(PersistedRestoreError::UnrecoverableBattlefieldEntryTokenStatus { object_id })
            if object_id == bears
    ));
}

#[test]
fn legacy_entry_token_status_refuses_an_entrant_with_no_record_or_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    play_land(&mut runner, forest);
    let state = runner.state_mut();
    // Neither the entry's zone-change record nor the object survives to say
    // whether the entrant was a token.
    state.zone_changes_this_turn.clear();
    state.objects.remove(&forest);
    state.battlefield.retain(|id| *id != forest);

    let result = legacy_save_without_entry_token_status(state)
        .prepare_for_restore(PersistedRestoreFinalization::Immediate);
    assert!(matches!(
        result,
        Err(PersistedRestoreError::UnrecoverableBattlefieldEntryTokenStatus { object_id })
            if object_id == forest
    ));
}
