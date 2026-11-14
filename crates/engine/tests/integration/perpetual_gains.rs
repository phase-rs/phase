//! Runtime tests for Alchemy "perpetually gains" riders: U1 plural-anaphor
//! subjects ("They"), U2 quoted trigger grants, U3 quoted self
//! alternative-cost grants.
//!
//! Digital-only Alchemy (no CR entry for "perpetually"): each test drives a
//! motivating card's verbatim Oracle text through the real cast pipeline and
//! asserts the granted characteristic survives the zone changes the card
//! funnels it through (library → hand → stack → battlefield).
//!
//! V1 (Indris, U1 fan-out to four Bolts) resolves through the `LastCreated`
//! fan-out arm in the perpetual resolver (`game/effects/perpetual.rs`), which
//! fans the rider to the whole conjured set — the shared targeting path stays
//! singular by design. V2b (Jewel Mine, U1×U2 composition) resolves through
//! the "on top of your library" conjure-destination leaf. Parse-side U1 is
//! pinned by `perpetual_they_subject_binds_last_created_after_conjure` and
//! composition by `perpetual_they_subject_with_trigger_body_composes`.

use std::collections::HashMap;
use std::sync::Arc;

use super::rules::{
    cast_spell_action, AttackTarget, GameAction, GameEvent, GameRunner, GameScenario, Keyword,
    ObjectId, Phase, PlayerId, WaitingFor, Zone, P0, P1,
};
use engine::types::ability::{
    AbilityCost, Effect, PtValue, TargetFilter, TriggerDefinitionOccurrenceRef,
};
use engine::types::actions::DebugAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::self_spell_cost_mod_active_zones;

const JESSIE: &str = "Deathtouch\nWhen Jessie Zane enters and whenever you cast a Snake spell, conjure a card named Ambush Viper into the top six cards of your library at random. It perpetually gains \"When this creature enters, draw a card.\"";
const SANGUINE: &str = "Flying, lifelink\nWhenever this creature attacks, conjure a card named Sanguine Bond into the top fifteen cards of your library at random. It perpetually gains \"You may pay {0} rather than pay this spell's mana cost\" and \"When this permanent enters, draw a card.\"";
const SANGUINE_TWO_SELF_COSTS: &str = "Flying, lifelink\nWhenever this creature attacks, conjure a card named Sanguine Bond into the top fifteen cards of your library at random. It perpetually gains \"You may pay {5} rather than pay this spell's mana cost\" and \"You may pay {0} rather than pay this spell's mana cost.\"";
const SANGUINE_TWO_UNPAYABLE_COSTS: &str = "Flying, lifelink\nWhenever this creature attacks, conjure a card named Two Cost Bear into the top fifteen cards of your library at random. It perpetually gains \"You may pay {5} rather than pay this spell's mana cost\" and \"You may pay {9} rather than pay this spell's mana cost.\"";
const FIST_OF_SUNS: &str =
    "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.";
const ROOFTOP_STORM: &str =
    "You may pay {0} rather than pay the mana cost for Zombie creature spells you cast.";
const INDRIS: &str = "When Indris, the Hydrostatic Surge enters, conjure four cards named Lightning Bolt into your library. They perpetually gain storm. Then shuffle.\nWhenever you cast an instant or sorcery spell, draw a card.";
const JEWEL_MINE: &str = "When Jewel Mine Overseer enters, conjure seven cards named Seven Dwarves on top of your library. They perpetually gain \"When this creature enters, draw a card.\" Then shuffle.\nAt the beginning of your upkeep, exile the top card of your library. You may play that card this turn.";
const OGLOR: &str = "At the beginning of your upkeep, look at the top two cards of your library, then put one of them into your graveyard.\nWhenever a creature card is put into your graveyard from your library or hand, it perpetually gains \"When this card leaves your graveyard, create a tapped 2/2 black Zombie creature token.\"";
const OPPONENT_UNION_SENTINEL: &str = "Whenever a creature card is put into an opponent's graveyard from an opponent's library or hand, create a tapped 2/2 black Zombie creature token.";

fn mana(color: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn colored_cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Minimal conjurable creature face: real name, cost, types, and P/T, but no
/// printed abilities — the asserted behavior in every test below comes from
/// the perpetual GRANT, and the minimal face keeps the tests independent of
/// unrelated parser support for the conjured card's own text.
fn minimal_creature_face(
    name: &str,
    mana_cost: ManaCost,
    subtypes: Vec<&str>,
    power: i32,
    toughness: i32,
) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost,
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: subtypes.into_iter().map(str::to_string).collect(),
        },
        power: Some(PtValue::Fixed(power)),
        toughness: Some(PtValue::Fixed(toughness)),
        ..Default::default()
    }
}

/// Minimal conjurable instant face (same rationale as
/// [`minimal_creature_face`]): real name, cost, and type, but no printed
/// effect — the asserted behavior is the granted Storm, observed through the
/// `SpellCopied` events its trigger emits (CR 707.10), not through damage.
fn minimal_instant_face(name: &str, mana_cost: ManaCost) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost,
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Instant],
            subtypes: vec![],
        },
        power: None,
        toughness: None,
        ..Default::default()
    }
}

/// Minimal conjurable enchantment face (same rationale as
/// [`minimal_creature_face`]).
fn minimal_enchantment_face(name: &str, mana_cost: ManaCost) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost,
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Enchantment],
            subtypes: vec![],
        },
        power: None,
        toughness: None,
        ..Default::default()
    }
}

fn seed_conjure_registry(runner: &mut GameRunner, faces: Vec<CardFace>) {
    let map: HashMap<String, CardFace> = faces
        .into_iter()
        .map(|face| (face.name.to_lowercase(), face))
        .collect();
    runner.state_mut().card_face_registry = Arc::new(map);
}

/// Debug-draw until a card named `name` is in `player`'s hand (bounded loop —
/// the conjure under test guarantees the card is in the library).
fn draw_until_in_hand(runner: &mut GameRunner, player: PlayerId, name: &str) -> ObjectId {
    for _ in 0..24 {
        let found = runner.state().players[player.0 as usize]
            .hand
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == name);
        if let Some(id) = found {
            return id;
        }
        runner
            .act(GameAction::Debug(DebugAction::DrawCards {
                player_id: player,
                count: 1,
            }))
            .expect("debug draw must succeed");
    }
    panic!("never drew {name} within the bound");
}

fn named_object_ids(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .filter_map(|(id, obj)| (obj.name == name).then_some(*id))
        .collect()
}

/// Walks a sub-ability chain for any `Unimplemented` link. Shared by the
/// whole-card reach-guard below and the ETB-scoped V2b reach-guard.
fn chain_has_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
    let mut current = Some(def);
    while let Some(d) = current {
        if matches!(&*d.effect, Effect::Unimplemented { .. }) {
            return true;
        }
        current = d.sub_ability.as_deref();
    }
    false
}

/// Deep reach-guard: `has_unimplemented_mechanics` only inspects ability-head
/// effects and trigger modes (registry membership) — it does NOT walk
/// sub-ability chains or trigger `execute` bodies, so a card whose trigger
/// body holds `Unimplemented` reads clean. Walk the full trees instead.
fn assert_fully_parsed(runner: &GameRunner, id: ObjectId, name: &str) {
    let obj = &runner.state().objects[&id];
    assert!(
        !obj.has_unimplemented_mechanics(),
        "reach-guard: {name} must have no unimplemented head mechanics"
    );
    for def in obj.abilities.iter() {
        assert!(
            !chain_has_unimplemented(def),
            "reach-guard: {name} ability chain must hold no Unimplemented: {def:#?}"
        );
    }
    for entry in obj.trigger_definitions.iter_unchecked() {
        if let Some(execute) = entry.definition().execute.as_deref() {
            assert!(
                !chain_has_unimplemented(execute),
                "reach-guard: {name} trigger execute must hold no Unimplemented: {execute:#?}"
            );
        }
    }
}

/// Scoped reach-guard for V2b: Jewel Mine's upkeep half is rider-scoped OUT of
/// acceptance (Q6 — its support status is re-measured, not asserted), so only
/// the ETB trigger's execute chain must be `Unimplemented`-free. The all-seven
/// positive assertion proves the rider actually resolved.
fn assert_etb_trigger_fully_parsed(runner: &GameRunner, id: ObjectId, name: &str) {
    let obj = &runner.state().objects[&id];
    let mut found = false;
    for entry in obj.trigger_definitions.iter_unchecked() {
        // CR 603.6a: an ETB trigger is a zone-change trigger whose destination
        // is the battlefield.
        let def = entry.definition();
        if matches!(def.mode, TriggerMode::ChangesZone)
            && def.destination == Some(Zone::Battlefield)
        {
            found = true;
            if let Some(execute) = entry.definition().execute.as_deref() {
                assert!(
                    !chain_has_unimplemented(execute),
                    "reach-guard: {name} ETB execute must hold no Unimplemented: {execute:#?}"
                );
            }
        }
    }
    assert!(found, "reach-guard: {name} must carry an ETB trigger");
}

/// V1: Indris's "They perpetually gain storm" fans out to ALL four conjured
/// Bolts, and the granted Storm flows through the cast-time keyword snapshot
/// — casting one Bolt afterwards copies it once per spell cast before it this
/// turn (CR 702.40a; the count excludes the Bolt itself).
///
/// Revert-failing: without the fan-out arm only the first Bolt carries Storm
/// (pinned by the all-four assertion, not the copy count — the first Bolt
/// copies correctly either way); without snapshot flow there are no copies.
#[test]
fn indris_conjured_bolts_all_gain_storm_and_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Filler 0", "Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5", "Filler 6",
            "Filler 7", "Filler 8", "Filler 9",
        ],
    );
    let indris = scenario
        .add_creature_to_hand(P0, "Indris, the Hydrostatic Surge", 5, 5)
        .from_oracle_text(INDRIS)
        .with_mana_cost(colored_cost(
            vec![ManaCostShard::Blue, ManaCostShard::Red],
            4,
        ))
        .id();
    let vanilla_a = scenario
        .add_creature_to_hand(P0, "Vanilla A", 2, 2)
        .with_mana_cost(colored_cost(vec![ManaCostShard::Green], 0))
        .id();
    let vanilla_b = scenario
        .add_creature_to_hand(P0, "Vanilla B", 2, 2)
        .with_mana_cost(colored_cost(vec![ManaCostShard::Green], 0))
        .id();
    // Indris {4}{U}{R} + two {G} vanillas + the {R} Bolt.
    let mut pool = mana(ManaType::Colorless, 4);
    pool.extend(mana(ManaType::Blue, 1));
    pool.extend(mana(ManaType::Red, 2));
    pool.extend(mana(ManaType::Green, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(
        &mut runner,
        vec![minimal_instant_face(
            "Lightning Bolt",
            colored_cost(vec![ManaCostShard::Red], 0),
        )],
    );
    assert_fully_parsed(&runner, indris, "Indris, the Hydrostatic Surge");

    runner.cast(indris).resolve();

    let bolts = named_object_ids(&runner, "Lightning Bolt");
    assert_eq!(
        bolts.len(),
        4,
        "Indris must conjure exactly four Bolts, found {}",
        bolts.len()
    );
    for id in &bolts {
        assert!(
            runner.state().objects[id].has_keyword(&Keyword::Storm),
            "every conjured Bolt must carry the granted Storm"
        );
    }

    runner.cast(vanilla_a).resolve();
    runner.cast(vanilla_b).resolve();
    // N7 guard: the storm count excludes the Bolt itself but includes every
    // other spell cast this turn — Indris plus the two vanillas.
    let prior = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .map(|records| records.len())
        .unwrap_or(0);
    assert_eq!(
        prior, 3,
        "precondition: three spells must precede the Bolt, found {prior}"
    );

    let bolt = draw_until_in_hand(&mut runner, P0, "Lightning Bolt");
    // Manual drive: the Bolt's cast fires Storm AND Indris's own
    // instant-or-sorcery draw trigger, and neither the `SpellCast` driver's
    // `.resolve()` nor `drive_with_response` handles the resulting
    // `OrderTriggers` prompt — drive it by hand instead (both orders resolve
    // identically; identity order is kept).
    let mut events = runner
        .act(cast_spell_action(&runner, bolt))
        .expect("the granted-Storm Bolt must be castable")
        .events;
    for _ in 0..60 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                GameAction::PassPriority
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected prompt while resolving the Bolt: {other:?}"),
        };
        events.extend(runner.act(action).expect("drive window").events);
    }
    assert!(
        runner.state().stack.is_empty(),
        "the Bolt, its copies, and the Indris draw must all resolve"
    );
    let copies = events
        .iter()
        .filter(|event| matches!(event, GameEvent::SpellCopied { .. }))
        .count();
    assert_eq!(
        copies, 3,
        "granted Storm must copy the Bolt once per prior spell (CR 702.40a), found {copies}"
    );
}

/// V1 hostile: two sequential Indris ETBs — the second conjure ASSIGNs the
/// creation ledger, so the second rider grants the second set of four, and
/// each set keeps exactly its own Storm. The `GrantKeywords` installer dedups
/// by equality, so the exactly-one half is installer-satisfied and the
/// discriminating half is set-2 presence: under first-only resolution the
/// second set would carry a single Storm.
#[test]
fn indris_sequential_etbs_grant_each_set_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Filler 0", "Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5", "Filler 6",
            "Filler 7", "Filler 8", "Filler 9",
        ],
    );
    let indris_a = scenario
        .add_creature_to_hand(P0, "Indris, the Hydrostatic Surge", 5, 5)
        .from_oracle_text(INDRIS)
        .with_mana_cost(colored_cost(
            vec![ManaCostShard::Blue, ManaCostShard::Red],
            4,
        ))
        .id();
    let indris_b = scenario
        .add_creature_to_hand(P0, "Indris, the Hydrostatic Surge", 5, 5)
        .from_oracle_text(INDRIS)
        .with_mana_cost(colored_cost(
            vec![ManaCostShard::Blue, ManaCostShard::Red],
            4,
        ))
        .id();
    // Two Indris at {4}{U}{R} each.
    let mut pool = mana(ManaType::Colorless, 8);
    pool.extend(mana(ManaType::Blue, 2));
    pool.extend(mana(ManaType::Red, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(
        &mut runner,
        vec![minimal_instant_face(
            "Lightning Bolt",
            colored_cost(vec![ManaCostShard::Red], 0),
        )],
    );
    assert_fully_parsed(&runner, indris_a, "Indris, the Hydrostatic Surge");
    assert_fully_parsed(&runner, indris_b, "Indris, the Hydrostatic Surge");

    runner.cast(indris_a).resolve();
    let set1 = named_object_ids(&runner, "Lightning Bolt");
    assert_eq!(
        set1.len(),
        4,
        "the first ETB must conjure four Bolts, found {}",
        set1.len()
    );

    runner.cast(indris_b).resolve();
    let all = named_object_ids(&runner, "Lightning Bolt");
    assert_eq!(
        all.len(),
        8,
        "the second ETB must conjure four more Bolts, found {} total",
        all.len()
    );
    let set2: Vec<_> = all.iter().filter(|id| !set1.contains(id)).collect();
    assert_eq!(
        set2.len(),
        4,
        "the second set must hold four distinct Bolts, found {}",
        set2.len()
    );

    let storm_count = |runner: &GameRunner, id: &ObjectId| {
        runner.state().objects[id]
            .keywords
            .iter()
            .filter(|keyword| matches!(keyword, Keyword::Storm))
            .count()
    };
    for id in &set1 {
        assert_eq!(
            storm_count(&runner, id),
            1,
            "each first-set Bolt must keep exactly one Storm"
        );
        assert_eq!(
            runner.state().objects[id].perpetual_mods.len(),
            1,
            "each first-set Bolt must carry exactly one perpetual Storm install"
        );
    }
    for id in set2 {
        assert_eq!(
            storm_count(&runner, id),
            1,
            "each second-set Bolt must gain exactly one Storm"
        );
        assert_eq!(
            runner.state().objects[id].perpetual_mods.len(),
            1,
            "each second-set Bolt must carry exactly one perpetual Storm install"
        );
    }
}

/// V2: Jessie Zane's quoted trigger grant — the conjured Viper draws its
/// controller a card when it enters (CR 603.1), after crossing
/// library → hand → stack → battlefield.
///
/// Multi-authority hostile: casting the Viper (a Snake) re-triggers Jessie's
/// "whenever you cast a Snake spell" half, conjuring a second Viper with its
/// own independent grant — per-object trigger lists, no cross-install.
///
/// Revert-failing: without the U2 installer the Viper has no trigger and the
/// ETB draws nothing.
#[test]
fn jessie_conjured_viper_draws_on_etb() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Filler 0", "Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5", "Filler 6",
            "Filler 7",
        ],
    );
    let jessie = scenario
        .add_creature_to_hand(P0, "Jessie Zane, Fangbringer", 2, 2)
        .from_oracle_text_with_keywords(&["Deathtouch"], JESSIE)
        .with_mana_cost(colored_cost(
            vec![ManaCostShard::Green, ManaCostShard::Green],
            1,
        ))
        .id();
    let mut pool = mana(ManaType::Colorless, 2);
    pool.extend(mana(ManaType::Green, 3));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(
        &mut runner,
        vec![minimal_creature_face(
            "Ambush Viper",
            colored_cost(vec![ManaCostShard::Green], 1),
            vec!["Snake"],
            2,
            1,
        )],
    );
    assert_fully_parsed(&runner, jessie, "Jessie Zane");

    runner.cast(jessie).resolve();

    let viper = draw_until_in_hand(&mut runner, P0, "Ambush Viper");
    let outcome = runner.cast(viper).resolve();
    outcome.assert_hand_drawn(P0, 1);

    // The Snake-cast half re-triggered: a second Viper was conjured with its
    // own grant (the minimal face has no printed triggers, so any trigger on
    // it is the perpetual install).
    let vipers = named_object_ids(&runner, "Ambush Viper");
    assert_eq!(
        vipers.len(),
        2,
        "Jessie's Snake-cast half must conjure a second Viper, found {}",
        vipers.len()
    );
    for id in &vipers {
        assert_eq!(
            runner.state().objects[id].base_trigger_definitions.len(),
            1,
            "each Viper must carry exactly its own granted ETB trigger"
        );
    }
}

/// V2b: Jewel Mine Overseer's they-subject × trigger-body composition — all
/// seven conjured Dwarves carry the granted ETB-draw trigger (order-independent
/// read off the library objects: "Then shuffle" randomizes), and casting one
/// draws its controller a card (CR 603.1).
///
/// Multi-authority hostile: casting a second Dwarf draws again — per-object
/// trigger lists, no cross-install.
///
/// Revert-failing: without the fan-out arm only the first Dwarf is granted;
/// without the on-top leaf the verbatim head fails to parse at all.
#[test]
fn jewel_mine_grants_etb_draw_to_all_seven_dwarves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Filler 0", "Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5", "Filler 6",
            "Filler 7", "Filler 8", "Filler 9",
        ],
    );
    let overseer = scenario
        .add_creature_to_hand(P0, "Jewel Mine Overseer", 3, 3)
        .from_oracle_text(JEWEL_MINE)
        .with_mana_cost(colored_cost(
            vec![ManaCostShard::Red, ManaCostShard::White],
            1,
        ))
        .id();
    // Overseer {1}{R}{W} + two {1}{R} Dwarves.
    let mut pool = mana(ManaType::Colorless, 3);
    pool.extend(mana(ManaType::Red, 3));
    pool.extend(mana(ManaType::White, 1));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(
        &mut runner,
        vec![minimal_creature_face(
            "Seven Dwarves",
            colored_cost(vec![ManaCostShard::Red], 1),
            vec!["Dwarf"],
            2,
            2,
        )],
    );
    assert_etb_trigger_fully_parsed(&runner, overseer, "Jewel Mine Overseer");

    runner.cast(overseer).resolve();

    let dwarves = named_object_ids(&runner, "Seven Dwarves");
    assert_eq!(
        dwarves.len(),
        7,
        "the Overseer must conjure exactly seven Dwarves, found {}",
        dwarves.len()
    );
    for id in &dwarves {
        assert_eq!(
            runner.state().objects[id].base_trigger_definitions.len(),
            1,
            "each Dwarf must carry exactly its own granted ETB trigger"
        );
    }

    let dwarf_one = draw_until_in_hand(&mut runner, P0, "Seven Dwarves");
    let outcome = runner.cast(dwarf_one).resolve();
    outcome.assert_hand_drawn(P0, 1);

    let dwarf_two = draw_until_in_hand(&mut runner, P0, "Seven Dwarves");
    let outcome_two = runner.cast(dwarf_two).resolve();
    outcome_two.assert_hand_drawn(P0, 1);
}

/// V3: Sanguine Soothsayer's quoted self alternative-cost grant — the conjured
/// Bond casts for {0} from hand (CR 118.9 + CR 601.2f lock-in) with an empty
/// pool.
///
/// Multi-authority hostile: a Fist-of-Suns-style universal battlefield grant
/// is also active, offering {W}{U}{B}{R}{G} for the same spell. The pipeline
/// exposes a single alternative-vs-printed choice and deterministically
/// prefers the spell's own option — so a completed {0} cast with an empty pool
/// proves the SELF grant won (the Fist offer is unpayable here).
///
/// Revert-failing: without the U3 self-scan the only offer is Fist's {WUBRG},
/// unpayable from an empty pool, and the cast fails.
#[test]
fn sanguine_bond_castable_for_zero_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Twenty fillers: the Bond lands within the top fifteen, so drawing it
    // leaves at least five cards behind — the Bond's own granted ETB draw
    // must not deck P0.
    scenario.with_library_top(
        P0,
        &[
            "Filler 0",
            "Filler 1",
            "Filler 2",
            "Filler 3",
            "Filler 4",
            "Filler 5",
            "Filler 6",
            "Filler 7",
            "Filler 8",
            "Filler 9",
            "Filler 10",
            "Filler 11",
            "Filler 12",
            "Filler 13",
            "Filler 14",
            "Filler 15",
            "Filler 16",
            "Filler 17",
            "Filler 18",
            "Filler 19",
        ],
    );
    let sanguine = scenario
        .add_creature(P0, "Sanguine Soothsayer", 2, 2)
        .from_oracle_text_with_keywords(&["Flying", "Lifelink"], SANGUINE)
        .id();
    // The Fist's universal filter ({WUBRG} for any spell) genuinely contends
    // for Sanguine Bond — unlike a Zombie-only grant, which could never apply.
    let fist = scenario
        .add_artifact_from_oracle(P0, "Fist of Suns", FIST_OF_SUNS)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(
        &mut runner,
        vec![minimal_enchantment_face(
            "Sanguine Bond",
            colored_cost(vec![ManaCostShard::Black, ManaCostShard::Black], 3),
        )],
    );
    assert_fully_parsed(&runner, sanguine, "Sanguine Soothsayer");
    assert_fully_parsed(&runner, fist, "Fist of Suns");

    runner.pass_both_players();
    runner
        .declare_attackers(&[(sanguine, AttackTarget::Player(P1))])
        .expect("DeclareAttackers should succeed");
    for _ in 0..32 {
        let bond_conjured = !named_object_ids(&runner, "Sanguine Bond").is_empty();
        if bond_conjured
            && runner.state().stack.is_empty()
            && matches!(runner.state().phase, Phase::PostCombatMain)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.declare_blockers(&[]).expect("P1 blocks nothing");
        } else if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.pass_both_players();
        } else {
            panic!(
                "unexpected prompt while resolving the attack trigger: {:?}",
                runner.state().waiting_for
            );
        }
    }
    let bonds = named_object_ids(&runner, "Sanguine Bond");
    assert_eq!(
        bonds.len(),
        1,
        "the attack trigger must conjure exactly one Bond, found {}",
        bonds.len()
    );

    // ModifyCost-parity pin: the installed self alt-cost static carries the
    // same 6-zone `active_zones` reach as a granted self-spell cost modifier
    // (CR 113.6e any-zone-plus-stack) with `SelfRef` affected — without this
    // opt-in the grant is a silent no-op and the {0} cast below fails.
    {
        let bond = &runner.state().objects[&bonds[0]];
        let granted: Vec<_> = bond
            .static_definitions
            .iter_unchecked()
            .filter(|def| matches!(def.mode, StaticMode::CastWithAlternativeCost { .. }))
            .collect();
        assert_eq!(
            granted.len(),
            1,
            "the conjured Bond must carry exactly one granted self alt-cost static"
        );
        assert_eq!(
            granted[0].affected,
            Some(TargetFilter::SelfRef),
            "the granted alt-cost static must affect the spell itself"
        );
        assert_eq!(
            granted[0].active_zones,
            self_spell_cost_mod_active_zones(),
            "the granted alt-cost static must carry the 6-zone self-cost reach"
        );
        assert!(
            matches!(
                granted[0].mode,
                StaticMode::CastWithAlternativeCost {
                    cost: AbilityCost::Mana { ref cost },
                    ..
                } if *cost == ManaCost::zero()
            ),
            "the granted alt-cost offer must be {{0}}, got {:?}",
            granted[0].mode
        );
    }

    let bond = draw_until_in_hand(&mut runner, P0, "Sanguine Bond");
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "precondition: the pool must be empty so only a {{0}} offer can pay"
    );
    let outcome = runner.cast(bond).accept_optional().resolve();
    outcome.assert_zone(&[bond], Zone::Battlefield);
    // The Bond's second grant (the ETB-draw trigger) fires on entry — bonus
    // behavioral proof that BOTH Sanguine quotes installed.
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the {{0}} self grant must leave the empty pool untouched"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the {{0}} cast must resolve cleanly, halted at {:?}",
        outcome.final_waiting_for()
    );
}

/// V3 sibling: Rooftop Storm's battlefield-grant path is unchanged — a Zombie
/// spell still casts for {0} through the granted (non-self) scan.
#[test]
fn rooftop_storm_still_discounts_zombie_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rooftop = scenario
        .add_enchantment_from_oracle(P0, "Rooftop Storm", ROOFTOP_STORM)
        .id();
    let zombie = scenario
        .add_creature_to_hand(P0, "Zombie Test Subject", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .with_mana_cost(colored_cost(vec![ManaCostShard::Black], 2))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert_fully_parsed(&runner, rooftop, "Rooftop Storm");

    let outcome = runner.cast(zombie).accept_optional().resolve();
    outcome.assert_zone(&[zombie], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the Rooftop {{0}} grant must leave the empty pool untouched"
    );
}

/// Shared prefix for the self-cost-preference tests (MED #2): an attacker
/// carrying `attacker_oracle` conjures one `face` card with a two-quote
/// self-alt-cost grant, driven through combat to the post-trigger settle.
/// Returns the runner; the conjured card sits in the library (draw it with
/// `draw_until_in_hand`). Mirrors the V3 Sanguine drive without the
/// Fist-of-Suns multi-authority (the discrimination here is between two SELF
/// grants, not self-vs-battlefield).
fn conjure_two_cost_grant_runner(
    attacker_oracle: &str,
    conjured_name: &str,
    face: CardFace,
) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Filler 0",
            "Filler 1",
            "Filler 2",
            "Filler 3",
            "Filler 4",
            "Filler 5",
            "Filler 6",
            "Filler 7",
            "Filler 8",
            "Filler 9",
            "Filler 10",
            "Filler 11",
            "Filler 12",
            "Filler 13",
            "Filler 14",
            "Filler 15",
            "Filler 16",
            "Filler 17",
            "Filler 18",
            "Filler 19",
        ],
    );
    let attacker = scenario
        .add_creature(P0, "Sanguine Soothsayer", 2, 2)
        .from_oracle_text_with_keywords(&["Flying", "Lifelink"], attacker_oracle)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    seed_conjure_registry(&mut runner, vec![face]);
    assert_fully_parsed(&runner, attacker, "Sanguine Soothsayer");

    runner.pass_both_players();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers should succeed");
    for _ in 0..32 {
        let bond_conjured = !named_object_ids(&runner, conjured_name).is_empty();
        if bond_conjured
            && runner.state().stack.is_empty()
            && matches!(runner.state().phase, Phase::PostCombatMain)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.declare_blockers(&[]).expect("P1 blocks nothing");
        } else if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.pass_both_players();
        } else {
            panic!(
                "unexpected prompt while resolving the attack trigger: {:?}",
                runner.state().waiting_for
            );
        }
    }
    let conjured = named_object_ids(&runner, conjured_name);
    assert_eq!(
        conjured.len(),
        1,
        "the attack trigger must conjure exactly one {conjured_name}, found {}",
        conjured.len()
    );
    runner
}

/// Fixture-shape pin for the self-cost-preference tests: the conjured card
/// must carry exactly the two granted self alt-cost statics in quote order,
/// each `SelfRef`-affected with the 6-zone self-cost reach.
fn assert_two_self_cost_grants(
    runner: &GameRunner,
    id: ObjectId,
    first: ManaCost,
    second: ManaCost,
) {
    let obj = &runner.state().objects[&id];
    let granted: Vec<_> = obj
        .static_definitions
        .iter_unchecked()
        .filter(|def| matches!(def.mode, StaticMode::CastWithAlternativeCost { .. }))
        .collect();
    assert_eq!(
        granted.len(),
        2,
        "the conjured card must carry exactly two granted self alt-cost statics"
    );
    for (def, expected) in granted.iter().zip([&first, &second]) {
        assert_eq!(
            def.affected,
            Some(TargetFilter::SelfRef),
            "each granted alt-cost static must affect the spell itself"
        );
        assert_eq!(
            def.active_zones,
            self_spell_cost_mod_active_zones(),
            "each granted alt-cost static must carry the 6-zone self-cost reach"
        );
        assert!(
            matches!(
                def.mode,
                StaticMode::CastWithAlternativeCost {
                    cost: AbilityCost::Mana { ref cost },
                    ..
                } if cost == expected
            ),
            "granted alt-cost offer mismatch: expected {expected:?}, got {:?}",
            def.mode
        );
    }
}

/// MED #2: payable-first SELF alt-cost selection (CR 118.9 + CR 601.2f). The
/// conjured Bond carries two perpetual self-cost grants — unpayable {5} first,
/// payable {0} later — and casts for {0} from an empty pool, proving the
/// affordable later grant is selected instead of the unaffordable first one
/// masking it.
///
/// Revert-failing: first-grant selection takes {5}, finds it unpayable, and
/// offers no alternative — the printed {3}{B}{B} is unpayable from an empty
/// pool, so the cast fails.
#[test]
fn self_alt_cost_unpayable_first_payable_later_chooses_later() {
    let mut runner = conjure_two_cost_grant_runner(
        SANGUINE_TWO_SELF_COSTS,
        "Sanguine Bond",
        minimal_enchantment_face(
            "Sanguine Bond",
            colored_cost(vec![ManaCostShard::Black, ManaCostShard::Black], 3),
        ),
    );

    let bond = draw_until_in_hand(&mut runner, P0, "Sanguine Bond");
    assert_two_self_cost_grants(&runner, bond, colored_cost(vec![], 5), ManaCost::zero());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "precondition: the pool must be empty so only a {{0}} offer can pay"
    );

    let outcome = runner.cast(bond).accept_optional().resolve();
    outcome.assert_zone(&[bond], Zone::Battlefield);
    // No trigger quote on this grant (both quotes are costs) — nothing draws.
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the {{0}} later grant must leave the empty pool untouched"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the {{0}} cast must resolve cleanly, halted at {:?}",
        outcome.final_waiting_for()
    );
}

/// MED #2 control: when NO self alternative is payable, none is chosen — the
/// spell casts for its printed cost instead. The pool pays {1}{B} exactly
/// (neither {5} nor {9}), so a completed cast with an empty pool proves the
/// printed path was taken. Guards the fix against over-correction (choosing
/// an unpayable grant); not revert-failing under the old first-grant code,
/// which also yields no alternative here.
#[test]
fn self_alt_cost_all_unavailable_falls_back_to_printed() {
    let mut runner = conjure_two_cost_grant_runner(
        SANGUINE_TWO_UNPAYABLE_COSTS,
        "Two Cost Bear",
        minimal_creature_face(
            "Two Cost Bear",
            colored_cost(vec![ManaCostShard::Black], 1),
            vec!["Bear"],
            2,
            2,
        ),
    );

    let bear = draw_until_in_hand(&mut runner, P0, "Two Cost Bear");
    assert_two_self_cost_grants(
        &runner,
        bear,
        colored_cost(vec![], 5),
        colored_cost(vec![], 9),
    );
    // Seed the pool AFTER the combat drive (pools empty across phases): {B}
    // plus one colorless pays exactly the printed {1}{B} — neither {5} nor
    // {9}. Routes through `add_mana_to_pool` (the `with_mana_pool` authority)
    // so each unit gets a distinct pip id.
    for unit in mana(ManaType::Black, 1)
        .into_iter()
        .chain(mana(ManaType::Colorless, 1))
    {
        runner.state_mut().add_mana_to_pool(P0, unit);
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        2,
        "precondition: the pool must pay exactly the printed {{1}}{{B}}"
    );

    let outcome = runner.cast(bear).accept_optional().resolve();
    outcome.assert_zone(&[bear], Zone::Battlefield);
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "with no payable alternative the printed {{1}}{{B}} must be paid in full"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the printed-cost cast must resolve cleanly, halted at {:?}",
        outcome.final_waiting_for()
    );
}

/// Full-pipeline zone move through `act` (never raw `resolve`): with
/// `simulate: true` the move runs triggers, replacements, and SBAs.
fn move_zone_simulated(runner: &mut GameRunner, id: ObjectId, to: Zone) {
    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id: id,
            to_zone: to,
            library_position: None,
            simulate: true,
        }))
        .expect("simulated zone move must succeed");
}

/// Drain the stack to quiescence (bounded: every test using this leaves only
/// triggers it intends to resolve), answering CR 603.3b same-controller
/// ordering prompts in encounter order along the way.
fn settle_stack(runner: &mut GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 100, "stack did not settle within the bound");
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("trigger order must succeed");
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must succeed");
            }
        }
    }
}

fn subtype_tokens(runner: &GameRunner, controller: PlayerId, subtype: &str) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj.controller == controller
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
            })
        })
        .collect()
}

fn zombie_tokens(runner: &GameRunner, controller: PlayerId) -> Vec<ObjectId> {
    subtype_tokens(runner, controller, "Zombie")
}

const STORMFORGED_GENESIS: &str = "Deal 1 damage to target creature. Create X 1/1 white Soldier creature tokens. They perpetually gain storm.";

/// MED #3 (nonempty created set): the typed `LastCreated` authority beats
/// inherited chain-target propagation. The spell damages an unrelated victim
/// (whose id sits in the chain's inherited targets), creates two Soldiers,
/// and the Storm rider must hit the created set ONLY — never the victim.
///
/// Revert-failing: inherited propagation wins, so the victim gains Storm and
/// the Soldiers gain nothing.
#[test]
fn last_created_rider_hits_created_set_not_inherited_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stormforged Genesis", false, STORMFORGED_GENESIS)
        .with_mana_cost(colored_cost(vec![ManaCostShard::X, ManaCostShard::Red], 0))
        .id();
    let mut pool = mana(ManaType::Red, 1);
    pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert_fully_parsed(&runner, spell, "Stormforged Genesis");

    let outcome = runner.cast(spell).x(2).target_objects(&[victim]).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);

    // The damage clause resolved (positive signal the chain ran).
    let victim_obj = &runner.state().objects[&victim];
    assert_eq!(
        victim_obj.damage_marked, 1,
        "the victim must take the chain's 1 damage"
    );
    assert!(
        !victim_obj.has_keyword(&Keyword::Storm),
        "the unrelated inherited target must NOT gain Storm"
    );
    assert!(
        victim_obj.perpetual_mods.is_empty(),
        "the unrelated inherited target must carry no perpetual install"
    );

    let soldiers = subtype_tokens(&runner, P0, "Soldier");
    assert_eq!(
        soldiers.len(),
        2,
        "X=2 must create exactly two Soldiers, found {}",
        soldiers.len()
    );
    for id in &soldiers {
        let token = &runner.state().objects[id];
        assert!(
            token.has_keyword(&Keyword::Storm),
            "each created Soldier must gain the granted Storm"
        );
        assert_eq!(
            token.perpetual_mods.len(),
            1,
            "each created Soldier must carry exactly one perpetual install"
        );
    }
}

/// MED #3 (empty created set): with X=0 the creation antecedent yields no
/// objects, so the `LastCreated` rider applies to nothing — NOT to the
/// unrelated inherited target. The victim's marked damage is the positive
/// signal the chain resolved (without it the no-Storm assertions would pass
/// vacuously on a fizzled chain).
///
/// Revert-failing: inherited propagation wins and the victim gains Storm.
#[test]
fn last_created_rider_with_empty_created_set_hits_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stormforged Genesis", false, STORMFORGED_GENESIS)
        .with_mana_cost(colored_cost(vec![ManaCostShard::X, ManaCostShard::Red], 0))
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert_fully_parsed(&runner, spell, "Stormforged Genesis");

    let outcome = runner.cast(spell).x(0).target_objects(&[victim]).resolve();
    outcome.assert_zone(&[spell], Zone::Graveyard);

    assert!(
        subtype_tokens(&runner, P0, "Soldier").is_empty(),
        "X=0 must create no Soldiers"
    );
    let victim_obj = &runner.state().objects[&victim];
    assert_eq!(
        victim_obj.damage_marked, 1,
        "the victim must take the chain's 1 damage"
    );
    assert!(
        !victim_obj.has_keyword(&Keyword::Storm),
        "with an empty created set the rider must hit nothing, not the victim"
    );
    assert!(
        victim_obj.perpetual_mods.is_empty(),
        "with an empty created set the victim must carry no perpetual install"
    );
}

/// Matthewevans PR review (multiplicity): repeated independent perpetual
/// trigger grants on the same physical card must each install one occurrence
/// (CR 113.2c: "If an object has multiple instances of the same ability, each
/// instance functions independently"). Oglor-driven end to end: grant the same
/// Bears three times (twice via the library, once via the hand) with zone
/// cycles between — the first graveyard departure yields one Zombie, the next
/// two yield two each, and the last yields three — with distinct `Printed`
/// occurrence refs retained across layer recomputation and further zone
/// changes.
///
/// Both origin routes fire the outer trigger (CR 603.1): Oglor's "from your
/// library or hand" origin parses to the disjunctive `origin_zones` set. The
/// graveyard departures target the battlefield and exile (scanned zones): the
/// look-back scan only covers departures from the battlefield plus
/// current-zone scans of battlefield/graveyard/exile/stack, so a graveyard ->
/// hand/library departure of the trigger's own source is invisible to
/// collection (pre-existing look-back gap: CR 603.10a lists
/// leaves-graveyard triggers among those that look back, but the scan only
/// implements leaves-battlefield look-back; extending it is out of scope for
/// this fix).
///
/// Revert-failing: the structural-equality installer guard collapses the
/// second grant, so the `base_trigger_definitions` count stays 1 and every
/// post-second-grant departure yields one Zombie instead of two.
#[test]
fn oglor_repeated_grants_retain_multiplicity_across_zone_cycles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oglor = scenario
        .add_creature_to_hand(P0, "Oglor, Devoted Assistant", 3, 4)
        .from_oracle_text(OGLOR)
        .id();
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert_fully_parsed(&runner, oglor, "Oglor, Devoted Assistant");

    // Oglor to the battlefield (no ETB; the upkeep half never fires — the
    // whole test stays in PreCombatMain).
    move_zone_simulated(&mut runner, oglor, Zone::Battlefield);
    settle_stack(&mut runner);

    // Grant #1: library -> graveyard fires Oglor's trigger (CR 603.1). The
    // hand -> library hop matches nothing (destination is not the graveyard).
    move_zone_simulated(&mut runner, bears, Zone::Library);
    move_zone_simulated(&mut runner, bears, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&bears]
            .base_trigger_definitions
            .len(),
        1,
        "reach-guard: the first Oglor resolution must grant one trigger"
    );

    // Departure #1: one instance -> one Zombie. The battlefield is a scanned
    // zone, so the granted trigger is found for the graveyard -> battlefield
    // event; entry also runs the layer pipeline with one grant installed.
    move_zone_simulated(&mut runner, bears, Zone::Battlefield);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        1,
        "the first graveyard departure must fire the single granted trigger"
    );

    // Grant #2: the same physical card, a second independent resolution. The
    // battlefield -> hand hop is quiet (the granted origin=Graveyard clause
    // does not match a battlefield origin; Oglor needs a graveyard
    // destination), as is hand -> library.
    move_zone_simulated(&mut runner, bears, Zone::Hand);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        1,
        "leaving the battlefield must fire no granted trigger"
    );
    move_zone_simulated(&mut runner, bears, Zone::Library);
    move_zone_simulated(&mut runner, bears, Zone::Graveyard);
    settle_stack(&mut runner);
    {
        let obj = &runner.state().objects[&bears];
        assert_eq!(
            obj.base_trigger_definitions.len(),
            2,
            "CR 113.2c: two independent grants must install two trigger instances"
        );
        let refs: Vec<_> = obj
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| entry.occurrence.clone())
            .collect();
        assert_eq!(
            refs.len(),
            2,
            "the live list must carry both granted occurrences"
        );
        assert!(
            refs.iter().all(|occurrence| matches!(
                occurrence,
                TriggerDefinitionOccurrenceRef::Printed { .. }
            )),
            "both occurrences must be Printed slots, got {refs:?}"
        );
        assert_ne!(
            refs[0], refs[1],
            "the two occurrences must have distinct refs"
        );
    }

    // Layer recompute + zone change: a battlefield round-trip re-derives the
    // live list, then a non-granting return to the graveyard, then the final
    // departure. Leaving the graveyard for the battlefield fires both
    // instances (1 + 2 = 3 Zombies).
    move_zone_simulated(&mut runner, bears, Zone::Battlefield);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        3,
        "leaving the graveyard with two instances must create two more Zombies"
    );
    assert_eq!(
        runner.state().objects[&bears]
            .base_trigger_definitions
            .len(),
        2,
        "both instances must survive the battlefield layer recompute"
    );
    // Battlefield -> graveyard matches neither Oglor's origin filter
    // (library/hand only) nor the granted leaves-graveyard trigger.
    move_zone_simulated(&mut runner, bears, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        3,
        "a non-matching zone change must fire nothing and grant nothing"
    );
    assert_eq!(
        runner.state().objects[&bears]
            .base_trigger_definitions
            .len(),
        2,
        "both instances must survive the further zone change"
    );
    move_zone_simulated(&mut runner, bears, Zone::Exile);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        5,
        "the second graveyard departure must fire both instances (+2)"
    );

    // Grant #3 (hand route): exile -> hand is quiet (Oglor needs a graveyard
    // destination; the granted leaves-graveyard triggers need a graveyard
    // origin), then hand -> graveyard fires Oglor's disjunctive origin
    // (CR 603.1) for a third independent grant.
    move_zone_simulated(&mut runner, bears, Zone::Hand);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        5,
        "exile -> hand must fire nothing"
    );
    move_zone_simulated(&mut runner, bears, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&bears]
            .base_trigger_definitions
            .len(),
        3,
        "hand -> graveyard must fire Oglor's library-or-hand origin for a third grant"
    );

    // Departure #3: three instances -> three more Zombies (5 + 3 = 8).
    move_zone_simulated(&mut runner, bears, Zone::Battlefield);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        8,
        "leaving the graveyard with three instances must create three more Zombies"
    );
}

/// CR 109.5 + CR 400.3 (runtime discriminator): an ACCEPTED opponent-qualified
/// union ("...put into an opponent's graveyard from an opponent's library or
/// hand") fires on the opponent's library/hand → opponent's-graveyard events
/// and stays silent on the controller's own hand → own-graveyard event.
/// Mirrors the Oglor test structure (full-pipeline `move_zone_simulated` +
/// `settle_stack`, never raw resolve).
///
/// Revert-failing in both directions: if the union failed to parse (Unknown),
/// nothing would ever fire; if the destination narrowing were dropped, the
/// own-graveyard move would fire too.
#[test]
fn opponent_qualified_union_fires_on_opponent_events_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_to_hand(P0, "Union Sentinel", 2, 2)
        .from_oracle_text(OPPONENT_UNION_SENTINEL)
        .id();
    let enemy_bears = scenario.add_creature_to_hand(P1, "Enemy Bears", 2, 2).id();
    let enemy_cub = scenario.add_creature_to_hand(P1, "Enemy Cub", 2, 2).id();
    let own_bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert_fully_parsed(&runner, watcher, "Union Sentinel");

    move_zone_simulated(&mut runner, watcher, Zone::Battlefield);
    settle_stack(&mut runner);

    // Opponent's library -> opponent's graveyard fires the accepted trigger.
    // The hand -> library hop matches nothing (destination is not the graveyard).
    move_zone_simulated(&mut runner, enemy_bears, Zone::Library);
    move_zone_simulated(&mut runner, enemy_bears, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        1,
        "opponent library -> opponent graveyard must fire the opponent-qualified union"
    );

    // The ellipsis disjunct (hand, inheriting the opponent qualifier) fires too.
    move_zone_simulated(&mut runner, enemy_cub, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        2,
        "opponent hand -> opponent graveyard must fire the ellipsis disjunct"
    );

    // The controller's own hand -> own graveyard must NOT fire it.
    move_zone_simulated(&mut runner, own_bears, Zone::Graveyard);
    settle_stack(&mut runner);
    assert_eq!(
        zombie_tokens(&runner, P0).len(),
        2,
        "own hand -> own graveyard must not fire the opponent-qualified union"
    );
}
