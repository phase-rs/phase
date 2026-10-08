//! Coverage for issue #5942 ("Undying Malice effect doesn't trigger with
//! edict effects"): a creature granted a "dies" trigger by a
//! `GenericEffect`/`Continuous`/`GrantTrigger` static ability (Undying
//! Malice) must still have that trigger fire when the creature leaves the
//! battlefield via an "edict" sacrifice (a resolving `Effect::Sacrifice`
//! chosen by the affected player), not just a directly-targeted/known
//! sacrifice.
//!
//! CARD TEXT (verified against `client/public/card-data.json`):
//!   Undying Malice — "Until end of turn, target creature gains \"When this
//!   creature dies, return it to the battlefield tapped under its owner's
//!   control with a +1/+1 counter on it.\""
//!   Diabolic Edict — "Target player sacrifices a creature of their choice."
//!   Innocent Blood — "Each player sacrifices a creature of their choice."
//!
//! CR 603.6d + CR 603.10a: a leaves-the-battlefield triggered ability looks
//! back in time to the object's existence immediately before it left,
//! including abilities it had at that time due to a continuous effect — the
//! grant does not need to still be "live" after the object is gone.
//!
//! NOTE: this `GenericEffect`/`ParentTarget`/`GrantTrigger` seam had zero
//! direct test coverage before this file. Investigation of #5942 could not
//! reproduce the reported defect on current `main` across every plausible
//! edict shape (single-target player choice, single-target mandatory
//! fast-path, casting Malice in response to the edict already on the stack,
//! and a multiplayer simultaneous `player_scope: All` edict) — all four
//! below pass. These are added as durable coverage for a previously-untested
//! path, not as a regression test that failed before a fix; see the issue
//! comment for the full investigation.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const UNDYING_MALICE_ORACLE: &str = "Until end of turn, target creature gains \"When this \
creature dies, return it to the battlefield tapped under its owner's control with a +1/+1 \
counter on it.\"";

const DIABOLIC_EDICT_ORACLE: &str = "Target player sacrifices a creature of their choice.";

const INNOCENT_BLOOD_ORACLE: &str = "Each player sacrifices a creature of their choice.";

/// Add `count` units of `ty` mana to `player`'s pool — deterministic payment
/// without modelling lands (mirrors `chord_of_calling.rs::add_mana`).
fn add_mana(runner: &mut GameRunner, player: PlayerId, ty: ManaType, count: usize) {
    let unit_source = ObjectId(0);
    let target = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for _ in 0..count {
        target
            .mana_pool
            .add(ManaUnit::new(ty, unit_source, false, vec![]));
    }
}

/// RUNTIME — CR 603.6d + CR 603.10a. Undying Malice grants a
/// dies-trigger to a creature; a second player then Diabolic-Edicts the
/// controller, who has TWO eligible creatures and so must choose one via the
/// interactive `EffectZoneChoice` pool-choice path (not the mandatory-sac
/// fast path). Choosing the Undying-Malice creature must still return it to
/// the battlefield tapped with a +1/+1 counter.
#[test]
fn undying_malice_returns_creature_sacrificed_to_edict() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P0, "Doomed Traveler", 1, 1).id();
    let _decoy = scenario.add_creature(P0, "Decoy Bear", 2, 2).id();

    let malice = scenario
        .add_spell_to_hand_from_oracle(P0, "Undying Malice", true, UNDYING_MALICE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let edict = scenario
        .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 1);
    add_mana(&mut runner, P1, ManaType::Black, 2);

    // P0 casts Undying Malice targeting their own creature.
    runner.cast(malice).target_object(victim).resolve();

    // Hand priority to P1 so they may cast Diabolic Edict (CR 117.3c).
    // `waiting_for` is the engine's authority for whose action is legal — a
    // cast is rejected with `NotYourPriority` unless it agrees.
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // P1 casts Diabolic Edict targeting P0; P0 has two eligible creatures, so
    // the sacrifice pauses at `EffectZoneChoice` and P0 chooses `victim`.
    let outcome = runner
        .cast(edict)
        .target_player(P0)
        .effect_zone(&[victim])
        .resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);

    let obj = &outcome.state().objects[&victim];
    assert!(obj.tapped, "Undying Malice returns the creature tapped");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "Undying Malice returns the creature with exactly one +1/+1 counter"
    );
}

/// RUNTIME (mandatory fast path) — same as above, but `victim` is P0's ONLY
/// creature, so `resolve_sacrifice_scope`'s `eligible.len() <= count` branch
/// takes the synchronous fast path (no `EffectZoneChoice` pause) rather than
/// the interactive pool-choice path exercised above.
#[test]
fn undying_malice_returns_creature_sacrificed_to_mandatory_edict() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P0, "Doomed Traveler", 1, 1).id();

    let malice = scenario
        .add_spell_to_hand_from_oracle(P0, "Undying Malice", true, UNDYING_MALICE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let edict = scenario
        .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 1);
    add_mana(&mut runner, P1, ManaType::Black, 2);

    runner.cast(malice).target_object(victim).resolve();

    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(edict).target_player(P0).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);

    let obj = &outcome.state().objects[&victim];
    assert!(obj.tapped, "Undying Malice returns the creature tapped");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "Undying Malice returns the creature with exactly one +1/+1 counter"
    );
}

/// RUNTIME (`player_scope: All` — "each player sacrifices") — Innocent
/// Blood's `player_scope`-looped sacrifice moves TWO creatures from TWO
/// different controllers "simultaneously" (CR 101.4 / CR 603.3b), a
/// structurally different path (`player_scope_sacrifice_step` +
/// `mark_simultaneous_departures`) from the single-target Diabolic Edict
/// tests above. P0's creature carries the Undying Malice grant; P1's does
/// not. Only P0's creature should return.
#[test]
fn undying_malice_returns_creature_sacrificed_to_each_player_edict() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P0, "Doomed Traveler", 1, 1).id();
    let opponent_creature = scenario.add_creature(P1, "Bear", 2, 2).id();

    let malice = scenario
        .add_spell_to_hand_from_oracle(P0, "Undying Malice", true, UNDYING_MALICE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let blood = scenario
        .add_spell_to_hand_from_oracle(P1, "Innocent Blood", false, INNOCENT_BLOOD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    // Innocent Blood is a sorcery, castable only by the active player (CR
    // 307.1) — P1 is active so they may cast it on their own main phase.
    runner.state_mut().active_player = P1;
    add_mana(&mut runner, P0, ManaType::Black, 1);
    add_mana(&mut runner, P1, ManaType::Black, 1);

    // P0 casts the instant Undying Malice on P1's turn (CR 117.1b).
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(malice).target_object(victim).resolve();

    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(blood).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);
    outcome.assert_zone(&[opponent_creature], Zone::Graveyard);

    let obj = &outcome.state().objects[&victim];
    assert!(obj.tapped, "Undying Malice returns the creature tapped");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "Undying Malice returns the creature with exactly one +1/+1 counter"
    );
}

/// RUNTIME (respond-to-the-edict-on-the-stack) — the canonical real-game
/// pattern: an opponent's edict is ALREADY on the stack targeting the
/// controller, who casts Undying Malice in response on their only creature to
/// protect it, then passes. Malice resolves first (LIFO), granting the dies
/// trigger; the edict then resolves and forces the mandatory sacrifice.
#[test]
fn undying_malice_cast_in_response_to_edict_on_stack() {
    use engine::game::zones::create_object;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{ResolvedAbility, TargetRef};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use engine::types::identifiers::CardId;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P0, "Doomed Traveler", 1, 1).id();

    let malice = scenario
        .add_spell_to_hand_from_oracle(P0, "Undying Malice", true, UNDYING_MALICE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 1);

    // P1's Diabolic Edict already on the stack, targeting P0.
    let edict_parsed = parse_oracle_text(
        DIABOLIC_EDICT_ORACLE,
        "Diabolic Edict",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let edict_id = create_object(
        runner.state_mut(),
        CardId(9001),
        P1,
        "Diabolic Edict".to_string(),
        Zone::Stack,
    );
    let edict_ability = ResolvedAbility::new(
        edict_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Player(P0)],
        edict_id,
        P1,
    );
    {
        let edict_obj = runner.state_mut().objects.get_mut(&edict_id).unwrap();
        edict_obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: edict_id,
        source_id: edict_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(9001),
            ability: Some(Box::new(edict_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Hand P0 the priority window to respond.
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // P0 casts Undying Malice in response, targeting their only creature.
    // `resolve()` drives the whole remaining stack: Malice resolves first
    // (LIFO), then the Edict resolves and forces the mandatory sacrifice.
    let outcome = runner.cast(malice).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);

    let obj = &outcome.state().objects[&victim];
    assert!(obj.tapped, "Undying Malice returns the creature tapped");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "Undying Malice returns the creature with exactly one +1/+1 counter"
    );
}
