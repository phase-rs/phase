//! CR 118.9 + CR 601.2b: a hand card with a keyword alternative cost (Impending,
//! Prototype, More Than Meets the Eye, Mutate, Bestow, and structurally Awaken and
//! Cleave) is a legal cast whenever ANY of its cost routes is affordable. The
//! legal-action layer used to list such a card only when its printed mana cost
//! was payable (or under Omniscience), so a player holding exactly the alternative
//! cost was never offered the cast.
//!
//! Every row asserts both halves of the contract: `legal_actions` lists the
//! `CastSpell`, and performing exactly that offered action completes with the
//! outcome the card's Oracle text derives. Tests for the earlier Impending offer
//! (`overlord_impending_offer_2859`) send `CastSpell` straight to the handler and
//! never consult `legal_actions`, so they could not see this gap.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    AlternativeCastKeyword, CastingVariant, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const OMNISCIENCE: &str = "You may cast spells from your hand without paying their mana costs.";

/// Verbatim Oracle text (Scryfall), Overlord of the Mistmoors {5}{W}{W}. It is not
/// in the integration fixture, so it is built from its text with real lands.
const MISTMOORS_ORACLE: &str = "Impending 4\u{2014}{2}{W}{W} (If you cast this spell for its impending cost, it enters with four time counters and isn't a creature until the last is removed. At the beginning of your end step, remove a time counter from it.)\nWhenever this permanent enters or attacks, create two 2/1 white Insect creature tokens with flying.";

/// Verbatim Oracle text (Scryfall), Trickster's Elk {2}{G}. Not in the fixture.
const TRICKSTERS_ELK_ORACLE: &str = "Bestow {1}{G} (If you cast this card for its bestow cost, it's an Aura spell with enchant creature. It becomes a creature again if it's not attached.)\nEnchanted creature loses all abilities and is a green Elk creature with base power and toughness 3/3.";

/// Put exactly `count` mana of one type in P0's pool.
fn add_mana(runner: &mut GameRunner, mana: ManaType, count: usize) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

/// The `CastSpell` action `legal_actions` offers for `id`, if any.
fn offered_cast(runner: &GameRunner, id: ObjectId) -> Option<GameAction> {
    engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id),
        )
}

/// How the spell `id` currently on the stack was cast.
fn stacked_variant(runner: &GameRunner, id: ObjectId) -> Option<CastingVariant> {
    runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                casting_variant, ..
            } if entry.id == id => Some(*casting_variant),
            _ => None,
        })
}

/// Perform exactly the `CastSpell` that `legal_actions` offers for `id`, answer any
/// target prompt with the first legal target, and resolve the stack. Returns how
/// the spell sat on the stack (so a test can see which cost route was taken).
fn perform_offered_cast(runner: &mut GameRunner, id: ObjectId) -> CastingVariant {
    let offered = offered_cast(runner, id).expect("legal_actions must offer the cast");
    runner
        .act(offered)
        .expect("the offered cast must be accepted");
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ) {
            runner
                .choose_first_legal_target()
                .expect("a legal target exists");
        }
    }
    let variant = stacked_variant(runner, id).unwrap_or_else(|| {
        panic!(
            "the offered cast must put the spell on the stack, waiting_for = {:?}",
            runner.state().waiting_for
        )
    });
    runner.advance_until_stack_empty();
    if let WaitingFor::MutateMergeChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseMutateMergeSide {
                side: engine::game::merge::MergeSide::Top,
            })
            .expect("mutate side choice");
        runner.advance_until_stack_empty();
    }
    variant
}

fn pool_total(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// A scenario in main phase with `name` (a real fixture card) in P0's hand.
fn real_card_in_hand(name: &str) -> (GameScenario, ObjectId) {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario.add_real_card(P0, name, Zone::Hand, db);
    (scenario, id)
}

fn build(scenario: GameScenario) -> GameRunner {
    let db = load_db().expect("integration fixture");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

// ---------------------------------------------------------------------------
// V1 / V3: Overlord of the Balemurk, Impending 5-{1}{B}, printed {3}{B}{B}
// ---------------------------------------------------------------------------

/// CR 702.176a + CR 118.9: with only {1}{B} available, the impending cast is the
/// only affordable route and must be offered and performable.
#[test]
fn balemurk_with_only_the_impending_cost_is_offered_and_cast_for_it() {
    let (scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 2);

    let variant = perform_offered_cast(&mut runner, balemurk);

    assert_eq!(variant, CastingVariant::Impending);
    let state = runner.state();
    assert_eq!(state.objects[&balemurk].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&balemurk]
            .counters
            .get(&CounterType::Time)
            .copied(),
        Some(5),
        "Impending 5 enters with five time counters"
    );
    assert!(
        !is_creature(&runner, balemurk),
        "not a creature while a time counter remains"
    );
    assert_eq!(pool_total(&runner), 0, "exactly {{1}}{{B}} is spent");
}

/// Reach-guard sibling of the row above: with both costs affordable the player is
/// asked, so the alt-only offer is the case the new candidate unlocks, not a
/// change to the both-affordable prompt.
#[test]
fn balemurk_with_both_costs_affordable_still_asks_which_to_pay() {
    let (scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 5);

    let offered = offered_cast(&runner, balemurk).expect("both costs affordable: offered");
    runner.act(offered).expect("offered cast accepted");

    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice {
            keyword,
            normal_cost,
            alternative_cost,
            ..
        } => {
            assert_eq!(*keyword, AlternativeCastKeyword::Impending);
            assert_eq!(
                *normal_cost,
                cost(3, vec![ManaCostShard::Black, ManaCostShard::Black])
            );
            assert_eq!(*alternative_cost, Some(cost(1, vec![ManaCostShard::Black])));
        }
        other => panic!("expected AlternativeCastChoice(Impending), got {other:?}"),
    }
}

/// CR 601.2b + CR 118.9: when neither cost is payable there is no offer, and the
/// handler refuses a direct attempt. Paired with the positive 2-black row.
#[test]
fn balemurk_with_neither_cost_affordable_is_not_offered() {
    // Positive reach-guard: the same card with {1}{B} is offered.
    let (scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 2);
    assert!(
        offered_cast(&runner, balemurk).is_some(),
        "reach-guard: {{1}}{{B}} is offered"
    );

    for black in [1usize, 0] {
        let (scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
        let mut runner = build(scenario);
        add_mana(&mut runner, ManaType::Black, black);
        assert!(
            offered_cast(&runner, balemurk).is_none(),
            "{black} black mana pays neither cost, so there must be no offer"
        );
        let card_id = runner.state().objects[&balemurk].card_id;
        assert!(
            runner
                .act(GameAction::CastSpell {
                    object_id: balemurk,
                    card_id,
                    targets: vec![],
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                })
                .is_err(),
            "a direct cast with {black} black mana must be refused"
        );
    }
}

/// Timing siblings: {1}{B} is available, but the spell is a sorcery-speed
/// permanent, so outside the main phase or over a non-empty stack it is not
/// offered. This keeps the 2-black positive from passing for a timing reason.
#[test]
fn balemurk_with_the_impending_cost_is_not_offered_when_timing_forbids() {
    // Reach-guard: main phase, empty stack, offered.
    let (scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 2);
    assert!(offered_cast(&runner, balemurk).is_some());

    // Not a main phase.
    let (mut scenario, balemurk) = real_card_in_hand("Overlord of the Balemurk");
    scenario.at_phase(Phase::BeginCombat);
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 2);
    assert!(
        offered_cast(&runner, balemurk).is_none(),
        "a sorcery-speed permanent is not offered in the beginning of combat step"
    );

    // Main phase, but a spell is on the stack.
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let balemurk = scenario.add_real_card(P0, "Overlord of the Balemurk", Zone::Hand, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Red, 1);
    add_mana(&mut runner, ManaType::Black, 2);
    let bolt_cast = offered_cast(&runner, bolt).expect("Lightning Bolt is offered");
    runner.act(bolt_cast).expect("cast Lightning Bolt");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner.choose_first_legal_target().expect("bolt target");
    }
    assert!(
        !runner.state().stack.is_empty(),
        "Lightning Bolt is on the stack"
    );
    assert!(
        offered_cast(&runner, balemurk).is_none(),
        "a sorcery-speed permanent is not offered over a non-empty stack"
    );
}

// ---------------------------------------------------------------------------
// V2: Overlord of the Mistmoors with real lands (the Cantrip shape)
// ---------------------------------------------------------------------------

fn mistmoors_with_plains(plains: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mistmoors = scenario
        .add_creature_to_hand_from_oracle(P0, "Overlord of the Mistmoors", 6, 6, MISTMOORS_ORACLE)
        .with_mana_cost(cost(5, vec![ManaCostShard::White, ManaCostShard::White]))
        .from_oracle_text_with_keywords(&["Impending"], MISTMOORS_ORACLE)
        .id();
    for _ in 0..plains {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    (scenario.build(), mistmoors)
}

/// CR 702.176a: four Plains pay Impending 4-{2}{W}{W} (not the printed {5}{W}{W}).
#[test]
fn mistmoors_with_four_plains_is_cast_for_impending_and_makes_two_insects() {
    let (mut runner, mistmoors) = mistmoors_with_plains(4);
    assert!(
        runner.state().objects[&mistmoors]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Impending { .. })),
        "reach-guard: the Oracle build carries Impending"
    );

    let variant = perform_offered_cast(&mut runner, mistmoors);

    assert_eq!(variant, CastingVariant::Impending);
    let state = runner.state();
    assert_eq!(state.objects[&mistmoors].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&mistmoors]
            .counters
            .get(&CounterType::Time)
            .copied(),
        Some(4)
    );
    assert!(!is_creature(&runner, mistmoors));
    let insects: Vec<_> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types.subtypes.iter().any(|s| s == "Insect")
        })
        .collect();
    assert_eq!(insects.len(), 2, "the enters trigger creates two Insects");
    assert!(
        insects
            .iter()
            .all(|o| o.keywords.iter().any(|k| matches!(k, Keyword::Flying))),
        "the Insects have flying"
    );
}

/// Three Plains cannot pay {2}{W}{W}; the four-Plains row above is the reach-guard.
#[test]
fn mistmoors_with_three_plains_is_not_offered() {
    let (runner, mistmoors) = mistmoors_with_plains(4);
    assert!(
        offered_cast(&runner, mistmoors).is_some(),
        "reach-guard: four Plains are offered"
    );
    let (runner, mistmoors) = mistmoors_with_plains(3);
    assert!(
        offered_cast(&runner, mistmoors).is_none(),
        "three Plains pay neither {{2}}{{W}}{{W}} nor {{5}}{{W}}{{W}}"
    );
}

// ---------------------------------------------------------------------------
// V4: Omniscience + Impending, the two-authority board, is unchanged
// ---------------------------------------------------------------------------

/// CR 118.9a: under Omniscience the casting election menu is the N-way
/// `CastingVariantChoice` in a fixed order. Preservation row: the order is the one
/// measured at base (free, printed, impending).
#[test]
fn omniscience_with_impending_keeps_its_election_menu() {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Omniscience", 0, 0)
        .as_enchantment()
        .from_oracle_text(OMNISCIENCE);
    let balemurk = scenario.add_real_card(P0, "Overlord of the Balemurk", Zone::Hand, db);
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 5);

    let options: Vec<CastingVariant> =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, balemurk)
            .into_iter()
            .map(|option| option.variant)
            .collect();

    assert_eq!(options.len(), 3, "options: {options:?}");
    assert!(
        matches!(options[0], CastingVariant::HandPermission { .. }),
        "options: {options:?}"
    );
    assert_eq!(options[1], CastingVariant::Normal, "options: {options:?}");
    assert_eq!(
        options[2],
        CastingVariant::Impending,
        "options: {options:?}"
    );
}

// ---------------------------------------------------------------------------
// Granted alternative + printed alternative: the optional printed-cost cast stays
// ---------------------------------------------------------------------------

/// Henzie "Toolbox" Torre's static text (the part that grants blitz).
const HENZIE_ORACLE: &str = "Each creature spell you cast with mana value 4 or greater has blitz. The blitz cost is equal to its mana cost.";

/// P0 holds Overlord of the Balemurk ({3}{B}{B}, Impending 5-{1}{B}) while a
/// Henzie-style static gives it blitz at its own mana cost.
fn balemurk_under_henzie() -> (GameRunner, ObjectId) {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Henzie \"Toolbox\" Torre", 3, 3)
        .from_oracle_text(HENZIE_ORACLE);
    let balemurk = scenario.add_real_card(P0, "Overlord of the Balemurk", Zone::Hand, db);
    (build(scenario), balemurk)
}

fn menu_variants(runner: &GameRunner) -> Vec<CastingVariant> {
    match &runner.state().waiting_for {
        WaitingFor::CastingVariantChoice { options, .. } => {
            options.iter().map(|option| option.variant).collect()
        }
        other => panic!("expected CastingVariantChoice, got {other:?}"),
    }
}

/// CR 118.9b + CR 601.2b: alternative costs are optional. With Blitz (granted) and
/// Impending (printed) both available and the printed {3}{B}{B} payable, the
/// N-way election must still contain the ordinary cast; electing it pays
/// {3}{B}{B} and yields a plain creature (no time counters, not blitzed).
#[test]
fn granted_blitz_and_impending_menu_keeps_the_printed_cost_cast() {
    let (mut runner, balemurk) = balemurk_under_henzie();
    add_mana(&mut runner, ManaType::Black, 5);

    let offered = offered_cast(&runner, balemurk).expect("the cast is offered");
    runner.act(offered).expect("offered cast accepted");

    let variants = menu_variants(&runner);
    for expected in [
        CastingVariant::Normal,
        CastingVariant::Blitz,
        CastingVariant::Impending,
    ] {
        assert!(variants.contains(&expected), "menu: {variants:?}");
    }
    assert_eq!(variants.len(), 3, "menu: {variants:?}");
    let normal = variants
        .iter()
        .position(|variant| *variant == CastingVariant::Normal)
        .unwrap();

    runner
        .act(GameAction::ChooseCastingVariant { index: normal })
        .expect("electing the printed-cost cast");
    // The printed cost is elected: no further Normal-vs-alternative question.
    assert_eq!(
        stacked_variant(&runner, balemurk),
        Some(CastingVariant::Normal),
        "waiting_for = {:?}",
        runner.state().waiting_for
    );
    assert_eq!(pool_total(&runner), 0, "{{3}}{{B}}{{B}} is spent");
    runner.advance_until_stack_empty();

    let object = &runner.state().objects[&balemurk];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.counters.get(&CounterType::Time).copied(), None);
    assert!(is_creature(&runner, balemurk));
    assert!(
        !object.keywords.iter().any(|k| matches!(k, Keyword::Haste)),
        "a plain cast is not blitzed"
    );
}

/// Sibling: with only {1}{B} the printed {3}{B}{B} and the blitz cost (equal to
/// it) are both unaffordable, so `Normal` is filtered out of the election and the
/// sole payable route (Impending) is cast directly.
#[test]
fn granted_blitz_and_impending_menu_drops_the_unaffordable_printed_cost() {
    let (mut runner, balemurk) = balemurk_under_henzie();
    add_mana(&mut runner, ManaType::Black, 2);

    let options: Vec<CastingVariant> =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, balemurk)
            .into_iter()
            .map(|option| option.variant)
            .collect();
    assert_eq!(
        options,
        vec![CastingVariant::Impending],
        "options: {options:?}"
    );

    let variant = perform_offered_cast(&mut runner, balemurk);
    assert_eq!(variant, CastingVariant::Impending);
    assert_eq!(
        runner.state().objects[&balemurk]
            .counters
            .get(&CounterType::Time)
            .copied(),
        Some(5)
    );
}

// ---------------------------------------------------------------------------
// V5: Prototype
// ---------------------------------------------------------------------------

/// CR 702.160a + CR 718.3: Arcane Proxy {7} with Prototype {1}{U}{U} 2/1; exactly
/// {1}{U}{U} is offered and the permanent is the 2/1 prototyped version.
#[test]
fn arcane_proxy_with_only_the_prototype_cost_is_offered_and_prototyped() {
    let (scenario, proxy) = real_card_in_hand("Arcane Proxy");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 3);

    let variant = perform_offered_cast(&mut runner, proxy);

    assert_eq!(variant, CastingVariant::Prototype);
    let state = runner.state();
    let obj = &state.objects[&proxy];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.prototype_form.is_some(), "cast prototyped");
    assert_eq!((obj.power, obj.toughness), (Some(2), Some(1)));
    assert_eq!(pool_total(&runner), 0);
}

#[test]
fn arcane_proxy_with_two_blue_is_not_offered() {
    let (scenario, proxy) = real_card_in_hand("Arcane Proxy");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 3);
    assert!(offered_cast(&runner, proxy).is_some(), "reach-guard: 3 U");
    let (scenario, proxy) = real_card_in_hand("Arcane Proxy");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 2);
    assert!(offered_cast(&runner, proxy).is_none());
}

// ---------------------------------------------------------------------------
// V6: More Than Meets the Eye
// ---------------------------------------------------------------------------

/// CR 702.162a + CR 118.6a + CR 712.8c: Flamewar, Brash Veteran (printed {1}{B}{R})
/// with exactly {B}{R} is cast converted. The converted face has no mana cost, so
/// the NoCost guard must not reject the MTMTE route.
#[test]
fn flamewar_with_only_the_mtmte_cost_is_offered_and_cast_converted() {
    let (scenario, flamewar) = real_card_in_hand("Flamewar, Brash Veteran");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 1);
    add_mana(&mut runner, ManaType::Red, 1);

    let variant = perform_offered_cast(&mut runner, flamewar);

    assert_eq!(variant, CastingVariant::MoreThanMeetsTheEye);
    let state = runner.state();
    assert_eq!(state.objects[&flamewar].zone, Zone::Battlefield);
    assert!(
        state.objects[&flamewar].transformed,
        "cast converted: the back face is up"
    );
    assert_eq!(pool_total(&runner), 0);
}

#[test]
fn flamewar_with_only_red_is_not_offered() {
    let (scenario, flamewar) = real_card_in_hand("Flamewar, Brash Veteran");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Black, 1);
    add_mana(&mut runner, ManaType::Red, 1);
    assert!(
        offered_cast(&runner, flamewar).is_some(),
        "reach-guard: {{B}}{{R}}"
    );
    let (scenario, flamewar) = real_card_in_hand("Flamewar, Brash Veteran");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Red, 1);
    assert!(offered_cast(&runner, flamewar).is_none());
}

// ---------------------------------------------------------------------------
// V7: Mutate
// ---------------------------------------------------------------------------

fn gemrazer_with_host(subtypes: Option<Vec<&str>>) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let (mut scenario, gemrazer) = real_card_in_hand("Gemrazer");
    let host = subtypes.map(|subtypes| {
        scenario
            .add_creature(P0, "Host Creature", 2, 2)
            .with_subtypes(subtypes)
            .id()
    });
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Green, 3);
    (runner, gemrazer, host)
}

/// CR 702.140a: Gemrazer {3}{G} with Mutate {1}{G}{G}; three Forests and a
/// non-Human creature you own: the mutate cast is offered and merges.
#[test]
fn gemrazer_with_a_non_human_host_is_offered_and_mutates_onto_it() {
    let (mut runner, gemrazer, host) = gemrazer_with_host(Some(vec!["Bear"]));
    let host = host.expect("host");

    let variant = perform_offered_cast(&mut runner, gemrazer);

    assert_eq!(variant, CastingVariant::Mutate);
    let state = runner.state();
    // CR 730.2c: the merged permanent keeps the surviving creature's object id.
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&host].merged_components.len(),
        2,
        "the host and Gemrazer are one merged permanent"
    );
    assert_eq!(pool_total(&runner), 0);
}

#[test]
fn gemrazer_without_a_legal_host_is_not_offered() {
    // Reach-guard: a non-Human host makes the cast offered.
    let (runner, gemrazer, _) = gemrazer_with_host(Some(vec!["Bear"]));
    assert!(offered_cast(&runner, gemrazer).is_some());
    // No creature at all.
    let (runner, gemrazer, _) = gemrazer_with_host(None);
    assert!(
        offered_cast(&runner, gemrazer).is_none(),
        "no creature to merge with"
    );
    // Hostile: the only creature is a Human, which Mutate cannot target.
    let (runner, gemrazer, _) = gemrazer_with_host(Some(vec!["Human"]));
    assert!(
        offered_cast(&runner, gemrazer).is_none(),
        "a Human is not a legal mutate target"
    );
}

// ---------------------------------------------------------------------------
// V8: Bestow (Trickster's Elk, whose bestow cost is below its printed cost)
// ---------------------------------------------------------------------------

fn elk_with_forests(
    forests: usize,
    with_creature: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elk = scenario
        .add_creature_to_hand_from_oracle(P0, "Trickster's Elk", 2, 2, TRICKSTERS_ELK_ORACLE)
        .with_mana_cost(cost(2, vec![ManaCostShard::Green]))
        .from_oracle_text_with_keywords(&["Bestow"], TRICKSTERS_ELK_ORACLE)
        .id();
    let host = with_creature.then(|| scenario.add_creature(P0, "Host Creature", 2, 2).id());
    for _ in 0..forests {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();
    // The real card is an Enchantment Creature; a bestowed Aura that is not an
    // enchantment card would fall off as a state-based action.
    let obj = runner.state_mut().objects.get_mut(&elk).expect("elk");
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.base_card_types = obj.card_types.clone();
    (runner, elk, host)
}

/// CR 702.103a: two Forests pay Bestow {1}{G} but not the printed {2}{G}.
#[test]
fn trickster_elk_with_only_the_bestow_cost_is_offered_and_attaches() {
    let (mut runner, elk, host) = elk_with_forests(2, true);
    let host = host.expect("host");
    assert!(
        runner.state().objects[&elk]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Bestow(_))),
        "reach-guard: the Oracle build carries Bestow"
    );

    let variant = perform_offered_cast(&mut runner, elk);

    assert_eq!(variant, CastingVariant::Bestow);
    let state = runner.state();
    assert_eq!(state.objects[&elk].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&elk].attached_to,
        Some(AttachTarget::Object(host)),
        "bestowed: an Aura attached to the host"
    );
}

#[test]
fn trickster_elk_bestow_is_not_offered_without_a_host_or_mana() {
    // Reach-guard: two Forests and a creature.
    let (runner, elk, _) = elk_with_forests(2, true);
    assert!(offered_cast(&runner, elk).is_some());
    // No creature to enchant: the printed {2}{G} is also unaffordable.
    let (runner, elk, _) = elk_with_forests(2, false);
    assert!(
        offered_cast(&runner, elk).is_none(),
        "no creature to enchant"
    );
}

#[test]
fn trickster_elk_with_both_costs_affordable_still_asks() {
    let (mut runner, elk, _) = elk_with_forests(3, true);
    let offered = offered_cast(&runner, elk).expect("offered");
    runner.act(offered).expect("accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Bestow,
                ..
            }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// V9: Awaken and Cleave. SYNTHETIC: no printed card has such a keyword cost
// below its printed cost, so a real card's mana cost is restamped.
// ---------------------------------------------------------------------------

fn restamp_mana_cost(runner: &mut GameRunner, id: ObjectId, mana_cost: ManaCost) {
    let obj = runner.state_mut().objects.get_mut(&id).expect("object");
    obj.mana_cost = mana_cost;
}

/// SYNTHETIC: Rush of Ice (Tap target creature; Awaken 3-{4}{U}, printed {U})
/// restamped to a printed {9}. A creature an opponent controls is its spell target.
fn rush_of_ice_with(land_in_play: bool, pool_blue: usize) -> (GameRunner, ObjectId) {
    let (mut scenario, rush) = real_card_in_hand("Rush of Ice");
    scenario.add_creature(P1, "Opposing Creature", 2, 2);
    if land_in_play {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = build(scenario);
    restamp_mana_cost(&mut runner, rush, cost(9, vec![]));
    add_mana(&mut runner, ManaType::Blue, pool_blue);
    (runner, rush)
}

#[test]
fn synthetic_awaken_with_only_the_awaken_cost_is_offered_and_awakens() {
    let (mut runner, rush) = rush_of_ice_with(true, 5);

    let variant = perform_offered_cast(&mut runner, rush);

    assert_eq!(variant, CastingVariant::Awaken);
    assert_eq!(
        pool_total(&runner),
        0,
        "the awaken cost {{4}}{{U}} is spent"
    );
    let awakened = runner.state().objects.values().any(|o| {
        o.zone == Zone::Battlefield
            && o.card_types.core_types.contains(&CoreType::Land)
            && o.counters.get(&CounterType::Plus1Plus1).copied() == Some(3)
    });
    assert!(
        awakened,
        "Awaken puts three +1/+1 counters on the target land"
    );
}

#[test]
fn synthetic_awaken_without_a_land_is_not_offered() {
    let (runner, rush) = rush_of_ice_with(true, 5);
    assert!(
        offered_cast(&runner, rush).is_some(),
        "reach-guard: with a land"
    );
    let (runner, rush) = rush_of_ice_with(false, 5);
    assert!(offered_cast(&runner, rush).is_none(), "no land to awaken");
}

/// SYNTHETIC: Dig Up (Cleave {1}{B}{B}{G}) restamped to a printed {9}.
fn dig_up_with(pool_black: usize, pool_green: usize) -> (GameRunner, ObjectId) {
    let (scenario, dig_up) = real_card_in_hand("Dig Up");
    let mut runner = build(scenario);
    restamp_mana_cost(&mut runner, dig_up, cost(9, vec![]));
    add_mana(&mut runner, ManaType::Black, pool_black);
    add_mana(&mut runner, ManaType::Green, pool_green);
    (runner, dig_up)
}

#[test]
fn synthetic_cleave_with_only_the_cleave_cost_is_offered_and_cast_cleaved() {
    let (mut runner, dig_up) = dig_up_with(3, 1);
    assert!(
        runner.state().objects[&dig_up].cleave_variant.is_some(),
        "reach-guard: the cleave variant was parsed"
    );

    let variant = perform_offered_cast(&mut runner, dig_up);

    assert_eq!(variant, CastingVariant::Cleave);
    assert_eq!(
        pool_total(&runner),
        0,
        "the cleave cost {{1}}{{B}}{{B}}{{G}} is spent"
    );
}

#[test]
fn synthetic_cleave_short_of_the_cleave_cost_is_not_offered() {
    let (runner, dig_up) = dig_up_with(3, 1);
    assert!(
        offered_cast(&runner, dig_up).is_some(),
        "reach-guard: full cost"
    );
    let (runner, dig_up) = dig_up_with(2, 1);
    assert!(offered_cast(&runner, dig_up).is_none());
}

// ---------------------------------------------------------------------------
// V10: controls that are already correct and must stay so
// ---------------------------------------------------------------------------

/// Warp is chosen by `prepare_spell_cast` precedence, so it was never hidden.
#[test]
fn quantum_riddler_warp_stays_offered_with_only_the_warp_cost() {
    let (scenario, riddler) = real_card_in_hand("Quantum Riddler");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 2);
    assert!(
        offered_cast(&runner, riddler).is_some(),
        "{{1}}{{U}} pays Warp"
    );

    let (scenario, riddler) = real_card_in_hand("Quantum Riddler");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 1);
    assert!(
        offered_cast(&runner, riddler).is_none(),
        "one blue pays neither"
    );
}

/// Evoke is an unconditional candidate, the pattern the fix extends.
#[test]
fn mulldrifter_evoke_stays_offered_with_only_the_evoke_cost() {
    let (scenario, mulldrifter) = real_card_in_hand("Mulldrifter");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 3);
    assert!(
        offered_cast(&runner, mulldrifter).is_some(),
        "{{2}}{{U}} pays Evoke"
    );

    let (scenario, mulldrifter) = real_card_in_hand("Mulldrifter");
    let mut runner = build(scenario);
    add_mana(&mut runner, ManaType::Blue, 2);
    assert!(offered_cast(&runner, mulldrifter).is_none());
}
