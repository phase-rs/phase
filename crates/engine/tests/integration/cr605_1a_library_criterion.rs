//! CR 605.1a (2026 amendment) — the library-movement criterion, end to end.
//!
//! > 605.1a An activated ability is a mana ability if it meets all of the
//! > following criteria: it doesn't require a target (see rule 115.6), it could
//! > add mana to a player's mana pool when it resolves, it's not a loyalty
//! > ability (see rule 606, "Loyalty Abilities"), and **its cost and effect
//! > don't move any card to or from a library.**
//!
//! The classifier itself is unit-tested in `game/mana_abilities.rs` (rows
//! V1-V13). This file covers the four consequences that are only observable by
//! driving the real pipeline, and it deliberately asserts **no** AST-internal
//! flag: `is_mana_ability` never appears in an assertion here.
//!
//! | Row | Claim | Seam |
//! |---|---|---|
//! | V14  | a reclassified ability still produces its mana, **via the stack** | `engine.rs` dispatch fork -> `casting::handle_activate_ability` -> `stack::push_to_stack` |
//! | V14b | it leaves the instant-speed auto-tap pool | `mana_sources::activatable_mana_actions_for_player` |
//! | V15  | `FilterProp::HasManaAbility` stops matching it | `game/filter.rs` |
//! | V16  | `TriggerCondition::ActivatedAbilityIsNonMana` now fires on it | `game/triggers.rs` (unchanged code, changed input) |
//!
//! Every negative is paired with a positive reach-guard **in the same test**,
//! built from a still-qualifying mana source on the same battlefield, so a
//! fixture that never reached the seam cannot pass vacuously.

use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text. A paraphrase can take a different parser branch and go
/// green while the real card stays broken, so these are exact.
const CHROMATIC_SPHERE: &str =
    "{1}, {T}, Sacrifice this artifact: Add one mana of any color. Draw a card.";
const MILLIKIN: &str = "{T}, Mill a card: Add {C}.";
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
const RAGGADRAGGA: &str = "Each creature you control with a mana ability gets +2/+2.";
const BURNING_TREE_SHAMAN: &str = "Whenever a player activates an ability that isn't a mana \
                                   ability, this creature deals 1 damage to that player.";

fn generic_unit() -> ManaUnit {
    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])
}

/// **V14** — a reclassified ability still produces its mana and performs its
/// draw, and it does so through the **stack**.
///
/// Chromatic Sphere's draw makes its own resolution move a card from a library,
/// so under CR 605.1a it is no longer a mana ability. CR 605.3b ("an activated
/// mana ability doesn't use the stack") therefore stops applying to it: the
/// dispatch fork falls through to the ordinary activated-ability path and the
/// ability is put on the stack, where opponents get priority before the draw.
/// That is the point of the reclassification, not a side effect of it.
#[test]
fn chromatic_sphere_produces_mana_and_draws_through_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Make the draw observable rather than inferred from a hand count alone.
    scenario.with_library_top(P0, &["Forest", "Library Bottom"]);
    // The Sphere's cost is `{1}, {T}, Sacrifice this artifact`. Without a funded
    // pool the `{1}` is unpayable and the activation cannot even begin.
    scenario.with_mana_pool(P0, vec![generic_unit()]);
    let sphere = scenario
        .add_creature(P0, "Chromatic Sphere", 0, 0)
        .as_artifact()
        .from_oracle_text(CHROMATIC_SPHERE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.activate(sphere, 0).resolve();

    // The ability resolved: one card drawn, and the artifact was sacrificed as
    // part of paying the cost.
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_zone(&[sphere], Zone::Graveyard);
    // And the mana was still produced — the reclassification changes the ROUTE,
    // never the payload.
    assert!(
        outcome.mana_pool_total(P0) >= 1,
        "the mana ability's mana must still reach the pool"
    );
    // The ROUTE itself, which the three assertions above cannot see: all of them
    // hold identically on the off-stack mana fast path, so without this the test
    // would pass unchanged if the Sphere were still classified as a mana
    // ability. `GameEvent::AbilityActivated` is documented on the variant as
    // never emitted for mana abilities (CR 605.3b — they resolve immediately on
    // a separate path that never reaches the emission site), so its presence is
    // the discriminator between the stack path and the fast path.
    assert!(
        outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::AbilityActivated { .. })),
        "CR 605.3b: no longer a mana ability, so the activation must use the \
         stack and emit AbilityActivated (events: {:?})",
        outcome.events()
    );
}

/// **V14b** — a reclassified ability drops out of the instant-speed auto-tap
/// pool, because CR 605.3a grants a player permission to activate a **mana
/// ability** mid-cast or mid-payment. That permission is an exception; once an
/// ability stops being a mana ability the exception no longer covers it and the
/// general priority rule (CR 117.1b) governs again. An affordance a player
/// cannot legally use would be a UI lie, so the payment picker must stop
/// offering it.
///
/// The paired reach-guard is a Llanowar Elves on the same battlefield: it is
/// still a mana ability, so it must still be offered. Without that pair the
/// negative could pass because the harness produced no actions at all.
#[test]
fn chromatic_sphere_is_not_offered_as_an_instant_speed_mana_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Library Bottom"]);
    scenario.with_mana_pool(P0, vec![generic_unit()]);
    let sphere = scenario
        .add_creature(P0, "Chromatic Sphere", 0, 0)
        .as_artifact()
        .from_oracle_text(CHROMATIC_SPHERE)
        .id();
    let elves = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .from_oracle_text(LLANOWAR_ELVES)
        .id();

    let runner = scenario.build();
    let actions = activatable_mana_actions_for_player(runner.state(), P0);

    // Match the typed `source_id`, never a substring of the debug rendering: a
    // stringified `ObjectId` of `1` is a substring of `10`, `11`, `21`, ... so a
    // debug-text `contains` would answer for the wrong object.
    let mentions = |target: ObjectId| {
        actions.iter().any(|action| {
            matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == target)
        })
    };

    // Reach-guard first: a still-qualifying mana source IS offered, so the
    // enumeration ran and produced a non-empty, correctly-scoped result.
    assert!(
        mentions(elves),
        "Llanowar Elves is still a mana ability and must stay in the pool \
         (actions: {actions:?})"
    );
    assert!(
        !mentions(sphere),
        "Chromatic Sphere is no longer a mana ability (CR 605.1a), so CR 605.3a's \
         permission no longer covers it and the general priority rule governs — \
         it must not be offered in a payment window (actions: {actions:?})"
    );
}

/// **V15** — the in-game behavioral negative: `FilterProp::HasManaAbility` stops
/// matching a reclassified ability, because the prop is *defined by reference
/// to* CR 605.1a. Narrowing 605.1a therefore necessarily narrows every card that
/// keys off it.
///
/// Raggadragga, Goreguts Boss grants `+2/+2` to "each creature you control with
/// a mana ability" — a CR 613 layer-7c continuous effect, so the displayed P/T
/// changes on the board the instant this ships. Millikin's `{T}, Mill a card:
/// Add {C}` moves a card from a library as its **cost**, so it loses the anthem.
///
/// Paired reach-guard, same battlefield, same Raggadragga: Llanowar Elves is
/// still a mana ability and still gets the +2/+2. Without it the negative could
/// pass because Raggadragga was absent, under the wrong controller, or its
/// static never applied at all.
#[test]
fn raggadragga_no_longer_pumps_a_mill_cost_mana_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Library Bottom"]);
    scenario
        .add_creature(P0, "Raggadragga, Goreguts Boss", 4, 4)
        .from_oracle_text(RAGGADRAGGA);
    let millikin = scenario
        .add_creature(P0, "Millikin", 0, 3)
        .as_artifact()
        .from_oracle_text(MILLIKIN)
        .id();
    let elves = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .from_oracle_text(LLANOWAR_ELVES)
        .id();

    // The layer system materializes post-continuous-effect P/T back into
    // `GameObject::power` / `toughness` during `apply`, so drive one real action
    // (activating the still-qualifying Elves) to get a post-pipeline state
    // rather than reading pre-layer fields off a freshly-built runner.
    let mut runner = scenario.build();
    let outcome = runner.activate(elves, 0).resolve();

    // Reach-guard: the anthem is installed and applying to a qualifying
    // creature, so the negative below is a genuine non-match.
    assert_eq!(
        outcome.power_toughness(elves),
        (3, 3),
        "Llanowar Elves is still a mana ability and must get Raggadragga's +2/+2"
    );
    assert_eq!(
        outcome.power_toughness(millikin),
        (0, 3),
        "Millikin's Mill cost moves a card from a library, so under CR 605.1a it \
         has no mana ability and must NOT get the +2/+2"
    );
}

/// **V16** — the in-game behavioral negative in the other direction:
/// `TriggerCondition::ActivatedAbilityIsNonMana` now fires on a reclassified
/// activation.
///
/// This needs no code change — it is unchanged code seeing a changed input. A
/// mana ability never reaches a `GameEvent::AbilityActivated` emission site,
/// because all three sit downstream of a `stack::push_to_stack` and the mana
/// fast path forks before it. Once Chromatic Sphere is not a mana ability it
/// takes the stack path, `AbilityActivated` is emitted, and the trigger sees it.
///
/// Under CR 605.1a that is **correct**: the Sphere's ability genuinely is not a
/// mana ability, so a trigger reading "whenever a player activates an ability
/// that isn't a mana ability" *should* see it. This row also pins the behavior
/// so that `triggers.rs`'s comment — which today says `AbilityActivated` is
/// emitted only by stack-using activations "by construction" — cannot silently
/// rot into a false claim once the invariant becomes classification-based.
#[test]
fn burning_tree_shaman_now_sees_a_chromatic_sphere_activation() {
    fn activate_and_measure_life(oracle: &str, name: &str, fund_pool: bool) -> i32 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Forest", "Library Bottom"]);
        if fund_pool {
            scenario.with_mana_pool(P0, vec![generic_unit()]);
        }
        scenario
            .add_creature(P0, "Burning-Tree Shaman", 3, 4)
            .from_oracle_text(BURNING_TREE_SHAMAN);
        let mut builder = scenario.add_creature(P0, name, 0, 0);
        if name == "Chromatic Sphere" {
            builder.as_artifact();
        }
        let source = builder.from_oracle_text(oracle).id();

        let mut runner = scenario.build();
        let outcome = runner.activate(source, 0).resolve();
        outcome.life_delta(P0)
    }

    // Reach-guard FIRST — and here the reach-guard is the POSITIVE case. Only a
    // -1 life delta is evidence that the Shaman is on the battlefield with a
    // live trigger; the 0 below would also pass on a fixture with no Shaman at
    // all, because "nothing happened" and "nothing could have happened" are the
    // same observation. Do not reorder these or drop this one: the negative
    // carries no reachability evidence on its own.
    assert_eq!(
        activate_and_measure_life(CHROMATIC_SPHERE, "Chromatic Sphere", true),
        -1,
        "Chromatic Sphere is no longer a mana ability, so it uses the stack, \
         emits AbilityActivated, and Burning-Tree Shaman pings its controller"
    );
    // The discriminating negative: a still-qualifying mana ability must NOT
    // trigger it, so the reclassification is what moved and not the fixture.
    assert_eq!(
        activate_and_measure_life(LLANOWAR_ELVES, "Llanowar Elves", false),
        0,
        "Llanowar Elves is still a mana ability — CR 605.3b keeps it off the \
         stack, so no AbilityActivated event and no trigger"
    );
}
