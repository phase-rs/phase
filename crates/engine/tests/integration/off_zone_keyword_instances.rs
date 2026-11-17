//! Issue #10956: same-kind keyword grants to a card outside the battlefield
//! collapsed to the latest grant, so an earlier grant's ability was never
//! offered (Dregscape Sliver's unearth {2} lost to Sedris's unearth {2}{B}).
//!
//! CR 113.2c: "An object may also be granted additional abilities by a spell or
//! ability. If an object has multiple instances of the same ability, each
//! instance functions independently."
//! CR 702.84a (unearth), CR 702.141a (encore) and CR 702.29a (cycling) are
//! activated abilities, so each granted instance is its own activation.
//!
//! Verbatim Oracle text (Scryfall, 2026-11-16):
//! - Dregscape Sliver {1}{B}, Creature — Sliver 2/2.
//! - Sedris, the Traitor King {3}{U}{B}{R}, Legendary Creature — Zombie Warrior 5/5.
//! - Muscle Sliver {1}{G}, Creature — Sliver 1/1.
//! - Graywater's Fixer {2}{B}{R}, Creature — Lizard Mercenary 4/4.
//! - Wire Surgeons {4}{B}{B}, Creature — Human Artificer 6/5.
//! - Salvage Slasher {1}{B}, Artifact Creature — Human Rogue 1/1.
//! - Jo Grant {2}{W}, Legendary Creature — Human Soldier 3/2.
//! - Rhet-Tomb Mystic {1}{U}, Creature — Zombie Bird Wizard 2/1.
//! - Isamaru, Hound of Konda {W}, Legendary Creature — Dog 2/2.
//! - Murder {1}{B}{B}, Instant.

use engine::ai_support::legal_actions;
use engine::game::casting::activated_ability_definitions;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DREGSCAPE: &str = "Each Sliver creature card in your graveyard has unearth {2}.\nUnearth {2} ({2}: Return this card from your graveyard to the battlefield. It gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const SEDRIS: &str = "Each creature card in your graveyard has unearth {2}{B}. ({2}{B}: Return the card to the battlefield. The creature gains haste. Exile it at the beginning of the next end step or if it would leave the battlefield. Unearth only as a sorcery.)";
const MUSCLE_SLIVER: &str = "All Sliver creatures get +1/+1.";
const GRAYWATERS_FIXER: &str = "Each outlaw creature card in your graveyard has encore {X}, where X is its mana value. (Exile it and pay its encore cost: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";
const WIRE_SURGEONS: &str = "Fear (This creature can't be blocked except by artifact creatures and/or black creatures.)\nEach artifact creature card in your graveyard has encore. Its encore cost is equal to its mana cost. (Exile it and pay its mana cost: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";
const SALVAGE_SLASHER: &str = "This creature gets +1/+0 for each artifact card in your graveyard.";
const JO_GRANT: &str = "Each historic card in your hand has cycling {2}{W}. ({2}{W}, Discard that card: Draw a card.)\nWhenever you cycle a card, put a +1/+1 counter on Jo Grant.\nDoctor's companion (You can have two commanders if the other is the Doctor.)";
const RHET_TOMB_MYSTIC: &str = "Flying\nEach creature card in your hand has cycling {1}{U}. ({1}{U}, Discard that card: Draw a card.)";
const MURDER: &str = "Destroy target creature.";

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn pool(s: &mut GameScenario, player: PlayerId, colors: &[ManaType]) {
    let units = colors
        .iter()
        .map(|&c| ManaUnit::new(c, ObjectId(9_999), false, vec![]))
        .collect();
    s.with_mana_pool(player, units);
}

/// An identifiable spare that can't pay an activated ability (it is
/// restricted to spells), so it must survive every activation below.
fn spare() -> ManaUnit {
    ManaUnit::new(
        ManaType::Red,
        SPARE_SOURCE,
        false,
        vec![ManaRestriction::OnlyForSpell],
    )
}

const SPARE_SOURCE: ObjectId = ObjectId(8_888);

/// P0's pool: `colors` plus the spare.
fn p0_pool(s: &mut GameScenario, colors: &[ManaType]) {
    let mut units: Vec<ManaUnit> = colors
        .iter()
        .map(|&c| ManaUnit::new(c, ObjectId(9_999), false, vec![]))
        .collect();
    units.push(spare());
    s.with_mana_pool(P0, units);
}

/// Exactly the mana units that pay `cost`: colorless for the generic part,
/// the matching color for each colored shard.
fn exact_units(cost: &ManaCost) -> Vec<ManaType> {
    let ManaCost::Cost { generic, shards } = cost else {
        panic!("a fixed mana cost, got {cost:?}");
    };
    let mut units = vec![ManaType::Colorless; *generic as usize];
    units.extend(shards.iter().map(|shard| match shard {
        ManaCostShard::White => ManaType::White,
        ManaCostShard::Blue => ManaType::Blue,
        ManaCostShard::Black => ManaType::Black,
        ManaCostShard::Red => ManaType::Red,
        ManaCostShard::Green => ManaType::Green,
        other => panic!("unexpected shard {other:?}"),
    }));
    units
}

/// P0's pool as (color, source) pairs.
fn p0_pool_units(r: &GameRunner) -> Vec<(ManaType, ObjectId)> {
    r.state().players[0]
        .mana_pool
        .units()
        .map(|unit| (unit.color, unit.source_id))
        .collect()
}

/// The mana part of an activated ability's cost.
fn mana_part(cost: Option<&AbilityCost>) -> ManaCost {
    match cost {
        Some(AbilityCost::Mana { cost }) => cost.clone(),
        Some(AbilityCost::Composite { costs }) => costs
            .iter()
            .find_map(|c| match c {
                AbilityCost::Mana { cost } => Some(cost.clone()),
                _ => None,
            })
            .expect("the composite cost carries a mana part"),
        other => panic!("expected a mana cost, got {other:?}"),
    }
}

/// Every activation the engine offers for `source` (the client's action list),
/// with the mana part of its cost.
fn offers(r: &GameRunner, source: ObjectId) -> Vec<(usize, ManaCost)> {
    let defs = activated_ability_definitions(r.state(), source);
    legal_actions(r.state())
        .into_iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == source => Some(ability_index),
            _ => None,
        })
        .map(|index| {
            let (_, def) = defs
                .iter()
                .find(|(i, _)| *i == index)
                .expect("an offered index has a definition");
            (index, mana_part(def.cost.as_ref()))
        })
        .collect()
}

/// Asserts the offered costs for `source` are exactly `expected` (as a set).
fn assert_offered_costs(r: &GameRunner, source: ObjectId, expected: &[ManaCost]) {
    let mut offered: Vec<String> = offers(r, source)
        .into_iter()
        .map(|(_, c)| format!("{c:?}"))
        .collect();
    let mut wanted: Vec<String> = expected.iter().map(|c| format!("{c:?}")).collect();
    offered.sort();
    wanted.sort();
    assert_eq!(offered, wanted, "offered activation costs for {source:?}");
}

/// The offered activation of `source` whose mana cost is `wanted`.
fn offer_with_cost(r: &GameRunner, source: ObjectId, wanted: &ManaCost) -> usize {
    offers(r, source)
        .into_iter()
        .find(|(_, c)| c == wanted)
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("no offer costing {wanted:?} for {source:?}"))
}

/// Activates the offer costing `wanted` from a pool holding exactly `wanted`
/// plus the spare, asserts that payment left only the spare, then resolves.
fn activate_and_resolve(r: &mut GameRunner, source: ObjectId, wanted: &ManaCost) {
    activate_exact(r, source, wanted);
    r.advance_until_stack_empty();
}

/// Activates the offer costing `wanted` and asserts the pool afterwards holds
/// only the spare: every unit of `wanted`, and nothing else, was spent.
fn activate_exact(r: &mut GameRunner, source: ObjectId, wanted: &ManaCost) {
    let mut before = p0_pool_units(r);
    let mut expected_before: Vec<(ManaType, ObjectId)> = exact_units(wanted)
        .into_iter()
        .map(|c| (c, ObjectId(9_999)))
        .chain([(ManaType::Red, SPARE_SOURCE)])
        .collect();
    before.sort_by_key(|u| format!("{u:?}"));
    expected_before.sort_by_key(|u| format!("{u:?}"));
    assert_eq!(
        before, expected_before,
        "the board holds exactly {wanted:?} plus the spare"
    );

    let index = offer_with_cost(r, source, wanted);
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("the offered activation is legal");
    assert_eq!(
        p0_pool_units(r),
        vec![(ManaType::Red, SPARE_SOURCE)],
        "paying {wanted:?} spends exactly its units and leaves the spare"
    );
}

fn assert_settled(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "the stack must be empty");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        r.state().waiting_for
    );
}

/// Grantor indices to put on the battlefield, in timestamp order.
const BOTH_ORDERS: [[usize; 2]; 2] = [[0, 1], [1, 0]];

// ---------------------------------------------------------------------------
// Unearth: Dregscape Sliver + Sedris
// ---------------------------------------------------------------------------

fn add_dregscape(s: &mut GameScenario) -> ObjectId {
    s.add_creature(P0, "Dregscape Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Black]))
        .from_oracle_text(DREGSCAPE)
        .id()
}

fn add_sedris(s: &mut GameScenario) -> ObjectId {
    s.add_creature(P0, "Sedris, the Traitor King", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Warrior"])
        .with_mana_cost(cost(
            3,
            vec![
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Red,
            ],
        ))
        .from_oracle_text(SEDRIS)
        .id()
}

/// Dregscape Sliver (index 0) and/or Sedris (index 1) on the battlefield in
/// `grantors` order, Muscle Sliver in the graveyard, `p0` plus the spare in
/// P0's pool. With `murder`, P1 also holds Murder and exactly {1}{B}{B}.
struct UnearthBoard {
    runner: GameRunner,
    muscle: ObjectId,
    sedris: Option<ObjectId>,
    murder: Option<ObjectId>,
}

fn unearth_board_with(grantors: &[usize], p0: &[ManaType], murder: bool) -> UnearthBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mut sedris = None;
    for &g in grantors {
        match g {
            0 => {
                add_dregscape(&mut s);
            }
            _ => sedris = Some(add_sedris(&mut s)),
        }
    }
    let muscle = s
        .add_creature_to_graveyard(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Green]))
        .from_oracle_text(MUSCLE_SLIVER)
        .id();
    p0_pool(&mut s, p0);
    let murder = murder.then(|| {
        let id = s
            .add_spell_to_hand(P1, "Murder", true)
            .with_mana_cost(cost(1, vec![ManaCostShard::Black, ManaCostShard::Black]))
            .from_oracle_text(MURDER)
            .id();
        pool(&mut s, P1, &[ManaType::Black; 3]);
        id
    });
    UnearthBoard {
        runner: s.build(),
        muscle,
        sedris,
        murder,
    }
}

/// P0's pool that affords both unearth costs ({2} and {2}{B}).
const UNEARTH_FUNDS: [ManaType; 3] = [ManaType::Colorless, ManaType::Colorless, ManaType::Black];

fn unearth_board(grantors: &[usize], p0: &[ManaType]) -> (GameRunner, ObjectId) {
    let b = unearth_board_with(grantors, p0, false);
    (b.runner, b.muscle)
}

fn unearth_two() -> ManaCost {
    cost(2, vec![])
}

fn unearth_two_black() -> ManaCost {
    cost(2, vec![ManaCostShard::Black])
}

fn assert_unearthed(r: &GameRunner, card: ObjectId) {
    assert_eq!(r.state().objects[&card].zone, Zone::Battlefield);
    assert!(
        object_has_effective_keyword_kind(r.state(), card, KeywordKind::Haste),
        "CR 702.84a: the unearthed card gains haste"
    );
}

#[test]
fn each_unearth_grant_alone_offers_its_own_cost() {
    let (r, muscle) = unearth_board(&[0], &UNEARTH_FUNDS);
    assert_offered_costs(&r, muscle, &[unearth_two()]);
    let (r, muscle) = unearth_board(&[1], &UNEARTH_FUNDS);
    assert_offered_costs(&r, muscle, &[unearth_two_black()]);
}

/// CR 113.2c: both grants are offered whichever grantor is newer, and each one
/// is paid exactly and resolves through the production activation path.
#[test]
fn dregscape_and_sedris_unearth_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        let (r, muscle) = unearth_board(&order, &UNEARTH_FUNDS);
        assert_offered_costs(&r, muscle, &[unearth_two(), unearth_two_black()]);
        for wanted in [unearth_two(), unearth_two_black()] {
            let (mut r, muscle) = unearth_board(&order, &exact_units(&wanted));
            activate_and_resolve(&mut r, muscle, &wanted);
            assert_unearthed(&r, muscle);
            assert_settled(&r);
        }
    }
}

/// CR 113.7a: once Sedris's unearth is activated it resolves even if P1
/// destroys Sedris in response.
#[test]
fn sedris_unearth_resolves_after_p1_murders_sedris_in_response() {
    for order in BOTH_ORDERS {
        let UnearthBoard {
            runner: mut r,
            muscle,
            sedris,
            murder,
        } = unearth_board_with(&order, &exact_units(&unearth_two_black()), true);
        let (sedris, murder) = (sedris.unwrap(), murder.unwrap());
        assert_offered_costs(&r, muscle, &[unearth_two(), unearth_two_black()]);
        activate_exact(&mut r, muscle, &unearth_two_black());
        assert_eq!(r.state().stack.len(), 1, "unearth is on the stack");

        r.act(GameAction::PassPriority).expect("P0 passes");
        cast_murder(&mut r, murder, sedris);
        assert_eq!(
            r.state().objects[&sedris].zone,
            Zone::Graveyard,
            "Murder destroyed Sedris"
        );

        r.advance_until_stack_empty();
        assert_unearthed(&r, muscle);
        assert_settled(&r);
    }
}

/// With Sedris destroyed before activation, only Dregscape Sliver's unearth
/// {2} remains (P0 can still afford {2}{B}, so its absence is the grant
/// ending), and it resolves.
#[test]
fn only_dregscape_unearth_remains_after_p1_murders_sedris() {
    for order in BOTH_ORDERS {
        let UnearthBoard {
            runner: mut r,
            muscle,
            sedris,
            murder,
        } = unearth_board_with(&order, &UNEARTH_FUNDS, true);
        let (sedris, murder) = (sedris.unwrap(), murder.unwrap());
        assert_offered_costs(&r, muscle, &[unearth_two(), unearth_two_black()]);

        r.act(GameAction::PassPriority).expect("P0 passes");
        cast_murder(&mut r, murder, sedris);
        assert_eq!(r.state().objects[&sedris].zone, Zone::Graveyard);
        assert_settled(&r);
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 has priority again"
        );

        assert_offered_costs(&r, muscle, &[unearth_two()]);
        let index = offer_with_cost(&r, muscle, &unearth_two());
        r.act(GameAction::ActivateAbility {
            source_id: muscle,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Dregscape's unearth is offered");
        // {2} is paid from {C}{C}{B}: the spare survives and exactly one of the
        // three payable units remains.
        let after = p0_pool_units(&r);
        assert!(
            after.contains(&(ManaType::Red, SPARE_SOURCE)),
            "the spare survives"
        );
        assert_eq!(
            after.len(),
            2,
            "exactly two of {{C}}{{C}}{{B}} were spent, got {after:?}"
        );
        r.advance_until_stack_empty();
        assert_unearthed(&r, muscle);
        assert_settled(&r);
    }
}

/// P1, holding priority, casts Murder on `target`; both players pass so it
/// resolves.
fn cast_murder(r: &mut GameRunner, murder: ObjectId, target: ObjectId) {
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority, got {:?}",
        r.state().waiting_for
    );
    r.cast(murder).target_object(target).resolve();
}

// ---------------------------------------------------------------------------
// Encore: Graywater's Fixer + Wire Surgeons
// ---------------------------------------------------------------------------

/// Graywater's Fixer (index 0) and/or Wire Surgeons (index 1) in `grantors`
/// order, Salvage Slasher in the graveyard, `p0` plus the spare in P0's pool.
fn encore_board(grantors: &[usize], p0: &[ManaType]) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for &g in grantors {
        match g {
            0 => {
                s.add_creature(P0, "Graywater's Fixer", 4, 4)
                    .with_subtypes(vec!["Lizard", "Mercenary"])
                    .with_mana_cost(cost(2, vec![ManaCostShard::Black, ManaCostShard::Red]))
                    .from_oracle_text(GRAYWATERS_FIXER);
            }
            _ => {
                s.add_creature(P0, "Wire Surgeons", 6, 5)
                    .with_subtypes(vec!["Human", "Artificer"])
                    .with_mana_cost(cost(4, vec![ManaCostShard::Black, ManaCostShard::Black]))
                    .from_oracle_text_with_keywords(&["Fear"], WIRE_SURGEONS);
            }
        }
    }
    let slasher = s
        .add_creature_to_graveyard(P0, "Salvage Slasher", 1, 1)
        .as_artifact_creature()
        .with_subtypes(vec!["Human", "Rogue"])
        .with_mana_cost(cost(1, vec![ManaCostShard::Black]))
        .from_oracle_text(SALVAGE_SLASHER)
        .id();
    p0_pool(&mut s, p0);
    (s.build(), slasher)
}

/// P0's pool that affords both encore costs ({2} and {1}{B}).
const ENCORE_FUNDS: [ManaType; 2] = [ManaType::Colorless, ManaType::Black];

/// Graywater's Fixer: encore {X}, X = Salvage Slasher's mana value 2.
fn encore_mana_value() -> ManaCost {
    cost(2, vec![])
}

/// Wire Surgeons: encore equal to Salvage Slasher's mana cost {1}{B}.
fn encore_mana_cost() -> ManaCost {
    cost(1, vec![ManaCostShard::Black])
}

#[test]
fn each_encore_grant_alone_offers_its_own_cost() {
    let (r, slasher) = encore_board(&[0], &ENCORE_FUNDS);
    assert_offered_costs(&r, slasher, &[encore_mana_value()]);
    let (r, slasher) = encore_board(&[1], &ENCORE_FUNDS);
    assert_offered_costs(&r, slasher, &[encore_mana_cost()]);
}

/// CR 113.2c + CR 702.141a: both encore grants are offered, and each is paid
/// exactly, exiles the card and creates a token copy for the one opponent.
#[test]
fn fixer_and_wire_surgeons_encore_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        let (r, slasher) = encore_board(&order, &ENCORE_FUNDS);
        assert_offered_costs(&r, slasher, &[encore_mana_value(), encore_mana_cost()]);
        for wanted in [encore_mana_value(), encore_mana_cost()] {
            let (mut r, slasher) = encore_board(&order, &exact_units(&wanted));
            activate_and_resolve(&mut r, slasher, &wanted);
            assert_eq!(r.state().objects[&slasher].zone, Zone::Exile);
            let tokens = r
                .state()
                .battlefield
                .iter()
                .filter(|id| {
                    let obj = &r.state().objects[*id];
                    obj.is_token && obj.name == "Salvage Slasher" && obj.controller == P0
                })
                .count();
            assert_eq!(tokens, 1, "one token copy for the single opponent");
            assert_settled(&r);
        }
    }
}

// ---------------------------------------------------------------------------
// Cycling: Jo Grant + Rhet-Tomb Mystic
// ---------------------------------------------------------------------------

/// Jo Grant (index 0) and/or Rhet-Tomb Mystic (index 1) in `grantors` order,
/// Isamaru in hand, an Island on top of P0's library, `p0` plus the spare in
/// P0's pool. Returns (runner, Isamaru, the Island).
fn cycling_board(grantors: &[usize], p0: &[ManaType]) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for &g in grantors {
        match g {
            0 => {
                s.add_creature(P0, "Jo Grant", 3, 2)
                    .as_legendary()
                    .with_subtypes(vec!["Human", "Soldier"])
                    .with_mana_cost(cost(2, vec![ManaCostShard::White]))
                    .from_oracle_text(JO_GRANT);
            }
            _ => {
                s.add_creature(P0, "Rhet-Tomb Mystic", 2, 1)
                    .with_subtypes(vec!["Zombie", "Bird", "Wizard"])
                    .with_mana_cost(cost(1, vec![ManaCostShard::Blue]))
                    .from_oracle_text_with_keywords(&["Flying"], RHET_TOMB_MYSTIC);
            }
        }
    }
    let isamaru = s
        .add_creature_to_hand(P0, "Isamaru, Hound of Konda", 2, 2)
        .as_legendary()
        .with_subtypes(vec!["Dog"])
        .with_mana_cost(cost(0, vec![ManaCostShard::White]))
        .id();
    let island = s.add_card_to_library_top(P0, "Island");
    p0_pool(&mut s, p0);
    (s.build(), isamaru, island)
}

/// P0's pool that affords both cycling costs ({2}{W} and {1}{U}).
const CYCLING_FUNDS: [ManaType; 3] = [ManaType::Colorless, ManaType::White, ManaType::Blue];

fn cycling_jo_grant() -> ManaCost {
    cost(2, vec![ManaCostShard::White])
}

fn cycling_rhet_tomb() -> ManaCost {
    cost(1, vec![ManaCostShard::Blue])
}

#[test]
fn each_cycling_grant_alone_offers_its_own_cost() {
    let (r, isamaru, _) = cycling_board(&[0], &CYCLING_FUNDS);
    assert_offered_costs(&r, isamaru, &[cycling_jo_grant()]);
    let (r, isamaru, _) = cycling_board(&[1], &CYCLING_FUNDS);
    assert_offered_costs(&r, isamaru, &[cycling_rhet_tomb()]);
}

/// CR 113.2c + CR 702.29a: both cycling grants are offered, and each is paid
/// exactly, discards the card and draws.
#[test]
fn jo_grant_and_rhet_tomb_cycling_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        let (r, isamaru, _) = cycling_board(&order, &CYCLING_FUNDS);
        assert_offered_costs(&r, isamaru, &[cycling_jo_grant(), cycling_rhet_tomb()]);
        for wanted in [cycling_jo_grant(), cycling_rhet_tomb()] {
            let (mut r, isamaru, island) = cycling_board(&order, &exact_units(&wanted));
            activate_and_resolve(&mut r, isamaru, &wanted);
            assert_eq!(r.state().objects[&isamaru].zone, Zone::Graveyard);
            assert_eq!(
                r.state().objects[&island].zone,
                Zone::Hand,
                "cycling draws the library top"
            );
            assert_settled(&r);
        }
    }
}
