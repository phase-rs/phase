//! Kodama of the East Tree — anti-recursion intervening-if (CR 603.4).
//!
//! Oracle (relevant line):
//!   "Whenever another permanent you control enters, if it wasn't put onto the
//!    battlefield with this ability, you may put a permanent card with equal or
//!    lesser mana value from your hand onto the battlefield."
//!
//! The intervening-if `if it wasn't put onto the battlefield with this ability`
//! is an anti-recursion guard: a permanent that Kodama itself puts onto the
//! battlefield must NOT re-trigger Kodama (otherwise the player could chain the
//! entire hand onto the battlefield from one ETB). Previously the clause was
//! swallowed (`condition == null`), so Kodama-placed permanents re-triggered it.
//!
//! Fix (this change):
//!   - `GameObject.entered_via_ability_source` records the placing ability's
//!     source on every effect-driven battlefield entry (set in
//!     `deliver_replaced_zone_change`).
//!   - `TriggerCondition::PlacedByAbilitySource` reads that field and compares
//!     it to the trigger's own source id; the parser emits
//!     `Not(PlacedByAbilitySource)` for the "wasn't" phrasing.
//!
//! These tests drive the real cast -> stack -> ETB trigger -> resolution
//! pipeline through `apply` (not hand-constructed state), proving both:
//!   (1) a permanent entering by OTHER means DOES trigger Kodama, and
//!   (2) a permanent Kodama places does NOT re-trigger Kodama (anti-recursion).
//!
//! The "with equal or lesser mana value" qualifier is dropped from the fixture
//! Oracle text: it gates the eligible-card filter on the entering permanent's
//! mana value, which is orthogonal to the anti-recursion guard under test. The
//! "you may put a permanent card from your hand onto the battlefield" form is
//! already proven at runtime by `batched_trigger_subject_count.rs`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.4: intervening-if conditions are checked at trigger time and again
//!     at resolution.
//!   - CR 603.6a: each event that puts permanents onto the battlefield checks all
//!     permanents (including newcomers) for matching ETB triggers.
//!   - CR 400.7: a permanent that changes zones is a new object with no memory of
//!     its previous existence (provenance clears on entry/exit).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Kodama's trigger line only — the anti-recursion intervening-if guard plus the
/// optional put-from-hand effect (mana-value qualifier omitted; see module docs).
const KODAMA_TRIGGER: &str = "Whenever another permanent you control enters, if it wasn't put \
     onto the battlefield with this ability, you may put a permanent card from your hand onto \
     the battlefield.";

/// Cast a 0-cost creature from P0's hand through the real pipeline.
fn cast_free_creature(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting a 0-cost creature should succeed");
}

/// Drive the engine forward, passing priority to resolve the stack, until it
/// reaches the given target `WaitingFor` discriminant or the stack settles.
/// Returns `true` if an `OptionalEffectChoice` prompt was observed.
fn advance_to_optional_choice(runner: &mut engine::game::scenario::GameRunner) -> bool {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => return true,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return false;
                }
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

/// (1) A permanent entering by other means (a normally cast creature) DOES
///     trigger Kodama — its `entered_via_ability_source` is `None`, so
///     `Not(PlacedByAbilitySource)` is true and the trigger fires, raising the
///     optional "put a permanent from hand" prompt.
#[test]
fn permanent_entering_normally_triggers_kodama() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_TRIGGER)
        .id();
    // A 0-cost creature to cast (the non-ability entry that triggers Kodama).
    let trigger_creature = scenario
        .add_creature_to_hand(P0, "Plains Walker", 1, 1)
        .id();
    // A permanent for Kodama to optionally put from hand.
    scenario.add_creature_to_hand(P0, "Hand Beast", 2, 2).id();

    let mut runner = scenario.build();
    cast_free_creature(&mut runner, trigger_creature);

    assert!(
        advance_to_optional_choice(&mut runner),
        "CR 603.6a + CR 603.4: a creature cast normally enters with no \
         ability-placement provenance, so Kodama's `Not(PlacedByAbilitySource)` \
         guard is true and the trigger must fire (OptionalEffectChoice prompt)"
    );
}

/// (2) Anti-recursion: when Kodama itself puts a permanent from hand onto the
///     battlefield, that placement sets `entered_via_ability_source == Kodama`,
///     so `Not(PlacedByAbilitySource)` is false → Kodama does NOT re-trigger.
///     The stack settles after a single resolution; no second prompt appears.
#[test]
fn kodama_placed_permanent_does_not_retrigger_kodama() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_TRIGGER)
        .id();
    let trigger_creature = scenario
        .add_creature_to_hand(P0, "Plains Walker", 1, 1)
        .id();
    let hand_beast = scenario.add_creature_to_hand(P0, "Hand Beast", 2, 2).id();
    // A second eligible permanent so the engine raises an explicit
    // `EffectZoneChoice` (with a single eligible card it auto-selects — see
    // `change_zone::resolve`); having two forces the player-choice path.
    let _alternate = scenario
        .add_creature_to_hand(P0, "Alternate Beast", 1, 1)
        .id();

    let mut runner = scenario.build();
    cast_free_creature(&mut runner, trigger_creature);

    // Kodama's trigger fires for the cast creature.
    assert!(
        advance_to_optional_choice(&mut runner),
        "Kodama must trigger on the normally-cast creature"
    );

    // Accept the optional sub-ability and choose the hand permanent.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Kodama's optional put-from-hand sub-ability");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "accepting must prompt a hand-card selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_beast],
        })
        .expect("select Hand Beast to put onto the battlefield");

    // Resolve everything. If the anti-recursion guard were broken, the placed
    // Hand Beast would re-trigger Kodama, raising another OptionalEffectChoice
    // (and potentially looping). The guard holds → the stack settles to Priority.
    let retriggered = advance_to_optional_choice(&mut runner);
    assert!(
        !retriggered,
        "CR 603.4: the permanent Kodama placed has \
         `entered_via_ability_source == Kodama`, so `Not(PlacedByAbilitySource)` \
         is false — Kodama must NOT re-trigger on its own placement"
    );

    runner.advance_until_stack_empty();

    // Hand Beast reached the battlefield (the optional ability did resolve).
    assert_eq!(
        runner.state().objects[&hand_beast].zone,
        Zone::Battlefield,
        "the Kodama-placed permanent must be on the battlefield"
    );
    // The placed permanent carries the provenance stamp identifying Kodama.
    assert!(
        runner.state().objects[&hand_beast]
            .entered_via_ability_source
            .is_some(),
        "the Kodama-placed permanent must record its ability-placement provenance"
    );
    // And the stack is empty — no runaway recursion.
    assert!(
        runner.state().stack.is_empty(),
        "the stack must settle empty after a single Kodama resolution"
    );
}
