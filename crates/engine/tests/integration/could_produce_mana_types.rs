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

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::parse_counter_type;
use engine::types::events::ManaTapState;
use engine::types::game_state::{
    CastPaymentMode, ExileLink, ExileLinkKind, GameState, ManaChoicePrompt, PayCostKind,
    StackEntryKind, WaitingFor,
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
        .mana
        .iter()
        .map(|unit| unit.color)
        .collect()
}

/// Activates `source`'s first activated ability — its mana ability here,
/// which resolves immediately (CR 605.3b) — and reports the prompt it raised or
/// the mana it added. Gemstone Caverns' opening-hand ability comes first in its
/// list, so the index is looked up rather than assumed.
fn tap_for_mana(runner: &mut GameRunner, source: ObjectId) -> Produced {
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("reach-guard: the source has an activated ability");
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

/// F3-1 (CR 106.12 + CR 106.12b): an assembled Urza's Mine adds {C}{C} by one
/// {T} mana ability, so Contamination modifies that whole production, the
/// "add {C}{C} instead" link included, and only {B} arrives. Control: without
/// Contamination the same activation adds {C}{C}, so the link ran.
#[test]
fn tapping_assembled_urzas_mine_under_contamination_adds_only_black() {
    let (scenario, mine) = tron_scenario(true);
    let mut runner = scenario.build();
    assert_eq!(
        tap_for_mana(&mut runner, mine),
        Produced::Pool(vec![ManaType::Colorless, ManaType::Colorless]),
        "control: the assembled Mine's gated link adds its second colorless mana"
    );

    let (mut scenario, mine) = tron_scenario(true);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let mut runner = scenario.build();
    let Produced::Pool(produced) = tap_for_mana(&mut runner, mine) else {
        panic!("reach-guard: the Mine adds its mana without a type prompt");
    };
    assert!(!produced.is_empty(), "reach-guard: the Mine added mana");
    assert!(
        produced
            .iter()
            .all(|mana_type| *mana_type == ManaType::Black),
        "CR 106.12b: every unit the Mine produced was tapped for mana, got {produced:?}"
    );
}

/// F3-2 (CR 106.12 + CR 106.12b): Mana Reflection doubles every unit of an
/// assembled Urza's Mine's {C}{C}, the "instead" link's included, so four {C}
/// arrive. Disabled sibling: an unassembled Mine's single {C} doubles to two.
#[test]
fn tapping_assembled_urzas_mine_under_mana_reflection_doubles_both_halves() {
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

/// F3-8 (CR 106.12a): Vorinclex, Voice of Hunger's echo triggers once per
/// resolution of a mana ability that produced mana, however many of its links
/// produce: an assembled Urza's Mine adds {C}{C} and the echo one {C}.
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

/// F3-9 (CR 106.12b): under Contamination both halves of an assembled Urza's
/// Mine's production are tapped for mana and come from the Mine, so undoing
/// the manual tap refunds both. The refund itself is the engine's manual-tap
/// undo, not a rule of the game (CR 605.3b: the ability resolved at once).
#[test]
fn untapping_an_assembled_urzas_mine_refunds_both_halves() {
    let (mut scenario, mine) = tron_scenario(true);
    scenario.add_enchantment_from_oracle(P0, "Contamination", CONTAMINATION);
    let mut runner = scenario.build();
    let Produced::Pool(produced) = tap_for_mana(&mut runner, mine) else {
        panic!("reach-guard: the Mine adds its mana without a type prompt");
    };
    assert!(!produced.is_empty(), "reach-guard: the Mine added mana");
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
    assert!(pool(&runner, P0).is_empty(), "both halves were refunded");
    assert!(
        !runner.state().objects[&mine].tapped,
        "the Mine is untapped"
    );
}

/// F3-10 (CR 106.12): Wild Cantor's mana ability has no {T} in its cost, so its
/// production is not tapped for mana and Contamination leaves its five colors
/// alone; its Forest ability (from Ashaya) is tapped for mana and gives {B}.
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
    let mut runner = scenario.build();
    flush(&mut runner);
    assert_is_land(&runner, cantor);

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
