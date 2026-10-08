//! #7552: a Role token's catalog `token_image_ref` must survive an UNLISTED
//! source card. Every Role exists on two flip sheets, so the preset scan finds
//! two semantically identical candidates; the source gate used to turn that
//! into a silent `None`, stranding the display on a name search no printing
//! satisfies (the engine names Roles "<Role> Role", printings are titled by the
//! bare face — CR 111.10 / `role_normalized_display_name`).
//!
//! Also proven here (#7555 review): the ambiguity protection for semantically
//! DIFFERENT body-matches. The catalog really carries such twins — a bare
//! 1/1 red Goblin body matches the plain Goblin preset, Goblin Spymaster's
//! "attack each combat if able" token AND Hold the Perimeter's "can't block"
//! token — so an unlisted source resolving that body must get `None`, never a
//! silent first pick.

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

fn pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn a_role_from_an_unlisted_source_still_carries_its_catalog_image_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(4));
    let host = scenario.add_creature(P0, "Chosen Host", 2, 2).id();
    let caster = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Wicked Bard",
            1,
            1,
            "When this creature enters, create a Wicked Role token attached to target creature.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(caster).target_object(host).resolve();
    runner.advance_until_stack_empty();

    let role = runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            runner.state().objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|sub| sub == "Role")
        })
        .copied()
        .expect("the Wicked Role token exists");
    let obj = &runner.state().objects[&role];
    assert!(
        obj.token_image_ref.is_some(),
        "the catalog carries a Wicked image ref; the created token must too \
         (name={:?}, colors={:?}, subtypes={:?})",
        obj.name,
        obj.color,
        obj.card_types.subtypes
    );
}

/// The positive gate: a source the catalog DOES list keeps resolving exactly as
/// before — this row is what keeps the fallback from being the whole mechanism.
#[test]
fn a_role_from_a_listed_source_resolves_its_image_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(4));
    let host = scenario.add_creature(P0, "Chosen Host", 2, 2).id();
    // "Monstrous Rage" is in the Monster preset's `source_card_names`.
    let caster = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Monstrous Rage",
            true,
            "Target creature gets +2/+0 until end of turn. Create a Monster Role token attached to it.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(caster).target_object(host).resolve();
    runner.advance_until_stack_empty();

    let role = runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            runner.state().objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|sub| sub == "Role")
        })
        .copied()
        .expect("the Monster Role token exists");
    assert!(
        runner.state().objects[&role].token_image_ref.is_some(),
        "the listed-source path must keep resolving"
    );
}

/// #7555 review: a body the catalog holds with SEMANTICALLY DIFFERENT presets
/// (same name/types/colors/P/T, different rules text) must resolve NO image
/// ref from an unlisted source — a deterministic first pick would show art
/// (and reminder text) of a token the game never made.
#[test]
fn an_ambiguous_body_from_an_unlisted_source_resolves_no_image_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(4));
    let caster = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Backstreet Recruiter",
            1,
            1,
            "When this creature enters, create a 1/1 red Goblin creature token.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(caster).resolve();
    runner.advance_until_stack_empty();

    let goblin = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Goblin")
        .copied()
        .expect("the Goblin token exists — the ambiguous body WAS created");
    assert!(
        runner.state().objects[&goblin].token_image_ref.is_none(),
        "an ambiguous body (plain / Spymaster / Hold the Perimeter Goblins) \
         must resolve no ref from an unlisted source"
    );
}

/// Reach guard for the row above (both-candidates proof): the SAME bare body
/// resolves TWO DISTINCT presets when the source is listed — Krenko's Command
/// is in the plain Goblin preset's `source_card_names`, Goblin Spymaster only
/// in the "Creatures you control attack each combat if able." variant's. Both
/// listed rows pass the resolver's `token_body_matches` gate, so at least two
/// semantically different presets match this body. The `None` above therefore
/// cannot mean "no body-matching candidate": the body-only fallback saw the
/// candidates and refused to pick between them.
///
/// The Spymaster create clause is simplified to an ETB line: the subject here
/// is the resolver's name gate, not the card's opponent-end-step timing. The
/// token's quoted grant is omitted deliberately — preset BODIES exclude rules
/// text (that lives in `rules_text`, the semantic tie-breaker under test).
#[test]
fn the_same_body_resolves_two_distinct_presets_from_their_listed_sources() {
    let resolve_for = |source_name: &str, oracle: &str| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, pool(4));
        let caster = scenario
            .add_creature_to_hand_from_oracle(P0, source_name, 1, 1, oracle)
            .id();
        let mut runner = scenario.build();
        runner.cast(caster).resolve();
        runner.advance_until_stack_empty();

        let goblin = runner
            .state()
            .battlefield
            .iter()
            .find(|id| runner.state().objects[id].name == "Goblin")
            .copied()
            .expect("the Goblin token exists");
        runner.state().objects[&goblin]
            .token_image_ref
            .clone()
            .expect("a listed source resolves this body")
    };

    let plain = resolve_for(
        "Krenko's Command",
        "When this creature enters, create a 1/1 red Goblin creature token.",
    );
    let must_attack = resolve_for(
        "Goblin Spymaster",
        "When this creature enters, create a 1/1 red Goblin creature token.",
    );

    assert_ne!(
        plain.preset_id, must_attack.preset_id,
        "the two listed sources reach two DISTINCT body-matching presets — \
         the pair the unlisted row's ambiguity guard refuses to pick between"
    );
}
