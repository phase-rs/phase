//! CR 106.7 — the class of "could produce" readers, driven through the real
//! activation pipeline: Reflecting Pool ("any type that a land you control
//! could produce"), Exotic Orchard ("any color that a land an opponent controls
//! could produce") and Naga Vitalist, all answered by the one could-produce
//! authority (`game::could_produce`).
//!
//! CR 106.7: "The type of mana a permanent could produce at any time includes
//! any type of mana that an ability of that permanent would produce if the
//! ability were to resolve at that time, taking into account any applicable
//! replacement effects in any possible order. Ignore whether any costs of the
//! ability could or could not be paid."
//!
//! Exotic Orchard rulings (2009-02-01): could-produce lands "won't help each
//! other unless some other land allows one of them to actually produce some
//! type of mana"; Orchard "can't be tapped for colorless mana, even if a land an
//! opponent controls could produce colorless mana"; and it "takes into account
//! any applicable replacement effects".

use engine::ai_support::legal_actions;
use engine::game::derived::continuously_revealed_cards;
use engine::game::effects::attach::attach_to;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    inline_mana_preamble, is_mana_ability, AbilityCondition, AbilityKind, ChoiceType, Effect,
    EffectOutcomeSignal, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::parse_counter_type;
use engine::types::events::{GameEvent, ManaTapState};
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, GameState, ManaChoice, ManaChoicePrompt,
    PayCostKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const REFLECTING_POOL: &str =
    "{T}: Add one mana of any type that a land you control could produce.";
const EXOTIC_ORCHARD: &str =
    "{T}: Add one mana of any color that a land an opponent controls could produce.";
const NAGA_VITALIST: &str = "{T}: Add one mana of any type that a land you control could produce.";
const CONTAMINATION: &str = "At the beginning of your upkeep, sacrifice this enchantment unless \
     you sacrifice a creature.\nIf a land is tapped for mana, it produces {B} instead of any other \
     type and amount.";
const WASTES: &str = "{T}: Add {C}.";
const CALCIFORM_POOLS: &str = "{T}: Add {C}.\n{1}, {T}: Put a storage counter on this land.\n{1}, \
     Remove X storage counters from this land: Add X mana in any combination of {W} and/or {U}.";
const RIVER_OF_TEARS: &str = "{T}: Add {U}. If you played a land this turn, add {B} instead.";
const GEMSTONE_CAVERNS: &str = "If this card is in your opening hand and you're not the starting \
     player, you may begin the game with Gemstone Caverns on the battlefield with a luck counter \
     on it. If you do, exile a card from your hand.\n{T}: Add {C}. If Gemstone Caverns has a luck \
     counter on it, instead add one mana of any color.";
const URZAS_TOWER: &str =
    "{T}: Add {C}. If you control an Urza's Mine and an Urza's Power-Plant, add {C}{C}{C} instead.";
const URZAS_MINE: &str =
    "{T}: Add {C}. If you control an Urza's Power-Plant and an Urza's Tower, add {C}{C} instead.";
const URZAS_POWER_PLANT: &str =
    "{T}: Add {C}. If you control an Urza's Mine and an Urza's Tower, add {C}{C} instead.";
const UGINS_LABYRINTH: &str = "Imprint — When this land enters, you may exile a colorless card \
     with mana value 7 or greater from your hand.\n{T}: Add {C}. If a card is exiled with this \
     land, add {C}{C} instead.\n{T}: Return the exiled card to its owner's hand.";
const SQUANDERED_RESOURCES: &str =
    "Sacrifice a land: Add one mana of any type the sacrificed land could produce.";

/// What one activation surfaced: the type prompt's options, or the mana that
/// arrived without one.
#[derive(Debug, PartialEq, Eq)]
enum Produced {
    Prompt(Vec<ManaType>),
    Pool(Vec<ManaType>),
}

fn new_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn pool(runner: &GameRunner, player: PlayerId) -> Vec<ManaType> {
    runner.state().players[player.0 as usize]
        .mana_pool
        .units()
        .map(|unit| unit.color)
        .collect()
}

/// Activates `source`'s first activated ability — its mana ability here,
/// which resolves immediately (CR 605.3b) — and reports the prompt it raised or
/// the mana it added. Gemstone Caverns' opening-hand ability comes first in its
/// list, so the index is looked up rather than assumed.
fn tap_for_mana(runner: &mut GameRunner, source: ObjectId) -> Produced {
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: the source has an activated ability");
    activate_for_mana(runner, source, ability_index)
}

/// Empties P0's pool, activates `source`'s mana ability at `ability_index`,
/// which resolves at once (CR 605.3b), and reports the prompt it raised or the
/// mana it added.
fn activate_for_mana(runner: &mut GameRunner, source: ObjectId, ability_index: usize) -> Produced {
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: the mana ability is activatable");
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => Produced::Prompt(options),
        WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
            Produced::Pool(pool(runner, P0))
        }
        other => panic!("unexpected state after tapping for mana: {other:?}"),
    }
}

/// C1: the Exotic Orchard ruling's worked board. Each player has a Reflecting
/// Pool-or-Orchard chain anchored by a Forest, so P0's Pool could produce {G}
/// through P0's Orchard → P1's Forest.
#[test]
fn reflecting_pool_follows_an_orchard_anchored_by_an_opponents_forest() {
    let mut scenario = new_scenario();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_basic_land(P1, ManaColor::Green);
    scenario.add_land_from_oracle(P1, "Exotic Orchard", EXOTIC_ORCHARD);
    let mut runner = scenario.build();

    assert_eq!(
        tap_for_mana(&mut runner, orchard),
        Produced::Pool(vec![ManaType::Green]),
        "control: P0's Orchard reads P1's Forest"
    );
    assert_eq!(
        tap_for_mana(&mut runner, pool_land),
        Produced::Pool(vec![ManaType::Green]),
        "CR 106.7: P0's Pool reads P0's Orchard, which is anchored"
    );
}

/// C2: an unanchored board — no land on either side produces anything on its
/// own, so the could-produce graph's least fixed point is empty. C1 is the
/// positive pair.
#[test]
fn exotic_orchard_with_no_anchor_could_produce_nothing() {
    let mut scenario = new_scenario();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Exotic Orchard", EXOTIC_ORCHARD);
    scenario.add_land_from_oracle(P1, "Reflecting Pool", REFLECTING_POOL);
    let mut runner = scenario.build();

    assert_eq!(tap_for_mana(&mut runner, orchard), Produced::Pool(vec![]));
}

/// C3: Exotic Orchard asks for colors (CR 105.1), never colorless — facing
/// Wastes it adds nothing; facing Wastes and an Island it adds only {U}.
#[test]
fn exotic_orchard_never_taps_for_colorless() {
    let mut scenario = new_scenario();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Wastes", WASTES);
    let mut runner = scenario.build();
    assert_eq!(tap_for_mana(&mut runner, orchard), Produced::Pool(vec![]));

    let mut scenario = new_scenario();
    let orchard = scenario
        .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
        .id();
    scenario.add_land_from_oracle(P1, "Wastes", WASTES);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();
    assert_eq!(
        tap_for_mana(&mut runner, orchard),
        Produced::Pool(vec![ManaType::Blue])
    );
}

/// C4 (CR 106.7 + CR 614.1a): the reading goes through applicable
/// replacements. Under Contamination a Forest tapped for mana produces {B}, so
/// Naga Vitalist — a creature, not itself rewritten — adds {B}.
#[test]
fn naga_vitalist_reads_its_forest_through_contamination() {
    let mut scenario = new_scenario();
    let naga = scenario
        .add_creature_from_oracle(P0, "Naga Vitalist", 1, 2, NAGA_VITALIST)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let mut runner = scenario.build();

    assert_eq!(
        tap_for_mana(&mut runner, forest),
        Produced::Pool(vec![ManaType::Black]),
        "control: Contamination rewrites the Forest tapped for mana"
    );
    runner.state_mut().objects.get_mut(&forest).unwrap().tapped = false;
    assert_eq!(
        tap_for_mana(&mut runner, naga),
        Produced::Pool(vec![ManaType::Black])
    );
}

/// C5: Reflecting Pool reads an X producer at its resolution-time answer —
/// Calciform Pools could produce {W}, {U} and {C} (CR 106.7 ignores whether
/// the X could be paid).
#[test]
fn reflecting_pool_reads_an_x_producer() {
    let mut scenario = new_scenario();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_land_from_oracle(P0, "Calciform Pools", CALCIFORM_POOLS);
    let mut runner = scenario.build();

    assert_eq!(
        tap_for_mana(&mut runner, pool_land),
        Produced::Prompt(vec![ManaType::White, ManaType::Blue, ManaType::Colorless])
    );
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

/// C6 (CR 608.2c + CR 614.1a): Reflecting Pool reads a conditional land at
/// resolution time — River of Tears could produce exactly {U} before a land is
/// played and exactly {B} after.
#[test]
fn reflecting_pool_reads_river_of_tears_at_that_time() {
    let mut scenario = new_scenario();
    let pool_land = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS);
    let land_in_hand = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    assert_eq!(
        tap_for_mana(&mut runner, pool_land),
        Produced::Pool(vec![ManaType::Blue])
    );
    runner
        .state_mut()
        .objects
        .get_mut(&pool_land)
        .unwrap()
        .tapped = false;
    play_a_land(&mut runner, land_in_hand);
    assert_eq!(
        tap_for_mana(&mut runner, pool_land),
        Produced::Pool(vec![ManaType::Black])
    );
}

/// What Squandered Resources captured when `land` was sacrificed — the
/// could-produce set as the land last existed (CR 608.2h + CR 106.7) — as the
/// prompt's options or the single type that arrived.
fn squandered_reading(state: &GameState, squandered: ObjectId, land: ObjectId) -> Vec<ManaType> {
    let mut runner = GameRunner::from_state(state.clone());
    runner
        .act(GameAction::ActivateAbility {
            source_id: squandered,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: Squandered Resources is activatable");
    let WaitingFor::PayCost {
        kind: PayCostKind::Sacrifice,
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "expected the sacrifice choice, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("sacrificing the land");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "reach-guard: the land was sacrificed"
    );
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => options,
        _ => pool(&runner, P0),
    }
}

/// What the land's own activation offers or adds in `state`.
fn actual_types(state: &GameState, land: ObjectId) -> Vec<ManaType> {
    let mut runner = GameRunner::from_state(state.clone());
    let produced = tap_for_mana(&mut runner, land);
    let mut types = match produced {
        Produced::Prompt(options) => options,
        Produced::Pool(pool) => {
            assert!(
                !pool.is_empty(),
                "reach-guard: the real activation added mana"
            );
            pool
        }
    };
    types.sort();
    types.dedup();
    types
}

/// P0's Urza's Mine and Urza's Tower, plus Urza's Power Plant when
/// `assembled`, each with its land types (the Urza's lands ruling: "The
/// ability checks for land types, not the names of other permanents you
/// control."). Returns the scenario and the Mine.
fn tron_scenario(assembled: bool) -> (GameScenario, ObjectId) {
    let mut scenario = new_scenario();
    let mine = scenario
        .add_land_from_oracle(P0, "Urza's Mine", URZAS_MINE)
        .with_subtypes(vec!["Urza's", "Mine"])
        .id();
    scenario
        .add_land_from_oracle(P0, "Urza's Tower", URZAS_TOWER)
        .with_subtypes(vec!["Urza's", "Tower"]);
    if assembled {
        scenario
            .add_land_from_oracle(P0, "Urza's Power Plant", URZAS_POWER_PLANT)
            .with_subtypes(vec!["Urza's", "Power-Plant"]);
    }
    (scenario, mine)
}

/// C7 (differential): for every conditional producer in both states of its
/// condition, the CR 106.7 reading — observed through Squandered Resources'
/// capture of the sacrificed land — names exactly the types the land's own
/// activation offers or adds in the same state. Urza's Mine is also read under
/// Contamination, where every production of its {T} mana ability is tapped for
/// mana (CR 106.12b).
#[test]
fn hypothetical_reading_matches_the_real_activation() {
    fn with_squandered(scenario: &mut GameScenario) -> ObjectId {
        scenario
            .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
            .id()
    }

    let mut cases: Vec<(&str, GameState, ObjectId, ObjectId)> = Vec::new();

    for land_played in [false, true] {
        let mut scenario = new_scenario();
        let squandered = with_squandered(&mut scenario);
        let river = scenario
            .add_land_from_oracle(P0, "River of Tears", RIVER_OF_TEARS)
            .id();
        let land_in_hand = scenario.add_land_to_hand(P0, "Wastes").id();
        let mut runner = scenario.build();
        if land_played {
            play_a_land(&mut runner, land_in_hand);
        }
        cases.push(("River of Tears", runner.state().clone(), squandered, river));
    }

    for luck in [0, 1] {
        let mut scenario = new_scenario();
        let squandered = with_squandered(&mut scenario);
        let caverns = scenario
            .add_land_from_oracle(P0, "Gemstone Caverns", GEMSTONE_CAVERNS)
            .id();
        scenario.with_counter(caverns, parse_counter_type("luck"), luck);
        let runner = scenario.build();
        cases.push((
            "Gemstone Caverns",
            runner.state().clone(),
            squandered,
            caverns,
        ));
    }

    for assembled in [false, true] {
        let mut scenario = new_scenario();
        let squandered = with_squandered(&mut scenario);
        let tower = scenario
            .add_land_from_oracle(P0, "Urza's Tower", URZAS_TOWER)
            .with_subtypes(vec!["Urza's", "Tower"])
            .id();
        if assembled {
            scenario
                .add_land_from_oracle(P0, "Urza's Mine", URZAS_MINE)
                .with_subtypes(vec!["Urza's", "Mine"]);
            scenario
                .add_land_from_oracle(P0, "Urza's Power Plant", URZAS_POWER_PLANT)
                .with_subtypes(vec!["Urza's", "Power-Plant"]);
        }
        let runner = scenario.build();
        cases.push(("Urza's Tower", runner.state().clone(), squandered, tower));
    }

    for assembled in [false, true] {
        let (mut scenario, mine) = tron_scenario(assembled);
        let squandered = with_squandered(&mut scenario);
        scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
        let runner = scenario.build();
        cases.push((
            "Urza's Mine under Contamination",
            runner.state().clone(),
            squandered,
            mine,
        ));
    }

    for exiled in [false, true] {
        let mut scenario = new_scenario();
        let squandered = with_squandered(&mut scenario);
        let labyrinth = scenario
            .add_land_from_oracle(P0, "Ugin's Labyrinth", UGINS_LABYRINTH)
            .id();
        let mut runner = scenario.build();
        if exiled {
            let card = scenario_exiled_card(&mut runner);
            runner.state_mut().exile_links.push(ExileLink {
                exiled_id: card,
                source_id: labyrinth,
                kind: ExileLinkKind::TrackedBySource,
            });
        }
        cases.push((
            "Ugin's Labyrinth",
            runner.state().clone(),
            squandered,
            labyrinth,
        ));
    }

    for (name, state, squandered, land) in &cases {
        let mut reading = squandered_reading(state, *squandered, *land);
        reading.sort();
        assert_eq!(
            reading,
            actual_types(state, *land),
            "{name}: the hypothetical reading must equal the real activation"
        );
    }
}

/// A card in exile for a Labyrinth to have exiled.
fn scenario_exiled_card(runner: &mut GameRunner) -> ObjectId {
    engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(9_597),
        P0,
        "Exiled Colorless Card".to_string(),
        Zone::Exile,
    )
}

// ---- Hypothetical resolutions the class reader reads (CR 106.7) ----------

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const ASHAYA: &str = "Ashaya's power and toughness are each equal to the number of lands you \
     control.\nNontoken creatures you control are Forest lands in addition to their other types.";
const SOULBRIGHT_SEEKER: &str = "As an additional cost to cast this spell, behold an Elemental or \
     pay {2}.\n{R}: Target creature you control gains trample until end of turn. If this is the \
     third time this ability has resolved this turn, add {R}{R}{R}{R}.";
const OMNATH_LOCUS_OF_ALL: &str = "If you would lose unspent mana, that mana becomes black \
     instead.\nAt the beginning of your first main phase, look at the top card of your library. \
     You may reveal that card if it has three or more colored mana symbols in its mana cost. If \
     you do, add three mana in any combination of its colors and put it into your hand. If you \
     don't reveal it, put it into your hand.";
const COURSER_OF_KRUPHIX: &str = "Play with the top card of your library revealed.\nYou may play \
     lands from the top of your library.\nLandfall — Whenever a land you control enters, you \
     gain 1 life.";

/// Applies the layer system, which `GameScenario::build` does not run.
fn flush(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    flush_layers(runner.state_mut());
}

/// Reach-guard: Ashaya made `object` a land.
fn assert_is_land(runner: &GameRunner, object: ObjectId) {
    assert!(
        runner.state().objects[&object]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "reach-guard: Ashaya made it a land"
    );
}

/// T30′-pool (CR 106.7 + CR 608.2c): Reflecting Pool reads the Seeker's ordinal
/// against its next resolution — {R} only after exactly two real resolutions.
#[test]
fn reflecting_pool_reads_a_seekers_ordinal_against_its_next_resolution() {
    for prior in 0..=3u32 {
        let mut scenario = new_scenario();
        let reflecting_pool = scenario
            .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
            .id();
        scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
        let seeker = scenario
            .add_creature_from_oracle(P0, "Soulbright Seeker", 2, 1, SOULBRIGHT_SEEKER)
            .id();
        let mut runner = scenario.build();
        flush(&mut runner);
        for _ in 0..prior {
            runner.state_mut().players[P0.0 as usize]
                .mana_pool
                .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
            runner.activate(seeker, 0).target_object(seeker).resolve();
        }
        flush(&mut runner);
        assert_eq!(
            runner
                .state()
                .ability_resolutions_this_turn
                .get(&(seeker, 0))
                .copied(),
            (prior > 0).then_some(prior),
            "reach-guard: the Seeker's resolution ledger"
        );
        assert_is_land(&runner, seeker);

        let expected = if prior == 2 {
            Produced::Prompt(vec![ManaType::Red, ManaType::Green])
        } else {
            Produced::Pool(vec![ManaType::Green])
        };
        assert_eq!(
            tap_for_mana(&mut runner, reflecting_pool),
            expected,
            "{prior} prior resolutions"
        );
    }
}

/// Reflecting Pool beside Ashaya and Omnath, Locus of All (+ Courser of
/// Kruphix when `courser`), with a card costing `shards` + `generic` on top of
/// a filler card. Returns the runner and the Pool.
fn omnath_pool_board(
    courser: bool,
    shards: Vec<ManaCostShard>,
    generic: u32,
) -> (GameRunner, ObjectId) {
    let mut scenario = new_scenario();
    let reflecting_pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    if courser {
        scenario.add_creature_from_oracle(P0, "Courser of Kruphix", 2, 4, COURSER_OF_KRUPHIX);
    }
    let omnath = scenario
        .add_creature_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH_LOCUS_OF_ALL)
        .id();
    scenario.add_spell_to_library_top(P0, "Filler", false);
    scenario
        .add_spell_to_library_top(P0, "Top Card", false)
        .with_mana_cost(ManaCost::Cost { shards, generic });
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, omnath);
    (runner, reflecting_pool)
}

/// T32b–T32e (CR 106.7 + CR 202.2c + CR 608.2c): with the top card public,
/// Reflecting Pool reads Omnath's look exactly as Squandered Resources does —
/// an eligible card's colors, nothing of an ineligible or colorless one —
/// beside every land's intrinsic Forest {G}.
#[test]
fn reflecting_pool_reads_omnaths_public_top_card() {
    let cases = [
        (
            "Doomsday",
            vec![ManaCostShard::Black; 3],
            0,
            Produced::Prompt(vec![ManaType::Black, ManaType::Green]),
        ),
        (
            "Esper Charm",
            vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            0,
            Produced::Prompt(vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Green,
            ]),
        ),
        (
            "Dimir Charm",
            vec![ManaCostShard::Blue, ManaCostShard::Black],
            0,
            Produced::Pool(vec![ManaType::Green]),
        ),
        ("Sol Ring", vec![], 1, Produced::Pool(vec![ManaType::Green])),
    ];
    for (name, shards, generic, expected) in cases {
        let (mut runner, reflecting_pool) = omnath_pool_board(true, shards, generic);
        assert_eq!(
            tap_for_mana(&mut runner, reflecting_pool),
            expected,
            "{name} on top"
        );
    }
}

/// T33b (CR 400.2 + CR 401.2): without Courser the top card is hidden, so the
/// Pool does not read it. Paired positive: the Doomsday row of T32b.
#[test]
fn reflecting_pool_does_not_read_omnaths_hidden_top_card() {
    let (mut runner, reflecting_pool) = omnath_pool_board(false, vec![ManaCostShard::Black; 3], 0);
    let top = runner.state().players[P0.0 as usize].library[0];
    assert_eq!(
        runner.state().objects[&top].color,
        vec![ManaColor::Black],
        "reach-guard: Doomsday is on top"
    );
    assert_eq!(
        tap_for_mana(&mut runner, reflecting_pool),
        Produced::Pool(vec![ManaType::Green])
    );
}

// ---- One mana ability's tap-for-mana context (CR 106.12 + CR 106.12b) ----

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const MANA_REFLECTION: &str =
    "If you tap a permanent for mana, it produces twice as much of that mana instead.";
const VORINCLEX_VOICE_OF_HUNGER: &str = "Trample\nWhenever you tap a land for mana, add one mana \
     of any type that land produced.\nWhenever an opponent taps a land for mana, that land \
     doesn't untap during its controller's next untap step.";
const METALWORKER: &str = "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for \
     each card revealed this way.";
const EMPOWERED_AUTOGENERATOR: &str = "This artifact enters tapped.\n{T}: Put a charge counter on \
     this artifact. Add X mana of any one color, where X is the number of charge counters on this \
     artifact.";
const MARCH_OF_THE_MACHINES: &str = "Each noncreature artifact is an artifact creature with power \
     and toughness each equal to its mana value.";
const WILD_CANTOR: &str = "Sacrifice this creature: Add one mana of any color.";
const SHAUN_AND_REBECCA_AGENTS: &str = "Vigilance\nWhen Shaun & Rebecca enters, search your \
     graveyard, hand, and library for a card named The Animus and put it onto the battlefield, \
     then shuffle.\n{T}: Add {C}. When you do, mill two cards.";
const MAGUS_OF_THE_VINEYARD: &str =
    "At the beginning of each player's first main phase, that player adds {G}{G}.";

/// CR 106.12 + CR 106.12b + CR 614.15: an assembled Urza's Mine selects {C}{C}
/// as its whole tap production. Contamination governs the resulting mana type.
#[test]
fn tapping_assembled_urzas_mine_under_contamination_adds_only_black() {
    let (scenario, mine) = tron_scenario(true);
    let mut runner = scenario.build();
    assert_eq!(
        tap_for_mana(&mut runner, mine),
        Produced::Pool(vec![ManaType::Colorless, ManaType::Colorless]),
        "control: the assembled Mine selects its two-mana production"
    );

    let (mut scenario, mine) = tron_scenario(true);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let mut runner = scenario.build();
    let Produced::Pool(produced) = tap_for_mana(&mut runner, mine) else {
        panic!("the Mine produces mana without a type prompt");
    };
    assert!(!produced.is_empty(), "the Mine produced mana");
    assert!(
        produced
            .iter()
            .all(|mana_type| *mana_type == ManaType::Black),
        "CR 106.12b: only black mana reaches the pool, got {produced:?}"
    );
}

/// CR 106.12 + CR 106.12b: Mana Reflection doubles the selected production
/// of an assembled or unassembled Urza's Mine to four or two {C}.
#[test]
fn tapping_urzas_mine_under_mana_reflection_doubles_the_selected_production() {
    for (assembled, colorless) in [(false, 2), (true, 4)] {
        let (mut scenario, mine) = tron_scenario(assembled);
        scenario.add_enchantment_from_oracle(P0, "Mana Reflection", MANA_REFLECTION);
        let mut runner = scenario.build();
        assert_eq!(
            tap_for_mana(&mut runner, mine),
            Produced::Pool(vec![ManaType::Colorless; colorless]),
            "assembled: {assembled}"
        );
    }
}

/// F3-3 (CR 106.7 + CR 106.12b): the hypothetical resolution of Urza's Mine
/// reads every production of its {T} mana ability as tapped for mana, so under
/// Contamination the sacrificed Mine could produce only {B}, whether its
/// "instead" link runs (assembled) or not.
#[test]
fn sacrificed_urzas_mine_under_contamination_offers_only_black() {
    for assembled in [false, true] {
        let (mut scenario, mine) = tron_scenario(assembled);
        let squandered = scenario
            .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
            .id();
        scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
        let runner = scenario.build();
        assert_eq!(
            squandered_reading(runner.state(), squandered, mine),
            vec![ManaType::Black],
            "assembled: {assembled}"
        );
    }
}

/// F3-4 (CR 106.7 + CR 106.12b): the "could produce" census over P0's lands
/// reads Tron under Contamination as {B} alone, assembled or not. Naga Vitalist
/// (a creature, so Contamination leaves its own production alone) adds the
/// census's one type, and Reflecting Pool, offered one type, adds it without a
/// prompt.
#[test]
fn pool_census_reads_assembled_tron_under_contamination_as_black() {
    for assembled in [false, true] {
        let (mut scenario, _mine) = tron_scenario(assembled);
        let reflecting_pool = scenario
            .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
            .id();
        scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
        let vitalist = scenario
            .add_creature_from_oracle(P0, "Naga Vitalist", 1, 2, NAGA_VITALIST)
            .id();
        let runner = scenario.build();
        let vitalist_object = &runner.state().objects[&vitalist];
        assert!(
            !vitalist_object
                .card_types
                .core_types
                .contains(&CoreType::Land)
                && !vitalist_object.tapped
                && vitalist_object.controller == P0,
            "reach-guard: Naga Vitalist is P0's untapped nonland creature"
        );
        let pool_object = &runner.state().objects[&reflecting_pool];
        assert!(
            pool_object.card_types.core_types.contains(&CoreType::Land)
                && pool_object.controller == P0,
            "reach-guard: Reflecting Pool is P0's land"
        );

        for (name, reader) in [
            ("Naga Vitalist", vitalist),
            ("Reflecting Pool", reflecting_pool),
        ] {
            let mut read = GameRunner::from_state(runner.state().clone());
            assert_eq!(
                tap_for_mana(&mut read, reader),
                Produced::Pool(vec![ManaType::Black]),
                "{name}, assembled: {assembled}"
            );
        }
    }
}

/// F3-6 (CR 106.12 + CR 106.12b): Metalworker's production follows its {T}
/// mana ability's first instruction (the reveal) and is still tapped for mana.
/// Ashaya makes Metalworker a Forest land for Squandered Resources to
/// sacrifice; under Contamination it could produce only {B}.
#[test]
fn sacrificed_metalworker_under_contamination_offers_only_black() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER)
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, metalworker);

    assert_eq!(
        squandered_reading(runner.state(), squandered, metalworker),
        vec![ManaType::Black]
    );
}

/// F3-7 (CR 106.12 + CR 106.12b): Empowered Autogenerator's production follows
/// an inline counter instruction of the same {T} mana ability and is still
/// tapped for mana. March of the Machines makes the Autogenerator a creature
/// and Ashaya makes it a Forest land; under Contamination it could produce only
/// {B}. One charge counter keeps the production's amount positive.
#[test]
fn sacrificed_charged_autogenerator_under_contamination_offers_only_black() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES);
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let autogenerator = scenario
        .add_artifact_from_oracle(P0, "Empowered Autogenerator", EMPOWERED_AUTOGENERATOR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    scenario.with_counter(autogenerator, parse_counter_type("charge"), 1);
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, autogenerator);
    assert_eq!(
        runner.state().objects[&autogenerator]
            .counters
            .get(&parse_counter_type("charge"))
            .copied(),
        Some(1),
        "reach-guard: the Autogenerator has one charge counter"
    );

    assert_eq!(
        squandered_reading(runner.state(), squandered, autogenerator),
        vec![ManaType::Black]
    );
}

/// CR 106.12a: Vorinclex, Voice of Hunger's echo triggers once per resolution
/// of a tap mana ability. An assembled Urza's Mine adds {C}{C} and the echo adds {C}.
#[test]
fn vorinclex_echoes_an_assembled_urzas_mine_once() {
    let (mut scenario, mine) = tron_scenario(true);
    let vorinclex = scenario
        .add_creature_from_oracle(
            P0,
            "Vorinclex, Voice of Hunger",
            7,
            6,
            VORINCLEX_VOICE_OF_HUNGER,
        )
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&vorinclex].controller,
        P0,
        "reach-guard: P0 controls Vorinclex"
    );

    assert_eq!(
        tap_for_mana(&mut runner, mine),
        Produced::Pool(vec![ManaType::Colorless; 3])
    );
}

/// CR 106.12b + CR 605.3b: Contamination modifies the Mine's tap production
/// as the ability resolves. The engine's manual-tap undo refunds all mana
/// deposited by that activation.
#[test]
fn untapping_an_assembled_urzas_mine_refunds_the_whole_replacement() {
    let (mut scenario, mine) = tron_scenario(true);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let mut runner = scenario.build();
    let Produced::Pool(produced) = tap_for_mana(&mut runner, mine) else {
        panic!("the Mine produces mana without a type prompt");
    };
    assert!(!produced.is_empty(), "the Mine produced mana");
    assert!(
        runner
            .state()
            .lands_tapped_for_mana
            .get(&P0)
            .is_some_and(|lands| lands.contains(&mine)),
        "the Mine's manual tap is recorded for undo"
    );

    runner
        .act(GameAction::UntapLandForMana { object_id: mine })
        .expect("undoing the Mine's manual tap");
    assert!(
        pool(&runner, P0).is_empty(),
        "the Mine's replaced production was refunded"
    );
    assert!(
        !runner.state().objects[&mine].tapped,
        "the Mine is untapped"
    );
}

/// F3-10 (CR 106.12): Wild Cantor's mana ability has no {T} in its cost, so its
/// production is not tapped for mana and Contamination leaves its five colors
/// alone; a control Forest on the same board, tapped for mana, gives {B}.
#[test]
fn sacrificed_wild_cantor_under_contamination_keeps_every_color() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let cantor = scenario
        .add_creature_from_oracle(P0, "Wild Cantor", 1, 1, WILD_CANTOR)
        .id();
    let control_forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, cantor);
    let mut control = GameRunner::from_state(runner.state().clone());
    assert_eq!(
        tap_for_mana(&mut control, control_forest),
        Produced::Pool(vec![ManaType::Black]),
        "reach-guard: Contamination rewrites a Forest tapped for mana"
    );

    let mut reading = squandered_reading(runner.state(), squandered, cantor);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]
    );
}

/// F3-11 (CR 603.12 + CR 106.12b): Shaun & Rebecca, Agents' "When you do, mill
/// two cards" is a reflexive triggered ability, separate from the {T} mana
/// ability whose resolution created it, so it is not tapped for mana. The mana
/// ability resolves at once (CR 605.3b) and the trigger waits on the stack.
#[test]
fn shaun_and_rebeccas_reflexive_mill_is_not_tapped_for_mana() {
    let mut scenario = new_scenario();
    let shaun = scenario
        .add_creature_from_oracle(
            P0,
            "Shaun & Rebecca, Agents",
            4,
            4,
            SHAUN_AND_REBECCA_AGENTS,
        )
        .id();
    let mut runner = scenario.build();
    let source = &runner.state().objects[&shaun];
    assert!(
        !source.tapped && !source.summoning_sick,
        "reach-guard: Shaun & Rebecca can tap for mana"
    );
    let ability_index = source
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: Shaun & Rebecca has an activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: shaun,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: the mana ability is activatable");

    assert_eq!(
        pool(&runner, P0),
        vec![ManaType::Colorless],
        "reach-guard: the mana ability resolved at once"
    );
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "reach-guard: priority returns with the trigger on the stack"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "reach-guard: the reflexive trigger is on the stack"
    );
    let StackEntryKind::TriggeredAbility {
        source_id, ability, ..
    } = &state.stack[0].kind
    else {
        panic!(
            "expected the reflexive trigger, got {:?}",
            state.stack[0].kind
        );
    };
    assert_eq!(*source_id, shaun, "reach-guard: Shaun & Rebecca's trigger");
    assert!(
        matches!(ability.effect, Effect::Mill { .. }),
        "reach-guard: the trigger mills"
    );
    assert_eq!(
        ability.context.mana_tap_state,
        ManaTapState::NotFromTap,
        "CR 603.12: the reflexive ability is not the mana ability, so not tapped for mana"
    );
}

/// F3-12 (CR 106.12): Magus of the Vineyard's {G}{G} comes from a triggered
/// ability, which is not activated and so never tapped for mana: under
/// Contamination the sacrificed Magus could still produce {G}, beside the {B}
/// its Forest ability (from Ashaya) gives.
#[test]
fn sacrificed_magus_of_the_vineyard_under_contamination_keeps_its_triggered_green() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let magus = scenario
        .add_creature_from_oracle(P0, "Magus of the Vineyard", 1, 1, MAGUS_OF_THE_VINEYARD)
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, magus);
    assert_eq!(
        runner.state().objects[&magus].trigger_definitions.len(),
        1,
        "reach-guard: Magus carries its one triggered ability"
    );

    let mut reading = squandered_reading(runner.state(), squandered, magus);
    reading.sort();
    assert_eq!(reading, vec![ManaType::Black, ManaType::Green]);
}

// ---- A repeated mana instruction and the set a reveal publishes (CR 608.2c + CR 701.20a) ----

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const SELVALA_EXPLORER_RETURNED: &str = "Parley — {T}: Each player reveals the top card of their \
     library. For each nonland card revealed this way, add {G} and you gain 1 life. Then each \
     player draws a card.";

/// What a Selvala board puts on top of one player's library.
#[derive(Clone, Copy)]
enum LibraryTop {
    Land,
    Nonland,
}

/// A Selvala board's runner and the objects its rows read.
struct SelvalaBoard {
    runner: GameRunner,
    squandered: ObjectId,
    reflecting_pool: ObjectId,
    selvala: ObjectId,
    /// The staged library tops, in the order `tops` named them.
    tops: Vec<ObjectId>,
}

/// Squandered Resources, Reflecting Pool, Ashaya, Soul of the Wild and
/// Contamination beside P0's Selvala, Explorer Returned, with a Courser of
/// Kruphix for each player in `coursers` and each `(player, top)` of `tops`
/// on top of that player's library; a player `tops` does not name keeps an
/// empty library.
fn selvala_board(coursers: &[PlayerId], tops: &[(PlayerId, LibraryTop)]) -> SelvalaBoard {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    let reflecting_pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    for &player in coursers {
        scenario.add_creature_from_oracle(player, "Courser of Kruphix", 2, 4, COURSER_OF_KRUPHIX);
    }
    let selvala = scenario
        .add_creature_from_oracle(
            P0,
            "Selvala, Explorer Returned",
            2,
            4,
            SELVALA_EXPLORER_RETURNED,
        )
        .id();
    let staged_tops: Vec<ObjectId> = tops
        .iter()
        .map(|&(player, top)| match top {
            LibraryTop::Land => scenario.add_land_to_library_top(player, "Land Top").id(),
            LibraryTop::Nonland => scenario
                .add_spell_to_library_top(player, "Nonland Top", false)
                .id(),
        })
        .collect();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, selvala);
    let parley = runner.state().objects[&selvala]
        .abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: Selvala has its Parley ability");
    let repeated = parley
        .sub_ability
        .as_deref()
        .expect("reach-guard: Parley goes on after its reveal");
    assert!(
        matches!(*repeated.effect, Effect::Mana { .. }) && repeated.repeat_for.is_some(),
        "reach-guard: Selvala's Parley carries the repeated {{G}}"
    );
    SelvalaBoard {
        runner,
        squandered,
        reflecting_pool,
        selvala,
        tops: staged_tops,
    }
}

/// F4-1 (CR 608.2c + CR 106.7 + CR 106.12b): over two empty libraries Selvala,
/// Explorer Returned's Parley reveals nothing, so "for each nonland card
/// revealed this way, add {G}" is performed zero times and could add no mana.
/// Its Forest ability (from Ashaya) is tapped for mana, so under Contamination
/// the sacrificed Selvala could produce only {B}. Paired positive: F4-4.
#[test]
fn sacrificed_selvala_over_empty_libraries_offers_only_black() {
    let board = selvala_board(&[], &[]);
    let state = board.runner.state();
    assert!(
        state.library_of(P0).is_empty() && state.library_of(P1).is_empty(),
        "reach-guard: both libraries are empty"
    );
    assert_eq!(
        squandered_reading(state, board.squandered, board.selvala),
        vec![ManaType::Black]
    );
}

/// F4-2 (CR 608.2c + CR 701.20a + CR 401.5): with a Courser of Kruphix for
/// each player, both library tops are revealed to every seat, and both are
/// land cards. Parley's reveal would show exactly those cards, so its count of
/// nonland cards revealed this way is zero and Selvala, Explorer Returned
/// could produce only the {B} Contamination makes of its Forest ability.
/// Paired positive: F4-4.
#[test]
fn sacrificed_selvala_over_public_land_tops_offers_only_black() {
    let board = selvala_board(&[P0, P1], &[(P0, LibraryTop::Land), (P1, LibraryTop::Land)]);
    let state = board.runner.state();
    let continuous = continuously_revealed_cards(state);
    for top in &board.tops {
        assert!(
            state.objects[top]
                .card_types
                .core_types
                .contains(&CoreType::Land),
            "reach-guard: each library top is a land card"
        );
        assert!(
            continuous.contains(top),
            "reach-guard: each library top is revealed by its owner's Courser"
        );
    }
    assert_eq!(
        squandered_reading(state, board.squandered, board.selvala),
        vec![ManaType::Black]
    );
}

/// F4-3 (CR 400.2 + CR 401.2 + CR 608.2c): a library top no Courser of
/// Kruphix reveals is hidden, so Parley's reveal is not known to every seat
/// and its count is not read: Selvala, Explorer Returned's {G} stays admitted
/// beside its {B}, both with no Courser and with only P0's Courser (P0's top
/// public, P1's hidden). Fixed sibling: F4-2, the same land tops made public.
#[test]
fn sacrificed_selvala_over_hidden_tops_offers_green() {
    let cases: [(&[PlayerId], bool); 2] = [(&[], false), (&[P0], true)];
    for (coursers, p0_revealed) in cases {
        let board = selvala_board(coursers, &[(P0, LibraryTop::Land), (P1, LibraryTop::Land)]);
        let state = board.runner.state();
        let continuous = continuously_revealed_cards(state);
        assert_eq!(
            continuous.contains(&board.tops[0]),
            p0_revealed,
            "reach-guard: P0's top is revealed exactly when P0 has a Courser"
        );
        assert!(
            !continuous.contains(&board.tops[1]),
            "reach-guard: P1's top is never revealed"
        );
        let mut reading = squandered_reading(state, board.squandered, board.selvala);
        reading.sort();
        assert_eq!(
            reading,
            vec![ManaType::Black, ManaType::Green],
            "Coursers: {coursers:?}"
        );
    }
}

/// F4-4 (CR 608.2c + CR 701.20a): with a Courser of Kruphix for each player
/// and a nonland card on top of P0's library, Parley's reveal would show one
/// nonland card, so "for each nonland card revealed this way, add {G}" is
/// performed once. Parley is not a mana ability, so its {G} is not tapped for
/// mana (CR 106.12) and Contamination leaves it {G}, beside the {B} it makes
/// of Selvala, Explorer Returned's Forest ability.
#[test]
fn sacrificed_selvala_over_a_public_nonland_top_offers_green() {
    let board = selvala_board(
        &[P0, P1],
        &[(P0, LibraryTop::Nonland), (P1, LibraryTop::Land)],
    );
    let state = board.runner.state();
    let continuous = continuously_revealed_cards(state);
    assert!(
        board.tops.iter().all(|top| continuous.contains(top)),
        "reach-guard: both library tops are revealed"
    );
    assert!(
        !state.objects[&board.tops[0]]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "reach-guard: P0's top is a nonland card"
    );
    let mut reading = squandered_reading(state, board.squandered, board.selvala);
    reading.sort();
    assert_eq!(reading, vec![ManaType::Black, ManaType::Green]);
}

/// F4-5 (CR 106.7 + CR 106.12b): Reflecting Pool reads every land P0 controls,
/// Selvala, Explorer Returned among them (a Forest under Ashaya). Over two
/// empty libraries Parley's {G} is performed zero times, so each of those
/// lands could produce only {B} under Contamination and the Pool offers that
/// one type without a prompt.
#[test]
fn reflecting_pool_reads_selvala_over_empty_libraries_as_black() {
    let mut board = selvala_board(&[], &[]);
    let state = board.runner.state();
    assert!(
        state.library_of(P0).is_empty() && state.library_of(P1).is_empty(),
        "reach-guard: both libraries are empty"
    );
    let pool_land = &state.objects[&board.reflecting_pool];
    assert!(
        pool_land.controller == P0 && pool_land.card_types.core_types.contains(&CoreType::Land),
        "reach-guard: the Pool is a land P0 controls"
    );
    assert_eq!(
        tap_for_mana(&mut board.runner, board.reflecting_pool),
        Produced::Pool(vec![ManaType::Black])
    );
}

/// F4-6 (CR 608.2c + CR 701.20a): Selvala, Explorer Returned's real
/// resolution. Parley reveals each player's top card; both are land cards, so
/// "for each nonland card revealed this way, add {G} and you gain 1 life" is
/// performed zero times: no {G} and no life. Paired positive:
/// `issue_841_selvala_explorer_returned.rs`, where one nonland reveal adds one
/// {G} and 1 life.
#[test]
fn selvala_revealing_only_lands_adds_no_green() {
    let mut scenario = new_scenario();
    let selvala = scenario
        .add_creature_from_oracle(
            P0,
            "Selvala, Explorer Returned",
            2,
            4,
            SELVALA_EXPLORER_RETURNED,
        )
        .id();
    let p0_top = scenario.add_land_to_library_top(P0, "Land Top").id();
    let p1_top = scenario.add_land_to_library_top(P1, "Land Top").id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let outcome = runner.activate(selvala, 0).resolve();
    for (player, top) in [(P0, p0_top), (P1, p1_top)] {
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::CardsRevealed { player: revealer, card_ids, .. }
                    if *revealer == player && *card_ids == [top]
            )),
            "reach-guard: {player:?} revealed the top card of their library"
        );
    }
    runner.advance_until_stack_empty();

    let state = runner.state();
    let controller = &state.players[P0.0 as usize];
    assert_eq!(
        controller.mana_pool.count_color(ManaType::Green),
        0,
        "no {{G}} for zero nonland cards revealed"
    );
    assert_eq!(
        controller.life, life_before,
        "no life for zero nonland cards revealed"
    );
    assert!(state.stack.is_empty(), "the stack is empty");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the resolution completed and priority returned"
    );
}

// ---- Written-order state before a later mana amount (CR 608.2c) ----

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const SOLEMNITY: &str = "Players can't get counters.\nCounters can't be put on artifacts, \
     creatures, enchantments, or lands.";
const CHEERING_CROWD: &str = "At the beginning of each player's first main phase, that player \
     may put a +1/+1 counter on this creature. If they do, they add {C} for each counter on it.";
const MOLTEN_CORE_MAESTRO: &str = "Menace\nOpus — Whenever you cast an instant or sorcery \
     spell, put a +1/+1 counter on this creature. If five or more mana was spent to cast that \
     spell, add an amount of {R} equal to this creature's power.";
const WEAKNESS: &str = "Enchant creature\nEnchanted creature gets -2/-1.";
const DIABOLIC_EDICT: &str = "Target player sacrifices a creature of their choice.";

/// An Empowered Autogenerator board's runner and the objects its rows read.
struct AutogeneratorBoard {
    runner: GameRunner,
    squandered: ObjectId,
    reflecting_pool: ObjectId,
    autogenerator: ObjectId,
}

/// Squandered Resources, Reflecting Pool, March of the Machines and Ashaya,
/// Soul of the Wild beside P0's Empowered Autogenerator ({4}) holding
/// `charge_counters` charge counters, plus Solemnity when `solemnity`. March
/// of the Machines makes the Autogenerator a creature and Ashaya makes it a
/// Forest land (CR 613.8: Ashaya's effect depends on March's).
fn autogenerator_board(charge_counters: u32, solemnity: bool) -> AutogeneratorBoard {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    let reflecting_pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES);
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    if solemnity {
        scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY);
    }
    let autogenerator = scenario
        .add_artifact_from_oracle(P0, "Empowered Autogenerator", EMPOWERED_AUTOGENERATOR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    scenario.with_counter(autogenerator, parse_counter_type("charge"), charge_counters);
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, autogenerator);
    let object = &runner.state().objects[&autogenerator];
    assert_eq!(
        object
            .counters
            .get(&parse_counter_type("charge"))
            .copied()
            .unwrap_or(0),
        charge_counters,
        "reach-guard: the Autogenerator's initial charge counters"
    );
    assert!(
        !object.tapped && !object.summoning_sick,
        "reach-guard: the Autogenerator can tap for mana"
    );
    let printed = object
        .abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: the Autogenerator has its activated ability");
    assert!(
        is_mana_ability(printed) && inline_mana_preamble(printed).is_some(),
        "reach-guard: its first activated ability is a mana ability whose production follows \
         an inline counter instruction"
    );
    AutogeneratorBoard {
        runner,
        squandered,
        reflecting_pool,
        autogenerator,
    }
}

/// A Cheering Crowd board's runner and the objects its rows read.
struct CheeringCrowdBoard {
    runner: GameRunner,
    squandered: ObjectId,
    crowd: ObjectId,
}

/// Squandered Resources and Ashaya, Soul of the Wild beside P0's Cheering
/// Crowd with no counters, which Ashaya makes a Forest land, plus Solemnity
/// when `solemnity`, added to `scenario` (which may already hold a card a row
/// casts first).
fn cheering_crowd_board(mut scenario: GameScenario, solemnity: bool) -> CheeringCrowdBoard {
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    if solemnity {
        scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY);
    }
    let crowd = scenario
        .add_creature_from_oracle(P0, "Cheering Crowd", 2, 2, CHEERING_CROWD)
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, crowd);
    let object = &runner.state().objects[&crowd];
    assert!(
        object.counters.values().all(|count| *count == 0),
        "reach-guard: Cheering Crowd has no counters"
    );
    assert_eq!(
        object.trigger_definitions.len(),
        1,
        "reach-guard: Cheering Crowd carries its one triggered ability"
    );
    let execute = object.trigger_definitions[0]
        .definition
        .execute
        .as_deref()
        .expect("reach-guard: the trigger has an effect");
    assert!(
        execute.optional
            && execute.effect.is_decision_free_state_instruction()
            && execute.sub_ability.as_deref().is_some_and(|sub| {
                matches!(*sub.effect, Effect::Mana { .. })
                    && matches!(
                        sub.condition,
                        Some(AbilityCondition::EffectOutcome {
                            signal: EffectOutcomeSignal::OptionalEffectPerformed,
                        })
                    )
            }),
        "reach-guard: \"that player may put a +1/+1 counter\" is an optional counter \
         instruction followed by its mana, gated on \"If they do\""
    );
    CheeringCrowdBoard {
        runner,
        squandered,
        crowd,
    }
}

/// F1-1 (CR 608.2c + CR 106.7): Empowered Autogenerator's mana ability puts a
/// charge counter on it before "Add X mana of any one color, where X is the
/// number of charge counters on this artifact.", so with no charge counter X is
/// one and it could produce every color. March of the Machines and Ashaya, Soul
/// of the Wild make it a Forest land, so its Forest ability adds {G} too.
#[test]
fn sacrificed_autogenerator_at_zero_charge_counters_offers_every_color() {
    let board = autogenerator_board(0, false);
    let mut reading =
        squandered_reading(board.runner.state(), board.squandered, board.autogenerator);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]
    );
}

/// F1-2 (CR 608.2c + CR 106.7): with two charge counters the Autogenerator's X
/// is three, and it could produce every color.
#[test]
fn sacrificed_autogenerator_with_charge_counters_offers_every_color() {
    let board = autogenerator_board(2, false);
    let mut reading =
        squandered_reading(board.runner.state(), board.squandered, board.autogenerator);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]
    );
}

/// F1-3 (CR 608.2c + CR 106.5): under Solemnity ("Counters can't be put on
/// artifacts, creatures, enchantments, or lands.") the Autogenerator's charge
/// counter is never placed, so X stays at its initial count. With none, its
/// mana ability adds no mana and only its Forest ability's {G} remains. Paired
/// positives: without Solemnity the same board reads every color, and with
/// Solemnity an Autogenerator holding two charge counters still reads every
/// color.
#[test]
fn solemnity_keeps_a_zero_counter_autogenerator_from_producing() {
    let board = autogenerator_board(0, false);
    let mut reading =
        squandered_reading(board.runner.state(), board.squandered, board.autogenerator);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
        "reach-guard: without Solemnity the projected counter makes X one"
    );

    let board = autogenerator_board(2, true);
    let mut reading =
        squandered_reading(board.runner.state(), board.squandered, board.autogenerator);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ],
        "reach-guard: under Solemnity the Autogenerator's production is still read, with X at two"
    );

    let board = autogenerator_board(0, true);
    assert_eq!(
        squandered_reading(board.runner.state(), board.squandered, board.autogenerator),
        vec![ManaType::Green]
    );
}

/// F1-4 (CR 106.7 + CR 608.2c): Reflecting Pool reads every land P0 controls,
/// the Autogenerator among them, so with no charge counter or with two it
/// offers every color.
#[test]
fn reflecting_pool_reads_an_autogenerator_at_zero_and_positive_counters() {
    for charge_counters in [0, 2] {
        let mut board = autogenerator_board(charge_counters, false);
        let state = board.runner.state();
        let pool_land = &state.objects[&board.reflecting_pool];
        assert!(
            pool_land.controller == P0 && pool_land.card_types.core_types.contains(&CoreType::Land),
            "reach-guard: the Pool is a land P0 controls"
        );
        assert_eq!(
            tap_for_mana(&mut board.runner, board.reflecting_pool),
            Produced::Prompt(vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]),
            "{charge_counters} charge counters"
        );
    }
}

/// F1-5 (CR 608.2c + CR 605.3b): the Autogenerator's real activation puts its
/// charge counter on first and only then offers a color for X, so even with no
/// charge counter it offers every color, with one counter on it at the prompt.
/// The runtime reference for F1-1 and F1-2.
#[test]
fn tapping_a_forest_autogenerator_places_its_counter_before_offering_every_color() {
    for charge_counters in [0, 2] {
        let mut board = autogenerator_board(charge_counters, false);
        assert_eq!(
            tap_for_mana(&mut board.runner, board.autogenerator),
            Produced::Prompt(vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ])
        );
        assert_eq!(
            board.runner.state().objects[&board.autogenerator]
                .counters
                .get(&parse_counter_type("charge"))
                .copied()
                .unwrap_or(0),
            charge_counters + 1,
            "CR 608.2c: the counter is placed before the color prompt"
        );
    }
}

/// F1-6 (CR 608.2c + CR 608.2d): Cheering Crowd's trigger is read as two
/// worlds. If that player puts the +1/+1 counter, "they add {C} for each
/// counter on it" counts it, so a Crowd with no counters could produce {C};
/// declined, the trigger adds nothing. Ashaya, Soul of the Wild makes it a
/// Forest land, so its Forest ability adds {G} too.
#[test]
fn sacrificed_cheering_crowd_offers_the_colorless_its_trigger_would_add() {
    let board = cheering_crowd_board(new_scenario(), false);
    let mut reading = squandered_reading(board.runner.state(), board.squandered, board.crowd);
    reading.sort();
    assert_eq!(reading, vec![ManaType::Green, ManaType::Colorless]);
}

/// F1-7 (CR 608.2c + CR 608.2d): under Solemnity no counter can be put on
/// Cheering Crowd (the Solemnity ruling: "If a resolving spell or ability says
/// that a player may put counters on one of those objects, that player can't
/// choose to do so."), so a Crowd with no counters has none for its trigger to
/// count; it adds no {C} and only its Forest ability's {G} remains. Paired
/// positive: the same board without Solemnity reads {C} too.
#[test]
fn solemnity_keeps_cheering_crowds_trigger_from_adding_colorless() {
    let board = cheering_crowd_board(new_scenario(), false);
    let mut reading = squandered_reading(board.runner.state(), board.squandered, board.crowd);
    reading.sort();
    assert_eq!(
        reading,
        vec![ManaType::Green, ManaType::Colorless],
        "reach-guard: without Solemnity the projected counter adds {{C}}"
    );

    let board = cheering_crowd_board(new_scenario(), true);
    assert_eq!(
        squandered_reading(board.runner.state(), board.squandered, board.crowd),
        vec![ManaType::Green]
    );
}

/// F1-8 (CR 608.2c + CR 613.4c): Molten-Core Maestro's trigger puts a +1/+1
/// counter on it before "add an amount of {R} equal to this creature's power"
/// reads its power, so a Maestro that Weakness ("Enchanted creature gets
/// -2/-1.") leaves at power 0 could produce {R}: the counter makes it 1/2. "If
/// five or more mana was spent to cast that spell" names a spell a
/// could-produce reader does not have, so that gate is read both ways. Ashaya,
/// Soul of the Wild makes it a Forest land, so its Forest ability adds {G} too.
#[test]
fn sacrificed_weakened_molten_core_maestro_offers_the_red_its_trigger_would_add() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let maestro = scenario
        .add_creature_from_oracle(P0, "Molten-Core Maestro", 2, 2, MOLTEN_CORE_MAESTRO)
        .id();
    let weakness = scenario
        .add_enchantment_from_oracle(P0, "Weakness", WEAKNESS)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    // CR 303.4: Weakness enchants the Maestro, through the engine's
    // attachment authority.
    attach_to(runner.state_mut(), weakness, maestro);
    flush(&mut runner);
    assert_is_land(&runner, maestro);
    let object = &runner.state().objects[&maestro];
    assert!(
        object.attachments.contains(&weakness)
            && object.power == Some(0)
            && object.toughness == Some(1),
        "reach-guard: Weakness (\"Enchanted creature gets -2/-1.\") leaves the 2/2 Maestro \
         at 0/1"
    );
    assert!(
        object.counters.values().all(|count| *count == 0),
        "reach-guard: the Maestro has no counters"
    );
    assert_eq!(
        object.trigger_definitions.len(),
        1,
        "reach-guard: the Maestro carries its one triggered ability"
    );
    let execute = object.trigger_definitions[0]
        .definition
        .execute
        .as_deref()
        .expect("reach-guard: the trigger has an effect");
    assert!(
        !execute.optional
            && execute.effect.is_decision_free_state_instruction()
            && execute
                .sub_ability
                .as_deref()
                .is_some_and(|sub| matches!(*sub.effect, Effect::Mana { .. })),
        "reach-guard: \"put a +1/+1 counter on this creature\" is a non-optional counter \
         instruction followed by its mana"
    );
    let mut reading = squandered_reading(runner.state(), squandered, maestro);
    reading.sort();
    assert_eq!(reading, vec![ManaType::Red, ManaType::Green]);
}

/// F1-9 (CR 608.2c + CR 608.2d): answering Cheering Crowd's "you may" clears a
/// payment failure an earlier resolution left set, so in the world where that
/// player puts the counter, "If they do" reads true and the Crowd could still
/// produce {C}. On F1-6's board, P0 first resolves Diabolic Edict at P1, who
/// controls no creature, so its sacrifice fails and leaves that failure set.
#[test]
fn sacrificed_cheering_crowd_offers_colorless_after_an_earlier_failed_sacrifice() {
    let mut scenario = new_scenario();
    // The Edict's payment is not under test: a zero cost keeps the cast from
    // tapping one of the board's Forest creatures for mana.
    let edict = scenario
        .add_spell_to_hand_from_oracle(P0, "Diabolic Edict", true, DIABOLIC_EDICT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut board = cheering_crowd_board(scenario, false);
    let outcome = board.runner.cast(edict).target_player(P1).resolve();
    assert!(
        outcome.zone_of(edict) == Zone::Graveyard
            && outcome.state().stack.is_empty()
            && matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "reach-guard: Diabolic Edict resolved and priority returned"
    );
    let state = board.runner.state();
    assert!(
        !state.battlefield.iter().any(|id| {
            let object = &state.objects[id];
            object.controller == P1 && object.card_types.core_types.contains(&CoreType::Creature)
        }),
        "reach-guard: P1 controls no creature to sacrifice"
    );
    assert!(
        state.cost_payment_failed_flag,
        "reach-guard: the sacrifice found nothing eligible and left the resolution's failure \
         signal set"
    );
    let mut reading = squandered_reading(state, board.squandered, board.crowd);
    reading.sort();
    assert_eq!(
        reading,
        vec![ManaType::Green, ManaType::Colorless],
        "CR 608.2d: answering the \"you may\" clears the earlier failure, so \"If they do\" \
         reads true"
    );
}

// ---- A color chosen from objects, or chosen elsewhere (CR 105.4 + CR 607.2d + CR 608.2d) ----

// Verbatim Oracle text (MTGJSON), reminder text omitted.
const CORRUPTED_GRAFSTONE: &str = "This artifact enters tapped.\n{T}: Choose a color of a card in \
     your graveyard. Add one mana of that color.";
const METEOR_CRATER: &str =
    "{T}: Choose a color of a permanent you control. Add one mana of that color.";
const MYR_WELDER: &str = "Imprint — {T}: Exile target artifact card from a graveyard.\nThis \
     creature has all activated abilities of all cards exiled with it.";
const HERALDIC_BANNER: &str = "As this artifact enters, choose a color.\nCreatures you control of \
     the chosen color get +1/+0.\n{T}: Add one mana of the chosen color.";
const RADIANT_LOTUS: &str = "{T}, Sacrifice one or more artifacts: Choose a color. Target player \
     adds three mana of the chosen color for each artifact sacrificed this way.";
const THRIVING_GROVE: &str = "This land enters tapped. As it enters, choose a color other than \
     green.\n{T}: Add {G} or one mana of the chosen color.";
const THE_GREY_HAVENS: &str = "When The Grey Havens enters, scry 1.\n{T}: Add {C}.\n{T}: Add one \
     mana of any color among legendary creature cards in your graveyard.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
     creature. It gains haste until end of turn.";
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";

/// A printed vanilla card the Corrupted Grafstone rows put in P0's graveyard.
#[derive(Debug, Clone, Copy)]
enum GraveyardCard {
    /// Walking Corpse: {1}{B}, black.
    WalkingCorpse,
    /// Savannah Lions: {W}, white.
    SavannahLions,
    /// Memnite: a {0} artifact creature, colorless.
    Memnite,
}

/// A Corrupted Grafstone board's runner and the objects its rows read.
struct GrafstoneBoard {
    runner: GameRunner,
    squandered: ObjectId,
    reflecting_pool: ObjectId,
    grafstone: ObjectId,
    /// The cards staged in P0's graveyard, in the order the row named them.
    staged: Vec<ObjectId>,
}

/// Squandered Resources, Reflecting Pool, March of the Machines and Ashaya,
/// Soul of the Wild beside P0's Corrupted Grafstone ({2}), with `graveyard` in
/// P0's graveyard and a red Gray Ogre in P1's. March of the Machines makes the
/// Grafstone a 2/2 artifact creature and Ashaya makes it a Forest land
/// (CR 613.8: Ashaya's effect depends on March's). The scenario builder applies
/// neither "This artifact enters tapped." nor a chosen color, which the reach
/// guards below confirm before any read.
fn grafstone_board(graveyard: &[GraveyardCard]) -> GrafstoneBoard {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    let reflecting_pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES);
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let grafstone = scenario
        .add_artifact_from_oracle(P0, "Corrupted Grafstone", CORRUPTED_GRAFSTONE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let mut staged = Vec::with_capacity(graveyard.len());
    let mut printed_colors = Vec::with_capacity(graveyard.len());
    for card in graveyard {
        let (id, color) = match card {
            GraveyardCard::WalkingCorpse => (
                scenario
                    .add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::Black],
                        generic: 1,
                    })
                    .id(),
                vec![ManaColor::Black],
            ),
            GraveyardCard::SavannahLions => (
                scenario
                    .add_creature_to_graveyard(P0, "Savannah Lions", 2, 1)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::White],
                        generic: 0,
                    })
                    .id(),
                vec![ManaColor::White],
            ),
            GraveyardCard::Memnite => (
                scenario
                    .add_creature_to_graveyard(P0, "Memnite", 1, 1)
                    .as_artifact_creature()
                    .with_subtypes(vec!["Construct"])
                    .with_mana_cost(ManaCost::zero())
                    .id(),
                vec![],
            ),
        };
        staged.push(id);
        printed_colors.push(color);
    }
    let gray_ogre = scenario
        .add_creature_to_graveyard(P1, "Gray Ogre", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, grafstone);
    let state = runner.state();
    let object = &state.objects[&grafstone];
    assert!(
        object
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Forest")
            && !object.tapped
            && !object.summoning_sick,
        "reach-guard: the Grafstone is an untapped Forest that can tap for mana"
    );
    assert_eq!(
        object.chosen_color(),
        None,
        "reach-guard: the Grafstone holds no chosen color"
    );
    for (id, color) in staged.iter().zip(&printed_colors) {
        assert_eq!(
            &state.objects[id].color, color,
            "reach-guard: each staged card has its printed color"
        );
    }
    assert!(
        state.players[P1.0 as usize].graveyard.contains(&gray_ogre)
            && state.objects[&gray_ogre].color == vec![ManaColor::Red],
        "reach-guard: P1's graveyard holds the red Gray Ogre"
    );
    GrafstoneBoard {
        runner,
        squandered,
        reflecting_pool,
        grafstone,
        staged,
    }
}

/// F2-1, F2-2 and F2-8 (CR 106.7 + CR 105.4 + CR 106.5 + CR 605.2): Corrupted
/// Grafstone ("{T}: Choose a color of a card in your graveyard. Add one mana
/// of that color.") chooses among the colors of the cards in P0's graveyard,
/// never P1's red Gray Ogre, and a colorless card has no color (the Grafstone
/// ruling: "Colorless cards have no color, so you can't add {C} this way.").
/// With no colored card it adds no mana and is still a mana ability. Its Forest
/// ability (from Ashaya, Soul of the Wild) adds {G}. Each row is read through
/// Squandered Resources' sacrifice of the Grafstone, through Reflecting Pool,
/// and through the Grafstone's own activation, whose two-color prompt is
/// answered with black.
#[test]
fn grafstone_reads_only_its_graveyard_colors() {
    let rows = [
        (
            vec![GraveyardCard::WalkingCorpse],
            vec![ManaType::Black, ManaType::Green],
            Produced::Prompt(vec![ManaType::Black, ManaType::Green]),
            Produced::Pool(vec![ManaType::Black]),
            None,
        ),
        (
            vec![GraveyardCard::WalkingCorpse, GraveyardCard::SavannahLions],
            vec![ManaType::White, ManaType::Black, ManaType::Green],
            Produced::Prompt(vec![ManaType::White, ManaType::Black, ManaType::Green]),
            Produced::Prompt(vec![ManaType::White, ManaType::Black]),
            Some(ManaType::Black),
        ),
        (
            vec![],
            vec![ManaType::Green],
            Produced::Pool(vec![ManaType::Green]),
            Produced::Pool(vec![]),
            None,
        ),
        (
            vec![GraveyardCard::Memnite],
            vec![ManaType::Green],
            Produced::Pool(vec![ManaType::Green]),
            Produced::Pool(vec![]),
            None,
        ),
    ];
    for (graveyard, squandered_types, pool_reading, grafstone_reading, answer) in rows {
        let board = grafstone_board(&graveyard);
        let state = board.runner.state();
        let mut in_graveyard: Vec<ObjectId> = state.players[P0.0 as usize]
            .graveyard
            .iter()
            .copied()
            .collect();
        in_graveyard.sort();
        let mut staged = board.staged.clone();
        staged.sort();
        assert_eq!(
            in_graveyard, staged,
            "reach-guard: P0's graveyard holds exactly {graveyard:?}"
        );

        // F2-1: what Squandered Resources' sacrifice of the Grafstone captures.
        let mut reading = squandered_reading(state, board.squandered, board.grafstone);
        reading.sort();
        assert_eq!(
            reading, squandered_types,
            "Squandered Resources, graveyard {graveyard:?}"
        );
        // After the reading, so a missing lowering fails on the reading first:
        // the Grafstone's printed ability is one production over a graveyard.
        let printed = state.objects[&board.grafstone]
            .abilities
            .iter()
            .find(|ability| ability.kind == AbilityKind::Activated)
            .expect("reach-guard: the Grafstone has its activated ability");
        assert!(
            matches!(
                &*printed.effect,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColorAmongPermanents { filter, .. },
                    ..
                } if filter.extract_in_zone() == Some(Zone::Graveyard)
            ),
            "reach-guard: the Grafstone's ability reads a graveyard, got {:?}",
            printed.effect
        );

        // F2-2: Reflecting Pool reads every land P0 controls, the Grafstone too.
        let mut read = GameRunner::from_state(state.clone());
        assert_eq!(
            tap_for_mana(&mut read, board.reflecting_pool),
            pool_reading,
            "Reflecting Pool, graveyard {graveyard:?}"
        );

        // F2-8: the Grafstone's own activation (CR 106.5 + CR 605.2).
        let mut read = GameRunner::from_state(state.clone());
        assert_eq!(
            tap_for_mana(&mut read, board.grafstone),
            grafstone_reading,
            "Corrupted Grafstone, graveyard {graveyard:?}"
        );
        assert!(
            read.state().objects[&board.grafstone].tapped,
            "reach-guard: the Grafstone's ability ran, graveyard {graveyard:?}"
        );
        if let Some(color) = answer {
            read.act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(color),
                count: 1,
            })
            .expect("the answered color is one the prompt offered");
            assert_eq!(
                pool(&read, P0),
                vec![color],
                "CR 605.3b: the chosen color arrives, graveyard {graveyard:?}"
            );
            assert!(
                read.state().stack.is_empty()
                    && matches!(read.state().waiting_for, WaitingFor::Priority { .. }),
                "the mana ability completed and priority returned, graveyard {graveyard:?}"
            );
        }
    }
}

/// F2-3 (CR 106.7 + CR 105.4 + CR 109.5): Meteor Crater ("{T}: Choose a color
/// of a permanent you control. Add one mana of that color.") chooses among the
/// colors of the permanents P0 controls: Squandered Resources' black and green
/// and Gray Ogre's red, never the white of P1's Savannah Lions. Sacrificed to
/// Squandered Resources, it could produce exactly those three colors.
#[test]
fn sacrificed_meteor_crater_offers_the_colors_of_permanents_you_control() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let meteor = scenario
        .add_land_from_oracle(P0, "Meteor Crater", METEOR_CRATER)
        .id();
    let ogre = scenario
        .add_creature(P0, "Gray Ogre", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let lions = scenario
        .add_creature(P1, "Savannah Lions", 2, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let runner = scenario.build();
    let state = runner.state();
    assert_eq!(
        state.objects[&squandered].color,
        vec![ManaColor::Black, ManaColor::Green],
        "reach-guard: Squandered Resources is black and green"
    );
    assert!(
        state.objects[&ogre].color == vec![ManaColor::Red] && state.objects[&ogre].controller == P0,
        "reach-guard: P0 controls the red Gray Ogre"
    );
    assert!(
        state.objects[&lions].color == vec![ManaColor::White]
            && state.objects[&lions].controller == P1,
        "reach-guard: P1 controls the white Savannah Lions"
    );

    let mut reading = squandered_reading(state, squandered, meteor);
    reading.sort();
    assert_eq!(
        reading,
        vec![ManaType::Black, ManaType::Red, ManaType::Green]
    );
    // After the reading, so a missing lowering fails on the reading first:
    // Meteor Crater's printed ability is one production over permanents.
    let printed = state.objects[&meteor]
        .abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: Meteor Crater has its activated ability");
    assert!(
        matches!(
            &*printed.effect,
            Effect::Mana {
                produced: ManaProduction::AnyOneColorAmongPermanents { filter, .. },
                ..
            } if filter.extract_in_zone().is_none()
        ),
        "reach-guard: Meteor Crater's ability reads the battlefield, got {:?}",
        printed.effect
    );
}

/// F2-4 (CR 607.5a + CR 106.5 + CR 106.7): Myr Welder ("This creature has all
/// activated abilities of all cards exiled with it.") imprints Heraldic Banner
/// through its real ability, so it gains "{T}: Add one mana of the chosen
/// color." without the Banner's linked "As this artifact enters, choose a
/// color." That choice is undefined (the Heraldic Banner ruling: "If there is
/// somehow no chosen color, Heraldic Banner's static ability has no effect,
/// and its mana ability produces no mana if you activate it."), so the acquired
/// ability defines no type, and the sacrificed Welder could produce only the
/// {G} of the Forest that Ashaya, Soul of the Wild makes it.
#[test]
fn sacrificed_welder_with_an_imprinted_banner_offers_only_its_forest_green() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let welder = scenario
        .add_creature_from_oracle(P0, "Myr Welder", 1, 4, MYR_WELDER)
        .id();
    let banner = scenario
        .add_artifact_from_oracle(P0, "Heraldic Banner", HERALDIC_BANNER)
        .id();
    let mut runner = scenario.build();
    // Ashaya's power and toughness come from layers, so they are applied before
    // the imprint activation checks state-based actions.
    flush(&mut runner);
    engine::game::zones::move_to_zone(runner.state_mut(), banner, Zone::Graveyard, &mut Vec::new());
    let imprint = runner.state().objects[&welder]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == AbilityKind::Activated
                && matches!(
                    &*ability.effect,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        ..
                    }
                )
        })
        .expect("reach-guard: Myr Welder has its imprint ability");
    runner
        .activate(welder, imprint)
        .target_object(banner)
        .resolve();
    runner.state_mut().objects.get_mut(&welder).unwrap().tapped = false;
    flush(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects[&banner].zone,
        Zone::Exile,
        "reach-guard: the Banner was exiled"
    );
    assert!(
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == banner && link.source_id == welder),
        "reach-guard: the Banner was exiled with the Welder"
    );
    let welder_object = &state.objects[&welder];
    assert!(
        welder_object.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated
                && matches!(
                    &*ability.effect,
                    Effect::Mana {
                        produced: ManaProduction::ChosenColor {
                            fixed_alternative: None,
                            ..
                        },
                        ..
                    }
                )
        }),
        "reach-guard: the Welder gained the Banner's chosen-color mana ability"
    );
    assert_eq!(
        welder_object.chosen_color(),
        None,
        "reach-guard: the Welder holds no chosen color"
    );
    assert!(
        welder_object
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Forest")
            && !welder_object.tapped
            && !welder_object.summoning_sick,
        "reach-guard: the Welder is an untapped Forest"
    );
    assert_is_land(&runner, welder);

    assert_eq!(
        squandered_reading(state, squandered, welder),
        vec![ManaType::Green]
    );
}

/// F2-5 (CR 608.2d + CR 105.4 + CR 106.7): Radiant Lotus ("{T}, Sacrifice one
/// or more artifacts: Choose a color. Target player adds three mana of the
/// chosen color for each artifact sacrificed this way.") chooses its color as
/// the ability resolves, and that choice could name any of the five colors, so
/// the sacrificed Lotus could produce every color. March of the Machines and
/// Ashaya, Soul of the Wild make it a Forest land.
#[test]
fn sacrificed_radiant_lotus_offers_every_color_its_choice_could_name() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "March of the Machines", MARCH_OF_THE_MACHINES);
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let lotus = scenario
        .add_artifact_from_oracle(P0, "Radiant Lotus", RADIANT_LOTUS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 6,
        })
        .id();
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, lotus);
    let object = &runner.state().objects[&lotus];
    assert!(
        object
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Forest"),
        "reach-guard: the Lotus is a Forest"
    );
    assert_eq!(
        object.chosen_color(),
        None,
        "reach-guard: the Lotus holds no chosen color"
    );
    let printed = object
        .abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: the Lotus has its activated ability");
    assert!(
        matches!(
            &*printed.effect,
            Effect::Choose {
                choice_type: ChoiceType::Color { excluded },
                persist: false,
                ..
            } if excluded.is_empty()
        ) && printed.sub_ability.as_deref().is_some_and(|sub| {
            matches!(
                &*sub.effect,
                Effect::Mana {
                    produced: ManaProduction::ChosenColor {
                        fixed_alternative: None,
                        ..
                    },
                    ..
                }
            )
        }),
        "reach-guard: \"Choose a color.\" is a resolution-time choice that its \"of the \
         chosen color\" production reads, got {printed:?}"
    );

    let mut reading = squandered_reading(runner.state(), squandered, lotus);
    reading.sort();
    assert_eq!(
        reading,
        vec![
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]
    );
}

/// F2-9 (CR 607.2d + CR 614.12a + CR 106.7): Thriving Grove ("This land enters
/// tapped. As it enters, choose a color other than green." and "{T}: Add {G} or
/// one mana of the chosen color.") is played and red is chosen as it enters,
/// so the sacrificed Grove could produce its chosen {R} and its fixed {G}.
#[test]
fn sacrificed_thriving_grove_offers_its_chosen_color_and_green() {
    let mut scenario = new_scenario();
    let squandered = scenario
        .add_enchantment_from_oracle(P0, "Squandered Resources", SQUANDERED_RESOURCES)
        .id();
    let grove = scenario
        .add_land_to_hand(P0, "Thriving Grove")
        .from_oracle_text(THRIVING_GROVE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&grove].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: grove,
            card_id,
        })
        .expect("reach-guard: playing Thriving Grove starts its entry");
    let prompt = runner.state().waiting_for.clone();
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = &prompt
    else {
        panic!("expected Thriving Grove's as-enters color choice, got {prompt:?}");
    };
    assert!(
        matches!(choice_type, ChoiceType::Color { .. }),
        "reach-guard: CR 614.12a: the color is chosen as the Grove enters"
    );
    let mut offered: Vec<&str> = options.iter().map(String::as_str).collect();
    offered.sort_unstable();
    assert_eq!(
        offered,
        vec!["Black", "Blue", "Red", "White"],
        "CR 105.4: \"a color other than green\" is the four other colors"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Red".to_string(),
        })
        .expect("red is one of the offered colors");

    let state = runner.state();
    let object = &state.objects[&grove];
    assert!(
        object.zone == Zone::Battlefield
            && object.controller == P0
            && object.chosen_color() == Some(ManaColor::Red),
        "reach-guard: the Grove entered under P0's control with red chosen"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "reach-guard: the entry completed and priority returned"
    );
    assert!(
        object.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated
                && matches!(
                    &*ability.effect,
                    Effect::Mana {
                        produced: ManaProduction::ChosenColor {
                            fixed_alternative: Some(ManaColor::Green),
                            ..
                        },
                        ..
                    }
                )
        }),
        "reach-guard: the Grove's mana ability offers {{G}} or the chosen color"
    );

    let mut reading = squandered_reading(state, squandered, grove);
    reading.sort();
    assert_eq!(reading, vec![ManaType::Red, ManaType::Green]);
}

/// A stolen-legend board's state and the objects its reads use.
struct StolenLegendBoard {
    state: GameState,
    grey_havens: ObjectId,
    /// The index of The Grey Havens' "any color among legendary creature cards
    /// in your graveyard" mana ability.
    any_color_among_index: usize,
    reflecting_pool: ObjectId,
    grafstone: ObjectId,
    raise_the_alarm: ObjectId,
}

/// CR 400.3 + CR 404.1 + CR 701.21a: P0 controls The Grey Havens, Reflecting
/// Pool and Corrupted Grafstone ({2}) and holds Raise the Alarm ({1}{W}).
/// `thief` casts Act of Treason ({2}{R}, paid from {R}{R}{R} floating) on
/// `owner`'s Isamaru, Hound of Konda, a white legendary creature, and
/// sacrifices it to Ashnod's Altar beside Memnite, so the card is put into its
/// owner's graveyard. A thief other than P0 then passes priority to P0
/// (CR 117.3d). The board is returned within that same step, while the card's
/// last known information still names the thief as its controller.
fn stolen_legend_board(thief: PlayerId, owner: PlayerId) -> StolenLegendBoard {
    let mut scenario = new_scenario();
    let grey_havens = scenario
        .add_land_from_oracle(P0, "The Grey Havens", THE_GREY_HAVENS)
        .as_legendary()
        .id();
    let reflecting_pool = scenario
        .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
        .id();
    let grafstone = scenario
        .add_artifact_from_oracle(P0, "Corrupted Grafstone", CORRUPTED_GRAFSTONE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();
    let raise_the_alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();
    let altar = scenario
        .add_artifact_from_oracle(thief, "Ashnod's Altar", ASHNODS_ALTAR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    let memnite = scenario
        .add_creature(thief, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let act_of_treason = scenario
        .add_spell_to_hand_from_oracle(thief, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        thief,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 3],
    );
    let legend = scenario
        .add_creature(owner, "Isamaru, Hound of Konda", 2, 2)
        .as_legendary()
        .with_subtypes(vec!["Dog"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    // CR 307.1: the thief casts its sorcery in its own main phase, with
    // priority and an empty stack.
    {
        let state = runner.state_mut();
        state.active_player = thief;
        state.priority_player = thief;
        state.waiting_for = WaitingFor::Priority { player: thief };
    }

    runner.cast(act_of_treason).target_object(legend).resolve();
    let state = runner.state();
    assert_eq!(
        state.objects[&legend].controller, thief,
        "reach-guard: Act of Treason gave the thief control of the legend"
    );
    assert!(
        state.players[thief.0 as usize]
            .graveyard
            .contains(&act_of_treason)
            && state.objects[&act_of_treason].color == vec![ManaColor::Red],
        "reach-guard: the red Act of Treason resolved into the thief's graveyard"
    );
    assert!(
        pool(&runner, thief).is_empty(),
        "reach-guard: the floating {{R}}{{R}}{{R}} paid for Act of Treason"
    );
    assert!(
        [grey_havens, reflecting_pool, grafstone]
            .iter()
            .all(|source| !state.objects[source].tapped),
        "reach-guard: P0's three mana sources are untapped"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: Ashnod's Altar is activatable");
    let prompt = runner.state().waiting_for.clone();
    let WaitingFor::PayCost {
        kind: PayCostKind::Sacrifice,
        choices,
        ..
    } = &prompt
    else {
        panic!("expected the Altar's sacrifice choice, got {prompt:?}");
    };
    assert!(
        choices.contains(&legend) && choices.contains(&memnite),
        "reach-guard: the sacrifice is a real choice between the legend and Memnite"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![legend],
        })
        .expect("reach-guard: sacrificing the stolen legend");
    assert_eq!(
        pool(&runner, thief),
        vec![ManaType::Colorless, ManaType::Colorless],
        "reach-guard: Ashnod's Altar added {{C}}{{C}}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the Altar's mana ability resolved at once"
    );
    if thief != P0 {
        // CR 117.3d: the reads below are P0's, so the thief hands it priority.
        runner
            .act(GameAction::PassPriority)
            .expect("reach-guard: the thief passes priority");
    }
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach-guard: P0 holds priority"
    );

    let state = runner.state();
    let legend_object = &state.objects[&legend];
    assert!(
        legend_object.zone == Zone::Graveyard
            && legend_object.owner == owner
            && state.players[owner.0 as usize].graveyard.contains(&legend)
            && !state.players[thief.0 as usize].graveyard.contains(&legend),
        "CR 400.3 + CR 404.1: the sacrificed legend is in its owner's graveyard"
    );
    assert_eq!(
        state.lki_cache.get(&legend).map(|lki| lki.controller),
        Some(thief),
        "reach-guard: the legend's last known information still names the thief"
    );
    assert!(
        state.phase == Phase::PreCombatMain && state.active_player == thief,
        "reach-guard: the reads happen in the step the legend was sacrificed in"
    );
    assert!(
        legend_object.color == vec![ManaColor::White]
            && legend_object
                .card_types
                .supertypes
                .contains(&Supertype::Legendary)
            && legend_object
                .card_types
                .core_types
                .contains(&CoreType::Creature),
        "reach-guard: the legend is a white legendary creature card"
    );
    let (any_color_among_index, among_filter) = state.objects[&grey_havens]
        .abilities
        .iter()
        .enumerate()
        .find_map(|(index, ability)| match &*ability.effect {
            Effect::Mana {
                produced: ManaProduction::AnyOneColorAmongPermanents { filter, .. },
                ..
            } if ability.kind == AbilityKind::Activated => Some((index, filter)),
            _ => None,
        })
        .expect("reach-guard: The Grey Havens has its \"any color among\" mana ability");
    assert_eq!(
        among_filter.extract_in_zone(),
        Some(Zone::Graveyard),
        "reach-guard: The Grey Havens reads a graveyard"
    );
    StolenLegendBoard {
        state: state.clone(),
        grey_havens,
        any_color_among_index,
        reflecting_pool,
        grafstone,
        raise_the_alarm,
    }
}

/// F2-10 (a) (CR 400.3 + CR 404.1 + CR 701.21a + CR 109.4 + CR 108.4a +
/// CR 109.5): P0 steals P1's Isamaru, Hound of Konda with Act of Treason and
/// sacrifices it to Ashnod's Altar, so it is put into P1's graveyard. In that
/// same step "your graveyard" is still the cards P0 owns there. The Grey Havens
/// ("{T}: Add one mana of any color among legendary creature cards in your
/// graveyard.") finds only Act of Treason, which is not a creature card, so it
/// adds no mana (CR 106.5). Reflecting Pool could produce only The Grey Havens'
/// {C} (CR 106.7). Raise the Alarm ({1}{W}) is not offered, as P0 has no white
/// source. Corrupted Grafstone ("{T}: Choose a color of a card in your
/// graveyard. Add one mana of that color.") adds only Act of Treason's red.
/// Paired control: the mirror board below.
#[test]
fn your_graveyard_skips_a_legend_you_stole_and_sacrificed_this_step() {
    let board = stolen_legend_board(P0, P1);

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        activate_for_mana(&mut read, board.grey_havens, board.any_color_among_index),
        Produced::Pool(vec![]),
        "The Grey Havens reads only the cards P0 owns"
    );
    assert!(
        read.state().objects[&board.grey_havens].tapped,
        "reach-guard: The Grey Havens' ability ran"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        tap_for_mana(&mut read, board.reflecting_pool),
        Produced::Pool(vec![ManaType::Colorless]),
        "Reflecting Pool reads The Grey Havens"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    // The Altar's {C}{C} floats in P0's pool on this board.
    read.state_mut().players[P0.0 as usize].mana_pool.clear();
    assert!(
        !legal_actions(read.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == board.raise_the_alarm
        )),
        "Raise the Alarm needs a white source P0 does not have"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        tap_for_mana(&mut read, board.grafstone),
        Produced::Pool(vec![ManaType::Red]),
        "Corrupted Grafstone reads only the cards P0 owns"
    );
}

/// F2-10 (b) (CR 400.3 + CR 404.1 + CR 701.21a + CR 109.4 + CR 108.4a +
/// CR 109.5): the mirror. P1 steals P0's Isamaru, Hound of Konda and sacrifices
/// it, so it is put into P0's graveyard, and in that same step it is a card P0
/// owns there. The Grey Havens adds its white, Reflecting Pool could produce
/// that white beside The Grey Havens' {C}, Raise the Alarm is offered, and
/// Corrupted Grafstone adds white too (Act of Treason is in P1's graveyard).
/// Paired control: the board above.
#[test]
fn your_graveyard_counts_your_legend_an_opponent_stole_and_sacrificed_this_step() {
    let board = stolen_legend_board(P1, P0);

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        activate_for_mana(&mut read, board.grey_havens, board.any_color_among_index),
        Produced::Pool(vec![ManaType::White]),
        "The Grey Havens reads the legend P0 owns"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        tap_for_mana(&mut read, board.reflecting_pool),
        Produced::Prompt(vec![ManaType::White, ManaType::Colorless]),
        "Reflecting Pool reads The Grey Havens"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    read.state_mut().players[P0.0 as usize].mana_pool.clear();
    assert!(
        legal_actions(read.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == board.raise_the_alarm
        )),
        "Raise the Alarm is paid with The Grey Havens' white"
    );

    let mut read = GameRunner::from_state(board.state.clone());
    assert_eq!(
        tap_for_mana(&mut read, board.grafstone),
        Produced::Pool(vec![ManaType::White]),
        "Corrupted Grafstone reads the legend P0 owns"
    );
}
