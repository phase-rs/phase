//! Squandered Resources: "Sacrifice a land: Add one mana of any type the
//! sacrificed land could produce."
//!
//! Scryfall ruling: "Can sacrifice a land which can't produce mana, but you
//! don't get any mana from this ability."
//!
//! CR 106.7 — the type of mana a permanent "could produce" is any type an
//! ability of that permanent would produce if it resolved now, ignoring whether
//! its costs could be paid. CR 106.5 — an ability that would produce mana of an
//! undefined type produces no mana. CR 608.2k + CR 400.7j — "the sacrificed
//! land" is the untargeted object the cost moved; CR 608.2h reads it through
//! last known information. CR 605.1a — the ability is a mana ability, so it
//! resolves without the stack (CR 605.3b) and can be activated while paying a
//! spell's cost (CR 605.3a + CR 117.1d + CR 601.2g).
//!
//! Every row drives the real pipeline (`ActivateAbility` → `PayCost` →
//! `SelectCards` → optional `ChooseManaColor`) and asserts that the sacrifice
//! really ran before asserting what mana arrived.

use engine::ai_support::legal_actions;
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::casting::can_cast_object_now;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::PhaseOutCause;
use engine::game::layers::flush_layers;
use engine::game::mana_abilities::can_activate_mana_ability_now;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, ManaProduction};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::parse_counter_type;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaChoice, ManaChoicePrompt, PayCostKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall).
const SQUANDERED_RESOURCES: &str =
    "Sacrifice a land: Add one mana of any type the sacrificed land could produce.";
const WASTES: &str = "{T}: Add {C}.";
const SIMIC_GUILDGATE: &str = "This land enters tapped.\n{T}: Add {G} or {U}.";
const EVOLVING_WILDS: &str = "{T}, Sacrifice this land: Search your library for a basic land \
     card, put it onto the battlefield tapped, then shuffle.";
const URBORG: &str = "Each land is a Swamp in addition to its other land types.";
const REFLECTING_POOL: &str =
    "{T}: Add one mana of any type that a land you control could produce.";

/// What one Squandered Resources activation surfaced before it finished.
struct Activation {
    /// The `ChooseManaColor` options, when the activation paused for a choice.
    /// `None` means the mana arrived without a prompt.
    prompt_options: Option<Vec<ManaType>>,
}

fn board() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    (scenario, squandered)
}

/// Activates Squandered Resources and sacrifices `land` to it. Stops at the
/// colour prompt (returning its options) or once the mana has arrived.
///
/// There is deliberately no `PassPriority` arm: a non-empty stack hits the
/// panic arm, so reaching the end proves the ability resolved as a mana ability
/// without using the stack (CR 605.3b).
fn activate_and_sacrifice(
    runner: &mut GameRunner,
    squandered: ObjectId,
    land: ObjectId,
) -> Activation {
    runner
        .act(GameAction::ActivateAbility {
            source_id: squandered,
            ability_index: 0,
        })
        .expect("reach-guard: Squandered Resources must be activatable");

    let mut saw_sacrifice_prompt = false;
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&land),
                    "reach-guard: the land must be offered to the sacrifice cost; got {choices:?}"
                );
                saw_sacrifice_prompt = true;
                runner
                    .act(GameAction::SelectCards { cards: vec![land] })
                    .expect("sacrificing the chosen land must succeed");
            }
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert!(
                    saw_sacrifice_prompt,
                    "the colour prompt follows the sacrifice"
                );
                return Activation {
                    prompt_options: Some(options),
                };
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::ManaPayment { .. } => break,
            other => panic!("unexpected state while activating Squandered Resources: {other:?}"),
        }
    }

    assert!(
        saw_sacrifice_prompt,
        "reach-guard: the sacrifice cost prompt must have been surfaced"
    );
    Activation {
        prompt_options: None,
    }
}

fn choose(runner: &mut GameRunner, mana_type: ManaType) {
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(mana_type),
            count: 1,
        })
        .expect("a type from the prompt must be accepted");
}

fn pool(runner: &GameRunner, player: PlayerId) -> Vec<ManaType> {
    runner.state().players[player.0 as usize]
        .mana_pool
        .mana
        .iter()
        .map(|unit| unit.color)
        .collect()
}

fn sorted(mut types: Vec<ManaType>) -> Vec<ManaType> {
    types.sort_by_key(|mana_type| format!("{mana_type:?}"));
    types
}

fn assert_in_graveyard(runner: &GameRunner, land: ObjectId) {
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "reach-guard: the sacrifice cost must have moved the land to the graveyard"
    );
}

fn tap(runner: &mut GameRunner, object_id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .expect("the object exists")
        .tapped = true;
}

/// T1: a Forest could produce only {G}, so Squandered adds {G} with no prompt.
#[test]
fn sacrificed_forest_adds_green() {
    let (mut scenario, squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T2: the referent is the sacrificed land, not every land you control. An
/// untapped Island stays on the battlefield and contributes nothing.
#[test]
fn only_the_sacrificed_land_is_consulted() {
    let (mut scenario, squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    let island_object = &runner.state().objects[&island];
    assert_eq!(island_object.zone, Zone::Battlefield);
    assert!(
        !island_object.tapped,
        "reach-guard: the Island stays untapped"
    );
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T3: a dual land offers exactly its two types, and a type outside that set
/// is refused.
#[test]
fn sacrificed_dual_land_offers_both_of_its_types() {
    let (mut scenario, squandered) = board();
    let guildgate = scenario
        .add_land_from_oracle(P0, "Simic Guildgate", SIMIC_GUILDGATE)
        .id();
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, guildgate);

    assert_in_graveyard(&runner, guildgate);
    let options = activation
        .prompt_options
        .expect("a two-type land must prompt for the type");
    assert_eq!(
        sorted(options),
        sorted(vec![ManaType::Green, ManaType::Blue])
    );
    assert!(
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Black),
                count: 1,
            })
            .is_err(),
        "a type the sacrificed land could not produce must be refused"
    );
    choose(&mut runner, ManaType::Blue);
    assert_eq!(pool(&runner, P0), vec![ManaType::Blue]);
}

/// T4: "type" includes colorless (CR 106.1b), so Wastes gives {C}.
#[test]
fn sacrificed_wastes_adds_colorless() {
    let (mut scenario, squandered) = board();
    let wastes = scenario.add_land_from_oracle(P0, "Wastes", WASTES).id();
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, wastes);

    assert_in_graveyard(&runner, wastes);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);
}

/// T5: a land with no mana ability is still a legal sacrifice, but it could
/// produce no type, so no mana is added (Scryfall ruling + CR 106.5).
#[test]
fn sacrificed_land_without_mana_abilities_adds_nothing() {
    let (mut scenario, squandered) = board();
    let wilds = scenario
        .add_land_from_oracle(P0, "Evolving Wilds", EVOLVING_WILDS)
        .id();
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, wilds);

    assert_in_graveyard(&runner, wilds);
    assert_eq!(activation.prompt_options, None);
    assert!(
        pool(&runner, P0).is_empty(),
        "a land that could produce no mana yields no mana"
    );
}

/// T6: the could-produce set is read as the land last existed on the
/// battlefield (CR 608.2h). Urborg makes the Wastes a Swamp, which grants it
/// "{T}: Add {B}" (CR 305.6); that ability is gone once the Wastes is in the
/// graveyard, so only last known information can supply {B}.
#[test]
fn sacrificed_land_uses_its_layered_abilities_as_it_last_existed() {
    let (mut scenario, squandered) = board();
    scenario.add_land_from_oracle(P0, "Urborg, Tomb of Yawgmoth", URBORG);
    let wastes = scenario.add_land_from_oracle(P0, "Wastes", WASTES).id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());

    let wastes_produces_black = runner.state().objects[&wastes]
        .abilities
        .iter()
        .any(|ability| match &*ability.effect {
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } => colors.contains(&ManaColor::Black),
            _ => false,
        });
    assert!(
        wastes_produces_black,
        "reach-guard: Urborg must grant the Wastes the intrinsic Swamp mana ability"
    );

    let activation = activate_and_sacrifice(&mut runner, squandered, wastes);

    assert_in_graveyard(&runner, wastes);
    let options = activation
        .prompt_options
        .expect("a Swamp-Wastes could produce two types");
    assert_eq!(
        sorted(options),
        sorted(vec![ManaType::Colorless, ManaType::Black])
    );
    choose(&mut runner, ManaType::Black);
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
}

/// T7: CR 106.7 ignores whether the land's costs could be paid, so a tapped
/// Forest still could produce {G}.
#[test]
fn sacrificed_tapped_forest_still_adds_green() {
    let (mut scenario, squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    tap(&mut runner, forest);
    assert!(
        runner.state().objects[&forest].tapped,
        "reach-guard: the Forest is tapped before activation"
    );

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T8: a land you control but do not own goes to its owner's graveyard
/// (CR 701.21a), and its mana still goes to you.
#[test]
fn sacrificed_borrowed_land_goes_to_its_owner_and_mana_to_its_controller() {
    let (mut scenario, squandered) = board();
    let forest = scenario
        .add_land_from_oracle(P1, "Forest", "{T}: Add {G}.")
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&forest].owner, P1);
    assert_eq!(runner.state().objects[&forest].controller, P0);

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    assert!(
        runner.state().players[P1.0 as usize]
            .graveyard
            .contains(&forest),
        "reach-guard: the Forest goes to its owner's graveyard"
    );
    assert!(!runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&forest));
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
    assert!(pool(&runner, P1).is_empty());
}

/// T10: a sacrificed Reflecting Pool could produce whatever the lands you
/// control could produce at that moment (CR 106.7), here {G} from a Forest.
#[test]
fn sacrificed_reflecting_pool_adds_what_it_could_produce() {
    let (mut scenario, squandered) = board();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, pool_land);

    assert_in_graveyard(&runner, pool_land);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Battlefield);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

fn cast_manually(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announcing the spell must succeed");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach-guard: a manual cast opens the mana payment step, got {:?}",
        runner.state().waiting_for
    );
}

/// Whether the engine's legal-action list offers casting `spell`, which is
/// where the castability gate surfaces to players and the AI.
fn cast_is_offered(state: &GameState, spell: ObjectId) -> bool {
    legal_actions(state).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    )
}

fn finish_payment_casts(runner: &mut GameRunner, spell: ObjectId) -> bool {
    let result = runner
        .act(GameAction::PassPriority)
        .expect("finishing the mana payment must succeed");
    result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
}

fn green_creature_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, "Test Green Creature", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id()
}

fn generic_artifact_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_to_hand_from_oracle(P0, "Test Artifact", "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id()
}

/// T9: a {G} spell payable only through Squandered Resources is offered
/// (CR 117.1d + CR 601.2g), and sacrificing the only (tapped) Forest during
/// payment pays it (CR 605.3a).
#[test]
fn green_spell_payable_only_through_squandered_is_offered_and_paid() {
    let (mut scenario, squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, forest);

    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "the castability gate must credit Squandered Resources' {{G}}"
    );
    assert!(
        cast_is_offered(runner.state(), spell),
        "the spell must be among the legal actions"
    );

    cast_manually(&mut runner, spell);
    let activation = activate_and_sacrifice(&mut runner, squandered, forest);
    assert_in_graveyard(&runner, forest);
    assert_eq!(activation.prompt_options, None);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the mana ability returns to the payment step (CR 605.3a)"
    );
    assert!(
        finish_payment_casts(&mut runner, spell),
        "the {{G}} from the sacrificed Forest must pay for the spell"
    );
}

/// T11: a generic-only spell payable only through Squandered Resources is
/// offered (the capacity branch, not the coloured-shard branch) and resolves.
#[test]
fn generic_spell_payable_only_through_squandered_is_offered_and_resolves() {
    let (mut scenario, squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = generic_artifact_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, forest);

    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "the castability gate must count Squandered Resources' one mana"
    );

    cast_manually(&mut runner, spell);
    activate_and_sacrifice(&mut runner, squandered, forest);
    assert_in_graveyard(&runner, forest);
    assert!(finish_payment_casts(&mut runner, spell));
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
}

/// T12(a): with no land to sacrifice the ability cannot be activated, so a
/// spell that needs its mana is not offered. The otherwise identical board
/// with one tapped Forest is the positive pair.
#[test]
fn spell_is_not_offered_without_a_land_to_sacrifice() {
    let (mut scenario, _squandered) = board();
    let spell = green_creature_in_hand(&mut scenario);
    let runner = scenario.build();

    assert!(!can_cast_object_now(runner.state(), P0, spell));
    assert!(!cast_is_offered(runner.state(), spell));

    let (mut scenario, _squandered) = board();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, forest);
    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "positive pair: one tapped Forest makes the same spell castable"
    );
    assert!(cast_is_offered(runner.state(), spell));
}

/// T12(b): a land that could produce no mana makes the activation legal but
/// adds nothing (Scryfall ruling + CR 106.5), so neither a {G} nor a {1} spell
/// may be offered.
#[test]
fn spells_are_not_offered_when_the_only_land_could_produce_nothing() {
    let (mut scenario, squandered) = board();
    scenario.add_land_from_oracle(P0, "Evolving Wilds", EVOLVING_WILDS);
    let green_spell = green_creature_in_hand(&mut scenario);
    let generic_spell = generic_artifact_in_hand(&mut scenario);
    let runner = scenario.build();

    let definition = runner.state().objects[&squandered].abilities[0].clone();
    assert!(
        can_activate_mana_ability_now(runner.state(), P0, squandered, 0, &definition),
        "reach-guard: Squandered Resources itself is activatable (Evolving Wilds is fodder)"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, green_spell),
        "a {{G}} spell must not be offered when no sacrifice could produce mana"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, generic_spell),
        "a {{1}} spell must not be offered when no sacrifice could produce mana"
    );
    assert!(!cast_is_offered(runner.state(), green_spell));
    assert!(!cast_is_offered(runner.state(), generic_spell));
}

// ---------------------------------------------------------------------------
// CR 106.7 hostile fixtures: every ability's would-be resolution, read through
// the one could-produce authority (`game::could_produce`). Each row drives the
// real sacrifice, then asserts what arrived.
// ---------------------------------------------------------------------------

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const ASHAYA: &str = "Ashaya's power and toughness are each equal to the number of lands \
     you control.\nNontoken creatures you control are Forest lands in addition to their other types.";
const WILD_CANTOR: &str = "Sacrifice this creature: Add one mana of any color.";
const MILLIKIN: &str = "{T}, Mill a card: Add {C}.";
const CONTAMINATION: &str = "At the beginning of your upkeep, sacrifice this enchantment unless \
     you sacrifice a creature.\nIf a land is tapped for mana, it produces {B} instead of any other \
     type and amount.";
const GAEAS_CRADLE: &str = "{T}: Add {G} for each creature you control.";
const DRESS_DOWN: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose all \
     abilities.\nAt the beginning of the end step, sacrifice this enchantment.";
const EXOTIC_ORCHARD: &str =
    "{T}: Add one mana of any color that a land an opponent controls could produce.";
const CALCIFORM_POOLS: &str = "{T}: Add {C}.\n{1}, {T}: Put a storage counter on this land.\n{1}, \
     Remove X storage counters from this land: Add X mana in any combination of {W} and/or {U}.";
const CRUMBLING_VESTIGE: &str = "This land enters tapped.\nWhen this land enters, add one mana of \
     any color.\n{T}: Add {C}.";
const RIVER_OF_TEARS: &str = "{T}: Add {U}. If you played a land this turn, add {B} instead.";
const GEMSTONE_CAVERNS: &str = "If this card is in your opening hand and you're not the starting \
     player, you may begin the game with Gemstone Caverns on the battlefield with a luck counter \
     on it. If you do, exile a card from your hand.\n{T}: Add {C}. If Gemstone Caverns has a luck \
     counter on it, instead add one mana of any color.";
const URZAS_SAGA: &str = "(As this Saga enters and after your draw step, add a lore counter. \
     Sacrifice after III.)\nI — This Saga gains \"{T}: Add {C}.\"\nII — This Saga gains \"{2}, \
     {T}: Create a 0/0 colorless Construct artifact creature token with 'This token gets +1/+1 \
     for each artifact you control.'\"\nIII — Search your library for an artifact card with mana \
     cost {0} or {1}, put it onto the battlefield, then shuffle.";
const SOLDEVI_ADNATE: &str = "{T}, Sacrifice a black or artifact creature: Add an amount of {B} \
     equal to the sacrificed creature's mana value.";
const BOTTOMLESS_VAULT: &str = "This land enters tapped.\nYou may choose not to untap this land \
     during your untap step.\nAt the beginning of your upkeep, if this land is tapped, put a \
     storage counter on it.\n{T}, Remove any number of storage counters from this land: Add {B} \
     for each storage counter removed this way.";
const SOULBRIGHT_SEEKER: &str = "As an additional cost to cast this spell, behold an Elemental or \
     pay {2}.\n{R}: Target creature you control gains trample until end of turn. If this is the \
     third time this ability has resolved this turn, add {R}{R}{R}{R}.";

/// Applies the layer system, which `GameScenario::build` does not run.
fn flush(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
}

/// Reach-guard: Ashaya made `object` a Forest land (CR 305.6 grants it
/// "{T}: Add {G}"), so it is legal fodder for "Sacrifice a land".
fn assert_ashaya_made_it_a_forest_land(runner: &GameRunner, object: ObjectId) {
    let card_types = &runner.state().objects[&object].card_types;
    assert!(
        card_types.core_types.contains(&CoreType::Land)
            && card_types.core_types.contains(&CoreType::Creature),
        "reach-guard: Ashaya must make the creature a land; got {:?}",
        card_types.core_types
    );
    assert!(
        card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Forest"),
        "reach-guard: Ashaya must make the creature a Forest; got {:?}",
        card_types.subtypes
    );
}

fn ashaya_board_with(name: &str, oracle: &str) -> (GameRunner, ObjectId, ObjectId) {
    let (mut scenario, squandered) = board();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let fodder = scenario
        .add_creature_from_oracle(P0, name, 1, 1, oracle)
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_ashaya_made_it_a_forest_land(&runner, fodder);
    (runner, squandered, fodder)
}

/// T13 (review 3, finding 1): a non-tap production counts. Wild Cantor's
/// sacrifice ability could produce any color, and Ashaya's intrinsic Forest
/// ability adds Green, so all five colors are offered and Red arrives.
#[test]
fn sacrificed_wild_cantor_offers_every_color_its_sacrifice_ability_could_produce() {
    let (mut runner, squandered, cantor) = ashaya_board_with("Wild Cantor", WILD_CANTOR);

    let activation = activate_and_sacrifice(&mut runner, squandered, cantor);

    assert_in_graveyard(&runner, cantor);
    assert_eq!(
        activation.prompt_options,
        Some(vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]),
        "CR 106.7: the untapped sacrifice ability counts, not only the {{T}} ability"
    );
    choose(&mut runner, ManaType::Red);
    assert_eq!(pool(&runner, P0), vec![ManaType::Red]);
}

/// T14 (review 3, finding 6): a non-mana ability's production counts.
/// Millikin's "{T}, Mill a card: Add {C}" is not a mana ability (CR 605.1a — its
/// cost moves a card from a library), yet it would produce {C} if it resolved
/// (CR 106.7 ignores costs). Positive sibling: without Ashaya, Millikin is not a
/// land and is not offered, while a Forest is.
#[test]
fn sacrificed_millikin_offers_the_colorless_its_non_mana_ability_could_produce() {
    let (mut runner, squandered, millikin) = ashaya_board_with("Millikin", MILLIKIN);

    let activation = activate_and_sacrifice(&mut runner, squandered, millikin);

    assert_in_graveyard(&runner, millikin);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::Green, ManaType::Colorless])
    );
    choose(&mut runner, ManaType::Colorless);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);

    let (mut scenario, squandered) = board();
    let millikin = scenario
        .add_creature_from_oracle(P0, "Millikin", 0, 1, MILLIKIN)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: squandered,
            ability_index: 0,
        })
        .expect("Squandered Resources is activatable with a Forest to sacrifice");
    let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the sacrifice choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        choices.contains(&forest),
        "reach-guard: the Forest is offered"
    );
    assert!(
        !choices.contains(&millikin),
        "without Ashaya, Millikin is not a land and is not offered"
    );
}

/// T15 (review 3, findings 3 + 6, CR 106.12): only the {T} mana ability is
/// "tapped for mana". Under Contamination, the intrinsic Forest ability's Green
/// becomes Black, but Millikin's {C} — from a non-mana ability — does not.
#[test]
fn contamination_rewrites_only_the_tapped_for_mana_production() {
    let (mut scenario, squandered) = board();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let millikin = scenario
        .add_creature_from_oracle(P0, "Millikin", 0, 1, MILLIKIN)
        .id();
    let control_forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_ashaya_made_it_a_forest_land(&runner, millikin);

    runner
        .act(GameAction::ActivateAbility {
            source_id: control_forest,
            ability_index: 0,
        })
        .expect("control: tapping the Forest for mana");
    assert_eq!(
        pool(&runner, P0),
        vec![ManaType::Black],
        "control: Contamination rewrites a Forest tapped for mana"
    );
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();

    let activation = activate_and_sacrifice(&mut runner, squandered, millikin);

    assert_in_graveyard(&runner, millikin);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::Black, ManaType::Colorless])
    );
    choose(&mut runner, ManaType::Colorless);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);
}

/// T16 (review 3, finding 3): the captured could-produce set reads through
/// the applicable replacement, so a Forest under Contamination yields {B}.
#[test]
fn sacrificed_forest_under_contamination_adds_black() {
    let (mut scenario, squandered) = board();
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let control_forest = scenario.add_basic_land(P0, ManaColor::Green);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: control_forest,
            ability_index: 0,
        })
        .expect("control: tapping a Forest for mana");
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
}

fn creature_in_hand(scenario: &mut GameScenario, name: &str, shard: ManaCostShard) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, name, 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![shard],
            generic: 0,
        })
        .id()
}

/// T17 (review 3, finding 3, castability): the estimator reads the same
/// replacement-aware set, so with only a tapped Forest under Contamination a
/// {B} spell is offered and a {G} spell is not. T9 is the paired board without
/// Contamination, where the {G} spell is castable.
#[test]
fn castability_under_contamination_credits_black_not_green() {
    let (mut scenario, _squandered) = board();
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let black_spell = creature_in_hand(&mut scenario, "Test Black Creature", ManaCostShard::Black);
    let green_spell = creature_in_hand(&mut scenario, "Test Green Creature", ManaCostShard::Green);
    let mut runner = scenario.build();
    tap(&mut runner, forest);

    assert!(can_cast_object_now(runner.state(), P0, black_spell));
    assert!(cast_is_offered(runner.state(), black_spell));
    assert!(!can_cast_object_now(runner.state(), P0, green_spell));
    assert!(!cast_is_offered(runner.state(), green_spell));
}

fn cradle_board(creatures: usize) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut scenario, squandered) = board();
    let cradle = scenario
        .add_land_from_oracle(P0, "Gaea's Cradle", GAEAS_CRADLE)
        .id();
    for _ in 0..creatures {
        scenario.add_vanilla(P0, 1, 1);
    }
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, cradle);
    (runner, squandered, cradle, spell)
}

/// T18 (review 3, finding 4): an instruction that would add no mana defines no
/// type (CR 106.5). Gaea's Cradle with no creatures could produce nothing, so a
/// {G} spell is not offered and sacrificing it adds nothing; with one creature
/// it could produce {G}.
#[test]
fn gaeas_cradle_with_no_creatures_could_produce_nothing() {
    let (mut runner, squandered, cradle, spell) = cradle_board(0);
    assert!(!can_cast_object_now(runner.state(), P0, spell));
    assert!(!cast_is_offered(runner.state(), spell));
    let activation = activate_and_sacrifice(&mut runner, squandered, cradle);
    assert_in_graveyard(&runner, cradle);
    assert_eq!(activation.prompt_options, None);
    assert!(pool(&runner, P0).is_empty());

    let (mut runner, squandered, cradle, spell) = cradle_board(1);
    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "positive pair: with one creature the Cradle could produce {{G}}"
    );
    assert!(cast_is_offered(runner.state(), spell));
    let activation = activate_and_sacrifice(&mut runner, squandered, cradle);
    assert_in_graveyard(&runner, cradle);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T19: a count fixed by the ability's own activation (X, CR 107.3a) is
/// admitted — CR 106.7 ignores whether the X could be paid — so Calciform
/// Pools could produce {W}, {U} and {C}.
#[test]
fn sacrificed_calciform_pools_offers_its_x_production_types() {
    let (mut scenario, squandered) = board();
    let calciform = scenario
        .add_land_from_oracle(P0, "Calciform Pools", CALCIFORM_POOLS)
        .id();
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, calciform);

    assert_in_graveyard(&runner, calciform);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::White, ManaType::Blue, ManaType::Colorless])
    );
    choose(&mut runner, ManaType::White);
    assert_eq!(pool(&runner, P0), vec![ManaType::White]);
}

fn dryad_arbor_board(dress_down: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut scenario, squandered) = board();
    let arbor = scenario
        .add_land_from_oracle(P0, "Dryad Arbor", "")
        .with_subtypes(vec!["Forest", "Dryad"])
        .id();
    if dress_down {
        scenario.add_enchantment_from_oracle(P0, "Dress Down", DRESS_DOWN);
    }
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&arbor).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
        object.tapped = true;
    }
    flush(&mut runner);
    let card_types = &runner.state().objects[&arbor].card_types;
    assert!(
        card_types.core_types.contains(&CoreType::Land)
            && card_types.core_types.contains(&CoreType::Creature)
            && card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Forest"),
        "reach-guard: Dryad Arbor keeps its types under Dress Down; got {card_types:?}"
    );
    (runner, squandered, arbor, spell)
}

/// T20 (review 3, finding 5): a layer-6 ability removal (CR 613.1f) takes away
/// the intrinsic Forest ability layer 4 granted (CR 305.6 + CR 613.1d); the
/// retained Forest subtype does not resurrect it.
#[test]
fn blanked_dryad_arbor_could_produce_nothing() {
    let (mut runner, squandered, arbor, spell) = dryad_arbor_board(true);
    assert!(
        runner.state().objects[&arbor].abilities.is_empty(),
        "reach-guard: Dress Down removed the intrinsic mana ability"
    );
    assert!(!cast_is_offered(runner.state(), spell));
    let activation = activate_and_sacrifice(&mut runner, squandered, arbor);
    assert_in_graveyard(&runner, arbor);
    assert_eq!(activation.prompt_options, None);
    assert!(pool(&runner, P0).is_empty());

    let (mut runner, squandered, arbor, spell) = dryad_arbor_board(false);
    assert!(
        !runner.state().objects[&arbor].abilities.is_empty(),
        "positive pair: without Dress Down the intrinsic ability is present"
    );
    assert!(can_cast_object_now(runner.state(), P0, spell));
    let activation = activate_and_sacrifice(&mut runner, squandered, arbor);
    assert_in_graveyard(&runner, arbor);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T21 (review 3, finding 2): an anchored chain crosses controllers. Your
/// Reflecting Pool reads your Exotic Orchard, which reads the opponent's
/// Forest, so the Pool could produce {G}.
#[test]
fn sacrificed_reflecting_pool_follows_an_anchored_chain() {
    let (mut scenario, squandered) = board();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_basic_land(P1, ManaColor::Green);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: orchard,
            ability_index: 0,
        })
        .expect("control: tapping the Orchard");
    assert_eq!(
        pool(&runner, P0),
        vec![ManaType::Green],
        "control: the Orchard reads the opponent's Forest"
    );
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();
    runner.state_mut().objects.get_mut(&orchard).unwrap().tapped = false;

    let activation = activate_and_sacrifice(&mut runner, squandered, pool_land);

    assert_in_graveyard(&runner, pool_land);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T21c (review 4, CR 702.26b + CR 702.26c + CR 702.26d): a phased-out land is
/// treated as though it doesn't exist, though it stays on the battlefield under
/// its controller. With the opponent's Forest — the only direct anchor —
/// phased out, the Orchard surveys no land and your Reflecting Pool, which
/// reads the Orchard, could produce nothing; sacrificing the Pool adds nothing.
/// Phased in, the same chain gives {G}.
#[test]
fn phased_out_forest_anchors_no_chain_through_the_orchard() {
    for phased_out in [true, false] {
        let (mut scenario, squandered) = board();
        let pool_land = scenario
            .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
            .id();
        let orchard = scenario
            .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
            .id();
        let forest = scenario.add_basic_land(P1, ManaColor::Green);
        let mut runner = scenario.build();
        if phased_out {
            let mut events = Vec::new();
            phase_out_object(
                runner.state_mut(),
                forest,
                PhaseOutCause::Directly,
                &mut events,
            );
        }

        let state = runner.state();
        let forest_object = &state.objects[&forest];
        assert_eq!(forest_object.is_phased_out(), phased_out, "reach-guard");
        assert_eq!(
            (forest_object.zone, forest_object.controller),
            (Zone::Battlefield, P1),
            "reach-guard (CR 702.26d): phasing moves the Forest nowhere"
        );
        let reads_a_census = |object: ObjectId| {
            state.objects[&object].abilities.iter().any(|ability| {
                matches!(
                    &*ability.effect,
                    Effect::Mana {
                        produced: ManaProduction::AnyTypeProduceableBy { .. }
                            | ManaProduction::OpponentLandColors { .. },
                        ..
                    }
                )
            })
        };
        assert!(
            reads_a_census(pool_land) && reads_a_census(orchard),
            "reach-guard: both recursive producers carry their could-produce clause"
        );
        let live_lands: Vec<ObjectId> = state
            .battlefield_phased_in_ids()
            .into_iter()
            .filter(|object| {
                state.objects[object]
                    .card_types
                    .core_types
                    .contains(&CoreType::Land)
            })
            .collect();
        let mut expected_lands = vec![pool_land, orchard];
        if !phased_out {
            expected_lands.push(forest);
        }
        assert_eq!(
            live_lands, expected_lands,
            "reach-guard: no other land could anchor the chain"
        );

        let activation = activate_and_sacrifice(&mut runner, squandered, pool_land);

        assert_in_graveyard(&runner, pool_land);
        assert_eq!(activation.prompt_options, None);
        let expected_mana = if phased_out {
            Vec::new()
        } else {
            vec![ManaType::Green]
        };
        assert_eq!(pool(&runner, P0), expected_mana, "phased out: {phased_out}");
    }
}

/// T21b (castability, labelled NON-discriminating): on the same board with the
/// Pool and the Orchard tapped, a {G} spell is offered. Not revert-sensitive by
/// construction — the Orchard is itself a legal sacrifice that reaches the
/// Forest — so T21 and the class row C1 are the discriminating proofs.
#[test]
fn anchored_chain_board_offers_a_green_spell() {
    let (mut scenario, _squandered) = board();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_basic_land(P1, ManaColor::Green);
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, pool_land);
    tap(&mut runner, orchard);

    assert!(can_cast_object_now(runner.state(), P0, spell));
    assert!(cast_is_offered(runner.state(), spell));
}

/// T22 (review 3, finding 2): an unanchored cycle stays empty — "won't help
/// each other unless some other land allows one of them to actually produce
/// some type of mana" (Exotic Orchard ruling). T21 is the positive pair.
#[test]
fn sacrificed_reflecting_pool_in_an_unanchored_cycle_adds_nothing() {
    let (mut scenario, squandered) = board();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Exotic Orchard", EXOTIC_ORCHARD);
    let spell = green_creature_in_hand(&mut scenario);
    let mut runner = scenario.build();
    tap(&mut runner, orchard);
    tap(&mut runner, pool_land);
    assert!(!cast_is_offered(runner.state(), spell));
    runner
        .state_mut()
        .objects
        .get_mut(&pool_land)
        .unwrap()
        .tapped = false;

    let activation = activate_and_sacrifice(&mut runner, squandered, pool_land);

    assert_in_graveyard(&runner, pool_land);
    assert_eq!(activation.prompt_options, None);
    assert!(pool(&runner, P0).is_empty());
}

/// T23: an Orchard clause asks for *colors* (CR 105.1): "can't be tapped for
/// colorless mana, even if a land an opponent controls could produce colorless
/// mana" (Exotic Orchard ruling). Facing only Wastes it could produce nothing;
/// facing Wastes and an Island it could produce only {U}.
#[test]
fn sacrificed_exotic_orchard_never_offers_colorless() {
    let (mut scenario, squandered) = board();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Wastes", WASTES);
    let mut runner = scenario.build();
    let activation = activate_and_sacrifice(&mut runner, squandered, orchard);
    assert_in_graveyard(&runner, orchard);
    assert_eq!(activation.prompt_options, None);
    assert!(pool(&runner, P0).is_empty());

    let (mut scenario, squandered) = board();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Wastes", WASTES);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();
    let activation = activate_and_sacrifice(&mut runner, squandered, orchard);
    assert_in_graveyard(&runner, orchard);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Blue]);
}

/// T24 (CR 113.3 + CR 113.3c): "an ability" includes a triggered ability, so
/// Crumbling Vestige's "When this land enters, add one mana of any color" counts
/// beside its {T}: Add {C}.
#[test]
fn sacrificed_crumbling_vestige_offers_its_triggered_production() {
    let (mut scenario, squandered) = board();
    let vestige = scenario
        .add_land_from_oracle(P0, "Crumbling Vestige", CRUMBLING_VESTIGE)
        .id();
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, vestige);

    assert_in_graveyard(&runner, vestige);
    assert_eq!(
        activation.prompt_options,
        Some(vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
            ManaType::Colorless,
        ])
    );
    choose(&mut runner, ManaType::Red);
    assert_eq!(pool(&runner, P0), vec![ManaType::Red]);
}

fn play_a_land(runner: &mut GameRunner, land: ObjectId) {
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the land from hand");
    assert_eq!(
        runner.state().players[P0.0 as usize].lands_played_this_turn,
        1,
        "reach-guard: a land was played this turn"
    );
}

/// Taps `land` for its first ability and returns what arrived (no prompt).
fn tap_for_mana(runner: &mut GameRunner, land: ObjectId) -> Vec<ManaType> {
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("tapping the land for mana");
    let produced = pool(runner, P0);
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();
    produced
}

/// T25 (B1, CR 608.2c + CR 614.1a): "instead" replaces the earlier
/// instruction. River of Tears could produce exactly {U} before a land is
/// played and exactly {B} after — never both. A second River of Tears tapped
/// in the same state is the differential control.
#[test]
fn sacrificed_river_of_tears_reads_its_instead_at_that_time() {
    let (mut scenario, squandered) = board();
    let river = scenario
        .add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS)
        .id();
    let control_river = scenario
        .add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS)
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().players[P0.0 as usize].lands_played_this_turn,
        0
    );
    assert_eq!(
        tap_for_mana(&mut runner, control_river),
        vec![ManaType::Blue]
    );
    let activation = activate_and_sacrifice(&mut runner, squandered, river);
    assert_in_graveyard(&runner, river);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Blue]);

    let (mut scenario, squandered) = board();
    let river = scenario
        .add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS)
        .id();
    let control_river = scenario
        .add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS)
        .id();
    let land_in_hand = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();
    play_a_land(&mut runner, land_in_hand);
    assert_eq!(
        tap_for_mana(&mut runner, control_river),
        vec![ManaType::Black]
    );
    let activation = activate_and_sacrifice(&mut runner, squandered, river);
    assert_in_graveyard(&runner, river);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
}

/// T26 (B1): an "instead" that changes the type. Gemstone Caverns without a
/// luck counter could produce exactly {C}; with one, exactly any color.
#[test]
fn sacrificed_gemstone_caverns_reads_its_luck_counter() {
    let (mut scenario, squandered) = board();
    let caverns = scenario
        .add_land_from_oracle(P0, "Gemstone Caverns", GEMSTONE_CAVERNS)
        .id();
    let mut runner = scenario.build();
    let activation = activate_and_sacrifice(&mut runner, squandered, caverns);
    assert_in_graveyard(&runner, caverns);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);

    let (mut scenario, squandered) = board();
    let caverns = scenario
        .add_land_from_oracle(P0, "Gemstone Caverns", GEMSTONE_CAVERNS)
        .id();
    let luck = parse_counter_type("luck");
    scenario.with_counter(caverns, luck.clone(), 1);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&caverns]
            .counters
            .get(&luck)
            .copied(),
        Some(1),
        "reach-guard: the luck counter is on Gemstone Caverns"
    );
    let activation = activate_and_sacrifice(&mut runner, squandered, caverns);
    assert_in_graveyard(&runner, caverns);
    assert_eq!(
        activation.prompt_options,
        Some(vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ])
    );
    choose(&mut runner, ManaType::Green);
    assert_eq!(pool(&runner, P0), vec![ManaType::Green]);
}

/// T28 (CR 608.2k): a count fixed by the ability's own cost referent is
/// admitted — Soldevi Adnate's "{B} equal to the sacrificed creature's mana
/// value" could produce {B} whatever it would sacrifice (CR 106.7 ignores
/// costs).
#[test]
fn sacrificed_soldevi_adnate_offers_its_cost_referent_black() {
    let (mut runner, squandered, adnate) = ashaya_board_with("Soldevi Adnate", SOLDEVI_ADNATE);

    let activation = activate_and_sacrifice(&mut runner, squandered, adnate);

    assert_in_graveyard(&runner, adnate);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::Black, ManaType::Green])
    );
    choose(&mut runner, ManaType::Black);
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
}

/// T29: a storage land's count is the counters its own cost removes
/// (`PreviousEffectAmount`), fixed by the activation — so a Bottomless Vault
/// with no storage counters still could produce {B}. Paired with T18: Gaea's
/// Cradle's count is state at that time, and zero there means nothing.
#[test]
fn sacrificed_bottomless_vault_with_no_counters_adds_black() {
    let (mut scenario, squandered) = board();
    let vault = scenario
        .add_land_from_oracle(P0, "Bottomless Vault", BOTTOMLESS_VAULT)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&vault].counters.is_empty(),
        "reach-guard: the Vault has no storage counters"
    );

    let activation = activate_and_sacrifice(&mut runner, squandered, vault);

    assert_in_graveyard(&runner, vault);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Black]);
}

/// T30 (CR 608.2c): a gate on the ability's own resolution count is read both
/// ways — the resolution that asks it bumps the ledger first, so the reading
/// cannot settle it beforehand. Soulbright Seeker could produce {R}.
#[test]
fn sacrificed_soulbright_seeker_offers_its_ordinal_red() {
    let (mut runner, squandered, seeker) =
        ashaya_board_with("Soulbright Seeker", SOULBRIGHT_SEEKER);
    assert!(
        !runner
            .state()
            .ability_resolutions_this_turn
            .keys()
            .any(|(source, _)| *source == seeker),
        "reach-guard: the Seeker's ability has not resolved this turn"
    );

    let activation = activate_and_sacrifice(&mut runner, squandered, seeker);

    assert_in_graveyard(&runner, seeker);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::Red, ManaType::Green])
    );
    choose(&mut runner, ManaType::Red);
    assert_eq!(pool(&runner, P0), vec![ManaType::Red]);
}

/// T30's behavioral control: the Seeker's ordinal gate really adds
/// {R}{R}{R}{R} on the third resolution and nothing on the first two, so T30
/// reads a real ordinal producer, not an AST shape.
#[test]
fn soulbright_seeker_adds_red_only_on_its_third_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let seeker = scenario
        .add_creature_from_oracle(P0, "Soulbright Seeker", 2, 1, SOULBRIGHT_SEEKER)
        .id();
    let mountains: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let mut runner = scenario.build();

    let red_in_pool = |runner: &GameRunner| {
        pool(runner, P0)
            .iter()
            .filter(|mana_type| **mana_type == ManaType::Red)
            .count()
    };
    for (resolution, mountain) in mountains.iter().enumerate() {
        runner
            .activate(seeker, 0)
            .pay_with(&[*mountain])
            .target_object(seeker)
            .resolve();
        let expected = if resolution == 2 { 4 } else { 0 };
        assert_eq!(
            red_in_pool(&runner),
            expected,
            "resolution {} of the Seeker's ability",
            resolution + 1
        );
    }
}

/// Whether `object`'s layered abilities include a mana ability.
fn has_mana_ability(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().objects[&object]
        .abilities
        .iter()
        .any(|ability| matches!(&*ability.effect, Effect::Mana { .. }))
}

fn urzas_saga_board() -> (GameRunner, ObjectId, ObjectId) {
    let (mut scenario, squandered) = board();
    // The Saga subtype must be in place before the Oracle text is parsed, so
    // the chapter lines lower to chapter triggers.
    let saga = scenario
        .add_enchantment_from_oracle(P0, "Urza's Saga", "")
        .as_land()
        .with_subtypes(vec!["Urza's", "Saga"])
        .from_oracle_text(URZAS_SAGA)
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    (runner, squandered, saga)
}

/// T27 (CR 106.7 + CR 603.3): only an ability's OWN resolution counts. Urza's
/// Saga's chapter I *grants* "{T}: Add {C}" — a payload registered for later —
/// so before chapter I the Saga could produce nothing; once the chapter's effect
/// has resolved, the granted ability is in its layered abilities and it could
/// produce {C}.
#[test]
fn urzas_saga_could_produce_colorless_only_once_chapter_one_granted_it() {
    let (mut runner, squandered, saga) = urzas_saga_board();
    assert!(
        !has_mana_ability(&runner, saga),
        "reach-guard: before chapter I the Saga has no mana ability"
    );
    let activation = activate_and_sacrifice(&mut runner, squandered, saga);
    assert_in_graveyard(&runner, saga);
    assert_eq!(activation.prompt_options, None);
    assert!(pool(&runner, P0).is_empty());

    let (mut runner, squandered, saga) = urzas_saga_board();
    let chapter_one = runner.state().objects[&saga]
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| entry.definition())
        .find_map(|trigger| {
            trigger
                .execute
                .as_deref()
                .filter(|execute| matches!(&*execute.effect, Effect::GenericEffect { .. }))
                .cloned()
        })
        .expect("reach-guard: chapter I lowers to a granting effect");
    let resolved = build_resolved_from_def(&chapter_one, saga, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter I's effect resolves");
    flush(&mut runner);
    assert!(
        has_mana_ability(&runner, saga),
        "reach-guard: chapter I granted the Saga its mana ability"
    );
    let activation = activate_and_sacrifice(&mut runner, squandered, saga);
    assert_in_graveyard(&runner, saga);
    assert_eq!(activation.prompt_options, None);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);
}

// Verbatim Oracle text (Scryfall). Its ruling: "The effects of multiple Mana
// Reflections are cumulative."
const MANA_REFLECTION: &str =
    "If you tap a permanent for mana, it produces twice as much of that mana instead.";
const RITUAL_OF_SUBDUAL: &str = "Cumulative upkeep {2}\nIf a land is tapped for mana, it produces \
     colorless mana instead of any other type.";

/// A board with ten Mana Reflections (Copy Enchantment / Replication Technique
/// are authentic ways to have them) and a Forest.
fn ten_reflections_board() -> (GameScenario, ObjectId, ObjectId) {
    let (mut scenario, squandered) = board();
    for _ in 0..10 {
        scenario.add_enchantment_from_oracle(P0, "Mana Reflection", MANA_REFLECTION);
    }
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    (scenario, squandered, forest)
}

/// T31 (review 4, CR 106.7 + CR 614.5 + CR 616.1f): ten cumulative Mana
/// Reflections give 10! replacement orders on every hypothetical read. Your
/// Reflecting Pool reads the Forest through all of them (still Green), and
/// tapping the Pool for mana really produces 2^10 Green.
#[test]
fn reflecting_pool_under_ten_mana_reflections_adds_exactly_1024_green() {
    let (mut scenario, _squandered, _forest) = ten_reflections_board();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let mut runner = scenario.build();

    let produced = tap_for_mana(&mut runner, pool_land);

    assert!(
        runner.state().objects[&pool_land].tapped,
        "reach-guard: the Pool was tapped for mana"
    );
    assert_eq!(produced, vec![ManaType::Green; 1024]);
}

/// T32 (review 4, CR 106.7 + CR 616.1e): the same ten Reflections joined by
/// Contamination and Ritual of Subdual, whose order decides the type: the
/// sacrificed Forest could produce exactly {B} or {C}. Squandered's own
/// production is not "tapped for mana", so the chosen type arrives once.
#[test]
fn sacrificed_forest_under_ten_reflections_and_two_type_rewrites_offers_black_or_colorless() {
    let (mut scenario, squandered, forest) = ten_reflections_board();
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    scenario.add_enchantment_from_oracle(P0, "Ritual of Subdual", RITUAL_OF_SUBDUAL);
    let mut runner = scenario.build();

    let activation = activate_and_sacrifice(&mut runner, squandered, forest);

    assert_in_graveyard(&runner, forest);
    assert_eq!(
        activation.prompt_options,
        Some(vec![ManaType::Black, ManaType::Colorless])
    );
    choose(&mut runner, ManaType::Colorless);
    assert_eq!(pool(&runner, P0), vec![ManaType::Colorless]);
}
