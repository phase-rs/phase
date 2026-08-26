//! CR 701.27f + CR 707.10: a COPY of an activated ability captures its source's
//! transformation generation at **copy-creation** time, not at the time the
//! original ability was put onto the stack.
//!
//! CR 701.27f: "If an activated or triggered ability of a permanent ... tries to
//! transform it, the permanent does so only if it hasn't transformed or
//! converted since the ability was put onto the stack." Copying such an ability
//! puts a NEW ability onto the stack (CR 707.10), so the copy's own
//! "since" boundary is the moment the copy was created.
//!
//! This is the discriminating test for `stack::push_copy_to_stack`, the
//! copy-onto-stack authority. It stamps the generation UNCONDITIONALLY, unlike
//! its put-onto-stack sibling `stack::push_to_stack`, which guards the stamp
//! with `is_none()` so that a delayed triggered ability's creation-time
//! generation survives firing. Applying that `is_none()` guard to a copy would
//! leave the copy carrying the ORIGINAL ability's older generation and silently
//! no-op — which is what this test's final assertion detects.
//!
//! Fixture cards (Oracle text verified against Scryfall):
//!   * Thraben Gargoyle // Stonewing Antagonizer — "Defender\n{6}: Transform
//!     this creature." An unconditional self-transform activated ability with no
//!     `{T}` in its cost, so it can be activated twice in one priority window.
//!   * Lithoform Engine — "{2}, {T}: Copy target activated or triggered ability
//!     you control. You may choose new targets for the copy."
//!
//! Thraben Gargoyle is printed as an Artifact Creature; this fixture builds it
//! as a plain creature because the CR 701.27f guard in `transform_effect.rs`
//! keys on `back_face` and `transformation_count`, never on card types.
//!
//! Stack ordering is the whole point. Activating in the order A1 → Lithoform
//! (targeting A1) → A2 makes the stack, bottom to top: [A1, COPY-ABILITY, A2].
//! Resolving LIFO:
//!   1. A2 transforms the Gargoyle — generation 0 -> 1.
//!   2. Lithoform copies A1 — the COPY is stamped at THIS moment.
//!   3. the COPY resolves — generation 1 -> 2, but only if it was stamped 1.
//!   4. A1 resolves — it is stamped 0 and the generation is now 2, so
//!      CR 701.27f correctly ignores it.
//!
//! Reverting `push_copy_to_stack` to the `is_none()`-guarded stamp leaves the
//! COPY carrying A1's generation 0 while the live generation is 1, so step 3
//! silently does nothing and the final count is 1 instead of 2.

use engine::game::casting::activated_ability_definitions;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const THRABEN_GARGOYLE_ORACLE: &str = "Defender\n{6}: Transform this creature.";
const LITHOFORM_ENGINE_ORACLE: &str = "{2}, {T}: Copy target activated or triggered ability you control. You may choose new targets for the copy.";

/// CR 712: the back face Thraben Gargoyle transforms into.
fn stonewing_antagonizer_back_face() -> BackFaceData {
    BackFaceData {
        name: "Stonewing Antagonizer".to_string(),
        power: Some(4),
        toughness: Some(2),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Gargoyle".to_string(), "Horror".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![engine::types::keywords::Keyword::Flying],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    }
}

fn ability_index_matching(
    runner: &GameRunner,
    source: ObjectId,
    label: &str,
    pred: impl Fn(&Effect) -> bool,
) -> usize {
    activated_ability_definitions(runner.state(), source)
        .into_iter()
        .find(|(_, ability)| pred(ability.effect.as_ref()))
        .unwrap_or_else(|| panic!("{label} must expose a matching activated ability"))
        .0
}

fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gargoyle = scenario
        .add_creature(P0, "Thraben Gargoyle", 2, 2)
        .from_oracle_text(THRABEN_GARGOYLE_ORACLE)
        .id();
    let lithoform = scenario
        .add_creature(P0, "Lithoform Engine", 0, 0)
        .as_artifact()
        .from_oracle_text(LITHOFORM_ENGINE_ORACLE)
        .id();
    // {6} + {6} for two Gargoyle activations, {2} for Lithoform Engine.
    scenario.with_mana_pool(
        P0,
        (0..14)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    // CR 701.27a: only a permanent with a back face can transform at all.
    runner
        .state_mut()
        .objects
        .get_mut(&gargoyle)
        .unwrap()
        .back_face = Some(stonewing_antagonizer_back_face());
    (runner, gargoyle, lithoform)
}

/// CR 701.27f + CR 707.10: the copy of the Gargoyle's transform ability is
/// created AFTER an intervening transform, so it captures the post-transform
/// generation and its own transform instruction is NOT ignored.
#[test]
fn copied_transform_ability_captures_copy_time_generation() {
    let (mut runner, gargoyle, lithoform) = setup();

    let transform_idx = ability_index_matching(&runner, gargoyle, "Thraben Gargoyle", |effect| {
        matches!(effect, Effect::Transform { .. })
    });
    let copy_idx = ability_index_matching(&runner, lithoform, "Lithoform Engine", |effect| {
        matches!(effect, Effect::CopySpell { .. })
    });

    let mut stack_pushed = 0usize;
    let mut record = |result: &engine::types::game_state::ActionResult| {
        stack_pushed += result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
            .count();
    };

    // A1: the ability whose copy this test is about. Stamped generation 0.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: gargoyle,
            ability_index: transform_idx,
        })
        .expect("Thraben Gargoyle's {6} transform ability must be activatable");
    record(&result);
    let a1 = runner
        .state()
        .stack
        .back()
        .expect("A1 must be on the stack")
        .id;

    // Lithoform Engine, targeting A1. Resolves ABOVE A1 but BELOW A2.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: lithoform,
            ability_index: copy_idx,
        })
        .expect("Lithoform Engine's {2}, {T} copy ability must be activatable");
    record(&result);
    // A1 is the only activated ability on the stack when Lithoform Engine's own
    // ability announces, so the engine auto-selects it (CR 601.2c). Assert the
    // binding rather than assuming it: if the copy ability ever targeted itself
    // or nothing, the whole ordering premise below would be silently wrong.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        let result = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(a1)],
            })
            .expect("A1 is an activated ability its controller controls — a legal target");
        record(&result);
    }
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone())
            .unwrap_or_default(),
        vec![TargetRef::Object(a1)],
        "Lithoform Engine's copy ability must be targeting A1, not itself"
    );

    // A2: the intervening transform. Resolves FIRST, moving the generation to 1
    // while A1 (stamped 0) and the not-yet-created copy are still on the stack.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: gargoyle,
            ability_index: transform_idx,
        })
        .expect("the {6} transform ability has no {T} in its cost, so it activates twice");
    record(&result);

    // REACH GUARD 1: all three activations actually landed on the stack in the
    // order this test depends on. Without this, a silently-failed activation
    // would make every later assertion meaningless.
    assert_eq!(
        runner.state().stack.len(),
        3,
        "expected [A1, copy-ability, A2] on the stack, got {:?}",
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    );

    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => record(&result),
            Err(_) => break,
        }
    }

    // REACH GUARD 2: a COPY was genuinely put onto the stack. Three activations
    // push three entries; a fourth `StackPushed` can only be the copy that
    // Lithoform Engine created. This fails FIRST if the fixture never produced a
    // copy, so the generation assertion below can never pass vacuously.
    assert_eq!(
        stack_pushed, 4,
        "expected 4 StackPushed events (A1, Lithoform ability, A2, and the COPY) \
         — a count of 3 means no copy reached the stack and this test proves nothing"
    );

    // REACH GUARD 3: the intervening transform (A2) really resolved. If the
    // Gargoyle never transformed at all, a final count of 1 vs 2 would be
    // measuring a dead fixture rather than the copy's captured generation.
    assert!(
        runner.state().objects[&gargoyle].transformation_count >= 1,
        "A2 must have transformed the Gargoyle at least once before the copy was created"
    );

    // THE DISCRIMINATOR (CR 701.27f + CR 707.10). The copy was stamped with the
    // generation as of copy-creation time (1), which still matches when it
    // resolves, so it transforms the Gargoyle a second time. Under the
    // `is_none()`-guarded stamp the copy inherits A1's generation 0, no longer
    // matches the live generation 1, and is ignored — leaving this at 1.
    assert_eq!(
        runner.state().objects[&gargoyle].transformation_count,
        2,
        "the copy must capture the generation at COPY-CREATION time (1) and transform; \
         a count of 1 means it inherited the original ability's stale generation 0"
    );

    // Transforming twice returns the permanent to its front face (CR 701.27a),
    // which is the visible consequence of the second transform having happened.
    assert!(
        !runner.state().objects[&gargoyle].transformed,
        "two transforms must land the Gargoyle back on its front face"
    );
    assert_eq!(runner.state().objects[&gargoyle].name, "Thraben Gargoyle");
}
