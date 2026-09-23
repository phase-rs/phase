//! FIN Sidequest turn-history conditions + the transform-then-attach anaphor.
//!
//! Two transform DFCs whose end-step/end-of-combat intervening-if clauses were
//! swallowed (`Swallow:Duration_ThisTurn`), plus the anaphor that binds the
//! second sentence's bare attachment pronoun to the source:
//!
//!   - Sidequest: Play Blitzball (`fin/158`) — "At the end of combat on your
//!     turn, if a player was dealt 6 or more combat damage this turn, transform
//!     this enchantment, then attach it to a creature you control."
//!     U1 (the `combat` damage-kind axis, CR 120.2a) + U3 (the
//!     `Transform{SelfRef}` → `Attach.attachment = SelfRef` anaphor, CR 608.2c).
//!   - Sidequest: Hunt the Mark (`fin/119`) — "At the beginning of your end
//!     step, if a creature died under an opponent's control this turn, create a
//!     Treasure token. Then if you control three or more Treasures, transform
//!     this enchantment."
//!     U2 (the opponent possessor axis on the dies condition, CR 608.2h).
//!
//! CR set (each verified against `docs/MagicCompRules.txt` before writing):
//! CR 109.4 + CR 109.5 (control and "you/your"), CR 120.1 + CR 120.2a +
//! CR 120.2b + CR 120.3 (damage, combat/noncombat, results), CR 301.5
//! (Equipment attaches to a creature), CR 603.3d (a trigger with no legal
//! choice is removed), CR 603.4 (intervening-if: checked at fire AND
//! resolution), CR 608.2c (follow instructions in order; later text may modify
//! earlier text), CR 608.2h (last-known information for the dead permanent's
//! controller), CR 608.2i (turn look-back), CR 700.4 (dies = battlefield to
//! graveyard), CR 701.3a (attach), CR 701.27a (transform).
//!
//! Oracle text is verbatim from Scryfall and byte-identical to the local
//! regenerated export (`client/public/card-data.json`). The file mirrors
//! `l02_bb4_intervening_if.rs` (parse fidelity rows with paired reach-guards +
//! discriminating runtime rows through the real trigger pipeline) and
//! `issue_605_calming_licid.rs` (attach `attachment`/`attached_to`/host
//! `attachments` assertions).
//!
//! Runtime rows cover both halves of the class: the U4 per-recipient damage
//! threshold (R7–R9) and the U5 resolution-time described attach host (R10–R13),
//! including the moved-card cascade (R13, Stonehewer Giant). The Fumble row
//! pins the honest-unsupported outcome for the plural-anaphor attachment
//! operand (CR 608.2c + CR 400.7) on the real cast pipeline.
//!
//! Negative rows are paired with positive reach-guards: every "does not fire" /
//! "not attached" assertion is preceded by a proof that the path was reached
//! (the damage happened, the victim died, the parse produced the typed clause).

use engine::game::combat::AttackTarget;
use engine::game::effects::attach;
use engine::game::game_object::AttachTarget;
use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    AggregateFunction, Comparator, ControllerRef, DamageChannel, DamageGroupKey, DamageKindFilter,
    Effect, EffectKind, FilterProp, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
    TriggerCondition, TriggerConstraint, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// The third seat of the multiplayer rows (`P0`/`P1` are the scenario
/// constants; the engine exports no `P2`).
const P2: PlayerId = PlayerId(2);

// ---------------------------------------------------------------------------
// Verbatim Oracle text (Scryfall + the local export, 2026-09-20)
// ---------------------------------------------------------------------------

const PLAY_BLITZBALL: &str = "At the beginning of combat on your turn, target creature you control gets +2/+0 until end of turn.\nAt the end of combat on your turn, if a player was dealt 6 or more combat damage this turn, transform this enchantment, then attach it to a creature you control.";

const HUNT_THE_MARK: &str = "When this enchantment enters, destroy up to one target creature.\nAt the beginning of your end step, if a creature died under an opponent's control this turn, create a Treasure token. Then if you control three or more Treasures, transform this enchantment.";

/// The real back face of Sidequest: Play Blitzball (`face_index 1`,
/// `Legendary Artifact — Equipment`). CR 701.27a: the transform is what makes
/// the source attachable at all (CR 301.5).
const WORLD_CHAMPION: &str = "Double Overdrive — Equipped creature gets +2/+0 and has double strike.\nEquip {3} ({3}: Attach to target creature you control. Equip only as a sorcery.)";

const DESTROY: &str = "Destroy target creature.";

const TREASURE_TOKEN: &str = "{T}, Sacrifice this token: Add one mana of any color.";

// ---------------------------------------------------------------------------
// Parse helpers (the `l02_bb4_intervening_if.rs` idiom)
// ---------------------------------------------------------------------------

fn parse_card(oracle: &str, name: &str) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(oracle, name, &[], &["Enchantment".to_string()], &[])
}

/// True when the parse produced a `SwallowedClause` diagnostic with `detector`.
fn has_swallowed(oracle: &str, name: &str, detector: &str) -> bool {
    parse_card(oracle, name).parse_warnings.iter().any(|w| {
        matches!(
            w,
            OracleDiagnostic::SwallowedClause { detector: d, .. } if d == detector
        )
    })
}

// ===========================================================================
// P1 — Sidequest: Play Blitzball (parse fidelity)
// ===========================================================================
//
// V4: the EndCombat trigger carries the exact combat-only threshold condition,
// BOTH swallows are cleared, and the "then attach it to a creature you control"
// clause survives with `attachment == SelfRef` (before U1 the condition was
// dropped and the clause tree was only `Transform`; with the condition but
// without U3 the clause survives with `attachment == ParentTarget` — the
// operand assertion, not the sub-ability's existence, discriminates U3).

#[test]
fn play_blitzball_parse_carries_combat_condition_and_self_ref_attach() {
    let parsed = parse_card(PLAY_BLITZBALL, "Sidequest: Play Blitzball");
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::EndCombat))
        .expect("Play Blitzball must carry an EndCombat phase trigger");

    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DamageDealtThisTurn {
                    source: Box::new(TargetFilter::Any),
                    target: Box::new(TargetFilter::Player),
                    // CR 603.4: "a player" names a SET, so the threshold is read
                    // per recipient (`Max` over `Some(Target)`) — never as a sum
                    // across recipients.
                    aggregate: AggregateFunction::Max,
                    group_by: Some(DamageGroupKey::Target),
                    damage_kind: DamageKindFilter::CombatOnly,
                    channel: DamageChannel::Total,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 6 },
        }),
        "CR 603.4 + CR 120.2a: the intervening-if must be the combat-only \
         per-recipient player-damage threshold (\"a player was dealt 6 or more \
         combat damage this turn\")"
    );
    assert_eq!(
        trigger.constraint,
        Some(TriggerConstraint::OnlyDuringYourTurn),
        "\"on your turn\" is a typed trigger constraint, not part of the condition"
    );

    // Reach-guards for the two swallow negatives below: the clause tree survived
    // the condition extraction (otherwise the negatives pass vacuously — the
    // swallow checker returns early on `Effect::Unimplemented`).
    let execute = trigger
        .execute
        .as_ref()
        .expect("the EndCombat trigger has an execute body");
    match &*execute.effect {
        Effect::Transform {
            target: TargetFilter::SelfRef,
            ..
        } => {}
        other => panic!("expected `transform this enchantment` = Transform SelfRef, got {other:?}"),
    }
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("the transform must chain the \"then attach it\" sub-ability");
    match &*sub.effect {
        Effect::Attach {
            attachment, target, ..
        } => {
            assert_eq!(
                *attachment,
                TargetFilter::SelfRef,
                "CR 608.2c + CR 701.3a: the bare-pronoun attachment names the \
                 source the previous clause transformed"
            );
            match target {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "the host is \"a creature you control\", got {:?}",
                        tf.type_filters
                    );
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                other => panic!("expected the typed host filter, got {other:?}"),
            }
        }
        other => panic!("expected `then attach it` = Attach, got {other:?}"),
    }

    assert!(
        !has_swallowed(PLAY_BLITZBALL, "Sidequest: Play Blitzball", "Condition_If"),
        "Condition_If must clear once the intervening-if attaches"
    );
    assert!(
        !has_swallowed(
            PLAY_BLITZBALL,
            "Sidequest: Play Blitzball",
            "Duration_ThisTurn"
        ),
        "Duration_ThisTurn must clear: the `DamageDealtThisTurn` quantity is the \
         typed evidence the detector's unit probe reads"
    );
}

// ===========================================================================
// P2 — Sidequest: Hunt the Mark (parse fidelity)
// ===========================================================================
//
// V5: the End trigger carries the exact opponent-control dies condition, both
// swallows clear, and the "Then if you control three or more Treasures"
// sub-ability survives (the preservation half of the row).

#[test]
fn hunt_the_mark_parse_carries_opponent_control_dies_condition() {
    let parsed = parse_card(HUNT_THE_MARK, "Sidequest: Hunt the Mark");
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::End))
        .expect("Hunt the Mark must carry an End phase trigger");

    let condition = trigger
        .condition
        .as_ref()
        .expect("the end-step intervening-if must attach to the trigger");
    match condition {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneChangeCountThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                            filter,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => {
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected a typed creature filter, got {filter:?}");
            };
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected the Creature type filter, got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.controller,
                Some(ControllerRef::Opponent),
                "CR 608.2h: \"under an opponent's control\" is the dead permanent's \
                 last-known controller"
            );
            assert!(
                tf.properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::InZone {
                        zone: Zone::Battlefield
                    }
                )),
                "CR 109.4: the dies condition is battlefield-scoped, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected the opponent-control zone-change count, got {other:?}"),
    }
    assert_eq!(
        trigger.constraint,
        Some(TriggerConstraint::OnlyDuringYourTurn)
    );

    // Reach-guard for the swallow negatives: execute is the Treasure token, and
    // its conditional sibling (the 3+ Treasures transform) survived.
    let execute = trigger
        .execute
        .as_ref()
        .expect("the End trigger has an execute body");
    assert!(
        matches!(&*execute.effect, Effect::Token { .. }),
        "execute must be the Treasure token effect, not Unimplemented: {:?}",
        execute.effect
    );
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("the \"Then if you control three or more Treasures\" sub-ability survives");
    assert!(
        matches!(&*sub.effect, Effect::Transform { .. }),
        "the conditional sub-ability must stay the transform: {:?}",
        sub.effect
    );

    assert!(
        !has_swallowed(HUNT_THE_MARK, "Sidequest: Hunt the Mark", "Condition_If"),
        "Condition_If must clear once the intervening-if attaches"
    );
    assert!(
        !has_swallowed(
            HUNT_THE_MARK,
            "Sidequest: Hunt the Mark",
            "Duration_ThisTurn"
        ),
        "Duration_ThisTurn must clear: the `ZoneChangeCountThisTurn` quantity is \
         the typed evidence the detector's unit probe reads"
    );
}

// ---------------------------------------------------------------------------
// Runtime harness
// ---------------------------------------------------------------------------

/// One interjected instant cast: `player` casts `spell` at `target` the first
/// time they receive priority, then the ordinary loop resumes. Mirrors
/// `rules.rs`'s `PriorityResponse` interjection (99-120).
struct PriorityResponse {
    player: PlayerId,
    spell: ObjectId,
    target: TargetRef,
}

/// Drive interactive windows until `stop` holds. `targets` answers
/// declared-target prompts in FIFO order (CR 601.2c declaration order for
/// spells, CR 603.3d for triggers — the engine surfaces the trigger variant as
/// `WaitingFor::TriggerTargetSelection`); `attach_choices` answers a parked
/// resolution-time Attach host choice (`WaitingFor::EffectZoneChoice` with
/// `effect_kind: EffectKind::Attach`, CR 115.1d + CR 608.2d) in the same FIFO
/// order. The one turn-based declaration the helper answers is the active
/// player's attack declaration: CR 508.1a lets the active player choose which
/// creatures, IF ANY, attack, so a row whose board holds a legal attacker but
/// whose scenario is a no-attack row submits the empty declaration explicitly —
/// a deliberate play, not a silent skip (the prompt only surfaces when a legal
/// attacker exists, and attacking rows declare through `declare_attackers`
/// after stopping on the prompt). Every other window panics: a silent skip must
/// never make a negative row pass vacuously. Mirrors `rules.rs`'s drive loop
/// (98) and the `drain_order_triggers_with_identity` idiom.
fn drive(
    runner: &mut GameRunner,
    targets: &mut Vec<TargetRef>,
    attach_choices: &mut Vec<ObjectId>,
    stop: impl FnMut(&GameRunner) -> bool,
) {
    drive_with_optional_response(runner, targets, attach_choices, None, stop);
}

/// [`drive`] with one interjected instant cast (R11: the host leaves while the
/// trigger is on the stack).
fn drive_with_priority_response(
    runner: &mut GameRunner,
    targets: &mut Vec<TargetRef>,
    attach_choices: &mut Vec<ObjectId>,
    response: PriorityResponse,
    stop: impl FnMut(&GameRunner) -> bool,
) {
    drive_with_optional_response(runner, targets, attach_choices, Some(response), stop);
}

fn drive_with_optional_response(
    runner: &mut GameRunner,
    targets: &mut Vec<TargetRef>,
    attach_choices: &mut Vec<ObjectId>,
    mut response: Option<PriorityResponse>,
    mut stop: impl FnMut(&GameRunner) -> bool,
) {
    for _ in 0..120 {
        if stop(runner) {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                if targets.is_empty() {
                    panic!(
                        "drive: a target prompt arrived with an empty queue: {:?}",
                        runner.state().waiting_for
                    );
                }
                GameAction::ChooseTarget {
                    target: Some(targets.remove(0)),
                }
            }
            // CR 115.1d + CR 608.2d: a resolution-time described attach host is
            // not a target — it arrives as an `EffectZoneChoice`. Answering it
            // from the explicit FIFO (never by skipping) is what makes a
            // wrong-kind/wrong-timing firing observable.
            WaitingFor::EffectZoneChoice {
                cards,
                effect_kind: EffectKind::Attach,
                ..
            } => {
                if attach_choices.is_empty() {
                    panic!(
                        "drive: an Attach EffectZoneChoice arrived with an empty choice queue: \
                         cards={cards:?}"
                    );
                }
                GameAction::SelectCards {
                    cards: vec![attach_choices.remove(0)],
                }
            }
            // CR 508.1a: "chooses which creatures that they control, if any,
            // will attack" — no-attack rows declare that choice here.
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::Priority { player } => {
                match response.take_if(|queued| queued.player == player) {
                    Some(PriorityResponse { spell, target, .. }) => {
                        targets.push(target);
                        GameAction::CastSpell {
                            object_id: spell,
                            card_id: runner.state().objects[&spell].card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        }
                    }
                    None => GameAction::PassPriority,
                }
            }
            other => panic!("drive: unexpected window {other:?}"),
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("drive: action rejected: {err:?}"));
    }
    panic!("drive: the stop condition was not reached within the iteration budget");
}

/// A quiet priority window in `phase` — the stop shape every runtime row uses.
fn at_phase_quiet(runner: &GameRunner, phase: Phase) -> bool {
    runner.state().phase == phase
        && runner.state().stack.is_empty()
        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// Inject a real Equipment back face onto `target` from `donor` — the
/// `cr733_resolved_transform.rs` recipe. CR 701.27a: only a permanent with a
/// back face can transform, and CR 301.5 makes the transformed back face (an
/// Equipment) the attachable object, so without this the attach could never be
/// observed at all.
fn inject_back_face(runner: &mut GameRunner, target: ObjectId, donor: ObjectId) {
    let back_face = snapshot_object_face(&runner.state().objects[&donor]);
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .expect("the double-faced permanent exists")
        .back_face = Some(back_face);
}

/// Token Treasures named "Treasure" controlled by `player` (the
/// `issue_3876_gadrak_treasure_count.rs` idiom).
fn treasure_token_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == player
                && o.zone == Zone::Battlefield
                && o.is_token
                && o.name == "Treasure"
        })
        .count()
}

/// Treasures counted by the SUBTYPE. R6 must use this: its pre-seeded Treasures
/// are non-token artifacts, so the token-filtered count above would report 1 of
/// 3 and the conditional transform could never be observed.
fn treasure_subtype_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == player
                && o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == "Treasure")
        })
        .count()
}

// ===========================================================================
// R1–R3 — Sidequest: Play Blitzball (real trigger pipeline)
// ===========================================================================

/// Play Blitzball board: the enchantment, one attacker (also the sole legal
/// attach host), and a donor Equipment back face on P1. Returns
/// (runner, sidequest, attacker).
fn blitzball_board(attacker_power: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attacker = scenario
        .add_creature(P0, "Grizzly Bears", attacker_power, 4)
        .id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);
    (runner, sidequest, attacker)
}

/// R1: 6 combat damage at end of combat → the trigger fires, transforms, AND
/// attaches the transformed source to the chosen host.
///
/// The attacker is a 4/4: the card's OWN beginning-of-combat ability pumps it
/// to 6/4, so the damage the condition reads is the post-pump 6 (the reach-guard
/// below proves the pump landed — an un-pumped 4 would fail the life assertion).
///
/// Revert-failing: under a U1 revert the condition never attaches and the
/// trigger's clause tree collapses (no transform); under a U3 revert the
/// attachment stays `ParentTarget` and resolves to a self-attach no-op, so
/// `attached_to` stays `None` and the host's attachment list stays empty.
#[test]
fn play_blitzball_six_combat_damage_transforms_and_attaches_to_host() {
    let (mut runner, sidequest, attacker) = blitzball_board(4);

    // Resolve the beginning-of-combat Pump trigger (full card text, so this
    // prompt exists and must be answered — unanswered it would strand the run).
    let mut pump_targets = vec![TargetRef::Object(attacker)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });

    run_combat(&mut runner, vec![attacker], vec![]);

    // End of combat: the trigger fires (condition true) and its described host
    // ("a creature you control") is a RESOLUTION-time choice (CR 115.1d +
    // CR 608.2d) — the attacker is the sole legal host, so it is auto-bound and
    // NO declared-target prompt appears. The empty target queue is
    // load-bearing: a stack-time host regression would panic here rather than
    // pass silently.
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    // Reach-guards: the 6 damage actually happened and the window was reached,
    // so the transform/attach assertions below are not vacuous.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        14,
        "reach-guard: the 4/4 attacker was pumped to 6/4 by the card's own \
         beginning-of-combat ability and dealt 6 combat damage to P1"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);

    assert!(
        runner.state().objects[&sidequest].transformed,
        "CR 120.2a + CR 603.4: 6 combat damage satisfies the intervening-if, so \
         the transform instruction resolves"
    );
    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        Some(AttachTarget::Object(attacker)),
        "CR 608.2c + CR 701.3a: \"then attach it\" attaches the SOURCE to the \
         chosen host"
    );
    assert!(
        runner.state().objects[&attacker]
            .attachments
            .contains(&sidequest),
        "the host must list the transformed Sidequest among its attachments"
    );
}

/// R2: 5 combat damage (< 6) → the intervening-if is false at fire time → NO
/// transform and nothing attached. The attacker is a 3/3, so the card's own
/// beginning-of-combat pump takes it to 5/3 (still below the threshold — a
/// 5-power base would be pumped to 7 and wrongly fire). Reach-guards: the
/// damage happened (life 15) and the EndCombat window was reached.
#[test]
fn play_blitzball_five_combat_damage_does_not_fire() {
    let (mut runner, sidequest, attacker) = blitzball_board(3);

    let mut pump_targets = vec![TargetRef::Object(attacker)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });

    run_combat(&mut runner, vec![attacker], vec![]);

    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        15,
        "reach-guard: 5 combat damage was dealt (CR 120.2a)"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "5 < 6 → the intervening-if (CR 603.4) blocks the trigger at fire time"
    );
    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        None,
        "nothing was attached (the trigger never resolved)"
    );
    assert!(
        !runner.state().objects[&attacker]
            .attachments
            .contains(&sidequest),
        "the host's attachment list must stay empty"
    );
}

/// R3: 6 NONCOMBAT damage, 0 combat damage → NO transform. This is the
/// damage-kind discriminator: P1 sits at 14, so a threshold-only reading
/// (`DamageKindFilter::Any`) would satisfy "6 or more damage" and fire the
/// trigger — and because the row keeps a legal attach host on the battlefield
/// (a 1/1 that stays home), that wrong-kind trigger would resolve the transform
/// and the assertion below would flip. This row previously left the board
/// empty, so a wrong-kind regression would ALSO have left `!transformed` green
/// (the trigger would have been dropped for the missing host) — the negative
/// was not discriminating.
#[test]
fn play_blitzball_six_noncombat_damage_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    // A legal attach host that never attacks: its presence is what makes the
    // wrong-kind regression observable (the trigger would otherwise be dropped
    // for the missing host even if its damage-kind condition wrongly matched).
    let host = scenario.add_creature(P0, "Homebody", 1, 1).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Six Damage", true, "Deal 6 damage to target player.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    runner.cast(bolt).target_player(P1).resolve();

    // The 1/1 host is a legal target for the beginning-of-combat pump, so that
    // prompt exists and is answered first; the host then stays home (the drive
    // submits the empty attack declaration at CR 508.1a). No attach choice is
    // queued: with the correct combat-only condition no trigger fires, while a
    // wrong-kind (`Any`) regression would fire the trigger and open its
    // resolution-time host prompt as an `EffectZoneChoice` — which the drive
    // answers only from the (empty) choice queue and therefore panics on,
    // making the regression observable rather than vacuous.
    let mut targets = vec![TargetRef::Object(host)];
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        14,
        "reach-guard: 6 noncombat damage WAS dealt — only the kind excludes it"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "reach-guard: a legal attach host exists at the end-of-combat window"
    );
    assert!(
        !runner.state().objects[&host].tapped,
        "reach-guard: the host stayed home (did not attack)"
    );
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "CR 120.2a: 6 noncombat damage does not satisfy \"6 or more combat \
         damage\" (the kind axis discriminates, not the threshold); a wrong-kind \
         regression would transform onto the surviving host and fail here"
    );
}

// ===========================================================================
// R4–R6 — Sidequest: Hunt the Mark (real trigger pipeline)
// ===========================================================================

/// Hunt the Mark board: the enchantment, a victim controlled by `victim_owner`
/// (destroyed from hand with a {0} instant in PreCombatMain), a donor Equipment
/// back face, and optional pre-seeded non-token Treasures. Returns
/// (runner, sidequest, victim).
///
/// `victim_controller` optionally DIVERGES the Victim's controller from its
/// owner (`CardBuilder::controlled_by`, battlefield-only) so the condition's
/// controller axis can be told apart from an owner-based matcher.
///
/// The donor supplies the back face only to make `back_face.is_some()` true, so
/// the "Then if you control three or more Treasures" transform is a LIVE
/// possibility rather than a disabled one. The face's identity is irrelevant to
/// these rows (no assertion reads its characteristics); the card's real back
/// face is Yiazmat, Ultimate Mark.
fn hunt_the_mark_board(
    victim_owner: PlayerId,
    victim_controller: Option<PlayerId>,
    seeded_treasures: usize,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Hunt the Mark", HUNT_THE_MARK)
        .id();
    let mut victim_builder = scenario.add_creature(victim_owner, "Victim", 2, 2);
    if let Some(controller) = victim_controller {
        victim_builder.controlled_by(controller);
    }
    let victim = victim_builder.id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    for _ in 0..seeded_treasures {
        scenario
            .add_artifact_from_oracle(P0, "Treasure", TREASURE_TOKEN)
            .with_subtypes(vec!["Treasure"])
            .id();
    }
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    if let Some(controller) = victim_controller {
        // CR 108.4a + CR 400.7 + `reset_for_battlefield_exit`: the divergence is a
        // BATTLEFIELD state — once the Victim dies its controller reverts to its
        // owner — so it is asserted here rather than in the rows, which read the
        // graveyard card.
        assert_eq!(
            runner.state().objects[&victim].controller,
            controller,
            "reach-guard: the Victim's controller diverges from its owner on the battlefield"
        );
    }
    inject_back_face(&mut runner, sidequest, donor);
    runner.cast(destroy).target_object(victim).resolve();
    (runner, sidequest, victim)
}

/// R4: an OPPONENT's creature dies this turn → the end-step trigger fires and
/// creates a Treasure. The victim is in P1's graveyard and `!transformed` (1
/// Treasure < 3) is asserted with the back face injected, so the negative is a
/// live possibility rather than a disabled transform.
#[test]
fn hunt_the_mark_opponent_creature_death_creates_treasure() {
    let (mut runner, sidequest, victim) = hunt_the_mark_board(P1, None, 0);

    let mut targets = Vec::new();
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    // Reach-guard: the death actually happened under P1's control.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the destroyed creature died (CR 700.4)"
    );
    assert_eq!(runner.state().objects[&victim].controller, P1);
    assert_eq!(
        treasure_token_count(&runner, P0),
        1,
        "CR 603.4 + CR 608.2h: a creature died under an opponent's control → one \
         Treasure token"
    );
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the row's window was the end step"
    );
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "1 Treasure < 3 → the conditional transform does not resolve"
    );
}

/// R5: YOUR OWN creature dies the same turn via the identical destroy path →
/// the possessor axis is false and NO Treasure is created. This is the
/// possessor discriminator: it fails if the combinator injected `You` into the
/// condition's typing in a way that matches, and fails under a U2 revert (no
/// condition → the trigger fires on any death). Reach-guard: the creature is in
/// the graveyard.
#[test]
fn hunt_the_mark_own_creature_death_creates_no_treasure() {
    let (mut runner, _sidequest, victim) = hunt_the_mark_board(P0, None, 0);

    let mut targets = Vec::new();
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the creature DID die — only its controller differs"
    );
    assert_eq!(
        treasure_token_count(&runner, P0),
        0,
        "\"died under an opponent's control\" is false for your own creature"
    );
}

/// R4b: the CONTROLLER axis with a DIVERGENT owner — a P0-owned creature
/// controlled by P1 dies this turn → the condition's `controller: Opponent`
/// matches the event-time CONTROLLER, so the end-step trigger CREATES the
/// Treasure. An owner-based matcher would read the P0 owner as "you" and skip
/// the Treasure, so this row fails if the condition's axis is really ownership.
/// Reach-guards: the creature died (graveyard) with the divergent controller,
/// and the end-step window was reached.
#[test]
fn hunt_the_mark_opponent_controlled_creature_death_creates_treasure() {
    let (mut runner, sidequest, victim) = hunt_the_mark_board(P0, Some(P1), 0);

    let mut targets = Vec::new();
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().objects[&victim].owner,
        P0,
        "reach-guard: the creature is OWNED by the sidequest's controller"
    );
    // CR 108.4a + CR 400.7 + `reset_for_battlefield_exit`: the pre-death divergence
    // (P1-controlled) was asserted in the helper; once the creature leaves the
    // battlefield its controller reverts to its owner.
    assert_eq!(
        runner.state().objects[&victim].controller,
        P0,
        "CR 108.4a + CR 400.7: the graveyard card's controller is its owner again"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the destroyed creature died (CR 700.4)"
    );
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the row's window was the end step"
    );
    assert_eq!(
        treasure_token_count(&runner, P0),
        1,
        "CR 603.4 + CR 608.2h: the event-time CONTROLLER was the opponent → one Treasure"
    );
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "1 Treasure < 3 → the conditional transform does not resolve"
    );
}

/// R5b: the mirror divergence — a P1-owned creature controlled by P0 dies this
/// turn → NO Treasure, because the death was under YOUR control even though the
/// owner is the opponent. Pairs with R4b: together they pin the controller axis
/// in both directions against an owner-based matcher.
#[test]
fn hunt_the_mark_opponent_owned_creature_under_your_control_creates_no_treasure() {
    let (mut runner, _sidequest, victim) = hunt_the_mark_board(P1, Some(P0), 0);

    let mut targets = Vec::new();
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().objects[&victim].owner,
        P1,
        "reach-guard: the creature is OWNED by the opponent"
    );
    // CR 108.4a + CR 400.7 + `reset_for_battlefield_exit`: the pre-death divergence
    // (P0-controlled) was asserted in the helper; once the creature leaves the
    // battlefield its controller reverts to its owner.
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "CR 108.4a + CR 400.7: the graveyard card's controller is its owner again"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the creature DID die — only the owner differs"
    );
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the row's window was the end step"
    );
    assert_eq!(
        treasure_token_count(&runner, P0),
        0,
        "the death was under your control, so \"under an opponent's control\" is false"
    );
}

/// R6 (PRESERVATION row, explicitly not revert-failing by itself): two
/// pre-seeded Treasures + the token created by the trigger reach exactly three,
/// so the surviving "Then if you control three or more Treasures" sub-ability
/// resolves and transforms. This pins the non-regression of the conditional
/// clause that U2 must leave attached.
#[test]
fn hunt_the_mark_three_treasures_transform_preservation_row() {
    let (mut runner, sidequest, victim) = hunt_the_mark_board(P1, None, 2);

    let mut targets = Vec::new();
    drive(&mut runner, &mut targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "reach-guard: the opponent's creature died"
    );
    assert_eq!(
        treasure_subtype_count(&runner, P0),
        3,
        "reach-guard: 2 seeded + 1 created = exactly 3 Treasures (counted by \
         subtype, since the seeded ones are non-token artifacts)"
    );
    assert!(
        runner.state().objects[&sidequest].transformed,
        "PRESERVATION row (not revert-failing by itself): the conditional \
         transform fires at 3 Treasures"
    );
}

// ===========================================================================
// R7–R9 — U4: the existential subjects read PER RECIPIENT (CR 603.4)
// ===========================================================================

/// Three-player Play Blitzball board: the enchantment, `attacker_count`
/// attackers of power `attacker_power`, a non-attacking pump target, and a donor
/// Equipment back face on P1. Returns (runner, sidequest, attackers, pump_target).
///
/// The pump target is deliberately NOT one of the attackers: the card's own
/// beginning-of-combat ability must be answered by a creature that does not
/// attack, so each attacker's combat damage stays exactly `attacker_power`.
fn blitzball_multiplayer_board(
    attacker_power: i32,
    attacker_count: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attackers: Vec<ObjectId> = (0..attacker_count)
        .map(|_| {
            scenario
                .add_creature(P0, "Grizzly Bears", attacker_power, 3)
                .id()
        })
        .collect();
    let pump_target = scenario.add_creature(P0, "Homebody", 1, 1).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);
    (runner, sidequest, attackers, pump_target)
}

/// CR 603.4: 3 combat damage to P1 + 3 to P2 → NEITHER recipient reached 6, so
/// the existential "a player was dealt 6 or more combat damage this turn" does
/// NOT fire. Under the ungrouped `Sum` reading (the revert) 3+3 = 6 would fire,
/// transform, and open its resolution-time host prompt — which this row's drive
/// answers only from the empty choice queue and therefore panics on.
#[test]
fn play_blitzball_split_damage_across_two_recipients_does_not_fire() {
    let (mut runner, sidequest, attackers, pump_target) = blitzball_multiplayer_board(3, 2);

    // The card's own beginning-of-combat pump, answered by the non-attacker.
    let mut pump_targets = vec![TargetRef::Object(pump_target)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });

    // CR 508.1a: stop ON the attack declaration, then declare the two attackers
    // at DIFFERENT players.
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .declare_attackers(&[
            (attackers[0], AttackTarget::Player(P1)),
            (attackers[1], AttackTarget::Player(P2)),
        ])
        .expect("both attackers declare against different players");

    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        17,
        "reach-guard: the first attacker connected (3 to P1)"
    );
    assert_eq!(
        runner.state().players[P2.0 as usize].life,
        17,
        "reach-guard: the second attacker connected (3 to P2)"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "neither recipient was dealt 6 — the existential reading must not sum \
         across recipients"
    );
}

/// CR 603.4: 3+3 combat damage to the SAME recipient → that recipient's
/// per-recipient sum is 6 → the existential threshold fires. The two attackers
/// and the pump target all survive, so the host choice parks at resolution and
/// the row answers it from the explicit FIFO (CR 115.1d + CR 608.2d).
#[test]
fn play_blitzball_damage_to_one_recipient_fires() {
    let (mut runner, sidequest, attackers, pump_target) = blitzball_multiplayer_board(3, 2);

    let mut pump_targets = vec![TargetRef::Object(pump_target)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });

    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .declare_attackers(&[
            (attackers[0], AttackTarget::Player(P1)),
            (attackers[1], AttackTarget::Player(P1)),
        ])
        .expect("both attackers declare against the same player");

    let mut attach_choices = vec![pump_target];
    drive(&mut runner, &mut Vec::new(), &mut attach_choices, |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        14,
        "reach-guard: 3+3 landed on ONE seat (a positive control that the \
         per-recipient partition is not a no-records-match bug)"
    );
    assert!(
        runner.state().objects[&sidequest].transformed,
        "one recipient was dealt 3+3 = 6 → the existential threshold fires"
    );
    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        Some(AttachTarget::Object(pump_target)),
        "the chosen host receives the attachment"
    );
    assert!(
        runner.state().objects[&pump_target]
            .attachments
            .contains(&sidequest),
        "the chosen host must list the Sidequest among its attachments"
    );
}

/// CR 120.2a + CR 603.4: 3 COMBAT + 3 NONCOMBAT damage to the same player. The
/// per-recipient sum is 6, but the combat-only qualifier keeps only the 3 combat
/// damage → the condition is false. A legal attach host stays on the battlefield
/// so a wrong-kind (`Any`) regression would fire the trigger and open its
/// resolution-time host prompt, which the drive panics on (empty choice queue).
#[test]
fn play_blitzball_mixed_kinds_on_one_recipient_do_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 3, 3).id();
    // A legal host that never attacks (the pump's target and the attach host).
    let host = scenario.add_creature(P0, "Homebody", 1, 1).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Three Damage", true, "Deal 3 damage to target player.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    // 3 NONCOMBAT damage to P1 before combat.
    runner.cast(bolt).target_player(P1).resolve();

    // Pump on the non-attacker, then the 3-power attacker deals 3 COMBAT damage.
    let mut pump_targets = vec![TargetRef::Object(host)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });
    run_combat(&mut runner, vec![attacker], vec![]);
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        14,
        "reach-guard: 3 noncombat + 3 combat damage WAS dealt — only the kind \
         split excludes the combat-qualified threshold"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "reach-guard: a legal attach host exists at the end-of-combat window, so \
         a wrong-kind firing would be observable rather than vacuous"
    );
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "CR 120.2a: only 3 of the 6 damage was combat — the combat-qualified \
         per-recipient threshold is not met"
    );
}

/// CR 120.1 + CR 120.3 + CR 120.9 (MED): 6 COMBAT damage dealt to an opponent's
/// CREATURE must NOT satisfy "a player was dealt 6 or more combat damage this
/// turn" — CR 120.1 lists players and permanents as distinct damage recipients.
/// The player-only recipient filter refuses object recipients, so the Blitzball
/// condition stays false.
///
/// NOTE on discrimination: Blitzball's printed subject is "a player", whose
/// `TargetFilter::Player` arm was already object-refusing before this fix, so
/// this row pins the class behavior (and the blocked-combat setup) rather than
/// the `an opponent` arm's repair — the revert-failing evidence for that arm is
/// the resolver unit row (`player_damage_threshold_refuses_object_recipients`)
/// and the two synthetic opponent-subject rows below.
#[test]
fn play_blitzball_creature_damage_does_not_satisfy_the_player_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 4, 4).id();
    // A 0/6 blocker: it absorbs the pumped attacker's whole 6 damage and deals
    // nothing back, so no damage reaches a player.
    let blocker = scenario.add_creature(P1, "Test Wall", 0, 6).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    let mut pump_targets = vec![TargetRef::Object(attacker)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });
    // The pumped 6/4 attacker is blocked by the 0/6 wall.
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        20,
        "reach-guard: no combat damage reached the player (the blocker absorbed it)"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "reach-guard: the blocker took 6 lethal damage and died"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "reach-guard: the 0-power blocker dealt nothing back"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "CR 120.1 + CR 120.3: 6 damage to an opponent's CREATURE is not damage \
         dealt to a player, so the player threshold is not met"
    );
}

/// Synthetic class row for the existential OPPONENT subject (the corpus cards
/// with it — Lightning Phoenix, Spinerock Knoll — need their own scaffolding).
/// "if an opponent was dealt 3 or more damage this turn" must NOT be satisfied
/// by 3 combat damage dealt to an opponent's CREATURE. Revert-failing: with the
/// pre-fix contentless `Typed{Opponent}` recipient the creature's damage matched
/// (its controller is the opponent) and the trigger drew a card.
#[test]
fn opponent_damage_threshold_refuses_creature_damage() {
    let (mut runner, attacker, blocker, _enchantment) = opponent_threshold_board();

    // The 3/3 attacker is blocked by the 0/3 wall: all 3 damage goes to the
    // creature, none to a player.
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        20,
        "reach-guard: no damage reached the player"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "reach-guard: the blocker took 3 lethal damage and died"
    );
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "CR 120.1 + CR 120.3: damage to an opponent's CREATURE does not satisfy \
         the opponent-player threshold"
    );
}

/// Paired positive for the synthetic opponent-subject row: the same 3 damage
/// dealt to the opponent PLAYER satisfies the threshold and the trigger draws.
#[test]
fn opponent_damage_threshold_fires_on_player_damage() {
    let (mut runner, attacker, _blocker, _enchantment) = opponent_threshold_board();

    // Unblocked: the 3/3 attacker deals 3 to P1.
    run_combat(&mut runner, vec![attacker], vec![]);
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::End)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        17,
        "reach-guard: 3 damage reached the player"
    );
    assert_eq!(
        hand_len(&runner, P0),
        1,
        "the opponent-player threshold is met and the end-step trigger draws"
    );
}

/// Board for the two synthetic opponent-subject rows: P0's end-step enchantment
/// ("if an opponent was dealt 3 or more damage this turn, draw a card"), a 3/3
/// attacker, P1's 0/3 blocker, and a stocked library. Returns
/// (runner, attacker, blocker, enchantment).
fn opponent_threshold_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    const SYNTHETIC: &str = "At the beginning of your end step, if an opponent \
        was dealt 3 or more damage this turn, draw a card.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let enchantment = scenario
        .add_enchantment_from_oracle(P0, "Opponent Threshold Probe", SYNTHETIC)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Test Wall", 0, 3).id();
    scenario.with_library_top(P0, &["D1", "D2", "D3"]);
    let runner = scenario.build();
    (runner, attacker, blocker, enchantment)
}

/// P0's hand size.
fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

// ===========================================================================
// R10–R13 — U5: the described attach host is chosen while resolving
// ===========================================================================

/// CR 115.1d + CR 608.2d + CR 609.3: with no creature left at end of combat,
/// the Sidequest STILL transforms and only the attach does nothing.
///
/// This row fails before the timing fix: the described host was a mandatory
/// STACK-time target slot, so with no legal creature the whole triggered ability
/// was removed (CR 603.3d) and the transform never happened.
#[test]
fn play_blitzball_transforms_without_a_legal_attach_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 4, 4).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    let mut pump_targets = vec![TargetRef::Object(attacker)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });
    run_combat(&mut runner, vec![attacker], vec![]);

    // Destroy the 4/4 (pumped to 6/4) after combat damage and before the
    // end-of-combat step, so the described host has no candidate when the
    // trigger fires.
    runner.cast(destroy).target_object(attacker).resolve();
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "reach-guard: the only potential attach host is gone before end of combat"
    );

    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        14,
        "reach-guard: the 4/4 was pumped to 6/4 and dealt 6 combat damage"
    );
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert!(
        runner.state().objects[&sidequest].transformed,
        "CR 609.3: the transform instruction still resolves when the attach has \
         no legal host (before the fix the whole trigger was dropped)"
    );
    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        None,
        "the attach has no legal host and does nothing"
    );
    assert!(
        !runner.state().objects[&attacker]
            .attachments
            .contains(&sidequest),
        "the dead attacker's attachment list must stay empty"
    );
}

/// CR 115.1d + CR 608.2d: the host is chosen while the trigger RESOLVES, so a
/// host that leaves play while the trigger is on the stack changes the choice —
/// the trigger still transforms and the attach does nothing (CR 609.3).
///
/// Revert-failing twice: with the pre-fix stack-time slot the trigger is
/// announced with the attacker as its declared target, so destroying it makes
/// the ability illegal and the transform never happens; a fire-time host
/// snapshot would attach the source to the dead host.
#[test]
fn play_blitzball_host_leaves_while_trigger_is_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    // The sole creature is both the attacker and the only potential host.
    let host = scenario.add_creature(P0, "Grizzly Bears", 4, 4).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    // P1 (the non-active player) holds the instant that removes the host.
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    let mut pump_targets = vec![TargetRef::Object(host)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });
    run_combat(&mut runner, vec![host], vec![]);

    // Precondition (asserted, not assumed): the end-of-combat trigger is ON THE
    // STACK and P1 holds priority.
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        !r.state().stack.is_empty()
            && matches!(
                r.state().waiting_for,
                WaitingFor::Priority { player } if player == P1
            )
    });
    assert!(
        !runner.state().stack.is_empty(),
        "precondition: the trigger is waiting on the stack"
    );

    // P1 destroys the only potential host while the trigger waits.
    drive_with_priority_response(
        &mut runner,
        &mut Vec::new(),
        &mut Vec::new(),
        PriorityResponse {
            player: P1,
            spell: destroy,
            target: TargetRef::Object(host),
        },
        |r| at_phase_quiet(r, Phase::EndCombat),
    );

    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Graveyard,
        "reach-guard: the host left the battlefield before the trigger resolved"
    );
    assert!(
        runner.state().objects[&sidequest].transformed,
        "the transform still resolves — the resolution-time host choice simply \
         finds no candidate"
    );
    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        None,
        "the host is gone; the attach does nothing (CR 609.3)"
    );
}

/// CR 608.2c + CR 115.1d: with TWO legal hosts the choice is parked, and it
/// arrives AFTER the transform (the instructions are followed in order). The
/// prompt offers exactly the surviving creatures; the chosen host receives the
/// attachment and the unchosen one does not.
#[test]
fn play_blitzball_two_hosts_prompt_after_the_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sidequest = scenario
        .add_enchantment_from_oracle(P0, "Sidequest: Play Blitzball", PLAY_BLITZBALL)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 4, 4).id();
    let host_a = scenario.add_creature(P0, "Homebody A", 1, 1).id();
    let host_b = scenario.add_creature(P0, "Homebody B", 1, 1).id();
    let donor = scenario
        .add_artifact_from_oracle(P1, "World Champion, Celestial Weapon", WORLD_CHAMPION)
        .with_subtypes(vec!["Equipment"])
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    inject_back_face(&mut runner, sidequest, donor);

    // Pump the ATTACKER (4/4 → 6/4), so combat deals the threshold 6; the two
    // home creatures then survive as the only legal hosts.
    let mut pump_targets = vec![TargetRef::Object(attacker)];
    drive(&mut runner, &mut pump_targets, &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::BeginCombat)
    });
    run_combat(&mut runner, vec![attacker], vec![]);

    // The attacker dies after dealing its 6, leaving exactly two hosts.
    runner.cast(destroy).target_object(attacker).resolve();
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "reach-guard: the attacker left, so exactly two legal hosts remain"
    );

    // Stop AT the parked host choice.
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::EffectZoneChoice { effect_kind, .. } if effect_kind == EffectKind::Attach
        )
    });

    assert!(
        runner.state().objects[&sidequest].transformed,
        "CR 608.2c: the transform instruction precedes the attach, so it has \
         already resolved when the host prompt appears"
    );
    let cards = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
        other => panic!("expected the parked attach host choice, got {other:?}"),
    };
    assert_eq!(
        cards.len(),
        2,
        "exactly the two surviving creatures are legal hosts: {cards:?}"
    );
    assert!(cards.contains(&host_a) && cards.contains(&host_b));
    assert!(
        !cards.contains(&attacker),
        "the destroyed attacker is not a host candidate"
    );
    assert!(
        !cards.contains(&sidequest),
        "the source itself is not a creature and never appears as a host"
    );

    // Choose host B; the attachment must land there and nowhere else.
    runner
        .act(GameAction::SelectCards {
            cards: vec![host_b],
        })
        .expect("the parked host choice must accept the selection");
    drive(&mut runner, &mut Vec::new(), &mut Vec::new(), |r| {
        at_phase_quiet(r, Phase::EndCombat)
    });

    assert_eq!(
        runner.state().objects[&sidequest].attached_to,
        Some(AttachTarget::Object(host_b)),
        "the chosen host receives the attachment"
    );
    assert!(
        runner.state().objects[&host_b]
            .attachments
            .contains(&sidequest),
        "the chosen host must list the Sidequest among its attachments"
    );
    assert!(
        !runner.state().objects[&host_a]
            .attachments
            .contains(&sidequest),
        "the unchosen host must not receive the attachment"
    );
}

/// Stonehewer Giant's searched-up Equipment (verbatim Oracle text).
const STONEHEWER_GIANT: &str = "Vigilance\n{1}{W}, {T}: Search your library for an Equipment card, put it onto the battlefield, attach it to a creature you control, then shuffle.";

/// Floating mana for activation costs (the `issue_605_calming_licid.rs` idiom).
fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 115.1d + CR 608.2d: the MOVED-CARD attach class. Stonehewer Giant searches
/// an Equipment onto the battlefield and attaches it to "a creature you control"
/// — a described host — so the host is chosen while the ability resolves. The
/// Giant is the only creature, so the single legal host is auto-bound and the
/// moved Equipment ends up attached to it (the attachment role still resolves
/// through the shared `resolve_attachment_ids` cascade).
#[test]
fn stonehewer_giant_searched_equipment_attaches_to_the_sole_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_from_oracle(P0, "Stonehewer Giant", 4, 4, STONEHEWER_GIANT)
        .id();
    let equipment = scenario.add_card_to_library_top(P0, "Bonesplitter");
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::White));
    let mut runner = scenario.build();
    {
        // The search filter is the Equipment subtype; the card must read as an
        // Equipment CARD in the library for the search to find it.
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&equipment)
            .expect("the library card exists");
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.base_card_types.subtypes.push("Equipment".to_string());
    }

    // CR 701.23a: the activation halts at the search offer.
    let outcome = runner.activate(giant, 0).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => assert!(
            cards.contains(&equipment),
            "the Equipment card must be offered: {cards:?}"
        ),
        other => panic!("expected SearchChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![equipment],
        })
        .expect("selecting the Equipment must continue resolution");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "reach-guard: the searched Equipment left the library and entered play"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(giant)),
        "the Giant is the sole legal host, so the moved-card attach auto-binds it"
    );
    assert!(
        runner.state().objects[&giant]
            .attachments
            .contains(&equipment),
        "the Giant must list the Equipment among its attachments"
    );
}

// ===========================================================================
// Fumble — the plural-anaphor attachment is honestly unsupported
// ===========================================================================

/// Verbatim Oracle text (Scryfall / the local export).
const FUMBLE: &str = "Return target creature to its owner's hand. Gain control of all Auras and Equipment that were attached to it, then attach them to another creature.";

/// CR 608.2c (rules of English — number agreement) + CR 400.7 (maintainer
/// finding, honest-coverage option): Fumble's "then attach them to another
/// creature" is a PLURAL-ANAPHOR Attach whose INTENDED attachment set is the
/// Auras/Equipment the previous instruction gained control of. `GainControlAll`
/// publishes no typed set and `TargetFilter` has no set-valued anaphor, so the
/// clause is REFUSED at parse time
/// (`Effect::unimplemented("plural_attachment_anaphor")`) and the card reports
/// as unsupported — an honest gap instead of an attach bound to the wrong
/// object.
///
/// The coverage-level honesty is pinned in
/// `attach_plural_anaphor_coverage_honesty.rs`; this row is the runtime
/// companion proving that the two MODELLED clauses in front of the refusal
/// still resolve through the real cast pipeline, and that the refused clause
/// contributes nothing.
///
/// MEASURED RUNTIME OUTCOME: the bounce and the control change resolve; nothing
/// is newly attached; the host-less Aura leaves play via the unattached-Aura
/// state-based action (CR 704.5m) and the Equipment stays on the battlefield,
/// merely unattached (CR 704.5n).
#[test]
fn fumble_plural_attachment_anaphor_is_unsupported() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Fumble Target", 2, 2).id();
    let other = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    // An Aura the caster does NOT control (the control change is observable) and
    // an Equipment the caster does not control. The Equipment survives the
    // bounce (CR 704.5n), so the control-change and "nothing newly attached"
    // assertions stay meaningful after the host leaves.
    let aura = scenario
        .add_enchantment_from_oracle(
            P1,
            "Test Aura",
            "Enchant creature\nEnchanted creature can't attack or block.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P1, "Test Sword", "Equipped creature gets +2/+0.")
        .with_subtypes(vec!["Equipment"])
        .id();
    let fumble = scenario
        .add_spell_to_hand_from_oracle(P0, "Fumble", false, FUMBLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    attach::attach_to(runner.state_mut(), aura, victim);
    attach::attach_to(runner.state_mut(), equipment, victim);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(victim)),
        "precondition: the Aura is attached to the Fumble target"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(victim)),
        "precondition: the Equipment is attached to the Fumble target"
    );
    assert_eq!(runner.state().objects[&aura].controller, P1);
    assert_eq!(runner.state().objects[&equipment].controller, P1);

    // The refused attach clause declares no host slot, so the ONLY announced
    // target is the bounce target: under-supplying a target here would panic if
    // the attach clause still claimed a host slot, which is the parse-level
    // evidence that the clause is gone.
    let outcome = runner.cast(fumble).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell must resolve to a quiet priority window — no error, no prompt: {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guards: the bounce and the control change both resolved.
    assert_eq!(
        outcome.zone_of(victim),
        Zone::Hand,
        "the target creature is returned to its owner's hand"
    );
    assert_eq!(
        runner.state().objects[&equipment].controller,
        P0,
        "control of the attached Equipment is gained"
    );

    // The refused clause contributes nothing: nothing was NEWLY attached. The
    // Equipment is merely unattached (CR 704.5n) and the host-less Aura left
    // play via the unattached-Aura state-based action (CR 704.5m).
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        None,
        "the Equipment is not attached to anything"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        None,
        "the Aura is not attached to anything"
    );
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "reach-guard: CR 704.5m swept the host-less Aura to the graveyard"
    );
    assert!(
        !runner.state().objects[&other]
            .attachments
            .contains(&equipment),
        "the bystander creature must not receive the Equipment"
    );
    assert!(
        !runner.state().objects[&other].attachments.contains(&aura),
        "the bystander creature must not receive the Aura"
    );
    // Reach-guard (CR 704.5n: an Equipment stays in play when its host leaves):
    // the Equipment must still be on the battlefield, otherwise the two guards
    // above could pass vacuously by the candidate object having left play.
    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "reach-guard: the Equipment survives its host leaving the battlefield"
    );
}
