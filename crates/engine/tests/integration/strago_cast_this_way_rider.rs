//! Strago and Relm — the cast-this-way gains-modifications rider through the
//! during-resolution cast permission.
//!
//! Verbatim Oracle text (Scryfall / `client/public/card-data.json`, 2026-10-01):
//! "Sketch and Lore — {2}{R}, {T}: Target opponent exiles cards from the top of
//! their library until they exile an instant, sorcery, or creature card. You
//! may cast that card without paying its mana cost. If you cast a creature
//! spell this way, it gains haste and \"At the beginning of the end step,
//! sacrifice this creature.\" Activate only as a sorcery."
//!
//! CR 608.2c — "this way" back-references the specific cast the grant
//! authorized, so the rider rides the ELECTED during-resolution permission into
//! finalization; CR 611.2a — no stated duration, applied as a
//! `Duration::Permanent` continuous effect; CR 611.2c — scoped to the cast
//! object. The rider is metadata: it must never resolve standalone, and a
//! declined offer must latch nothing for a later route.

use engine::ai_support::legal_actions;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CastingPermission, ContinuousModification, Duration, EffectKind, ExileGrantCostProvenance,
    TargetFilter,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const STRAGO: &str = "Sketch and Lore — {2}{R}, {T}: Target opponent exiles cards from the top of their library until they exile an instant, sorcery, or creature card. You may cast that card without paying its mana cost. If you cast a creature spell this way, it gains haste and \"At the beginning of the end step, sacrifice this creature.\" Activate only as a sorcery.";

/// Row 6.3's synthetic paid chosen-target grant with the same gains rider: the
/// "you may cast target creature card from your graveyard" clause lowers to a
/// paid `DuringResolution` `CastFromZone`, and the rider attaches as its
/// sub-ability exactly as on the free route.
const PAID_RIDER: &str = "{T}: You may cast target creature card from your graveyard. If you cast a creature spell this way, it gains haste and \"At the beginning of the end step, sacrifice this creature.\" Activate only as a sorcery.";

/// P0 has Strago and Relm plus exactly the {2}{R} activation cost in the pool;
/// `configure_top` stages the single card on top of P1's library (the only card
/// there), which the activated ability exiles.
fn build_strago_runner(
    configure_top: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let strago = scenario
        .add_creature_from_oracle(P0, "Strago and Relm", 3, 4, STRAGO)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let top = configure_top(&mut scenario);
    (scenario.build(), strago, top)
}

/// A creature card on top of P1's library with a real (unpayable-from-pool)
/// mana cost — so its arrival after the offer proves the free-cast route.
fn library_top_creature(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_library_top(P1, "Grizzly Bears", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(6))
        .id()
}

/// An instant card on top of P1's library.
fn library_top_instant(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_library_top(P1, "Lightning Bolt", true)
        .id()
}

/// P0 has the paid-rider source plus one red mana (the graveyard creature's
/// {1} cost); the graveyard creature is the chosen target the paid offer mints
/// for.
fn build_paid_rider_runner() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Paid Rider Source", 1, 1, PAID_RIDER)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let creature = scenario
        .add_spell_to_graveyard(P0, "Graveyard Bear", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(1))
        .id();
    (scenario.build(), source, creature)
}

/// The library-top spell builder seeds only the card TYPE; give the creature a
/// nonzero body so a 0-toughness state-based action cannot explain a graveyard.
fn give_body(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("library-top card exists");
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
}

/// Drive the turn to the beginning of the end step, declaring no attackers and
/// no blockers (CR 508.1 / CR 509.1) so the combat windows do not stall.
fn drive_to_end_step(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().phase == Phase::End {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    runner.pass_both_players();
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    runner.pass_both_players();
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            _ => runner.pass_both_players(),
        }
    }
    panic!(
        "never reached the end step; stuck in {:?} at {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// Whether any transient rider modification is scoped to `id`.
fn has_haste_modification_for(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste
                        }
                    )
                })
        })
}

/// Whether the run's events contain a standalone `AddPendingEntersModifications`
/// resolution (the fail-loud path a metadata rider must never reach).
fn standalone_rider_resolution(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AddPendingEntersModifications,
                ..
            }
        )
    })
}

/// Row 3.1: accepting the free cast makes the cast creature gain haste and the
/// granted end-step sacrifice trigger resolve it to the graveyard. Reverting
/// the parser rider or the request threading flips both assertions. The same
/// run asserts metadata consumption: the rider is never resolved standalone,
/// and the cast was free (the {6} cost is unpayable from the {2}{R} pool).
#[test]
fn creature_cast_this_way_gains_haste_and_sacrifice_trigger() {
    let (mut runner, strago, creature) = build_strago_runner(library_top_creature);
    give_body(&mut runner, creature);
    let outcome = runner
        .activate(strago, 0)
        .target_player(P1)
        .accept_optional()
        .resolve();

    // Reach guards: the exiled card was cast (exile → stack → battlefield).
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::SpellCast { object_id, .. } if *object_id == creature
        )),
        "reach guard: the exiled card must actually be cast this way; events: {:?}",
        outcome.events()
    );
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the cast creature must enter the battlefield"
    );
    assert!(
        !standalone_rider_resolution(outcome.events()),
        "the rider must be consumed as metadata, never resolved standalone; events: {:?}",
        outcome.events()
    );

    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(
        object_has_effective_keyword_kind(runner.state(), creature, KeywordKind::Haste),
        "the cast creature must gain haste from the rider"
    );

    // Reach guard: the granted trigger stacks at the beginning of the end step.
    drive_to_end_step(&mut runner);
    assert!(
        !runner.state().stack.is_empty()
            || matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "reach guard: the granted end-step sacrifice trigger must be put on the stack, \
         found {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "the granted end-step sacrifice trigger must resolve the creature to the graveyard"
    );
}

/// Row 3.3: declining the "you may cast" grants nothing and nothing latches for
/// a later route — even when another effect later moves the card to the
/// battlefield, it has neither haste nor the granted sacrifice trigger, and the
/// decline emitted no standalone rider resolution.
#[test]
fn declined_cast_grants_nothing() {
    let (mut runner, strago, creature) = build_strago_runner(library_top_creature);
    give_body(&mut runner, creature);
    let outcome = runner
        .activate(strago, 0)
        .target_player(P1)
        .decline_optional()
        .resolve();

    // Reach guard: the offer was reached; the declined card stays exiled.
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Exile,
        "a declined offer leaves the card exiled"
    );
    assert!(
        !standalone_rider_resolution(outcome.events()),
        "the decline path must not resolve the rider standalone; events: {:?}",
        outcome.events()
    );
    assert!(
        !has_haste_modification_for(&runner, creature),
        "a declined offer must latch no rider modification"
    );

    // Another effect moves the card to the battlefield (a direct zone move —
    // the reanimation shape Phase 1 drives). The rider must not manifest.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert!(
        !object_has_effective_keyword_kind(runner.state(), creature, KeywordKind::Haste),
        "a later route must not inherit the declined cast's rider"
    );

    drive_to_end_step(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the granted end-step sacrifice trigger must not exist for a declined cast"
    );
}

/// Row 3.4 (hostile): after the offer is declined, the same card cast under a
/// seeded un-riddled `ExileWithAltCost` permission has no haste and no granted
/// sacrifice trigger — the ELECTED permission decides, not the earlier offer.
#[test]
fn cast_under_unriddled_permission_has_no_rider() {
    let (mut runner, strago, creature) = build_strago_runner(library_top_creature);
    give_body(&mut runner, creature);
    runner
        .activate(strago, 0)
        .target_player(P1)
        .decline_optional()
        .resolve();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);

    // Seed the un-riddled permission — the lingering shape with no rider.
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .expect("exiled card exists")
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            source_id: None,
            cost_provenance: ExileGrantCostProvenance::Alternative,
            cost: ManaCost::zero(),
            cast_transformed: false,
            constraint: None,
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: Some(Duration::UntilEndOfTurn),
            graveyard_replacement: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            mana_spend_permission: None,
            cast_cost_modifier: None,
        });

    // Reach guard: the seeded permission is usable.
    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == creature)
        ),
        "reach guard: the seeded permission must offer the cast"
    );

    runner
        .cast(creature)
        .try_resolve()
        .expect("the un-riddled cast must resolve");
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert!(
        !object_has_effective_keyword_kind(runner.state(), creature, KeywordKind::Haste),
        "an un-riddled permission must not grant the rider"
    );
    drive_to_end_step(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "no granted sacrifice trigger may exist under the un-riddled permission"
    );
}

/// Row 3.5: a non-creature cast made this way shows no observable rider
/// manifestation — the granted trigger is battlefield-gated, and haste on an
/// instant is inert. Every transient rider modification is scoped to the
/// (now-departed) object id.
#[test]
fn noncreature_cast_shows_no_rider_manifestation() {
    let (mut runner, strago, instant) = build_strago_runner(library_top_instant);
    let outcome = runner
        .activate(strago, 0)
        .target_player(P1)
        .accept_optional()
        .resolve();

    // Reach guard: the instant was cast this way (stack → graveyard).
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::SpellCast { object_id, .. } if *object_id == instant
        )),
        "reach guard: the exiled instant must be cast this way; events: {:?}",
        outcome.events()
    );
    assert_eq!(
        runner.state().objects[&instant].zone,
        Zone::Graveyard,
        "the instant resolves into its owner's graveyard"
    );

    engine::game::layers::evaluate_layers(runner.state_mut());
    let battlefield: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| object.zone == Zone::Battlefield)
        .map(|(id, _)| *id)
        .collect();
    for id in battlefield {
        assert!(
            !object_has_effective_keyword_kind(runner.state(), id, KeywordKind::Haste),
            "no battlefield object may inherit the instant's inert haste ({id:?})"
        );
    }
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .all(|effect| {
                !effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste
                        }
                    )
                }) || effect.affected == TargetFilter::SpecificObject { id: instant }
            }),
        "the rider's modifications must be scoped to the cast object id only"
    );

    drive_to_end_step(&mut runner);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&instant].zone,
        Zone::Graveyard,
        "the granted trigger is battlefield-gated and must not sacrifice a non-permanent"
    );
}

/// Row 6.3: the PAID during-resolution route end-to-end — the activated
/// ability mints a `GraveyardPaidCast` offer for the chosen graveyard creature;
/// accepting it, paying its printed cost, and resolving grants the rider
/// (haste + the end-step sacrifice), and the rider is consumed as metadata.
/// Reverting the mint or the accept threading flips the assertions.
#[test]
fn paid_cast_this_way_gains_haste_and_sacrifice() {
    let (mut runner, source, creature) = build_paid_rider_runner();
    give_body(&mut runner, creature);
    let outcome = runner.activate(source, 0).target_object(creature).resolve();
    let mut events = outcome.events().to_vec();

    // Reach guard: the activation minted the paid offer for the chosen card.
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::GraveyardPaidCast { hit_card, .. },
                ..
            } if *hit_card == creature
        ),
        "reach guard: the paid during-resolution offer must be minted, found {:?}",
        runner.state().waiting_for
    );

    let accepted = runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("accepting the paid offer must succeed");
    events.extend(accepted.events);
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        let paid = runner
            .act(GameAction::PassPriority)
            .expect("paying the offered cast must succeed");
        events.extend(paid.events);
    }
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == creature),
        "reach guard: the paid cast is on the stack, found {:?}",
        runner.state().waiting_for
    );
    assert!(
        !standalone_rider_resolution(&events),
        "the rider must be consumed as metadata, never resolved standalone; events: {events:?}"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the paid cast creature must enter the battlefield"
    );
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(
        object_has_effective_keyword_kind(runner.state(), creature, KeywordKind::Haste),
        "the paid cast creature must gain haste from the rider"
    );

    drive_to_end_step(&mut runner);
    assert!(
        !runner.state().stack.is_empty()
            || matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "reach guard: the granted end-step sacrifice trigger must be put on the stack, \
         found {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "the granted end-step sacrifice trigger must resolve the creature to the graveyard"
    );
}
