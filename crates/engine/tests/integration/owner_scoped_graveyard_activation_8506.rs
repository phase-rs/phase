//! CR 108.4 + CR 108.4a (issue #8506): the hand and graveyard activation scans
//! in `ai_support` scope by the card's OWNER, and both halves of the surface —
//! the offered action set (`candidates.rs`) and the blocked-ability read-out
//! (`activation_block_reasons`, added in #8504) — must agree on that scope.
//!
//! **The rule.** CR 108.4: "A card doesn't have a controller unless that card
//! represents a permanent or spell." CR 108.4a: "If anything asks for the
//! controller of a card that doesn't have one (because it's not a permanent or
//! spell), use its owner instead." CR 404.1 puts a card into its OWNER's
//! graveyard. CR 602.2 carries the restriction that consumes all of this:
//! "Only an object's controller (or its owner, if it doesn't have a controller)
//! can activate its activated ability."
//!
//! **On the issue's premise.** #8506 predicted a live bug: a creature an
//! opponent had gained control of dies into its owner's graveyard still carrying
//! `obj.controller = opponent`, so the owner's flashback / unearth / escape
//! activation is dropped. Two comments in the tree assert the same thing
//! (`effects/change_zone.rs`'s player-scoped mass-move comment, and
//! `database/synthesis.rs`'s persist-test rationale). Both are STALE, and
//! `controller_defaults_to_owner_after_*` below is the measurement that says so:
//!
//! * `zones::apply_zone_exit_cleanup` reverts `controller` on a battlefield
//!   exit after all — `reset_for_battlefield_exit` rewrites `base_controller` to
//!   the owner (zones.rs:429), and `revert_layered_characteristics_to_base`
//!   reads it back into `controller` (zones.rs:573). The two sit in DIFFERENT
//!   `from == Zone::Battlefield` blocks — the ones opening at zones.rs:428 and
//!   zones.rs:538 — so a reader who checks only the first call sees the reset
//!   and misses the read-back. That is exactly how the two stale comments below
//!   went wrong.
//! * A spell's caster lives on its `StackEntry`, not on the `GameObject`, so a
//!   card one player casts out of another player's zones never diverges either.
//!
//! So the owner scope is not repairing an observable drop today; it states the
//! rule the owner-keyed zone lists already encode. Two mutations were run to
//! establish exactly that, and NEITHER of them turns this file red:
//!
//! * reverting all six scans to `obj.controller == player` — green, because
//!   `controller == owner` off the battlefield, so the two guards agree; and
//! * deleting the six guards outright (`if true`) — also green, because each
//!   loop already iterates `state.players[player].{hand,graveyard}`, which is
//!   keyed by owner, so no other player's card can reach the guard.
//!
//! The guard is therefore a statement of scope rather than a live filter, and no
//! test in this repository can discriminate on its predicate. What the file DOES
//! pin is the two things a future change could break silently: the zone-exit
//! invariant the scope relies on, and the read-out/offer partition from #8504.

use engine::ai_support::{activation_block_reasons, legal_actions_full};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CastingPermission, ContinuousModification,
    Duration, Effect, ExileGrantCostProvenance, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Ability index 0: `{0}: Draw a card.` from the graveyard — affordable, so it
/// belongs in the OFFERED set.
const FREE_ABILITY: usize = 0;
/// Ability index 1: `{2}: Draw a card.` from the graveyard — unaffordable with
/// an empty pool, so it belongs in the BLOCK READ-OUT.
const TAXED_ABILITY: usize = 1;

/// `{generic}: Draw a card.`, functioning only from the graveyard (CR 113.6b).
fn graveyard_draw(generic: u32) -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            generic,
            shards: vec![],
        },
    });
    // CR 113.6b: an ability that states which zone it functions in functions
    // only from there.
    ability.activation_zone = Some(Zone::Graveyard);
    ability
}

/// P0's card, in P0's graveyard, carrying the two graveyard abilities above.
/// P0 holds priority in their own precombat main phase with an empty pool.
fn owner_with_card_in_own_graveyard() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let state: &mut GameState = runner.state_mut();
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let card = create_object(
        state,
        CardId(8506),
        P0,
        "Borrowed Reckoning".to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&card).expect("card exists");
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    let abilities = std::sync::Arc::make_mut(&mut obj.abilities);
    abilities.push(graveyard_draw(0));
    abilities.push(graveyard_draw(2));
    obj.base_abilities = obj.abilities.clone();

    (runner, card)
}

/// The ability indices `legal_actions_full` OFFERS for `id`.
fn offered_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let mut out: Vec<usize> = grouped
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == id => Some(*ability_index),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out
}

/// The ability indices the block READ-OUT reports for `id`.
fn blocked_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
    let mut out: Vec<usize> = activation_block_reasons(runner.state())
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|e| e.ability_index)
        .collect();
    out.sort_unstable();
    out
}

// ── the invariant the owner scope rests on ──────────────────────────────────

/// MEASUREMENT, not a design statement: a permanent that dies while an opponent
/// controls it lands in its OWNER's graveyard reading `controller == owner`.
///
/// The control grab is a real Layer-2 `ChangeController` continuous effect
/// resolved through `evaluate_layers`, and the mid-test assertion proves the
/// divergence genuinely existed on the battlefield — so a passing test here is
/// evidence about the zone change, not a fixture that never diverged.
#[test]
fn controller_defaults_to_owner_after_dying_under_an_opponents_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Stolen Wanderer", 2, 2).id();
    let mut runner = scenario.build();

    // CR 613.1b: a genuine Layer-2 control-changing continuous effect.
    runner.state_mut().add_transient_continuous_effect(
        creature,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: creature },
        vec![ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&creature].controller,
        P1,
        "precondition: the control grab really took effect on the battlefield"
    );

    // CR 704.5f: zero toughness sends it to its owner's graveyard.
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .expect("creature exists")
        .toughness = Some(0);
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    let obj = &runner.state().objects[&creature];
    assert_eq!(obj.zone, Zone::Graveyard, "precondition: it died");
    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&creature),
        "CR 404.1: a card is put into its OWNER's graveyard"
    );
    assert_eq!(
        obj.controller, P0,
        "CR 400.7: the battlefield exit reverts `controller` to the owner — \
         `reset_for_battlefield_exit` rewrites `base_controller` (zones.rs:429), \
         and `revert_layered_characteristics_to_base` reads it back \
         (zones.rs:573) from a SECOND `from == Zone::Battlefield` block. \
         #8506 and two in-tree comments predict `P1` here; they are stale \
         because they cite the first call without the second."
    );
}

/// MEASUREMENT: a spell one player casts out of another player's zones carries
/// its caster on the `StackEntry`, never on the `GameObject`, so the card lands
/// in its owner's graveyard with `controller` untouched.
#[test]
fn controller_defaults_to_owner_after_a_foreign_cast_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let state: &mut GameState = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };

    let card = create_object(
        state,
        CardId(8507),
        P0,
        "Borrowed Reckoning".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&card).expect("card exists");
        obj.card_types.core_types.push(CoreType::Instant);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![],
        };
        obj.base_mana_cost = obj.mana_cost.clone();
        // CR 118.9 + CR 601.2: the "you may cast that card" grant of the Dire
        // Fleet Daredevil / Sen Triplets class, naming P1 — a player who does
        // NOT own the card — as its caster, for {0}. `resolution_cleanup` and
        // `graveyard_replacement` stay `None` so the spell reaches its owner's
        // graveyard by the ordinary CR 608.2n route.
        obj.casting_permissions
            .push(CastingPermission::ExileWithAltCost {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                },
                cost_provenance: ExileGrantCostProvenance::Alternative,
                cast_transformed: false,
                constraint: None,
                granted_to: Some(P1),
                resolution_cleanup: None,
                graveyard_replacement: None,
                duration: None,
                source_id: None,
                enters_with_counter: None,
                enters_with_modifications: vec![],
                mana_spend_permission: None,
            });
    }

    runner.cast(card).resolve();

    let obj = &runner.state().objects[&card];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "CR 608.2n: the spell is graveyarded"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&card),
        "CR 404.1: an instant that finished resolving goes to its OWNER's graveyard"
    );
    assert_eq!(
        obj.controller, P0,
        "a spell's controller lives on its `StackEntry`; the `GameObject` keeps \
         the owner throughout, so a foreign cast does not diverge either"
    );
}

// ── the player-scoping contract both halves must share ──────────────────────

/// CR 108.4a + CR 602.2: the owner of a card in their OWN graveyard is offered
/// its affordable graveyard ability, and the unaffordable one reaches the block
/// read-out instead. The two sets must PARTITION — a blocked row for an ability
/// that is never offered is exactly the inconsistency #8506 was raised to avoid.
#[test]
fn owner_partitions_graveyard_abilities_across_offers_and_the_block_readout() {
    let (mut runner, card) = owner_with_card_in_own_graveyard();
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();

    let offered = offered_indices(&runner, card);
    let blocked = blocked_indices(&runner, card);

    assert!(
        offered.contains(&FREE_ABILITY),
        "CR 602.2: the owner may activate their own graveyard card's affordable \
         ability; offered={offered:?}"
    );
    assert!(
        blocked.contains(&TAXED_ABILITY),
        "CR 118.3: the unpayable graveyard ability must reach the owner's \
         read-out rather than vanish; blocked={blocked:?}"
    );
    assert!(
        !blocked.contains(&FREE_ABILITY) && !offered.contains(&TAXED_ABILITY),
        "read-out and offers must PARTITION one ability space: \
         offered={offered:?} blocked={blocked:?}"
    );
}

/// An opponent gets neither an offer nor a blocked row for a card sitting in
/// someone else's graveyard.
///
/// SCOPE OF THIS TEST, measured: it does NOT discriminate on the guard's
/// predicate — deleting the guard leaves it green, because each loop iterates
/// the acting player's own owner-keyed graveyard list and P1's is empty. What it
/// does pin is the ITERATION SET: it fails if either scan is ever widened to walk
/// `state.objects` globally, the way the `spell_costs` sweep above it does.
#[test]
fn an_opponent_is_offered_nothing_from_another_players_graveyard() {
    let (mut runner, card) = owner_with_card_in_own_graveyard();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    // Ample mana, so affordability cannot be what withholds the ability.
    for _ in 0..4 {
        state.players[P1.0 as usize].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    assert!(
        offered_indices(&runner, card).is_empty(),
        "CR 108.4a names the OWNER: P1 must not be offered an activation on a \
         card in P0's graveyard"
    );
    assert!(
        blocked_indices(&runner, card).is_empty(),
        "and P1 must not receive a blocked row for it either"
    );
}
