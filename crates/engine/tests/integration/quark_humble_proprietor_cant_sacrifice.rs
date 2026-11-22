//! Quark, Humble Proprietor (TRK):
//!
//! "You can't sacrifice Treasure tokens.
//!  At the beginning of each upkeep, create a Treasure token."
//!
//! CR 701.21a + CR 101.2: a player-scoped "can't sacrifice <filter>" static
//! stops the sacrifice whether it is a cost (CR 118.3) or an effect, and only
//! for permanents matching the filter and players inside the scope.

use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::coverage::card_face_gaps;
use engine::game::mana_abilities::can_activate_mana_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

/// Reach guard: Quark parsed to a real prohibition, with no residue.
fn assert_quark_prohibition(runner: &GameRunner, quark: ObjectId) {
    let obj = &runner.state().objects[&quark];
    assert!(
        obj.static_definitions
            .iter_all()
            .any(|d| matches!(d.mode, StaticMode::CantSacrifice { .. })),
        "Quark must parse a CantSacrifice static, got {:?}",
        obj.static_definitions
    );
    assert!(!obj.abilities.iter().any(contains_unimplemented));
}

/// CR 111.10a: the Treasure token Quark's upkeep trigger creates — found on
/// the battlefield (`is_token`, subtype Treasure).
fn treasure_token(runner: &GameRunner) -> ObjectId {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Treasure")
            })
        })
        .expect("Quark's upkeep trigger must have created a Treasure token")
}

/// Quark on the battlefield at the untap step; the real runner then advances
/// into the upkeep (CR 503), so Quark's printed trigger ("At the beginning of
/// each upkeep, create a Treasure token.") creates the token through the
/// production trigger pipeline. Returns the runner and the created token.
fn quark_upkeep_treasure() -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    (runner, treasure)
}

fn on_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().battlefield.contains(&id)
}

fn mana_available(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn activate(
    runner: &mut GameRunner,
    source: ObjectId,
) -> Result<(), engine::game::engine::EngineError> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
}

#[test]
fn treasure_token_cannot_be_sacrificed_for_mana() {
    let (mut runner, treasure) = quark_upkeep_treasure();
    assert!(
        activate(&mut runner, treasure).is_err(),
        "sacrificing a Treasure token as a cost is prohibited while Quark is out"
    );
    assert!(on_battlefield(&runner, treasure));
    assert_eq!(mana_available(&runner), 0);
}

#[test]
fn nontoken_treasure_can_still_be_sacrificed() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    // Buried Treasure is a printed nontoken Treasure: a {2} artifact whose
    // subtype is Treasure and whose first ability is the Treasure mana
    // ability "{T}, Sacrifice this artifact: Add one mana of any color.".
    let buried = scenario.add_real_card(P0, "Buried Treasure", Zone::Battlefield, db);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    activate(&mut runner, buried)
        .expect("a nontoken Treasure is outside the 'Treasure tokens' filter");
    assert!(!on_battlefield(&runner, buried), "it was sacrificed");
}

/// CR 101.2 + CR 701.21a: Quark's prohibition names its controller ("You can't
/// sacrifice Treasure tokens"), so a Treasure token an opponent controls can
/// still be sacrificed. P1's printed Wily Goblin creates the token through its
/// enters trigger; P1 then sacrifices it for mana while Quark is out (the sacrifice is the cost).
#[test]
fn opponents_treasure_token_can_still_be_sacrificed() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let goblin = scenario.add_real_card(P1, "Wily Goblin", Zone::Hand, db);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    // CR 117.1a: a creature spell is cast on its caster's own turn, so P1 takes it.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    for _ in 0..2 {
        runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    assert_eq!(
        runner.state().objects[&treasure].controller,
        P1,
        "reach: the token is the opponent's"
    );
    activate(&mut runner, treasure)
        .expect("Quark's prohibition does not reach an opponent's Treasure token");
    assert!(!on_battlefield(&runner, treasure), "it was sacrificed");
}

/// Quark plus a Treasure token (created by Quark's own upkeep trigger) and
/// (optionally) a nontoken Sol Ring; P0 then casts Angrath's Rampage's
/// artifact mode targeting themselves.
fn rampage_on_self(with_sol_ring: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let sol_ring =
        with_sol_ring.then(|| scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db));
    let rampage = scenario.add_real_card(P0, "Angrath's Rampage", Zone::Hand, db);
    // CR 504.1: P0 draws during the draw step on the way to their main phase.
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    // Quark's printed upkeep trigger creates the Treasure token for real.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    // CR 117.1a + CR 307.1: Angrath's Rampage is a sorcery, so continue to
    // P0's precombat main, then fund its printed {B}{R} cost there (the pool
    // drains at each step/phase end, CR 500.5).
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Black,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.cast(rampage).modes(&[0]).target_player(P0).resolve();
    (runner, treasure, sol_ring)
}

/// CR 701.21a + CR 609.3: "sacrifice an artifact" offers only artifacts the
/// player can sacrifice. The Treasure token is not a legal choice, so the Sol
/// Ring is the one sacrificed and no prompt can pick the Treasure.
#[test]
fn sacrifice_an_artifact_effect_skips_treasure_token() {
    let (runner, treasure, sol_ring) = rampage_on_self(true);
    let sol_ring = sol_ring.expect("sol ring");
    assert!(
        !matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::EffectZoneChoice { .. }
        ),
        "only one artifact is a legal choice, so there is nothing to choose between"
    );
    assert!(!on_battlefield(&runner, sol_ring), "Sol Ring is sacrificed");
    assert!(
        on_battlefield(&runner, treasure),
        "the Treasure token is kept"
    );
}

/// CR 609.3: with only a Treasure token, the effect does as much as possible —
/// nothing — rather than "sacrificing" the token.
#[test]
fn sacrifice_an_artifact_effect_with_only_treasure_does_nothing() {
    let (runner, treasure, _) = rampage_on_self(false);
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "no legal artifact exists, so the effect opens no prompt: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(on_battlefield(&runner, treasure));
}

/// CR 701.21a + CR 609.3: "Each player sacrifices all artifacts, ..." names its
/// objects instead of asking for a choice. The nontoken Sol Ring is sacrificed;
/// the Treasure token Quark protects is not.
#[test]
fn wave_of_vitriol_sacrifices_every_artifact_except_the_treasure_token() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let wave = scenario.add_real_card(P0, "Wave of Vitriol", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    // Quark's printed upkeep trigger creates the Treasure token for real.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    // CR 117.1a: Wave of Vitriol is a sorcery; fund its printed {5}{G}{G} in P0's
    // precombat main (the pool drains at each step and phase end, CR 500.5).
    runner.advance_to_phase(Phase::PreCombatMain);
    for color in [ManaType::Green; 2]
        .into_iter()
        .chain([ManaType::Colorless; 5])
    {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner.cast(wave).resolve();
    runner.advance_until_stack_empty();
    assert!(
        !on_battlefield(&runner, sol_ring),
        "the nontoken artifact is sacrificed"
    );
    assert!(
        on_battlefield(&runner, treasure),
        "the Treasure token is kept"
    );
    assert!(matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
}

/// P0's Treasure count: Treasure tokens `player` controls.
fn treasures_controlled_by(
    runner: &GameRunner,
    player: engine::types::player::PlayerId,
) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player
                    && obj.is_token
                    && obj.card_types.subtypes.iter().any(|s| s == "Treasure")
            })
        })
        .collect()
}

/// CR 701.21a + CR 608.2d: "Each opponent sacrifices an artifact." (Visions of
/// Ruin) is a player-scoped choice pool. P1 controls Quark, a nontoken Sol
/// Ring and the Treasure token Quark's upkeep trigger made, so the Treasure is
/// not a legal choice: the Sol Ring is the only candidate and is sacrificed
/// without a prompt, the Treasure stays, and P0 creates one Treasure for the
/// one artifact sacrificed.
#[test]
fn each_opponent_sacrifices_an_artifact_never_offers_the_quark_controllers_treasure() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P1, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    let visions = scenario.add_real_card(P0, "Visions of Ruin", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    // "At the beginning of each upkeep" reaches P0's upkeep: P1's Quark makes
    // P1 a Treasure token.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasures = treasures_controlled_by(&runner, P1);
    assert_eq!(treasures.len(), 1, "reach: P1 has Quark's Treasure token");
    let treasure = treasures[0];
    // CR 117.1a + CR 307.1: Visions of Ruin is a sorcery; fund its printed
    // {3}{R} in P0's precombat main (the pool drains at each step end, CR 500.5).
    runner.advance_to_phase(Phase::PreCombatMain);
    for color in [
        ManaType::Red,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ] {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner.cast(visions).resolve();
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the Treasure is not a legal choice, so no prompt can open: {:?}",
        runner.state().waiting_for
    );
    assert!(!on_battlefield(&runner, sol_ring), "Sol Ring is sacrificed");
    assert!(
        on_battlefield(&runner, treasure),
        "the Treasure token Quark protects is kept"
    );
    assert_eq!(
        treasures_controlled_by(&runner, P0).len(),
        1,
        "one artifact was sacrificed, so P0 creates one Treasure"
    );
}

/// CR 702.21a + CR 118.3 + CR 701.21a: Ulamog, the Defiler's "Ward—Sacrifice
/// two permanents." The payer is Quark's controller, who controls Quark, an
/// Island and the Treasure token Quark made. The Treasure is not a legal
/// payment candidate: the prompt offers Quark and the Island only, and paying
/// with them (one at a time, Island first) leaves the Treasure on the
/// battlefield while Lightning Bolt resolves.
#[test]
fn ward_sacrifice_never_offers_the_quark_controllers_treasure() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let ulamog = scenario.add_real_card(P1, "Ulamog, the Defiler", Zone::Battlefield, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.cast(bolt).target_objects(&[ulamog]).commit();
    runner.advance_until_stack_empty();
    let WaitingFor::UnlessPayment { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Ulamog's Ward must prompt the Bolt's controller, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 accepts the Ward cost");
    let WaitingFor::WardSacrificeChoice {
        player,
        permanents,
        remaining,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the Ward sacrifice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(remaining, 2, "two sacrifices are owed");
    assert!(
        permanents.contains(&quark) && permanents.contains(&island),
        "Quark and the Island are legal payments, got {permanents:?}"
    );
    assert!(
        !permanents.contains(&treasure),
        "the Treasure token cannot be sacrificed, so it is not offered: {permanents:?}"
    );
    // A plain "sacrifice N permanents" Ward is paid one permanent at a time.
    // The Island goes first so Quark, the source of the bar, is still on the
    // battlefield when the prompt re-opens.
    runner
        .act(GameAction::SelectCards {
            cards: vec![island],
        })
        .expect("the Island pays the first sacrifice");
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            permanents,
            remaining: 1,
            ..
        } => assert_eq!(
            permanents,
            &vec![quark],
            "the re-prompt offers Quark only; the Treasure is still barred"
        ),
        other => panic!("one sacrifice must remain, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![quark] })
        .expect("Quark pays the second sacrifice");
    runner.advance_until_stack_empty();
    assert!(
        on_battlefield(&runner, treasure),
        "the Treasure token was not sacrificed"
    );
    assert!(!on_battlefield(&runner, quark) && !on_battlefield(&runner, island));
    assert_eq!(
        runner.state().objects[&ulamog].damage_marked,
        3,
        "the Ward was paid, so Lightning Bolt resolves"
    );
}

/// CR 701.21a + CR 118.3: the offered-action list reads the same prohibition
/// as payment. With Quark out, the Treasure token's "{T}, Sacrifice this
/// artifact" mana ability is not offered, while the printed nontoken Treasure
/// Buried Treasure beside it is.
#[test]
fn treasure_token_mana_ability_is_not_offered_while_quark_is_out() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let buried = scenario.add_real_card(P0, "Buried Treasure", Zone::Battlefield, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    let (_, _, grouped) = legal_actions_full(runner.state());
    let offers_activation = |id: ObjectId| {
        grouped.get(&id).is_some_and(|actions| {
            actions.iter().any(|a| {
                matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == id)
                    || matches!(
                        a,
                        GameAction::ActivateManaSource { selection }
                            if selection.source.object_id == id
                    )
            })
        })
    };
    assert!(
        offers_activation(buried),
        "control: the nontoken Treasure's sacrifice ability is offered: {:?}",
        runner.state().waiting_for
    );
    assert!(
        !offers_activation(treasure),
        "the Treasure token's sacrifice ability is not offered"
    );
}

/// CR 118.3 + CR 701.21a: the mana-readiness gate (`can_activate_mana_ability_now`,
/// the cheap pre-payment check that decides whether a Treasure counts as an
/// available mana source) reads Quark's prohibition through the cost-payability
/// authority, so a barred Treasure token is never reported as ready and then
/// fails at payment. Paired positive in the same state: the nontoken Treasure
/// Buried Treasure, with the same printed mana ability, is ready.
#[test]
fn mana_readiness_gate_reads_quarks_prohibition() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let buried = scenario.add_real_card(P0, "Buried Treasure", Zone::Battlefield, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    // Quark's printed upkeep trigger creates the Treasure token for real.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    runner.advance_to_phase(Phase::PreCombatMain);

    let ready = |id: ObjectId| {
        let def = runner.state().objects[&id].abilities[0].clone();
        // REACH: index 0 is the Treasure mana ability on both permanents, so a
        // `false` below is the prohibition and not a different ability.
        assert!(
            matches!(def.effect.as_ref(), Effect::Mana { .. }),
            "abilities[0] of {id:?} must be the Treasure mana ability: {def:?}"
        );
        can_activate_mana_ability_now(runner.state(), P0, id, 0, &def)
    };
    assert!(
        ready(buried),
        "control: the nontoken Treasure is outside the 'Treasure tokens' filter"
    );
    assert!(
        !ready(treasure),
        "a Treasure token cannot be sacrificed, so its mana ability is not ready"
    );
}

/// CR 701.21a: a card counts as supported only when no line of it is a coverage
/// gap. Both of Quark's lines (the sacrifice prohibition and the upkeep Treasure
/// trigger) are read from the production-written fixture entry; the Treasure
/// tests above are the runtime witnesses for the same card.
#[test]
fn quark_whole_card_has_no_coverage_gap() {
    let db = shared_card_db().expect("integration card fixture must load");
    let face = db
        .get_face_by_name("Quark, Humble Proprietor")
        .expect("Quark is in the integration fixture");
    // REACH: both printed lines were parsed into typed abilities.
    assert!(
        face.static_abilities
            .iter()
            .any(|d| matches!(d.mode, StaticMode::CantSacrifice { .. })),
        "the prohibition line parses to CantSacrifice: {:?}",
        face.static_abilities
    );
    assert!(
        !face.triggers.is_empty(),
        "the upkeep trigger line parses: {:?}",
        face.triggers
    );
    assert!(
        card_face_gaps(face).is_empty(),
        "Quark must have no coverage gaps: {:?}",
        card_face_gaps(face)
    );
}

/// CR 118.3 + CR 701.21a + CR 601.2g: whether a spell is offered as castable
/// is decided before any payment is attempted, from the mana the player could
/// make. A Treasure token under Quark can't be sacrificed, so it can't pay for
/// Lightning Bolt ({R}); the nontoken Treasure Buried Treasure, with the same
/// printed mana ability, can (the control, same board shape otherwise).
#[test]
fn spell_is_offered_only_when_a_sacrifice_legal_treasure_can_pay() {
    let db = shared_card_db().expect("integration card fixture must load");
    let offers_cast = |runner: &GameRunner, spell: ObjectId| {
        let (_, _, grouped) = legal_actions_full(runner.state());
        grouped.get(&spell).is_some_and(|actions| {
            actions.iter().any(
                |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == spell),
            )
        })
    };

    // Control: Quark out, only the nontoken Treasure to pay for the Bolt.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Buried Treasure", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        offers_cast(&runner, bolt),
        "control: a nontoken Treasure can pay for Lightning Bolt under Quark"
    );

    // Same shape, but the only Treasure is Quark's own token.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(on_battlefield(&runner, treasure), "REACH: the token is out");
    assert!(
        !offers_cast(&runner, bolt),
        "a Treasure token Quark bars from being sacrificed cannot pay for Lightning Bolt"
    );
}

/// CR 118.3 + CR 701.21a: `player_cant_sacrifice_as_cost` is the shared
/// authority the cost-payability gates (planning-mode readiness, the
/// self-sacrifice payment arms, Ward-style payments) read. It reports Quark's
/// prohibition for the Treasure token, not for the nontoken Treasure Buried
/// Treasure beside it, and not for a player Quark's static does not scope.
#[test]
fn sacrifice_cost_authority_reads_quarks_prohibition_per_permanent_and_player() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let quark = scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let buried = scenario.add_real_card(P0, "Buried Treasure", Zone::Battlefield, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();
    assert_quark_prohibition(&runner, quark);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure = treasure_token(&runner);
    let state = runner.state();
    assert!(
        engine::game::static_abilities::player_cant_sacrifice_as_cost(state, P0, treasure),
        "Quark's controller can't pay a sacrifice cost with a Treasure token"
    );
    assert!(
        !engine::game::static_abilities::player_cant_sacrifice_as_cost(state, P0, buried),
        "control: a nontoken Treasure is outside the 'Treasure tokens' filter"
    );
    assert!(
        !engine::game::static_abilities::player_cant_sacrifice_as_cost(state, P1, treasure),
        "control: 'You can't sacrifice' scopes Quark's controller only"
    );
}
