//! Auto-pay prefers a source's drawback-free mana ability when it suffices.
//!
//! A permanent with a plain `{T}: Add …` mana ability and a multi-mana
//! ability that costs more (a mana sub-cost, exert, sacrifice) must not have
//! the costlier ability chosen for it when a plan without it pays the whole
//! cost. When only the costlier ability pays the cost, auto-pay still uses it.
//!
//! CR 601.2g + CR 605.3b: mana abilities are activated during payment and
//! resolve immediately. CR 701.43a: exerting is the player's choice ("you
//! choose to have it not untap during your next untap step"); the automatic
//! payer must not make that choice, or a sacrifice, when it isn't needed.

use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, ContinuousModification, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const ARENA_OF_GLORY_ORACLE: &str = "This land enters tapped unless you control a Mountain.\n\
{T}: Add {R}.\n\
{R}, {T}, Exert this land: Add {R}{R}. If any of that mana is spent on a creature spell, \
it gains haste until end of turn. (An exerted permanent won't untap during your next untap step.)";
const DWARVEN_RUINS_ORACLE: &str =
    "This land enters tapped.\n{T}: Add {R}.\n{T}, Sacrifice this land: Add {R}{R}.";
const OASIS_RITUALIST_ORACLE: &str = "{T}: Add one mana of any color.\n\
{T}, Exert this creature: Add two mana of any one color. \
(An exerted creature won't untap during your next untap step.)";
const TWILIGHT_MIRE_ORACLE: &str = "{T}: Add {C}.\n{B/G}, {T}: Add {B}{B}, {B}{G}, or {G}{G}.";
const AZORIUS_SIGNET_ORACLE: &str = "{1}, {T}: Add {W}{U}.";
const GRAVEN_CAIRNS_ORACLE: &str = "{T}: Add {C}.\n{B/R}, {T}: Add {B}{B}, {B}{R}, or {R}{R}.";
const REST_IN_PEACE_ORACLE: &str = "When this enchantment enters, exile all graveyards.\n\
If a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID_ORACLE: &str = "If this card is in your opening hand, you may begin the \
game with it on the battlefield.\n\
If a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const CASCADING_CATARACTS_ORACLE: &str =
    "Indestructible\n{T}: Add {C}.\n{5}, {T}: Add five mana in any combination of colors.";
const ANCIENT_TOMB_ORACLE: &str = "{T}: Add {C}{C}. This land deals 2 damage to you.";
const URBORG_ORACLE: &str = "Each land is a Swamp in addition to its other land types.";
const SOLDEVI_ADNATE_ORACLE: &str = "{T}, Sacrifice a black or artifact creature: Add an amount \
of {B} equal to the sacrificed creature's mana value.";
const VAMPIRE_NIGHTHAWK_ORACLE: &str = "Flying\nDeathtouch\nLifelink";
const CETA_DISCIPLE_ORACLE: &str = "{R}, {T}: Target creature gets +2/+0 until end of turn.\n\
{G}, {T}: Add one mana of any color.";
const HAVENWOOD_BATTLEGROUND_ORACLE: &str =
    "This land enters tapped.\n{T}: Add {G}.\n{T}, Sacrifice this land: Add {G}{G}.";
const BLACK_KNIGHT_ORACLE: &str = "First strike\nProtection from white";
const SUNGRASS_PRAIRIE_ORACLE: &str = "{1}, {T}: Add {G}{W}.";
const TATYOVA_ORACLE: &str = "Land creatures you control have flying.\n\
Whenever a land you control enters, if you control seven or more lands, up to one target land \
you control becomes a 3/3 Elemental creature with haste. It's still a land.";
const AZORIUS_FIRST_WING_ORACLE: &str = "Flying, protection from enchantments";
const FETID_HEATH_ORACLE: &str = "{T}: Add {C}.\n{W/B}, {T}: Add {W}{W}, {W}{B}, or {B}{B}.";

/// Transient `CantUntap` effects aimed at `id` (the shape an exert cost installs).
fn exert_markers(state: &GameState, id: ObjectId) -> usize {
    state
        .transient_continuous_effects
        .iter()
        .filter(|e| e.affected == TargetFilter::SpecificObject { id })
        .filter(|e| {
            e.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap
                    }
                )
            })
        })
        .count()
}

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// A "You gain 1 life." sorcery in P0's hand costing `mana_cost`.
fn witness_spell(scenario: &mut GameScenario, mana_cost: ManaCost) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Auto-pay Witness", false, "You gain 1 life.")
        .with_mana_cost(mana_cost)
        .id()
}

/// Build the runner and untap `lands` (some printed lands enter tapped).
fn build_untapped(scenario: GameScenario, lands: &[ObjectId]) -> GameRunner {
    let mut runner = scenario.build();
    for land in lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = false;
    }
    runner
}

fn mana_added(events: &[GameEvent]) -> Vec<(ObjectId, ManaType)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAdded {
                source_id,
                mana_type,
                ..
            } => Some((*source_id, *mana_type)),
            _ => None,
        })
        .collect()
}

/// Cast the witness through the default auto-pay path and assert it resolved.
fn auto_pay_and_resolve(runner: &mut GameRunner, spell: ObjectId) -> CastOutcome {
    let outcome = runner.cast(spell).resolve();
    assert!(
        outcome.zone_of(spell) == Zone::Graveyard && outcome.life_delta(P0) == 1,
        "reach: the witness was auto-paid and resolved; zone {:?}, life delta {}, mana added {:?}",
        outcome.zone_of(spell),
        outcome.life_delta(P0),
        mana_added(outcome.events()),
    );
    outcome
}

// --- Arena of Glory: exert + {R} sub-cost, never needed while the plain ability pays ---

fn assert_arena_not_exerted(shards: &[ManaCostShard], generic: u32, extra_mountains: usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mut lands = vec![arena];
    for _ in 0..=extra_mountains {
        lands.push(scenario.add_basic_land(P0, ManaColor::Red));
    }
    let spell = witness_spell(&mut scenario, cost(shards, generic));
    let mut runner = build_untapped(scenario, &lands);
    assert_eq!(
        exert_markers(runner.state(), arena),
        0,
        "reach: Arena starts unexerted"
    );

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    let tapped = lands
        .iter()
        .filter(|land| outcome.state().objects[land].tapped)
        .count();
    assert_eq!(
        exert_markers(outcome.state(), arena),
        0,
        "auto-pay exerted Arena of Glory although its `{{T}}: Add {{R}}` pays the cost; \
         mana added {:?} [arena = {arena:?}]",
        mana_added(outcome.events()),
    );
    assert_eq!(
        tapped,
        shards.len() + generic as usize,
        "one land per mana; mana added {:?}",
        mana_added(outcome.events()),
    );
}

/// The triage repro: a one-red spell with Arena + Mountain taps one land and
/// never exerts Arena (it used to tap both and float a red).
#[test]
fn arena_of_glory_auto_pay_for_single_red_does_not_exert() {
    assert_arena_not_exerted(&[ManaCostShard::Red], 0, 0);
}

/// The Goblin Storm shape: {1}{R} with Arena + Mountain.
#[test]
fn arena_of_glory_auto_pay_for_one_and_red_does_not_exert() {
    assert_arena_not_exerted(&[ManaCostShard::Red], 1, 0);
}

/// {R}{R} with Arena + two Mountains: the exert row covers both shards, but
/// Arena's plain ability plus a Mountain pays the same cost without exerting.
#[test]
fn arena_of_glory_auto_pay_for_two_red_does_not_exert() {
    assert_arena_not_exerted(&[ManaCostShard::Red, ManaCostShard::Red], 0, 1);
}

/// X costs: X is announced before payment (CR 601.2b), so {X}{R} with X = 1
/// pays exactly like {1}{R}.
#[test]
fn arena_of_glory_auto_pay_for_x_spell_does_not_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "X Witness", false, "You gain X life.")
        .with_mana_cost(cost(&[ManaCostShard::X, ManaCostShard::Red], 0))
        .id();
    let mut runner = build_untapped(scenario, &[arena, mountain]);

    let outcome = runner.cast(spell).x(1).resolve();
    assert!(
        outcome.zone_of(spell) == Zone::Graveyard && outcome.life_delta(P0) == 1,
        "reach: the X = 1 witness resolved; zone {:?}, life delta {}, mana added {:?}",
        outcome.zone_of(spell),
        outcome.life_delta(P0),
        mana_added(outcome.events()),
    );
    assert!(outcome.state().objects[&arena].tapped && outcome.state().objects[&mountain].tapped);
    assert_eq!(
        exert_markers(outcome.state(), arena),
        0,
        "mana added {:?}",
        mana_added(outcome.events())
    );
}

// --- Dwarven Ruins: sacrifice row ---

/// {R} from Dwarven Ruins alone: its plain ability pays; it must not be sacrificed.
#[test]
fn dwarven_ruins_auto_pay_for_single_red_does_not_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let spell = witness_spell(&mut scenario, cost(&[ManaCostShard::Red], 0));
    let mut runner = build_untapped(scenario, &[ruins]);

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(
        outcome.zone_of(ruins),
        Zone::Battlefield,
        "Dwarven Ruins was sacrificed for a cost its `{{T}}: Add {{R}}` pays; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert!(outcome.state().objects[&ruins].tapped);
}

/// {R}{R} from Dwarven Ruins alone: only the sacrifice row pays it, so
/// auto-pay still uses it.
#[test]
fn dwarven_ruins_auto_pay_sacrifices_when_only_the_sacrifice_row_pays() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let spell = witness_spell(
        &mut scenario,
        cost(&[ManaCostShard::Red, ManaCostShard::Red], 0),
    );
    let mut runner = build_untapped(scenario, &[ruins]);

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(outcome.zone_of(ruins), Zone::Graveyard);
}

// --- Oasis Ritualist: exert row on a creature ---

/// {G} from Oasis Ritualist alone: its plain any-color ability pays it, so
/// the exert ability is not used.
#[test]
fn oasis_ritualist_auto_pay_for_single_green_does_not_exert() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ritualist = scenario
        .add_creature_from_oracle(P0, "Oasis Ritualist", 2, 4, OASIS_RITUALIST_ORACLE)
        .id();
    let spell = witness_spell(&mut scenario, cost(&[ManaCostShard::Green], 0));
    let mut runner = build_untapped(scenario, &[ritualist]);

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert!(outcome.state().objects[&ritualist].tapped);
    assert_eq!(
        exert_markers(outcome.state(), ritualist),
        0,
        "mana added {:?}",
        mana_added(outcome.events())
    );
}

// --- Twilight Mire: mana sub-cost (filter land) ---

fn mire_scenario(spell_cost: ManaCost) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mire = scenario
        .add_land_from_oracle(P0, "Twilight Mire", TWILIGHT_MIRE_ORACLE)
        .id();
    let spell = witness_spell(&mut scenario, spell_cost);
    (build_untapped(scenario, &[swamp, mire]), swamp, mire, spell)
}

/// {1}{B} with Swamp + Twilight Mire: Swamp's {B} and Mire's `{T}: Add {C}`
/// pay it exactly, so the `{B/G}`-costed filter ability is not used.
#[test]
fn twilight_mire_auto_pay_uses_its_plain_ability_when_it_suffices() {
    let (mut runner, swamp, mire, spell) = mire_scenario(cost(&[ManaCostShard::Black], 1));
    let outcome = auto_pay_and_resolve(&mut runner, spell);
    let mut added = mana_added(outcome.events());
    added.sort();
    let mut expected = vec![(swamp, ManaType::Black), (mire, ManaType::Colorless)];
    expected.sort();
    assert_eq!(added, expected);
}

/// {G} with Swamp + Twilight Mire: only the filter ability makes green, so it
/// is still used, with the Swamp paying its `{B/G}`.
#[test]
fn twilight_mire_auto_pay_uses_its_filter_ability_when_only_it_pays() {
    let (mut runner, swamp, mire, spell) = mire_scenario(cost(&[ManaCostShard::Green], 0));
    let outcome = auto_pay_and_resolve(&mut runner, spell);
    let added = mana_added(outcome.events());
    assert_eq!(added.first(), Some(&(swamp, ManaType::Black)), "{added:?}");
    assert!(added.contains(&(mire, ManaType::Green)), "{added:?}");
    assert!(outcome.state().objects[&swamp].tapped && outcome.state().objects[&mire].tapped);
}

// --- A drawback-free plan that would starve another source's mana sub-cost ---

/// {W}{U}{R}{R} with Azorius Signet + Dwarven Ruins + Mountain. Without the
/// sacrifice, the Signet's {W}{U} plus the Ruins' and Mountain's {R} cover
/// the cost's colors but leave nothing to pay the Signet's `{1}`. The only
/// complete payment sacrifices the Ruins for {R}{R} and taps the Mountain
/// for the Signet's `{1}`, so auto-pay must still find it.
#[test]
fn drawback_free_plan_is_rejected_when_it_cannot_fund_a_mana_sub_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let signet = scenario
        .add_artifact_from_oracle(P0, "Azorius Signet", AZORIUS_SIGNET_ORACLE)
        .id();
    let ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let spell = witness_spell(
        &mut scenario,
        cost(
            &[
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Red,
                ManaCostShard::Red,
            ],
            0,
        ),
    );
    let mut runner = build_untapped(scenario, &[signet, ruins, mountain]);

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(outcome.zone_of(ruins), Zone::Graveyard);
    assert!(outcome.state().objects[&signet].tapped && outcome.state().objects[&mountain].tapped);
}

// --- Two filter lands whose sub-costs must not be funded independently ---

/// P0: Graven Cairns, Fetid Heath, Dwarven Ruins, Swamp (in that order), all
/// untapped, and Hearthfire Hobgoblin ({R/W}{R/W}{R/W}) in hand.
fn cairns_heath_board() -> (GameRunner, [ObjectId; 4], ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cairns = scenario
        .add_land_from_oracle(P0, "Graven Cairns", GRAVEN_CAIRNS_ORACLE)
        .id();
    let heath = scenario
        .add_land_from_oracle(P0, "Fetid Heath", FETID_HEATH_ORACLE)
        .id();
    let ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let hobgoblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Hearthfire Hobgoblin", 2, 2, "Double strike")
        .with_mana_cost(cost(
            &[
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
            ],
            0,
        ))
        .id();
    let lands = [cairns, heath, ruins, swamp];
    let runner = build_untapped(scenario, &lands);
    for land in lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "reach: {land:?} starts untapped"
        );
    }
    for (land, abilities) in [(cairns, 2), (heath, 2), (ruins, 2)] {
        assert_eq!(
            runner.state().objects[&land].abilities.len(),
            abilities,
            "reach: {land:?} has its plain and its costed mana ability"
        );
    }
    (runner, lands, hobgoblin)
}

/// CR 601.2g + CR 605.3b: Cairns {R}{R} and Heath {W}{W} pay the Hobgoblin, and
/// each filter ability's hybrid sub-cost needs its own funding source. Auto-pay
/// must not fund Cairns' {B/R} with the Swamp that Heath's {W/B} needs: the
/// cast is affordable and resolves through the default automatic payment.
#[test]
fn two_filter_land_sub_costs_are_funded_together() {
    let (mut runner, [cairns, heath, _ruins, _swamp], hobgoblin) = cairns_heath_board();
    assert!(
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(
                |action| matches!(action, GameAction::CastSpell { object_id, .. }
                if *object_id == hobgoblin)
            ),
        "Hearthfire Hobgoblin must be affordable under automatic payment"
    );

    let outcome = runner.cast(hobgoblin).resolve();
    let filter_outputs: Vec<(ObjectId, ManaType)> = mana_added(outcome.events())
        .into_iter()
        .filter(|(source, _)| *source == cairns || *source == heath)
        .collect();
    assert_eq!(
        filter_outputs,
        vec![
            (cairns, ManaType::Red),
            (cairns, ManaType::Red),
            (heath, ManaType::White),
            (heath, ManaType::White),
        ],
        "reach: the selected filter outputs are Cairns {{R}}{{R}}, then Heath {{W}}{{W}}; all mana \
         added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        outcome.zone_of(hobgoblin),
        Zone::Battlefield,
        "mana added {:?}",
        mana_added(outcome.events())
    );
}

/// Manual control for the board above: Swamp funds Heath, Ruins' plain {R}
/// funds Cairns, and the Hobgoblin resolves without sacrificing the Ruins.
#[test]
fn two_filter_land_sub_costs_manual_control() {
    let (mut runner, [cairns, heath, ruins, swamp], hobgoblin) = cairns_heath_board();
    let activate = |runner: &mut GameRunner, source: ObjectId, output: &[ManaType]| {
        let action = activatable_mana_actions_for_player(runner.state(), P0)
            .into_iter()
            .find(|action| match action {
                GameAction::TapLandForMana { selection } => {
                    selection.source.object_id == source
                        && match output {
                            [single] => {
                                selection.atomic_combination.is_none()
                                    && selection.mana_type == *single
                            }
                            combination => {
                                selection.atomic_combination.as_deref() == Some(combination)
                            }
                        }
                }
                _ => false,
            })
            .unwrap_or_else(|| panic!("{source:?} offers {output:?}"));
        runner.act(action).expect("manual mana activation");
    };
    activate(&mut runner, swamp, &[ManaType::Black]);
    activate(&mut runner, heath, &[ManaType::White, ManaType::White]);
    activate(&mut runner, ruins, &[ManaType::Red]);
    activate(&mut runner, cairns, &[ManaType::Red, ManaType::Red]);

    let outcome = runner.cast(hobgoblin).resolve();
    assert_eq!(outcome.zone_of(hobgoblin), Zone::Battlefield);
    assert_eq!(outcome.zone_of(ruins), Zone::Battlefield);
}

// --- A payment that pauses keeps the payment's row policy when it resumes ---

/// CR 601.2g + CR 605.3b + CR 616.1: two Dwarven Ruins pay {R/W}{R/W}{R/W} only
/// with a sacrifice row, so the payment plans with every row (two sacrifices,
/// as before drawback avoidance). Rest in Peace and Leyline of the Void both
/// replace the first Ruins' trip to the graveyard, so the payment pauses for
/// P0's replacement choice. Drawback avoidance applies only to a payment that
/// completes without suspending: the resumed remainder plans with every row, so
/// the second Ruins is sacrificed too, exactly as before drawback avoidance.
#[test]
fn resumed_payment_keeps_its_row_policy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let second_ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE_ORACLE);
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID_ORACLE);
    let hobgoblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Hearthfire Hobgoblin", 2, 2, "Double strike")
        .with_mana_cost(cost(
            &[
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
            ],
            0,
        ))
        .id();
    let mut runner = build_untapped(scenario, &[first_ruins, second_ruins]);
    let card_id = runner.state().objects[&hobgoblin].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: hobgoblin,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the automatic cast starts paying");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach: the first Ruins' sacrifice pauses for a replacement choice, got {:?}",
        runner.state().waiting_for
    );
    let tapped_before_choice = [first_ruins, second_ruins]
        .into_iter()
        .filter(|ruins| runner.state().objects[ruins].tapped)
        .count();
    assert_eq!(
        tapped_before_choice, 1,
        "reach: one Ruins has paid {{T}} and waits on its sacrifice; the other is untouched"
    );

    // Each sacrificed Ruins asks P0 to order Rest in Peace and Leyline of the
    // Void (CR 616.1). Answer every prompt the payment raises.
    let mut replacement_choices = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the replacement choice resumes the payment");
        replacement_choices += 1;
        assert!(replacement_choices <= 2, "at most one choice per Ruins");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resumed payment completes the cast; waiting for {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        replacement_choices, 2,
        "the resumed payment keeps the every-row plan and sacrifices the second Ruins too"
    );
    for ruins in [first_ruins, second_ruins] {
        assert!(
            !runner.state().battlefield.contains(&ruins),
            "{ruins:?} was sacrificed"
        );
    }
    let exiled_ruins = runner
        .state()
        .exile
        .iter()
        .filter(|id| runner.state().objects[id].name == "Dwarven Ruins")
        .count();
    assert_eq!(
        exiled_ruins, 2,
        "reach: both sacrificed Ruins were exiled by the chosen replacement"
    );
    runner.resolve_top();
    assert!(
        runner.state().battlefield.contains(&hobgoblin),
        "reach: the paid Hobgoblin resolves"
    );
}

// --- A standalone manual activation's retry is not a resumed automatic payment ---

/// CR 605.3a + CR 602.2b + CR 117.1d: P0 activates Cascading Cataracts'
/// `{5}, {T}` ability by hand with an empty pool, which opens its own payment
/// window (no source is tapped). Inside it, Soldevi Adnate sacrifices Vampire
/// Nighthawk for {B}{B}{B}, and the Cataracts activation is retried. That retry
/// is the first automatic payment of the remaining {2}, not the remainder of a
/// suspended automatic payment, so it avoids drawbacks: Urborg makes Ancient
/// Tomb a Swamp, so Tomb's plain {B} and Urborg's {B} pay it, and Tomb's
/// `{C}{C}` row (2 damage) is not used. P0 stays at 20 life.
#[test]
fn manual_activation_retry_still_avoids_drawback_rows() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cataracts = scenario
        .add_land_from_oracle(P0, "Cascading Cataracts", CASCADING_CATARACTS_ORACLE)
        .id();
    let tomb = scenario
        .add_land_from_oracle(P0, "Ancient Tomb", ANCIENT_TOMB_ORACLE)
        .id();
    let urborg = scenario
        .add_land_from_oracle(P0, "Urborg, Tomb of Yawgmoth", URBORG_ORACLE)
        .id();
    let adnate = scenario
        .add_creature_from_oracle(P0, "Soldevi Adnate", 1, 2, SOLDEVI_ADNATE_ORACLE)
        .with_mana_cost(cost(&[ManaCostShard::Black], 1))
        .id();
    let nighthawk = scenario
        .add_creature_from_oracle(P0, "Vampire Nighthawk", 2, 3, VAMPIRE_NIGHTHAWK_ORACLE)
        .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 1))
        .id();
    let mut runner = build_untapped(scenario, &[cataracts, tomb, urborg, adnate, nighthawk]);
    assert_eq!(runner.state().players[0].life, 20, "reach: P0 starts at 20");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: empty pool"
    );

    let costed_mana_ability = |runner: &GameRunner, source: ObjectId, costed: bool| {
        runner.state().objects[&source]
            .abilities
            .iter()
            .position(|ability| {
                engine::game::mana_abilities::is_mana_ability(ability)
                    && matches!(
                        ability.cost,
                        Some(engine::types::ability::AbilityCost::Composite { .. })
                    ) == costed
            })
            .expect("the mana ability")
    };
    let five = costed_mana_ability(&runner, cataracts, true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cataracts,
            ability_index: five,
        })
        .expect("Cataracts' {5} ability starts activating");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaAbilityManaPayment { .. }
        ),
        "reach: Cataracts' {{5}} opens its payment window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().objects[&tomb].tapped && !runner.state().objects[&urborg].tapped,
        "reach: opening the window taps neither Tomb nor Urborg"
    );

    let adnate_ability = runner.state().objects[&adnate]
        .abilities
        .iter()
        .position(engine::game::mana_abilities::is_mana_ability)
        .expect("Adnate's mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: adnate,
            ability_index: adnate_ability,
        })
        .expect("Adnate activates inside the window");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => break,
            WaitingFor::ReplacementChoice { .. } => {
                panic!("reach: the board has no replacement that suspends the payment")
            }
            WaitingFor::ChooseManaColor { .. } => {
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(vec![ManaType::Black; 5]),
                        count: 1,
                    })
                    .expect("Cataracts' output choice");
            }
            _ => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![nighthawk],
                    })
                    .expect("Adnate sacrifices Vampire Nighthawk");
            }
        }
    }
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the activation completes, got {:?}",
        state.waiting_for
    );
    assert!(
        !state.battlefield.contains(&nighthawk),
        "reach: Nighthawk was sacrificed for {{B}}{{B}}{{B}}"
    );
    assert!(
        state.objects[&cataracts].tapped,
        "reach: Cataracts' ability was activated"
    );
    assert_eq!(
        state.players[0].mana_pool.total(),
        5,
        "reach: Cataracts made five mana"
    );
    assert!(
        state.objects[&tomb].tapped && state.objects[&urborg].tapped,
        "Tomb and Urborg paid the remaining {{2}}"
    );
    assert_eq!(
        state.players[0].life, 20,
        "the retry used Tomb's plain {{B}}, not its {{C}}{{C}} row that deals 2 damage"
    );
}

// --- A drawback-free plan whose selected source has its own mana cost ---

/// The cast's selected sources, in `ManaAdded` order, restricted to `sources`.
fn mana_added_by(events: &[GameEvent], sources: &[ObjectId]) -> Vec<(ObjectId, ManaType)> {
    mana_added(events)
        .into_iter()
        .filter(|(source, _)| sources.contains(source))
        .collect()
}

/// Azorius Signet, `drawback_land` and a Mountain, all untapped, with an empty
/// pool, and a {W}{U}{R} witness. The drawback-free plan selects the Signet
/// (its `{1}` is a mana sub-cost) for {W}{U} and the Mountain for {R}, and the
/// Signet's `{1}` is funded by `drawback_land`'s plain `{T}: Add {R}`.
fn signet_funded_by_plain_red_row(
    drawback_land: (&str, &str),
) -> (CastOutcome, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let signet = scenario
        .add_artifact_from_oracle(P0, "Azorius Signet", AZORIUS_SIGNET_ORACLE)
        .id();
    let land = scenario
        .add_land_from_oracle(P0, drawback_land.0, drawback_land.1)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let spell = witness_spell(
        &mut scenario,
        cost(
            &[
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Red,
            ],
            0,
        ),
    );
    let mut runner = build_untapped(scenario, &[signet, land, mountain]);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: empty pool"
    );
    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(
        mana_added_by(outcome.events(), &[signet]),
        vec![(signet, ManaType::White), (signet, ManaType::Blue)],
        "reach: the Signet's costed `{{1}}, {{T}}: Add {{W}}{{U}}` row was selected and resolved; \
         mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[mountain]),
        vec![(mountain, ManaType::Red)],
        "reach: the Mountain paid the spell's {{R}}"
    );
    (outcome, signet, land, mountain)
}

/// CR 601.2g + CR 605.3b + CR 701.43a: the drawback-free plan selects Azorius
/// Signet, whose `{1}` is a mana sub-cost, and Arena of Glory's plain
/// `{T}: Add {R}` funds it. Arena is tapped once for {R} and not exerted.
#[test]
fn signet_sub_cost_funded_by_arena_plain_row_does_not_exert() {
    let (outcome, _signet, arena, _mountain) =
        signet_funded_by_plain_red_row(("Arena of Glory", ARENA_OF_GLORY_ORACLE));
    assert_eq!(
        mana_added_by(outcome.events(), &[arena]),
        vec![(arena, ManaType::Red)],
        "Arena's plain row funded the Signet's {{1}}"
    );
    assert_eq!(
        exert_markers(outcome.state(), arena),
        0,
        "mana added {:?}",
        mana_added(outcome.events())
    );
}

/// CR 601.2g + CR 605.3b + CR 701.21a: the same plan with Dwarven Ruins: its
/// plain `{T}: Add {R}` funds the Signet's `{1}`, and the Ruins survive.
#[test]
fn signet_sub_cost_funded_by_dwarven_ruins_plain_row_keeps_ruins() {
    let (outcome, _signet, ruins, _mountain) =
        signet_funded_by_plain_red_row(("Dwarven Ruins", DWARVEN_RUINS_ORACLE));
    assert_eq!(
        mana_added_by(outcome.events(), &[ruins]),
        vec![(ruins, ManaType::Red)],
        "the Ruins' plain row funded the Signet's {{1}}"
    );
    assert_eq!(outcome.zone_of(ruins), Zone::Battlefield);
}

/// CR 601.2g + CR 605.3b + CR 701.21a: a {U}{G} witness with Ceta Disciple
/// (`{G}, {T}: Add one mana of any color.`, no plain mana ability), Havenwood
/// Battleground and a Forest. The drawback-free plan selects Ceta Disciple for
/// {U} and the Forest for {G}, and Ceta Disciple's colored `{G}` sub-cost is
/// funded by Havenwood's plain `{T}: Add {G}`. The nested sub-cost payment
/// keeps the payment's policy: planning it with every row would sacrifice
/// Havenwood for `{G}{G}`.
#[test]
fn colored_sub_cost_keeps_the_payment_row_policy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let disciple = scenario
        .add_creature_from_oracle(P0, "Ceta Disciple", 1, 1, CETA_DISCIPLE_ORACLE)
        .id();
    let havenwood = scenario
        .add_land_from_oracle(P0, "Havenwood Battleground", HAVENWOOD_BATTLEGROUND_ORACLE)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = witness_spell(
        &mut scenario,
        cost(&[ManaCostShard::Blue, ManaCostShard::Green], 0),
    );
    let mut runner = build_untapped(scenario, &[disciple, havenwood, forest]);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: empty pool"
    );

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(
        mana_added_by(outcome.events(), &[disciple]),
        vec![(disciple, ManaType::Blue)],
        "reach: Ceta Disciple's costed row was selected and resolved; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[forest]),
        vec![(forest, ManaType::Green)],
        "reach: the Forest paid the spell's {{G}}"
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[havenwood]),
        vec![(havenwood, ManaType::Green)],
        "Havenwood's plain row funded Ceta Disciple's {{G}}"
    );
    assert_eq!(
        outcome.zone_of(havenwood),
        Zone::Battlefield,
        "Havenwood was not sacrificed"
    );
}

/// CR 106.4 + CR 118.10 + CR 107.4e: P0 floats {B} from a Swamp, then casts
/// Black Knight ({B}{B}) with Twilight Mire and Havenwood Battleground untapped.
/// Only Mire's filter pair makes the second {B}, so the payment plans with
/// every row and selects Mire's {B}{B}. Its `{B/G}` sub-cost may use the
/// floating {B}: Mire's own {B}{B} then covers the whole outer demand. Holding
/// that {B} back for the outer cost would make the sub-cost sacrifice Havenwood
/// for {G}{G} instead.
#[test]
fn floating_mana_replaced_by_the_funded_output_is_not_reserved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mire = scenario
        .add_land_from_oracle(P0, "Twilight Mire", TWILIGHT_MIRE_ORACLE)
        .id();
    let havenwood = scenario
        .add_land_from_oracle(P0, "Havenwood Battleground", HAVENWOOD_BATTLEGROUND_ORACLE)
        .id();
    let knight = scenario
        .add_creature_to_hand_from_oracle(P0, "Black Knight", 2, 2, BLACK_KNIGHT_ORACLE)
        .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 0))
        .id();
    let mut runner = build_untapped(scenario, &[swamp, mire, havenwood]);

    let tap_swamp = activatable_mana_actions_for_player(runner.state(), P0)
        .into_iter()
        .find(|action| {
            matches!(action, GameAction::TapLandForMana { selection }
                if selection.source.object_id == swamp)
        })
        .expect("the Swamp offers {B}");
    runner.act(tap_swamp).expect("P0 floats {B} from the Swamp");
    let pool = &runner.state().players[0].mana_pool;
    assert!(
        pool.total() == 1 && pool.count_color(ManaType::Black) == 1,
        "reach: exactly one floating {{B}}"
    );

    let outcome = runner.cast(knight).resolve();
    assert_eq!(
        outcome.zone_of(knight),
        Zone::Battlefield,
        "reach: Black Knight was paid and resolved; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[mire]),
        vec![(mire, ManaType::Black), (mire, ManaType::Black)],
        "reach: Twilight Mire's {{B}}{{B}} pair paid the second {{B}}"
    );
    assert!(
        mana_added_by(outcome.events(), &[havenwood]).is_empty(),
        "Havenwood made no mana; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(outcome.zone_of(havenwood), Zone::Battlefield);
    assert!(!outcome.state().objects[&havenwood].tapped);
}

/// CR 601.2g + CR 605.3b + CR 106.4: two selected sources with their own mana
/// costs. P0 has an empty pool and, created in this order, Sungrass Prairie
/// (`{1}, {T}: Add {G}{W}`), Ceta Disciple (`{G}, {T}`: any color), Havenwood
/// Battleground, a Forest and a Plains, and casts Tatyova, Steward of Tides
/// ({G}{G}{U}). A plan funding each sub-cost from an unused source would pay the
/// Sungrass `{1}` from the Plains and Ceta's `{G}` from Havenwood's plain row.
/// Executed one source at a time, Ceta's `{G}` could instead take the {G}
/// Sungrass already added, leaving the cast one {G} short. Auto-pay must still
/// cast and resolve Tatyova.
#[test]
fn two_selected_sub_costs_still_pay_the_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sungrass = scenario
        .add_land_from_oracle(P0, "Sungrass Prairie", SUNGRASS_PRAIRIE_ORACLE)
        .id();
    let disciple = scenario
        .add_creature_from_oracle(P0, "Ceta Disciple", 1, 1, CETA_DISCIPLE_ORACLE)
        .id();
    let havenwood = scenario
        .add_land_from_oracle(P0, "Havenwood Battleground", HAVENWOOD_BATTLEGROUND_ORACLE)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let tatyova = scenario
        .add_creature_to_hand_from_oracle(P0, "Tatyova, Steward of Tides", 3, 3, TATYOVA_ORACLE)
        .with_mana_cost(cost(
            &[
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Blue,
            ],
            0,
        ))
        .id();
    let mut runner = build_untapped(scenario, &[sungrass, disciple, havenwood, forest, plains]);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: empty pool"
    );

    let outcome = runner.cast(tatyova).resolve();
    assert_eq!(
        outcome.zone_of(tatyova),
        Zone::Battlefield,
        "Tatyova was paid and resolved; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[disciple]),
        vec![(disciple, ManaType::Blue)],
        "reach: Ceta Disciple's costed row paid the {{U}}; mana added {:?}",
        mana_added(outcome.events()),
    );
}

/// CR 601.2g + CR 605.3b + CR 106.4: a drawback-free plan with a selected
/// sub-cost is only used when the pool is empty. P0 floats {G} from a Forest,
/// then casts a {U}{G}{G} witness with Ceta Disciple, Havenwood Battleground and
/// another Forest. The drawback-free plan (Ceta for {U}, the Forest for {G},
/// Havenwood's plain {G} for Ceta's sub-cost) would let Ceta's `{G}` take the
/// floating {G} and leave the cast one {G} short, so the payment plans with
/// every row instead and the cast is paid.
#[test]
fn selected_sub_cost_with_floating_mana_still_pays_the_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let disciple = scenario
        .add_creature_from_oracle(P0, "Ceta Disciple", 1, 1, CETA_DISCIPLE_ORACLE)
        .id();
    let havenwood = scenario
        .add_land_from_oracle(P0, "Havenwood Battleground", HAVENWOOD_BATTLEGROUND_ORACLE)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let floated_forest = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = witness_spell(
        &mut scenario,
        cost(
            &[
                ManaCostShard::Blue,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ),
    );
    let mut runner = build_untapped(scenario, &[disciple, havenwood, forest, floated_forest]);
    let tap_forest = activatable_mana_actions_for_player(runner.state(), P0)
        .into_iter()
        .find(|action| {
            matches!(action, GameAction::TapLandForMana { selection }
                if selection.source.object_id == floated_forest)
        })
        .expect("the Forest offers {G}");
    runner.act(tap_forest).expect("P0 floats {G}");
    let pool = &runner.state().players[0].mana_pool;
    assert!(
        pool.total() == 1 && pool.count_color(ManaType::Green) == 1,
        "reach: exactly one floating {{G}}"
    );

    let outcome = auto_pay_and_resolve(&mut runner, spell);
    assert_eq!(
        mana_added_by(outcome.events(), &[disciple]),
        vec![(disciple, ManaType::Blue)],
        "reach: Ceta Disciple's costed row paid the {{U}}; mana added {:?}",
        mana_added(outcome.events()),
    );
}

// --- Cost shapes the plan check does not model fall back to every row ---

fn mana_component(shards: &[ManaCostShard]) -> AbilityCost {
    AbilityCost::Mana {
        cost: cost(shards, 0),
    }
}

/// Azorius Signet with its `{1}, {T}` cost replaced by the typed `signet_cost`,
/// Dwarven Ruins, and (when `with_mountain`) a Mountain, all untapped with an
/// empty pool, and Azorius First-Wing ({W}{U}) in hand. Casts First-Wing
/// through the default automatic payment and returns the outcome.
fn signet_with_cost_casts_first_wing(
    signet_cost: AbilityCost,
    with_mountain: bool,
) -> (CastOutcome, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let signet = scenario
        .add_artifact_from_oracle(P0, "Azorius Signet", AZORIUS_SIGNET_ORACLE)
        .id();
    let ruins = scenario
        .add_land_from_oracle(P0, "Dwarven Ruins", DWARVEN_RUINS_ORACLE)
        .id();
    let mut lands = vec![signet, ruins];
    if with_mountain {
        lands.push(scenario.add_basic_land(P0, ManaColor::Red));
    }
    let first_wing = scenario
        .add_creature_to_hand_from_oracle(P0, "Azorius First-Wing", 2, 2, AZORIUS_FIRST_WING_ORACLE)
        .with_mana_cost(cost(&[ManaCostShard::White, ManaCostShard::Blue], 0))
        .id();
    let mut runner = build_untapped(scenario, &lands);
    {
        let object = runner.state_mut().objects.get_mut(&signet).unwrap();
        for abilities in [&mut object.abilities, &mut object.base_abilities] {
            std::sync::Arc::make_mut(abilities)[0].cost = Some(signet_cost.clone());
        }
    }
    assert_eq!(
        runner.state().objects[&signet].abilities[0].cost,
        Some(signet_cost),
        "reach: the typed cost survived construction"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "reach: empty pool"
    );
    let outcome = runner.cast(first_wing).resolve();
    assert_eq!(
        outcome.zone_of(first_wing),
        Zone::Battlefield,
        "Azorius First-Wing was paid and resolved; mana added {:?}",
        mana_added(outcome.events()),
    );
    assert_eq!(
        mana_added_by(outcome.events(), &[signet]),
        vec![(signet, ManaType::White), (signet, ManaType::Blue)],
        "reach: the Signet's costed row paid {{W}}{{U}}"
    );
    (outcome, signet, ruins)
}

/// CR 601.2h + CR 602.2b: a costed row whose cost makes two separate mana
/// payments (`Composite[Mana {R}, Mana {R}, Tap]`). The drawback-free plan's
/// check models one mana payment per row, so the plan is not used and the
/// payment plans with every row: Dwarven Ruins is sacrificed for {R}{R}, which
/// pays both components, and the cast resolves.
#[test]
fn two_mana_components_fall_back_to_every_row() {
    let (outcome, _signet, ruins) = signet_with_cost_casts_first_wing(
        AbilityCost::Composite {
            costs: vec![
                mana_component(&[ManaCostShard::Red]),
                mana_component(&[ManaCostShard::Red]),
                AbilityCost::Tap,
            ],
        },
        false,
    );
    assert_eq!(
        outcome.zone_of(ruins),
        Zone::Graveyard,
        "the every-row fallback sacrificed the Ruins for {{R}}{{R}}"
    );
}

/// CR 601.2h + CR 602.2b: a costed row whose only mana payment sits inside a
/// nested `Composite`, which the planner's pre-funding does not see. The plan
/// is not used and the payment plans with every row, and the cast resolves.
#[test]
fn nested_mana_component_falls_back_to_every_row() {
    let (outcome, _signet, ruins) = signet_with_cost_casts_first_wing(
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Composite {
                    costs: vec![mana_component(&[ManaCostShard::Red])],
                },
                AbilityCost::Tap,
            ],
        },
        true,
    );
    assert_eq!(
        outcome.zone_of(ruins),
        Zone::Graveyard,
        "the every-row fallback sacrificed the Ruins for the nested {{R}}"
    );
}

/// Positive control: one mana payment of {R}{R} (`Composite[Mana {R}{R}, Tap]`)
/// is the shape the check models, so the drawback-free plan is used: the Ruins'
/// plain {R} and the Mountain fund it, and the Ruins survive.
#[test]
fn single_mana_component_keeps_the_drawback_free_plan() {
    let (outcome, _signet, ruins) = signet_with_cost_casts_first_wing(
        AbilityCost::Composite {
            costs: vec![
                mana_component(&[ManaCostShard::Red, ManaCostShard::Red]),
                AbilityCost::Tap,
            ],
        },
        true,
    );
    assert_eq!(outcome.zone_of(ruins), Zone::Battlefield);
    assert_eq!(
        mana_added_by(outcome.events(), &[ruins]),
        vec![(ruins, ManaType::Red)],
        "the Ruins' plain row funded one {{R}}"
    );
}
