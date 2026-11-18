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

use engine::game::derived::continuously_revealed_cards;
use engine::game::effects::attach::attach_to;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    inline_mana_preamble, is_mana_ability, AbilityCondition, AbilityKind, Effect,
    EffectOutcomeSignal,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::parse_counter_type;
use engine::types::events::{GameEvent, ManaTapState};
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
/// when `solemnity`.
fn cheering_crowd_board(solemnity: bool) -> CheeringCrowdBoard {
    let mut scenario = new_scenario();
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
    let board = cheering_crowd_board(false);
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
    let board = cheering_crowd_board(false);
    let mut reading = squandered_reading(board.runner.state(), board.squandered, board.crowd);
    reading.sort();
    assert_eq!(
        reading,
        vec![ManaType::Green, ManaType::Colorless],
        "reach-guard: without Solemnity the projected counter adds {{C}}"
    );

    let board = cheering_crowd_board(true);
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
