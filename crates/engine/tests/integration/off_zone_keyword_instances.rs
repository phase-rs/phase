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
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
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

fn pool_total(r: &GameRunner, player: PlayerId) -> usize {
    r.state().players[player.0 as usize].mana_pool.total()
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

/// Activates the offer costing `wanted`, resolves the stack, and asserts that
/// exactly `wanted`'s mana value left P0's pool.
fn activate_and_resolve(r: &mut GameRunner, source: ObjectId, wanted: &ManaCost) {
    let index = offer_with_cost(r, source, wanted);
    let before = pool_total(r, P0);
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("the offered activation is legal");
    assert_eq!(
        before - pool_total(r, P0),
        wanted.mana_value() as usize,
        "the activation spends exactly {wanted:?}"
    );
    r.advance_until_stack_empty();
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
/// `grantors` order, Muscle Sliver in the graveyard, {B}{B}{B} in P0's pool.
/// With `murder`, P1 also holds Murder and exactly {1}{B}{B}.
struct UnearthBoard {
    runner: GameRunner,
    muscle: ObjectId,
    sedris: Option<ObjectId>,
    murder: Option<ObjectId>,
}

fn unearth_board_with(grantors: &[usize], murder: bool) -> UnearthBoard {
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
    pool(&mut s, P0, &[ManaType::Black; 3]);
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

fn unearth_board(grantors: &[usize]) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let b = unearth_board_with(grantors, false);
    (b.runner, b.muscle, b.sedris)
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
    let (r, muscle, _) = unearth_board(&[0]);
    assert_offered_costs(&r, muscle, &[unearth_two()]);
    let (r, muscle, _) = unearth_board(&[1]);
    assert_offered_costs(&r, muscle, &[unearth_two_black()]);
}

/// CR 113.2c: both grants are offered whichever grantor is newer, and each one
/// is paid and resolves through the production activation path.
#[test]
fn dregscape_and_sedris_unearth_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        for wanted in [unearth_two(), unearth_two_black()] {
            let (mut r, muscle, _) = unearth_board(&order);
            assert_offered_costs(&r, muscle, &[unearth_two(), unearth_two_black()]);
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
        } = unearth_board_with(&order, true);
        let (sedris, murder) = (sedris.unwrap(), murder.unwrap());
        let index = offer_with_cost(&r, muscle, &unearth_two_black());
        r.act(GameAction::ActivateAbility {
            source_id: muscle,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Sedris's unearth is offered");
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
/// {2} remains, and it resolves.
#[test]
fn only_dregscape_unearth_remains_after_p1_murders_sedris() {
    for order in BOTH_ORDERS {
        let UnearthBoard {
            runner: mut r,
            muscle,
            sedris,
            murder,
        } = unearth_board_with(&order, true);
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
        activate_and_resolve(&mut r, muscle, &unearth_two());
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
/// order, Salvage Slasher in the graveyard, {B}{B}{B} in P0's pool.
fn encore_board(grantors: &[usize]) -> (GameRunner, ObjectId) {
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
    pool(&mut s, P0, &[ManaType::Black; 3]);
    (s.build(), slasher)
}

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
    let (r, slasher) = encore_board(&[0]);
    assert_offered_costs(&r, slasher, &[encore_mana_value()]);
    let (r, slasher) = encore_board(&[1]);
    assert_offered_costs(&r, slasher, &[encore_mana_cost()]);
}

/// CR 113.2c + CR 702.141a: both encore grants are offered, and each exiles the
/// card and creates a token copy for the one opponent.
#[test]
fn fixer_and_wire_surgeons_encore_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        for wanted in [encore_mana_value(), encore_mana_cost()] {
            let (mut r, slasher) = encore_board(&order);
            assert_offered_costs(&r, slasher, &[encore_mana_value(), encore_mana_cost()]);
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
/// Isamaru in hand, an Island on top of P0's library, {W}{W}{U}{U} in P0's pool.
/// Returns (runner, Isamaru, the Island).
fn cycling_board(grantors: &[usize]) -> (GameRunner, ObjectId, ObjectId) {
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
    pool(
        &mut s,
        P0,
        &[
            ManaType::White,
            ManaType::White,
            ManaType::Blue,
            ManaType::Blue,
        ],
    );
    (s.build(), isamaru, island)
}

fn cycling_jo_grant() -> ManaCost {
    cost(2, vec![ManaCostShard::White])
}

fn cycling_rhet_tomb() -> ManaCost {
    cost(1, vec![ManaCostShard::Blue])
}

#[test]
fn each_cycling_grant_alone_offers_its_own_cost() {
    let (r, isamaru, _) = cycling_board(&[0]);
    assert_offered_costs(&r, isamaru, &[cycling_jo_grant()]);
    let (r, isamaru, _) = cycling_board(&[1]);
    assert_offered_costs(&r, isamaru, &[cycling_rhet_tomb()]);
}

/// CR 113.2c + CR 702.29a: both cycling grants are offered, and each discards
/// the card and draws.
#[test]
fn jo_grant_and_rhet_tomb_cycling_grants_are_both_offered_and_each_resolves() {
    for order in BOTH_ORDERS {
        for wanted in [cycling_jo_grant(), cycling_rhet_tomb()] {
            let (mut r, isamaru, island) = cycling_board(&order);
            assert_offered_costs(&r, isamaru, &[cycling_jo_grant(), cycling_rhet_tomb()]);
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
