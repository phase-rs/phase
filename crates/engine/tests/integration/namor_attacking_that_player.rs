//! Namor, Atlantean King — the attacked-player predicate and the
//! "attacking that player" defending-player anaphor.
//!
//! Verbatim Oracle text (Scryfall oracle id
//! `171a0a09-4aee-466c-aebd-3b0d4c1f51b4`):
//!
//! > Whenever Namor attacks a player who has more life than you, other
//! > creatures you control attacking that player get +2/+0 until end of turn.
//!
//! Two independent defects shipped in the same line, and each has its own
//! discriminating rows here:
//!
//! **Defect A — the event predicate was dropped.** `parse_attack_target`
//! discarded its remainder, so "who has more life than you" vanished
//! (`valid_target: null`) and the trigger fired on EVERY player attack. Rows
//! `fires_when_.._more_life` / `does_not_fire_when_..` / `..equal_life..`
//! discriminate it.
//!
//! **Defect B — the pump was board-wide.** With "attacking that player"
//! unconsumed by `parse_type_phrase`, the clause fell through to the numeric
//! imperative path, which emits the documented
//! `Effect::Pump { target: TargetFilter::Any }` sentinel. `TargetFilter::Any`
//! matches unconditionally, so +2/+0 landed on every permanent on the
//! battlefield — both players' creatures, Namor itself, and lands. Row
//! `pumps_only_co_attackers_of_the_same_defender` discriminates it three ways
//! at once.
//!
//! A third row set covers the SIBLING class this change unlocks: the `YouAttack`
//! cards that put the same anaphor in a TARGET filter (Ordruun Mentor, Echoing
//! Assault). Their slot-build door does not carry `trigger_source`, so before
//! `ability_utils::filter_needs_trigger_source` they enumerated ZERO legal
//! targets on any multi-attacker declaration and CR 603.3d removed the trigger
//! from the stack. Those rows deliberately use a TWO-attacker board, because a
//! single-attacker board passes without the fix.
//!
//! **Defect C — the sibling class bound the WRONG player in multiplayer.**
//! "Whenever you attack a player" is CR 508.3e, not CR 508.3a: the trigger
//! source need not be attacking (Echoing Assault is an Enchantment and never
//! can be), so CR 508.5 — which speaks only to "an ability of an attacking
//! creature" — cannot supply "that player". The declaration used to collapse
//! into ONE firing bound to the batch-global defender, so on a two-defender
//! board one attacked player's firing went missing entirely and the surviving
//! firing answered for the wrong lane. Row
//! `ordruun_mentor_binds_each_firing_to_its_own_attacked_player` discriminates
//! it, and reverting the parser's attacked-player object collapses it back to a
//! single firing.
//!
//! CR references:
//!   - CR 508.3e: "Whenever [a player] attacks [another player]" triggers for
//!     each attacked player, and does not trigger on planeswalker/battle
//!     attacks. Confirmed by the printed Echoing Assault ruling ("triggers once
//!     for each player you attacked").
//!   - CR 508.5 / CR 508.5a: an ability of an attacking creature that refers to
//!     a defending player means the player THAT creature is attacking, and in
//!     multiplayer that player is determined individually per attacker. This is
//!     Namor's and Owlbear Cub's binding rule — NOT the CR 508.3e siblings'.
//!   - CR 603.2: a trigger event (including a relative clause inside it) is
//!     checked once, when the event occurs.
//!   - CR 603.3d: a triggered ability with no legal choice for a required
//!     target is removed from the stack.
//!   - CR 611.2c: a continuous effect from a resolved ability fixes its
//!     affected set when it begins.
//!   - CR 119.1: life totals.

use engine::game::combat::AttackerInfo;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

/// Verbatim Scryfall Oracle text — a paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const NAMOR_ORACLE: &str = "Flying\n\
     Whenever you cast a noncreature spell, create a 1/1 blue Merfolk creature token.\n\
     Whenever Namor attacks a player who has more life than you, other creatures you control attacking that player get +2/+0 until end of turn.";

struct Board {
    runner: GameRunner,
    namor: ObjectId,
    /// A co-attacker P0 sends at the SAME defender as Namor.
    ally: ObjectId,
    /// A co-attacker P0 sends at the OTHER defender, in the same declaration.
    other_lane: ObjectId,
}

/// Three-player board. `p1_life` / `p2_life` set the two potential defenders'
/// life totals; P0 (Namor's controller) is always at 20.
fn board(p1_life: i32, p2_life: i32) -> Board {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, p1_life);
    scenario.with_life(P2, p2_life);

    let namor = {
        let mut builder = scenario.add_creature(P0, "Namor, Atlantean King", 2, 2);
        builder.from_oracle_text(NAMOR_ORACLE);
        builder.id()
    };
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let other_lane = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    Board {
        runner,
        namor,
        ally,
        other_lane,
    }
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = runner.state().objects.get(&id).expect("object must exist");
    (object.power, object.toughness)
}

// ---------------------------------------------------------------------------
// Defect B — the pump's affected set.
// ---------------------------------------------------------------------------

/// **The row that proves Defect B is fixed.** CR 508.5 + CR 508.5a: "other
/// creatures you control attacking THAT PLAYER" is scoped to the defender NAMOR
/// is attacking, individually determined per attacker.
///
/// Revert-failing three independent ways:
///   1. Drop `parse_attacking_defender_anaphor` and the clause lowers to the
///      `Pump { target: TargetFilter::Any }` sentinel — `other_lane` (and
///      Namor, and every other permanent) gets +2/+0.
///   2. Emit `Attacking { defender: None }` instead of
///      `Some(DefendingPlayer)` and `other_lane` — attacking the OTHER
///      defender in the same declaration — is pumped.
///   3. Drop `FilterProp::Another` and Namor pumps itself.
///
/// The two-defender split is the multi-authority fixture: it proves the anaphor
/// binds the SOURCE's defender (CR 508.5a, per-attacker) rather than a batch
/// global, because both lanes live in one `AttackersDeclared` event.
#[test]
fn namor_pumps_only_co_attackers_of_the_same_defender_cr_508_5a() {
    // P1 at 30 > P0 at 20, so the trigger's predicate is satisfied for P1.
    let Board {
        mut runner,
        namor,
        ally,
        other_lane,
    } = board(30, 30);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (namor, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
            (other_lane, AttackTarget::Player(P2)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, ally),
        (Some(4), Some(2)),
        "the co-attacker on Namor's defender must get +2/+0"
    );
    assert_eq!(
        power_toughness(&runner, other_lane),
        (Some(2), Some(2)),
        "a creature attacking the OTHER defender in the same declaration must \
         NOT be pumped — that is the CR 508.5a per-attacker binding, and the \
         assertion that fails if the anaphor degrades to `defender: None`"
    );
    assert_eq!(
        power_toughness(&runner, namor),
        (Some(2), Some(2)),
        "\"OTHER creatures you control\" excludes Namor itself"
    );
}

/// CR 611.2c: the affected set is fixed when the continuous effect begins, so a
/// creature that joins the battlefield after the trigger resolves is not pumped
/// even though it matches the filter's text.
///
/// The latecomer is built to MATCH the pump filter in full — `CoreType::Creature`
/// for the `type_filters`, controlled by P0, registered as an attacker against
/// the SAME defending player for `Attacking { defender: DefendingPlayer }`, and
/// distinct from Namor for `FilterProp::Another`. That is what makes the row
/// discriminating: a latecomer that failed the filter anyway (no card types, not
/// in `combat.attackers`) would assert the same `(2, 2)` under LIVE
/// re-evaluation, proving nothing about the snapshot.
///
/// Revert-failing: change `pump::resolve_all` to register one transient
/// continuous effect over the FILTER instead of one `SpecificObject { id }` per
/// matched object, and the latecomer is pumped to 4/2.
///
/// Reach-guarded twice: the same board provably DOES pump a matching
/// co-attacker, and the latecomer is proved to satisfy the filter live.
#[test]
fn namor_pump_is_a_resolution_snapshot_cr_611_2c() {
    let Board {
        mut runner,
        namor,
        ally,
        ..
    } = board(30, 30);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (namor, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    // Positive reach-guard first.
    assert_eq!(
        power_toughness(&runner, ally),
        (Some(4), Some(2)),
        "reach-guard: the trigger really did resolve and pump"
    );

    let latecomer = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(9_001),
        P0,
        "Latecomer".to_string(),
        engine::types::zones::Zone::Battlefield,
    );
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&latecomer)
            .expect("latecomer must exist");
        // `create_object` leaves `card_types` EMPTY, so without this the object
        // fails the pump filter's `type_filters: [Creature]` and the row proves
        // nothing.
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(2);
        object.toughness = Some(2);
        object.base_power = Some(2);
        object.base_toughness = Some(2);
    }
    // CR 508.1a: register it as an attacker against the SAME defender Namor is
    // attacking, so `FilterProp::Attacking { defender: DefendingPlayer }` is
    // satisfied for it too.
    runner
        .state_mut()
        .combat
        .as_mut()
        .expect("combat is live")
        .attackers
        .push(AttackerInfo::attacking_player(latecomer, P1));
    evaluate_layers(runner.state_mut());

    // Reach-guard the negative: the latecomer really does satisfy the pump's
    // filter as of NOW. If it did not, the assertion below would hold under live
    // re-evaluation too and would not discriminate CR 611.2c at all.
    assert!(
        engine::game::filter::matches_target_filter(
            runner.state(),
            latecomer,
            &namor_pump_filter(&runner, namor),
            &engine::game::filter::FilterContext::from_source(runner.state(), namor),
        ),
        "reach guard: the latecomer must MATCH the pump filter live — otherwise \
         this row cannot discriminate a snapshot from a live rescan"
    );

    assert_eq!(
        power_toughness(&runner, latecomer),
        (Some(2), Some(2)),
        "CR 611.2c: a creature that entered after resolution is outside the \
         snapshot and must not be pumped"
    );
}

/// The `PumpAll` filter Namor's attack trigger actually resolved, read off the
/// parsed card rather than re-typed here — a hand-written copy could drift from
/// the parser and silently make the reach-guard above assert the wrong thing.
fn namor_pump_filter(runner: &GameRunner, namor: ObjectId) -> TargetFilter {
    let object = runner
        .state()
        .objects
        .get(&namor)
        .expect("Namor must be on the battlefield");
    object
        .trigger_definitions
        .iter_unchecked()
        .filter_map(|trigger| trigger.definition().execute.as_deref())
        .find_map(|definition| match &*definition.effect {
            Effect::PumpAll { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Namor's attack trigger lowers to a PumpAll")
}

// ---------------------------------------------------------------------------
// Defect A — the event predicate.
// ---------------------------------------------------------------------------

/// CR 603.2 + CR 119.1: the trigger fires when the attacked player has MORE
/// life than the trigger's controller.
#[test]
fn namor_fires_when_the_attacked_player_has_more_life() {
    // P1 at 30, P0 at 20.
    let Board {
        mut runner,
        namor,
        ally,
        ..
    } = board(30, 5);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (namor, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, ally),
        (Some(4), Some(2)),
        "30 > 20 satisfies the predicate, so the trigger must fire and pump"
    );
}

/// **The row that proves Defect A is fixed.** CR 603.2: the trigger must NOT
/// fire when the attacked player does not have more life.
///
/// Revert-failing: with `valid_target: None` (the shipped bug) the trigger has
/// no defender predicate at all and fires on every player attack, so `ally`
/// would be pumped here. `player_matches_filter`'s `_ => true` tail is
/// fail-OPEN, so this row also catches a `PlayerMatching` arm that was added to
/// the type but never to the matcher.
///
/// Paired positive reach-guard: `namor_fires_when_the_attacked_player_has_more_life`
/// uses the same board shape and the same attack lane, differing only in the
/// defender's life total — so this negative cannot pass vacuously via a parse
/// failure or an unreachable combat driver.
#[test]
fn namor_does_not_fire_when_the_attacked_player_has_less_life_cr_603_2() {
    // P2 at 5 < P0 at 20.
    let Board {
        mut runner,
        namor,
        ally,
        ..
    } = board(30, 5);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (namor, AttackTarget::Player(P2)),
            (ally, AttackTarget::Player(P2)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, ally),
        (Some(2), Some(2)),
        "5 <= 20 fails the predicate, so nothing may be pumped"
    );
    assert_eq!(
        power_toughness(&runner, namor),
        (Some(2), Some(2)),
        "and Namor itself is never pumped either"
    );
}

/// The comparator boundary: "MORE life than you" is strictly greater
/// (`Comparator::GT`), so an attacked player at exactly the controller's life
/// total must not fire the trigger. Discriminates a `GE` mis-lowering, which
/// every other row in this file would pass.
#[test]
fn namor_does_not_fire_on_equal_life_totals_gt_not_ge() {
    // P1 at exactly 20 == P0 at 20.
    let Board {
        mut runner,
        namor,
        ally,
        ..
    } = board(20, 5);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (namor, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, ally),
        (Some(2), Some(2)),
        "equal life is not MORE life — GT, not GE"
    );
}

// ---------------------------------------------------------------------------
// The sibling TARGETED class — `ability_utils::filter_needs_trigger_source`.
// ---------------------------------------------------------------------------

/// **The row that proves the enumeration-door fix.** CR 603.3d: before
/// `filter_needs_trigger_source`, the slot-build door
/// (`targeting::find_legal_targets`) built a `FilterContext` with
/// `trigger_source: None`, so `combat::defending_player_cr508_5` fell through to
/// its live-combat tail, whose `AttackersDeclared` arm is gated on
/// `attacker_ids.len() == 1`. On this TWO-attacker board that yields `None`,
/// every candidate fails `attacking_defender_matches`, the slot is EMPTY, and
/// CR 603.3d removes the trigger from the stack — silently.
///
/// The board is deliberately multi-attacker: a single-attacker board passes
/// without the fix and would be a false green.
///
/// Revert-failing: remove the `filter_needs_trigger_source` disjunct from
/// `target_filter_needs_ability_context` and the `assert_eq!` below sees an
/// empty offered set (or no prompt at all).
#[test]
fn ordruun_mentor_offers_exactly_the_attackers_of_the_attacked_player_cr_603_3d() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Verbatim Oracle text (MTGJSON), Mentor reminder included.
    let mentor = {
        let mut builder = scenario.add_creature(P0, "Ordruun Mentor", 3, 2);
        builder.from_oracle_text(
            "Mentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\n\
             Whenever you attack a player, target creature that's attacking that player gains first strike until end of turn.",
        );
        builder.id()
    };
    let attacker_a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let attacker_b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    // Controlled, but NOT attacking — must never be offered.
    let bystander = scenario.add_creature(P0, "Alpha Myr", 2, 1).id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker_a, AttackTarget::Player(P1)),
            (attacker_b, AttackTarget::Player(P1)),
        ])
        .expect("DeclareAttackers should succeed");

    let offered = first_trigger_target_slot(&runner);

    assert!(
        !offered.is_empty(),
        "CR 603.3d: the slot must not be empty — an empty set silently removes \
         the trigger from the stack, which is the exact pre-fix failure"
    );
    let mut got: Vec<ObjectId> = offered
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();
    got.sort();
    let mut want = vec![attacker_a, attacker_b];
    want.sort();
    assert_eq!(
        got, want,
        "exactly the two creatures attacking P1 may be offered"
    );
    assert!(
        !got.contains(&bystander),
        "a non-attacking creature must never be offered"
    );
    assert!(
        !got.contains(&mentor),
        "Ordruun Mentor is not attacking, so it is not a legal target either"
    );
}

/// CR 508.3e — the RESTRICTION half: a planeswalker-only declaration must not
/// fire a "Whenever you attack a player" trigger at all.
///
/// CR 508.3e: "It won't trigger if a creature is put onto the battlefield
/// attacking or if a creature attacks a planeswalker or a battle."
///
/// Before this change the class parsed with `attack_target_filter: None` — a
/// bare CR 508.3d "you attack" — so any declaration fired it, planeswalker or
/// not. The parser row asserts the field; this row proves the field is actually
/// load-bearing at runtime.
///
/// It also answers the reachability question the per-attacked-player split
/// raises: that split maps a planeswalker to its controller (CR 508.5a /
/// CR 310.8d) when grouping, which in isolation looks like it could admit a
/// planeswalker attack as a player attack. It cannot — the
/// `attack_target_filter` type gate runs UPSTREAM, inside
/// `matching_you_attack_pairs`, so a planeswalker-only declaration produces
/// zero pairs and therefore zero firings before any grouping happens. This row
/// pins that ordering.
#[test]
fn ordruun_mentor_does_not_fire_on_a_planeswalker_only_attack_cr_508_3e() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    {
        let mut builder = scenario.add_creature(P0, "Ordruun Mentor", 3, 2);
        builder.from_oracle_text(
            "Mentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\n\
             Whenever you attack a player, target creature that's attacking that player gains first strike until end of turn.",
        );
        builder.id()
    };
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // A planeswalker controlled by the opponent, so the only declared attack is
    // aimed at a permanent rather than at P1 personally.
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Test Walker", "Jace", 4, "")
        .id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(walker))])
        .expect("DeclareAttackers should succeed");

    let firings = runner
        .state()
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .count();
    assert_eq!(
        firings,
        0,
        "CR 508.3e: attacking only a planeswalker is not attacking a PLAYER, so \
         the trigger must not fire at all. stack={:?}",
        runner.stack_names()
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "a non-firing trigger must not prompt for targets either; waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// CR 508.3e — the two-defender discrimination: each firing is bound to ITS
/// OWN attacked player.
///
/// "Whenever you attack a player" is CR 508.3e ("Whenever [a player] attacks
/// [another player]"), so attacking P1 and P2 in one declaration fires the
/// ability TWICE, once per attacked player. The printed ruling on Echoing
/// Assault — the other card on this arm — states it outright: "If you attack
/// multiple players in the same declare attackers step, Echoing Assault's last
/// ability triggers once for each player you attacked."
///
/// The binding this row pins cannot come from CR 508.5: that rule supplies a
/// defending player to *an ability of an attacking creature*, and Ordruun
/// Mentor is not attacking here (its sibling on this arm, Echoing Assault, is
/// an Enchantment and can never attack at all). The referent comes from the CR
/// 508.3e trigger event itself, which
/// `trigger_matchers::matching_you_attack_events_by_attacked_player` splits
/// into one synthesized `AttackersDeclared` per attacked player.
///
/// This row is the discriminating one: before the split, both firings collapsed
/// into a single trigger bound to the batch-global defender, which offered BOTH
/// lanes to one target slot. It fails if the split regresses (one firing, or a
/// firing that can reach across defenders) and it fails if the enumeration door
/// regresses (an empty slot → CR 603.3d removal).
#[test]
fn ordruun_mentor_binds_each_firing_to_its_own_attacked_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    {
        let mut builder = scenario.add_creature(P0, "Ordruun Mentor", 3, 2);
        builder.from_oracle_text(
            "Mentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\n\
             Whenever you attack a player, target creature that's attacking that player gains first strike until end of turn.",
        );
        builder.id()
    };
    let lane_p1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let lane_p2 = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (lane_p1, AttackTarget::Player(P1)),
            (lane_p2, AttackTarget::Player(P2)),
        ])
        .expect("DeclareAttackers should succeed");

    // CR 508.3e: one firing per attacked player. Each has exactly one legal
    // target (the single creature attacking THAT player), so the engine binds
    // both without prompting and both sit on the stack with pinned targets.
    let firings = pinned_trigger_targets_per_firing(&runner);
    assert_eq!(
        firings.len(),
        2,
        "attacking two players must fire the CR 508.3e trigger twice, once per \
         attacked player; got {firings:?}"
    );
    assert!(
        firings.iter().all(|targets| targets.len() == 1),
        "each firing sees exactly one creature attacking its own bound player; \
         got {firings:?}"
    );

    let mut bound: Vec<ObjectId> = firings
        .iter()
        .filter_map(|targets| match targets.first() {
            Some(TargetRef::Object(id)) => Some(*id),
            _ => None,
        })
        .collect();
    bound.sort();
    let mut want = vec![lane_p1, lane_p2];
    want.sort();
    assert_eq!(
        bound, want,
        "the two firings must bind the P1 lane and the P2 lane separately — \
         neither may reach across to the other defender's attacker"
    );

    // The cross-binding this row exists to forbid: no single firing may offer
    // both lanes. Before the CR 508.3e split, exactly that happened.
    assert!(
        firings.iter().all(|targets| {
            let ids: Vec<ObjectId> = targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect();
            !(ids.contains(&lane_p1) && ids.contains(&lane_p2))
        }),
        "a firing bound to one attacked player must never offer the other \
         player's attacker; got {firings:?}"
    );
}

/// One entry per triggered ability on the stack, holding that firing's own
/// pinned targets.
///
/// `first_trigger_target_slot` deliberately FLATTENS every stack entry into one
/// set, which answers "what got enumerated anywhere". CR 508.3e needs the
/// opposite question — *which firing bound which target* — so a cross-defender
/// leak cannot hide inside a flattened union. Kept separate rather than
/// replacing the flattening helper, because the single-defender rows genuinely
/// want the union.
fn pinned_trigger_targets_per_firing(runner: &GameRunner) -> Vec<Vec<TargetRef>> {
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "expected every firing to auto-bind its single legal target; a prompt \
         means some slot enumerated two or more candidates. waiting_for={:?}",
        runner.state().waiting_for
    );
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.targets.clone()),
            _ => None,
        })
        .collect()
}

/// The set of objects the trigger's target slot actually admitted.
///
/// When the slot has two or more legal targets the engine prompts, so the set
/// is read straight off `WaitingFor::TriggerTargetSelection`. When exactly one
/// legal target exists the engine binds it without prompting, so the same
/// information is read off the pinned `ResolvedAbility.targets` of the trigger
/// sitting on the stack. Both branches answer the one question these rows ask —
/// *what did slot-build enumerate?* — and neither may be empty: an empty
/// enumeration is the CR 603.3d removal this change exists to prevent.
fn first_trigger_target_slot(runner: &GameRunner) -> Vec<TargetRef> {
    if let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    {
        return target_slots[selection.current_slot].legal_targets.to_vec();
    }
    let pinned: Vec<TargetRef> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.targets.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        !pinned.is_empty(),
        "no target prompt and no pinned trigger target — the slot enumerated \
         nothing and CR 603.3d removed the ability. waiting_for={:?} stack={:?}",
        runner.state().waiting_for,
        runner.stack_names()
    );
    pinned
}
