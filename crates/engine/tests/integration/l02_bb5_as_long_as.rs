//! L02 BB5 — `Condition_AsLongAs`: Braided Net, Cloud (Planet's Champion), and
//! Tishana's Tidebinder each dropped an "as long as" duration/condition clause.
//! All three fixes reuse pre-existing `Duration` / `StaticCondition` / rider
//! types (ZERO new engine enum variants; one field-add on
//! `CounterSourceRider::LosesAbilities`).
//!
//! Parse-fidelity tests assert the exact typed representation each parser now
//! emits; runtime tests drive the real layer pipeline and prove the emitted
//! duration/condition gates behavior ON→OFF (each names its revert-probe).
//!
//! Oracle text is verbatim from `data/card-data.json` (self-refs already
//! normalized: Braided Net uses "this artifact"/"it", Cloud "Cloud"/"it",
//! Tishana "this creature").
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 611.2b: "for as long as …" durations (Braided Net tapped-bound gate).
//!   - CR 602.5: a player can't begin to activate a prohibited ability.
//!   - CR 110.5b: tapped/untapped is a permanent status.
//!   - CR 611.3a: a static ability's continuous effect isn't locked in — its
//!     `condition` is re-checked (Cloud's during-your-turn + equipped gate).
//!   - CR 109.5: "your" on a static refers to the object's controller.
//!   - CR 301.5a: "equipped" / SourceIsEquipped.
//!   - CR 611.2a: UntilHostLeavesPlay (Tishana's loses-abilities duration).

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CounterSourceRider, Duration, Effect, ObjectScope, StaticCondition,
};
use engine::types::counter::parse_counter_type;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const BRAIDED_NET: &str = "This artifact enters with three net counters on it.\n{T}, Remove a net counter from this artifact: Tap another target nonland permanent. Its activated abilities can't be activated for as long as it remains tapped.\nCraft with artifact {1}{U}";

const CLOUD: &str = "During your turn, as long as Cloud is equipped, it has double strike and indestructible. (This creature deals both first-strike and regular combat damage. Damage and effects that say \"destroy\" don't destroy this creature.)\nEquip abilities you activate that target Cloud cost {2} less to activate.";

const TISHANA: &str = "Flash\nWhen this creature enters, counter up to one target activated or triggered ability. If an ability of an artifact, creature, or planeswalker is countered this way, that permanent loses all abilities for as long as this creature remains on the battlefield. (Mana abilities can't be targeted.)";

/// The tapped-bound duration Braided Net must emit (CR 611.2b + CR 110.5b): the
/// prohibition holds `for as long as` the grant's TARGET is tapped.
fn tapped_bound() -> Duration {
    Duration::ForAsLongAs {
        condition: StaticCondition::IsTapped {
            scope: ObjectScope::Target,
        },
    }
}

fn find_counter_rider(a: &AbilityDefinition) -> Option<&CounterSourceRider> {
    if let Effect::Counter {
        source_rider: Some(rider),
        ..
    } = &*a.effect
    {
        return Some(rider);
    }
    a.sub_ability.as_deref().and_then(find_counter_rider)
}

// ── Parse-fidelity ────────────────────────────────────────────────────────

/// Braided Net: the CantBeActivated grant's duration is
/// `ForAsLongAs { IsTapped { Target } }` on BOTH the `GenericEffect.duration`
/// and the clause `duration` (was hard-coded `UntilEndOfTurn`, which swallowed
/// the "for as long as it remains tapped" clause).
/// REVERT-PROBE: restore `UntilEndOfTurn` → both asserts fail.
#[test]
fn braided_net_emits_tapped_bound_duration() {
    let parsed = parse_oracle_text(
        BRAIDED_NET,
        "Braided Net",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    let sub = parsed
        .abilities
        .iter()
        .find_map(|a| a.sub_ability.as_deref())
        .expect("the tap ability must carry a CantBeActivated sub-ability");

    assert_eq!(
        sub.duration.as_ref(),
        Some(&tapped_bound()),
        "clause duration must be tapped-bound, not UntilEndOfTurn"
    );
    match &*sub.effect {
        Effect::GenericEffect { duration, .. } => assert_eq!(
            duration.as_ref(),
            Some(&tapped_bound()),
            "GenericEffect duration must be tapped-bound, not UntilEndOfTurn"
        ),
        other => panic!("expected a GenericEffect sub-ability, got {other:?}"),
    }
}

/// Cloud: the double-strike/indestructible static gains
/// `condition: And { [DuringYourTurn, SourceIsEquipped] }` (was fully swallowed
/// pre-fix — `statics: null`). REVERT-PROBE: remove the leading-condition peel →
/// no such static is produced.
#[test]
fn cloud_emits_during_turn_equipped_condition() {
    let parsed = parse_oracle_text(
        CLOUD,
        "Cloud, Planet's Champion",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Soldier".to_string()],
    );
    let expected = StaticCondition::And {
        conditions: vec![
            StaticCondition::DuringYourTurn,
            StaticCondition::SourceIsEquipped,
        ],
    };
    let stat = parsed
        .statics
        .iter()
        .find(|s| s.condition.as_ref() == Some(&expected))
        .unwrap_or_else(|| {
            panic!(
                "Cloud must produce a static gated on And{{DuringYourTurn, SourceIsEquipped}}; statics = {:?}",
                parsed.statics
            )
        });
    assert!(
        !stat.modifications.is_empty(),
        "the gated static must still grant the double-strike/indestructible keywords"
    );
}

/// Tishana: the `Counter` rider now carries an explicit
/// `duration: UntilHostLeavesPlay` (surfaces the "for as long as this creature
/// remains on the battlefield" clause into the AST).
/// REVERT-PROBE: drop the rider `duration` field → the field-add / serde marker
/// disappears; a non-UntilHostLeavesPlay value fails this assert.
#[test]
fn tishana_rider_carries_until_host_leaves_play() {
    let parsed = parse_oracle_text(
        TISHANA,
        "Tishana's Tidebinder",
        &["Flash".to_string()],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Merfolk".to_string(), "Wizard".to_string()],
    );
    let rider = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_counter_rider)
        .expect("Tishana's ETB must be a Counter with a LosesAbilities source_rider");
    match rider {
        CounterSourceRider::LosesAbilities { duration, .. } => assert_eq!(
            **duration,
            Duration::UntilHostLeavesPlay,
            "the loses-abilities rider must carry UntilHostLeavesPlay"
        ),
        other => panic!("expected a LosesAbilities rider, got {other:?}"),
    }
}

// ── Runtime ON→OFF ─────────────────────────────────────────────────────────

/// Braided Net runtime: activating the tap ability grants the target a
/// `CantBeActivated` static that is gated `ForAsLongAs { IsTapped { Target } }`.
/// While the target is tapped the static is applied; once untapped it lifts.
/// REVERT-PROBE (discriminates from the old `UntilEndOfTurn`): with
/// `UntilEndOfTurn` the prohibition persists after untap → the "untapped ⇒
/// absent" assert fails. The tapped assert guards against a vacuous "always
/// absent".
#[test]
fn braided_net_prohibition_tracks_tap_state() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let braided = scenario
        .add_creature_from_oracle(P0, "Braided Net", 0, 1, BRAIDED_NET)
        .as_artifact()
        .id();
    let target = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    scenario.with_counter(braided, parse_counter_type("net"), 3);
    let mut runner = scenario.build();

    // Activate {T}, Remove a net counter: Tap target — auto-pays the tap +
    // counter-removal cost. Resolution taps the target and registers the
    // tapped-bound transient continuous effect.
    runner.activate(braided, 0).target_object(target).resolve();

    let prohibited = |runner: &engine::game::scenario::GameRunner| {
        runner.state().objects[&target]
            .static_definitions
            .iter_unchecked()
            .any(|d| matches!(d.mode, StaticMode::CantBeActivated { .. }))
    };

    evaluate_layers(runner.state_mut());
    assert!(
        prohibited(&runner),
        "while tapped, the target's activated abilities must be prohibited"
    );

    // Untap the target and force a full layer recompute (CR 611.2b: the
    // for-as-long-as duration lapses).
    runner.state_mut().objects.get_mut(&target).unwrap().tapped = false;
    evaluate_layers(runner.state_mut());
    assert!(
        !prohibited(&runner),
        "once untapped, the prohibition must lift (ForAsLongAs IsTapped, not UntilEndOfTurn)"
    );
}

/// Cloud runtime: the double-strike/indestructible keywords are granted only
/// while it is Cloud's turn AND Cloud is equipped.
/// REVERT-PROBE: with `condition == None` (no peel) the keywords are granted
/// unconditionally → the "unequipped ⇒ lacks" asserts fail. The equipped asserts
/// guard against a vacuous "always lacks".
#[test]
fn cloud_keywords_gated_on_equipped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // P0's turn → DuringYourTurn holds
    let cloud = scenario
        .add_creature_from_oracle(P0, "Cloud, Planet's Champion", 3, 3, CLOUD)
        .id();
    let sword = scenario
        .add_creature(P0, "Buster Sword", 0, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    // Attach the Equipment to Cloud (real `attached_to` signal — CR 301.5a).
    {
        let obj = runner.state_mut().objects.get_mut(&sword).unwrap();
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(AttachTarget::Object(cloud));
    }
    evaluate_layers(runner.state_mut());
    assert!(
        runner.state().objects[&cloud].has_keyword(&Keyword::DoubleStrike),
        "equipped, on your turn: Cloud has double strike"
    );
    assert!(
        runner.state().objects[&cloud].has_keyword(&Keyword::Indestructible),
        "equipped, on your turn: Cloud has indestructible"
    );

    // Detach the Equipment and force a full layer recompute — SourceIsEquipped
    // is now false, so both keywords must be gone.
    runner
        .state_mut()
        .objects
        .get_mut(&sword)
        .unwrap()
        .attached_to = None;
    evaluate_layers(runner.state_mut());
    assert!(
        !runner.state().objects[&cloud].has_keyword(&Keyword::DoubleStrike),
        "unequipped: Cloud must lose double strike (condition-gated, not unconditional)"
    );
    assert!(
        !runner.state().objects[&cloud].has_keyword(&Keyword::Indestructible),
        "unequipped: Cloud must lose indestructible (condition-gated, not unconditional)"
    );
}
