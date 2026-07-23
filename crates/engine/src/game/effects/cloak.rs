use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, CloakExileMember, GameState};
use crate::types::zones::Zone;

/// CR 701.58a: Cloak — put the top card of a player's library onto the
/// battlefield face down as a 2/2 creature **with ward {2}**. Like manifest
/// (CR 701.40a), a cloaked creature card can later be turned face up for its
/// mana cost; the sole behavioral difference is the ward {2} the cloaked
/// permanent enters with (granted via `FaceDownProfile::cloaked_2_2`).
///
/// `target` selects whose library is cloaked from (mirrors `Effect::Manifest`):
/// `Controller` for "you cloak the top card of your library",
/// `ParentTargetController` / `TriggeringPlayer` for relative-player bodies.
///
/// `object_source` selects WHICH cards are cloaked. `None` is the CR 701.58e
/// library-top source (Cryptic Coat, Ransom Note). `Some(filter)` names
/// explicit objects a preceding `Effect::ChooseFromZone` chose and forwarded
/// onto this ability's `targets` — Vannifar's "cloak a card from your hand".
///
/// `enters_under` is the CR 110.2a controller-on-entry override: the player
/// instructed to cloak puts the card onto the battlefield, so it enters under
/// that player's control (Etrata, Deadly Fugitive — the cloaker controls the
/// face-down card cloaked off an opponent's library, while the library owner
/// keeps owning it). `None` keeps the owner default.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, count, object_source, enters_under) = match &ability.effect {
        Effect::Cloak {
            target,
            count,
            object_source,
            enters_under,
        } => (
            target.clone(),
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            object_source.clone(),
            enters_under.clone(),
        ),
        _ => return Err(EffectError::MissingParam("count".to_string())),
    };

    let player = super::resolve_player_for_context_ref(state, ability, &target);
    // CR 110.2a: resolve the cloaking-player override through the single
    // canonical authority shared with ChangeZone/ChangeZoneAll/Manifest.
    let controller = super::change_zone::resolve_enters_under_player(
        state,
        ability,
        "Cloak",
        enters_under.as_ref(),
    )?;

    match object_source {
        // CR 701.58a: Cloak explicit objects chosen upstream. Two source shapes:
        //
        // (A) A face-down PILE modeled as the chain's tracked object set (Expose
        //     the Culprit's "Exile any number of face-up creatures you control
        //     with disguise in a face-down pile, shuffle that pile, then cloak
        //     them"). The chosen creatures are on the BATTLEFIELD and a preceding
        //     `Effect::Shuffle` already reordered the pile (CR 701.24a), so the
        //     order lives only in `tracked_object_sets` — read it DIRECTLY,
        //     order-preserving (`ability.targets` never reflects the shuffle).
        //     Because `manifest_card` on an already-battlefield permanent is a
        //     total NO-OP (the zone pipeline's Battlefield→Battlefield guard,
        //     CR 603.2g), each member is EXILED first (a real Battlefield→Exile
        //     zone change) and then manifested back from exile. object_id is
        //     stable across zones, so no tracked-set remap is needed.
        Some(TargetFilter::TrackedSet { .. }) => {
            // CR 608.2c: bind the `TrackedSetId(0)` sentinel to the chain's
            // published pile (the same set `Effect::Shuffle` just reordered).
            let member_ids: Vec<crate::types::identifiers::ObjectId> =
                match crate::game::targeting::resolve_tracked_set_sentinel(
                    state,
                    TargetFilter::TrackedSet {
                        id: crate::types::identifiers::TrackedSetId(0),
                    },
                ) {
                    TargetFilter::TrackedSet { id } => state
                        .tracked_object_sets
                        .get(&id)
                        .cloned()
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };

            // CR 603.10a: capture attachment identities while the departing
            // creatures still exist on the battlefield. Zone delivery severs the
            // live host edge before LTB trigger collection, so the typed tail
            // needs this pre-event snapshot to clear the former attachment
            // back-edges after the whole departure batch settles.
            let members = member_ids
                .into_iter()
                .map(|object_id| CloakExileMember {
                    object_id,
                    attachments: state
                        .objects
                        .get(&object_id)
                        .map(|object| object.attachments.clone())
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>();
            let requests = members
                .iter()
                .map(|member| {
                    ZoneMoveRequest::effect(member.object_id, Zone::Exile, ability.source_id)
                })
                .collect();

            // CR 614.1 + CR 616.1: this is a real effect-owned
            // Battlefield→Exile batch. Its detach/manifest tail belongs to the
            // typed completion so it cannot run before a replacement choice, and
            // it can re-park if an individual face-down entry later needs one.
            let result = zone_pipeline::move_objects_simultaneously_then(
                state,
                requests,
                Some(BatchCompletion::CloakExileDeliveryComplete {
                    player,
                    source_id: ability.source_id,
                    members,
                    enters_under: controller,
                }),
                events,
            );
            if matches!(result, BatchMoveResult::NeedsChoice) {
                return Ok(());
            }

            // The synchronous completion already performed the manifest tail and
            // emitted `EffectResolved`; do not let the common epilogue duplicate it.
            return Ok(());
        }
        // (B) An explicit object set forwarded onto `ability.targets` by a parent
        //     `ChooseFromZone` (Vannifar's "cloak a card from your hand"). Those
        //     cards live in a non-battlefield zone, so `manifest_card` is a real
        //     move (CR 608.2c — later instructions read the earlier selection).
        //     Each is turned face down as a 2/2 with ward {2} (CR 701.58a).
        Some(filter) => {
            let object_ids = crate::game::effects::effect_object_targets(&filter, &ability.targets);
            for object_id in object_ids {
                crate::game::morph::manifest_card(
                    state,
                    player,
                    object_id,
                    ability.source_id,
                    crate::types::ability::FaceDownProfile::cloaked_2_2(),
                    controller,
                    events,
                )
                .map_err(|e| EffectError::MissingParam(format!("{e}")))?;
            }
        }
        // CR 701.58e: cloak one at a time; CR 701.58a + CR 110.2a: each enters
        // face down under the cloaking player's control via `manifest_card`, the
        // single face-down-entry authority (attributed to the instructing
        // ability's source, matching Manifest).
        None => {
            for _ in 0..count {
                let object_id = match crate::game::morph::top_library_object(state, player) {
                    Ok(id) => id,
                    // The library owner has no cards left — stop cloaking.
                    Err(_) => break,
                };
                crate::game::morph::manifest_card(
                    state,
                    player,
                    object_id,
                    ability.source_id,
                    crate::types::ability::FaceDownProfile::cloaked_2_2(),
                    controller,
                    events,
                )
                .map_err(|e| EffectError::MissingParam(format!("{e}")))?;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.58a + CR 603.10a + CR 614.1 + CR 616.1: Finish the tracked-pile
/// cloak only after every proposed Battlefield→Exile move settles. A redirect
/// that leaves a member on the battlefield did not make it leave, so its
/// attachment edges stay intact. A redirect to any other zone did make it
/// leave, so the captured attachment back-edges are cleared, but only a member
/// that actually settled in Exile is manifested face down. This is the engine's
/// ruling for Expose-style "exile ... then cloak them": a card redirected away
/// from the exile pile is not re-manifested from a zone it never reached.
pub(crate) fn complete_tracked_set_exile_delivery(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    members: Vec<CloakExileMember>,
    enters_under: Option<crate::types::player::PlayerId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    for (index, member) in members.iter().enumerate() {
        let Some(zone) = state
            .objects
            .get(&member.object_id)
            .map(|object| object.zone)
        else {
            continue;
        };
        if zone == Zone::Battlefield {
            continue;
        }

        // CR 400.7 + CR 704.5m/704.5n: preserve the former attachment handling
        // after the whole departure batch settled. `ObjectId` is reused across
        // zones, so its former attachments must not point at a new incarnation.
        for &attachment_id in &member.attachments {
            if let Some(attachment) = state.objects.get_mut(&attachment_id) {
                attachment.attached_to = None;
            }
        }

        if zone != Zone::Exile {
            continue;
        }

        // `manifest_card` is itself the single replacement-aware authority for
        // the face-down battlefield entry (CR 701.58a); do not consult the zone
        // pipeline a second time here.
        let waiting_for_before_entry = state.waiting_for.clone();
        crate::game::morph::manifest_card(
            state,
            player,
            member.object_id,
            source_id,
            crate::types::ability::FaceDownProfile::cloaked_2_2(),
            enters_under,
            events,
        )
        .expect("a settled Cloak batch member exists for face-down entry");

        if state.waiting_for != waiting_for_before_entry {
            // CR 614.1 + CR 616.1: An individual manifest entry can park on a
            // replacement choice. Keep only the unstarted tail in this typed
            // completion; the current entry is already parked by `manifest_card`.
            crate::game::zone_pipeline::defer_completion_on_pause(
                state,
                BatchCompletion::CloakExileDeliveryComplete {
                    player,
                    source_id,
                    members: members[index + 1..].to_vec(),
                    enters_under,
                },
            );
            crate::game::layers::mark_layers_full(state);
            return BatchMoveResult::NeedsChoice;
        }
    }

    // The detach/exile/return churn changed the attachment graph and P/T; force
    // a layer recompute so downstream reads settle (mirrors exit severing).
    crate::game::layers::mark_layers_full(state);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Cloak,
        source_id,
        subject: None,
    });
    BatchMoveResult::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::{Keyword, WardCost};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn cloak_top_card_enters_face_down_with_ward_two() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let card = create_object(
            &mut state,
            CardId(70158),
            player,
            "Cloaked Card".to_string(),
            Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::Cloak {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                object_source: None,
                // CR 110.2a: pins the owner-default path — no controller override.
                enters_under: None,
            },
            vec![],
            ObjectId(999),
            player,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&card];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.face_down);
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        // allow-raw-authority: unit test asserts the exact Ward {2} cost the cloak profile grants on the raw keyword vec
        assert!(obj.keywords.iter().any(|keyword| matches!(
            keyword,
            Keyword::Ward(WardCost::Mana(cost)) if *cost == ManaCost::generic(2)
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to, .. } if *object_id == card && *to == Zone::Battlefield)));
    }

    // ---------------------------------------------------------------------
    // Expose the Culprit mode 2 — the pile → shuffle → cloak chain, driven
    // end-to-end through the cast pipeline (CR 701.24a + CR 701.58a/e).
    // ---------------------------------------------------------------------
    use crate::game::game_object::AttachTarget;
    use crate::game::scenario::{GameRunner, GameScenario, P0};
    use crate::types::ability::{TargetRef, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::events::PlayerActionKind;
    use crate::types::game_state::{ActionResult, WaitingFor};
    use crate::types::phase::Phase;

    // Verbatim Oracle text so the runtime tests exercise the real WithContext
    // parser branch (`parse_exile_pile_shuffle_cloak_ir`) plus the whole
    // ChooseObjectsIntoTrackedSet → Shuffle{TrackedSet} → Cloak{TrackedSet} chain.
    const EXPOSE_ORACLE: &str = "Choose one or both —\n\
        • Turn target face-down creature face up.\n\
        • Exile any number of face-up creatures you control with disguise in a face-down pile, shuffle that pile, then cloak them.";

    fn disguise_creature(scenario: &mut GameScenario, name: &str) -> ObjectId {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_keyword(Keyword::Disguise(ManaCost::generic(3).into()))
            .id()
    }

    /// Commit an Expose mode-2 cast (mode index 1 only), resolve to the pile
    /// selection prompt, select `chosen`, and return the selection's
    /// `ActionResult` — its `.events` carry the Shuffle+Cloak resolution and the
    /// post-action state-based actions (CR 704.3).
    fn cast_mode2_select(
        runner: &mut GameRunner,
        spell: ObjectId,
        chosen: &[ObjectId],
    ) -> ActionResult {
        // `.commit()` puts the modal spell on the stack; dropping the returned
        // CastCommit at the statement end releases its &mut borrow of `runner`.
        runner.cast(spell).modes(&[1]).commit();
        // Resolve the spell — the ChooseObjectsIntoTrackedSet head raises the
        // interactive pile-selection prompt (CR 608.2c).
        runner.advance_until_stack_empty();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseObjectsSelection { .. }
            ),
            "expected ChooseObjectsSelection, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::SelectTargets {
                targets: chosen.iter().map(|&id| TargetRef::Object(id)).collect(),
            })
            .expect("pile selection accepted")
    }

    /// CR 701.24a + CR 701.58a/e: casting mode 2 exiles the chosen face-up
    /// disguise creatures into the pile, shuffles it, and cloaks them back — as
    /// fresh face-down 2/2s with ward {2}, in the shuffled order, with NO
    /// library-shuffle side effect.
    #[test]
    fn expose_mode2_cloaks_pile_in_shuffled_order_without_library_shuffle() {
        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let names = ["Dis A", "Dis B", "Dis C", "Dis D", "Dis E"];
        let creatures: Vec<ObjectId> = names
            .iter()
            .map(|n| disguise_creature(&mut scenario, n))
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Expose the Culprit", true, EXPOSE_ORACLE)
            .id();
        let mut runner = scenario.build();

        let result = cast_mode2_select(&mut runner, spell, &creatures);

        // (a) CR 701.58a: every chosen creature is now a face-down 2/2 ward {2}.
        for &id in &creatures {
            let obj = &runner.state().objects[&id];
            assert!(obj.face_down, "creature {id:?} must be face down");
            assert_eq!(obj.power, Some(2), "creature {id:?} must be a 2/2");
            assert_eq!(obj.toughness, Some(2), "creature {id:?} must be a 2/2");
            assert_eq!(obj.zone, Zone::Battlefield);
            assert!(
                // allow-raw-authority: test — asserts the cloaked creature's OWN ward keyword, already checked on-battlefield above; not an off-zone effective query.
                obj.keywords.iter().any(|k| matches!(
                    k,
                    Keyword::Ward(WardCost::Mana(c)) if *c == ManaCost::generic(2)
                )),
                "creature {id:?} must have ward {{2}}"
            );
        }

        // (b) CR 701.24a: reconstruct the cloak (manifest-back) order from the
        // ZoneChanged{→Battlefield} events. It must be a permutation of the
        // selection AND differ from the selection order (the pile was shuffled;
        // seed 7 yields a non-identity permutation of these five creatures).
        let cloak_order: Vec<ObjectId> = result
            .events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } if creatures.contains(object_id) => Some(*object_id),
                _ => None,
            })
            .collect();
        let mut sorted_cloak = cloak_order.clone();
        sorted_cloak.sort_by_key(|i| i.0);
        let mut sorted_sel = creatures.clone();
        sorted_sel.sort_by_key(|i| i.0);
        assert_eq!(
            sorted_cloak, sorted_sel,
            "every selected creature must be cloaked exactly once"
        );
        assert_ne!(
            cloak_order, creatures,
            "the pile shuffle must reorder the cloak order (non-identity seed)"
        );

        // (c) CR 701.24a: a PILE shuffle is not a LIBRARY shuffle. The shuffle
        // effect resolved (reach guard) but emitted NO ShuffledLibrary action, so
        // "whenever you shuffle your library" triggers cannot fire.
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "the pile-shuffle effect must have resolved (reach guard)"
        );
        assert!(
            !result.events.iter().any(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )),
            "a pile shuffle must NOT emit ShuffledLibrary"
        );
    }

    /// CR 400.7 + CR 122.2 + CR 704.5m: the exile-and-return produces a FRESH
    /// object — a chosen creature carrying a +1/+1 counter and an attached Aura
    /// comes back as a face-down 2/2 with NO counter, and the orphaned Aura is
    /// put into its owner's graveyard by the post-resolution SBA. Discriminates
    /// rules-correct object reset from an in-place flip.
    #[test]
    fn expose_mode2_object_reset_drops_counters_and_auras() {
        let mut scenario = GameScenario::new_n_player(2, 3);
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario
            .add_creature(P0, "Countered Disguise", 2, 2)
            .with_keyword(Keyword::Disguise(ManaCost::generic(3).into()))
            .with_plus_counters(1)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Expose the Culprit", true, EXPOSE_ORACLE)
            .id();
        let mut runner = scenario.build();

        // Attach a simple "Enchant creature" Aura to the disguise creature.
        let aura = create_object(
            runner.state_mut(),
            CardId(50001),
            P0,
            "Test Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let a = runner.state_mut().objects.get_mut(&aura).unwrap();
            a.card_types.core_types = vec![CoreType::Enchantment];
            a.card_types.subtypes = vec!["Aura".to_string()];
            a.base_card_types = a.card_types.clone();
            a.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )));
            a.attached_to = Some(AttachTarget::Object(creature));
        }
        runner
            .state_mut()
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(aura);

        // Reach guards: the counter is present and the Aura is attached on the
        // battlefield before the cast.
        assert_eq!(
            runner.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
        assert_eq!(
            runner.state().objects[&aura].attached_to,
            Some(AttachTarget::Object(creature))
        );

        let _ = cast_mode2_select(&mut runner, spell, &[creature]);

        // CR 122.2: the +1/+1 counter is gone (a real Battlefield→Exile move).
        assert_eq!(
            runner.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "the +1/+1 counter must be cleared by the exile-and-return"
        );
        // CR 701.58a: it returns as a fresh face-down 2/2, not a boosted 3/3.
        assert!(runner.state().objects[&creature].face_down);
        assert_eq!(runner.state().objects[&creature].power, Some(2));
        // CR 704.5m: the Aura's host became a new object, so the Aura fell off and
        // the post-resolution SBA put it into its owner's graveyard.
        assert_eq!(
            runner.state().objects[&aura].zone,
            Zone::Graveyard,
            "the Aura must be in the graveyard after the object reset + SBA, got {:?}",
            runner.state().objects[&aura].zone
        );
    }

    /// CR 608.2c: mode 2 with zero eligible creatures is inert — the
    /// empty selection publishes an empty pile, the shuffle and cloak resolve as
    /// no-ops, and no permanent is cloaked. Guards against reading a stale prior
    /// tracked set.
    #[test]
    fn expose_mode2_empty_selection_is_inert() {
        let mut scenario = GameScenario::new_n_player(2, 1);
        scenario.at_phase(Phase::PreCombatMain);
        // A non-disguise bystander — never eligible for the "with disguise" pile.
        let bystander = scenario.add_creature(P0, "No Disguise", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Expose the Culprit", true, EXPOSE_ORACLE)
            .id();
        let mut runner = scenario.build();

        let result = cast_mode2_select(&mut runner, spell, &[]);

        // Reach guard: the chain resolved through Shuffle (so Cloak also ran) —
        // it just had an empty pile to act on.
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "the empty-pile chain must still resolve the Shuffle (reach guard)"
        );
        // Inert: the bystander is untouched and nothing was cloaked.
        let obj = &runner.state().objects[&bystander];
        assert!(!obj.face_down, "bystander must be untouched (face up)");
        assert_eq!(obj.power, Some(3));
        assert!(
            !result.events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    to: Zone::Battlefield,
                    ..
                }
            )),
            "no creature should be cloaked when the pile is empty"
        );
    }

    /// CR 110.2a + CR 701.58a: the tracked-pile cloak returns each settled
    /// member under the CLOAKING player's control, not its owner's. A P1-owned
    /// disguise creature that P0 controls (and can therefore exile into the
    /// pile) comes back as P0's face-down 2/2 — reverted code (an owner-default
    /// manifest entry) would return it under P1.
    #[test]
    fn expose_mode2_cloaks_opponent_owned_member_under_cloaking_player() {
        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        // P1 OWNS the disguise creature ...
        let stolen = scenario
            .add_creature(PlayerId(1), "Stolen Disguise", 2, 2)
            .with_keyword(Keyword::Disguise(ManaCost::generic(3).into()))
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Expose the Culprit", true, EXPOSE_ORACLE)
            .id();
        let mut runner = scenario.build();
        // ... but P0 CONTROLS it, so it is eligible for the "face-up creatures
        // you control with disguise" pile. `base_controller` must carry the
        // override too — the layer recompute rebuilds `controller` from it
        // (CR 613.1b), so a raw `controller` write alone would be clobbered.
        {
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&stolen)
                .expect("the stolen disguise creature exists");
            obj.base_controller = Some(P0);
            obj.controller = P0;
        }

        let _ = cast_mode2_select(&mut runner, spell, &[stolen]);

        let obj = &runner.state().objects[&stolen];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.face_down, "the pile member must return face down");
        // CR 110.2a: the revert-sensitive discriminator — the cloaker (P0)
        // controls the returned face-down permanent; its owner stays P1.
        assert_eq!(
            obj.controller, P0,
            "the cloaking player controls the returned face-down permanent"
        );
        assert_eq!(obj.owner, PlayerId(1), "ownership never changes");
    }
}
