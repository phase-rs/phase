use std::collections::HashSet;
use std::sync::Arc;

use crate::types::events::GameEvent;
use crate::types::game_state::{CastOfferKind, GameState, PayCostKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::{ExileCostSourceZone, Zone};

use super::players;
use super::turn_control;

const HIDDEN_CARD_NAME: &str = "Hidden Card";

/// Returns a filtered copy of the game state for the given viewer.
/// Hides all opponents' hand contents and all library contents except where the
/// viewer is explicitly allowed to see them.
pub fn filter_state_for_viewer(state: &GameState, viewer: PlayerId) -> GameState {
    let mut filtered = state.clone();
    // The replacement-resume cursor is server authority and can retain private
    // object IDs and last-known snapshots from a cost payment.
    filtered.pending_cost_move_resume = None;
    let replacement_candidate_source_ids = match &state.waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => Some(
            candidates
                .iter()
                .map(|candidate| candidate.source_id)
                .collect::<HashSet<_>>(),
        ),
        _ => None,
    };
    let mut hidden_replacement_candidate_source_ids = HashSet::new();
    filtered.pending_begin_game_abilities.clear();
    filtered.resolving_begin_game_abilities = false;

    // Hidden-information + fairness integrity: the game's RNG is a deterministic
    // ChaCha20 stream seeded from `rng_seed`, and that seed is a serialized field
    // of `GameState`. Broadcasting it to clients (every `StateUpdate` /
    // `GameStarted` carries the filtered `GameState`) would let any player
    // reconstruct the stream and predict every future shuffle, draw, coin flip,
    // and random selection — including their own and the opponent's hidden
    // library order, defeating the library redaction below and breaking ranked
    // integrity. The authoritative engine and on-disk persistence operate on the
    // UNFILTERED state, so redacting the seed here (and resetting the skipped RNG
    // handle for good measure) closes the wire leak without affecting
    // server-side randomness or session restore.
    filtered.rng_seed = 0;
    // Also drop the serialized stream position (issue #5466 sibling): a leaked
    // word offset would give an attacker the keystream alignment for free. Zero
    // it so no viewer snapshot carries either the seed or its stream position.
    filtered.rng_word_pos = 0;
    filtered.rng = <rand_chacha::ChaCha20Rng as rand::SeedableRng>::seed_from_u64(0);
    filtered.liminal_entries.clear();
    filtered.pending_liminal_entry_resume = None;

    let can_view_private_for_player =
        |player: PlayerId| viewer_has_private_access_to_player(state, viewer, player);
    let replacement_choice_authorized = matches!(
        &state.waiting_for,
        WaitingFor::ReplacementChoice { player, .. }
            if turn_control::authorized_submitter_for_player(state, *player) == viewer
    );

    // A pending replacement is the authoritative continuation record behind a
    // ReplacementChoice. It carries real replacement sources, so only the
    // prompted player (or their turn controller) may receive it. Other viewers
    // submit no replacement action and must not receive its private source IDs.
    if !replacement_choice_authorized {
        filtered.pending_replacement = None;
    }

    // CR 701.20e: A bare "look at" peek privately reveals card(s) to the looking
    // player only. `dig.rs` and `reveal_hand.rs` record the looker in
    // `private_look_player`; surface the peeked cards to that player without
    // leaking them to opponents.
    let private_look_visible: HashSet<ObjectId> = match state.private_look_player {
        Some(looker) if can_view_private_for_player(looker) => {
            state.private_look_ids.iter().copied().collect()
        }
        _ => HashSet::new(),
    };

    let opponents = players::opponents(state, viewer);
    let opp_hand_ids: Vec<ObjectId> = opponents
        .iter()
        .copied()
        .filter(|&opp| !can_view_private_for_player(opp))
        .flat_map(|opp| filtered.players[opp.0 as usize].hand.iter().copied())
        .collect();
    for obj_id in opp_hand_ids {
        if !is_visible_revealed_card(state, obj_id) && !private_look_visible.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
            record_hidden_replacement_candidate_source(
                replacement_candidate_source_ids.as_ref(),
                &mut hidden_replacement_candidate_source_ids,
                obj_id,
            );
        }
    }

    // CR 608.2d: While an `OpponentGuess` is pending, strip the secret the
    // guesser must not see so the round-trip can't be auto-won. Two redactions:
    //
    //   * `proposition_truth` — the resolved yes/no answer for a
    //     `GuessSubject::Proposition` (The Seventh Doctor: "is the face-down
    //     card's mana value greater than your artifact count"). For that card the
    //     guesser IS the viewer who receives this `WaitingFor`, so leaving the
    //     answer in would let them guess correctly every time. The engine always
    //     resolves correctness on the UNFILTERED state and the frontend never
    //     reads this field, so it is stripped for EVERY viewer.
    //
    //   * the controller's most-recently committed number for a
    //     `GuessSubject::CommittedChoice` (The Toymaker's Trap) — hidden from
    //     everyone except the controller until "then you reveal the number you
    //     chose" makes it public. Only the LAST committed number is hidden;
    //     numbers revealed on earlier upkeeps are already public and stay
    //     visible (re-hiding them would misreport which numbers were used up).
    if let WaitingFor::OpponentGuess {
        player,
        ref options,
        ref choice_type,
        source_id,
        proposition_truth: _,
    } = state.waiting_for
    {
        filtered.waiting_for = WaitingFor::OpponentGuess {
            player,
            options: options.clone(),
            choice_type: choice_type.clone(),
            source_id,
            proposition_truth: None,
        };
        let is_controller = state.objects.get(&source_id).map(|o| o.controller) == Some(viewer);
        if !is_controller {
            if let Some(obj) = filtered.objects.get_mut(&source_id) {
                if let Some(pos) = obj
                    .chosen_attributes
                    .iter()
                    .rposition(|a| matches!(a, crate::types::ability::ChosenAttribute::Number(_)))
                {
                    obj.chosen_attributes.remove(pos);
                }
            }
        }
    }

    let (manifest_dread_visible, manifest_dread_cards): (HashSet<ObjectId>, HashSet<ObjectId>) =
        if let WaitingFor::ManifestDreadChoice {
            player, ref cards, ..
        } = filtered.waiting_for
        {
            let all_cards: HashSet<ObjectId> = cards.iter().copied().collect();
            if can_view_private_for_player(player) {
                (all_cards.clone(), all_cards)
            } else {
                (HashSet::new(), all_cards)
            }
        } else {
            (HashSet::new(), HashSet::new())
        };

    let dig_visible: HashSet<ObjectId> = if let WaitingFor::DigChoice {
        player, ref cards, ..
    } = filtered.waiting_for
    {
        if can_view_private_for_player(player) {
            cards.iter().copied().collect()
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let search_visible: HashSet<ObjectId> =
        if let WaitingFor::SearchChoice {
            player, ref cards, ..
        } = filtered.waiting_for
        {
            if can_view_private_for_player(player) {
                cards.iter().copied().collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

    let effect_zone_hand_cards: HashSet<ObjectId> = if let WaitingFor::EffectZoneChoice {
        zone: Zone::Hand,
        ref cards,
        ..
    } = filtered.waiting_for
    {
        cards.iter().copied().collect()
    } else {
        HashSet::new()
    };
    let drawn_choice_hand_cards: HashSet<ObjectId> =
        if let WaitingFor::DrawnThisTurnTopdeckChoice { ref cards, .. } = filtered.waiting_for {
            cards.iter().copied().collect()
        } else {
            HashSet::new()
        };

    // Heist (Arena digital-only keyword action) and any future
    // `ChooseFromZoneChoice` that operates over a hidden zone (library,
    // opponent's hand) parks candidate object ids on the prompt. The loop
    // below hides every library object by default; the prompt player is
    // supposed to *look at* the candidates (Heist reminder: "Look at three
    // random nonland cards"), so the underlying object identities must be
    // visible to that player. Opponents and spectators keep seeing redacted
    // placeholders — `can_view_private_for_player(player)` is the same gate
    // the manifest/dig/private-look/search prompts use. The cards ARRAY is
    // also redacted for non-prompt viewers at the bottom of this function
    // (the `ChooseFromZoneChoice` redact block); the two protections
    // compose: prompt player sees both the array and the object contents,
    // everyone else sees neither.
    let choose_from_zone_hidden_visible: HashSet<ObjectId> =
        if let WaitingFor::ChooseFromZoneChoice {
            player, ref cards, ..
        } = filtered.waiting_for
        {
            if can_view_private_for_player(player) {
                cards.iter().copied().collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

    // Sandbox debug exposure: a viewer who holds debug permission in a sandbox
    // game (CR is silent; this is an out-of-game capability) sees the names of
    // cards in their *own* library, so the debug "move card from library to
    // hand" picker can identify a specific card. Opponents' libraries remain
    // hidden — sandbox is shared, but reading an opponent's deck is not. The
    // FE's debug picker alphabetizes within each zone bucket, so exposing names
    // does not leak draw order. The actual `library` Vec order on the wire is
    // left untouched (preserving simulate-mode draw semantics) but is never
    // surfaced as draw order anywhere the viewer can observe it.
    let sandbox_self_library_visible =
        state.format_config.allow_debug_actions && state.debug_permitted.contains(&viewer);
    // CR 701.20e + CR 400.2: "looking at a card ... is shown only to the
    // specified player." A player with a continuous "you may look at the top
    // card of your library" permission (MayLookAtTopOfLibrary — Vizier of the
    // Menagerie, Fblthp, Lost on the Range, etc.) privately sees their OWN
    // library top. Engine-authoritative exposure (never client-side): the
    // top-of-library object is the source/render target of cast-from-top
    // (CR 601.2a) and plot-from-top (CR 702.170f) actions, so without this it
    // would be redacted for the very player allowed to act on it. The derived
    // `can_look_at_top_of_library` flag already encodes the static check;
    // `can_view_private_for_player` extends the look to a player controlling
    // this player's turn, mirroring the private-look / face-down look paths.
    let look_top_visible: HashSet<ObjectId> = filtered
        .players
        .iter()
        .filter(|p| p.can_look_at_top_of_library && can_view_private_for_player(p.id))
        .filter_map(|p| p.library.front().copied())
        .collect();
    let all_library_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.library.iter().copied())
        .collect();
    for obj_id in all_library_ids {
        let owner = state.objects.get(&obj_id).map(|o| o.owner);
        let visible = manifest_dread_visible.contains(&obj_id)
            || dig_visible.contains(&obj_id)
            || private_look_visible.contains(&obj_id)
            || search_visible.contains(&obj_id)
            // Heist (and any ChooseFromZoneChoice over a hidden zone) — see
            // `choose_from_zone_hidden_visible` above.
            || choose_from_zone_hidden_visible.contains(&obj_id)
            // CR 701.20b: Revealed cards are visible to all players. For reveal-digs
            // ("reveal the top N"), dig cards are also in revealed_cards and must remain
            // public during DigChoice. For private digs ("look at"), revealed_cards won't
            // contain dig cards, so the exclusion still applies.
            || (state.revealed_cards.contains(&obj_id)
                && !manifest_dread_cards.contains(&obj_id))
            // CR 701.20e: own (or controlled-turn) library top under a
            // MayLookAtTopOfLibrary permission — see `look_top_visible` above.
            || look_top_visible.contains(&obj_id)
            || (sandbox_self_library_visible && owner == Some(viewer));
        if !visible
            && !effect_zone_hand_cards.contains(&obj_id)
            && !drawn_choice_hand_cards.contains(&obj_id)
        {
            hide_card(&mut filtered, obj_id);
            record_hidden_replacement_candidate_source(
                replacement_candidate_source_ids.as_ref(),
                &mut hidden_replacement_candidate_source_ids,
                obj_id,
            );
        }
    }

    // CR 717.2: A player's Attraction deck is a hidden-order supplementary
    // deck, like a library — even its owner doesn't know the order. Redact
    // every unrevealed Attraction card's identity for all viewers, mirroring
    // the library treatment above, so the serialized state can't leak the
    // contents or order of any player's Attraction deck.
    let all_attraction_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.attraction_deck.iter().copied())
        .collect();
    for obj_id in all_attraction_ids {
        if !state.revealed_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
            record_hidden_replacement_candidate_source(
                replacement_candidate_source_ids.as_ref(),
                &mut hidden_replacement_candidate_source_ids,
                obj_id,
            );
        }
    }

    let all_contraption_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.contraption_deck.iter().copied())
        .collect();
    for obj_id in all_contraption_ids {
        if !state.revealed_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
            record_hidden_replacement_candidate_source(
                replacement_candidate_source_ids.as_ref(),
                &mut hidden_replacement_candidate_source_ids,
                obj_id,
            );
        }
    }

    // CR 901.15 + CR 904.4: Planar and scheme decks are hidden-order
    // supplementary decks whose face-down cards live in the command zone. Redact
    // every unrevealed card identity for all viewers, matching the library and
    // Attraction deck treatment above.
    let supplementary_deck_ids: Vec<ObjectId> = filtered
        .planar_deck
        .iter()
        .chain(filtered.scheme_deck.iter())
        .copied()
        .collect();
    for obj_id in supplementary_deck_ids {
        if !state.revealed_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
            record_hidden_replacement_candidate_source(
                replacement_candidate_source_ids.as_ref(),
                &mut hidden_replacement_candidate_source_ids,
                obj_id,
            );
        }
    }

    // CR 406.3: A card exiled face down can't be examined by any player
    // except when an instruction allows it. Two modeled look-permission classes:
    // Foretell (the owner may look, CR 702.143e) and Hideaway (CR 702.75a — the
    // controller of the permanent that exiled the card may look, keyed on the
    // dedicated `ExileLinkKind::HideawayLookable` link). Every other face-down
    // exile class — including plain `TrackedBySource` exiles that grant no
    // look-permission (Bomat Courier's "(You can't look at it.)", Necropotence,
    // Asmodeus) — fails closed and redacts the card for every viewer.
    let hidden_facedown_exile_ids: Vec<ObjectId> = filtered
        .exile
        .iter()
        .copied()
        .filter(|obj_id| {
            state.objects.get(obj_id).is_some_and(|obj| {
                if !obj.face_down {
                    return false;
                }
                // CR 702.143e: foretold card — its owner may look.
                let foretell_ok = obj.foretold && can_view_private_for_player(obj.owner);
                // CR 702.75a + CR 607.2a: the controller of the permanent that
                // exiled this card under Hideaway may look at it. Keyed on the
                // dedicated `HideawayLookable` link kind so plain
                // `TrackedBySource` face-down exiles that grant no look-permission
                // (Bomat Courier, Necropotence, Asmodeus) stay redacted.
                let hideaway_lookable_by_viewer = state.exile_links.iter().any(|link| {
                    link.exiled_id == *obj_id
                        && link.kind == crate::types::game_state::ExileLinkKind::HideawayLookable
                        && state
                            .objects
                            .get(&link.source_id)
                            .is_some_and(|src| can_view_private_for_player(src.controller))
                });
                // CR 406.3a + CR 406.3b: a player who holds an active
                // play-from-exile grant for this face-down card may look at it —
                // the grant that lets them cast it is the same authority that
                // lets them look (single source:
                // `casting::player_may_look_at_facedown_exile`). Scoped by the
                // grant's `granted_to`, so a face-down card exiled by a different
                // source (no grant to this viewer) stays redacted, and the
                // targeted opponent (no grant) cannot see the cards either.
                let play_from_exile_lookable = state.players.iter().any(|pl| {
                    can_view_private_for_player(pl.id)
                        && crate::game::casting::player_may_look_at_facedown_exile(
                            state, obj, pl.id,
                        )
                });
                !(foretell_ok || hideaway_lookable_by_viewer || play_from_exile_lookable)
            })
        })
        .collect();
    for obj_id in hidden_facedown_exile_ids {
        hide_card(&mut filtered, obj_id);
        record_hidden_replacement_candidate_source(
            replacement_candidate_source_ids.as_ref(),
            &mut hidden_replacement_candidate_source_ids,
            obj_id,
        );
    }

    // CR 708.5: "At any time, you may look at a face-down permanent you control
    // (even if it's phased out). You can't look at face-down spells or
    // permanents controlled by another player." Face-down objects on the
    // battlefield (manifest / morph / disguise / cloak) and any future modeled
    // face-down stack spells keep their real identity in `back_face`. That
    // hidden identity is look-permission of the *controller* alone. Strip
    // `back_face` for every viewer who is not the controller so the underlying
    // card never leaks to opponents over the wire. The controller (turn-control
    // aware, matching the rest of this filter) retains it and gets only display
    // identity projected onto the filtered object; CR 708.2 face-down rules
    // characteristics stay intact. DFC back faces (`face_down == false`) are
    // public information and are intentionally left untouched.
    let facedown_object_ids: Vec<ObjectId> = filtered
        .battlefield
        .iter()
        .copied()
        .chain(filtered.stack.iter().map(|entry| entry.id))
        .filter(|obj_id| {
            state
                .objects
                .get(obj_id)
                .is_some_and(|obj| obj.face_down && obj.back_face.is_some())
        })
        .collect();
    for obj_id in facedown_object_ids {
        if let Some(source) = state.objects.get(&obj_id) {
            let controller = source.controller;
            // CR 708.5: the controller always sees their own face-down
            // permanents. A non-controller viewer may additionally see this
            // face-down permanent if they control an active "you may look at
            // face-down [filter] any time" static (CR 708.5 exception) whose
            // affected filter matches this permanent.
            let viewer_may_look = can_view_private_for_player(controller)
                || viewer_may_look_at_face_down(state, obj_id, &can_view_private_for_player);
            if let Some(obj) = filtered.objects.get_mut(&obj_id) {
                if viewer_may_look {
                    reveal_face_down_identity_to_controller(obj);
                } else {
                    redact_face_down_identity_from_observer(obj);
                    record_hidden_replacement_candidate_source(
                        replacement_candidate_source_ids.as_ref(),
                        &mut hidden_replacement_candidate_source_ids,
                        obj_id,
                    );
                }
            }
        }
    }

    // Replacement candidates snapshot their source name before this function
    // redacts the underlying object. Keep that display payload consistent with
    // the filtered object view, while the player making the choice retains the
    // real source identity needed by the action round-trip.
    if let WaitingFor::ReplacementChoice { candidates, .. } = &mut filtered.waiting_for {
        if !replacement_choice_authorized {
            for candidate in candidates {
                let source_is_hidden = candidate.source_id != ObjectId(0)
                    && (hidden_replacement_candidate_source_ids.contains(&candidate.source_id)
                        || !filtered.objects.contains_key(&candidate.source_id));
                if source_is_hidden {
                    candidate.source_id = ObjectId(0);
                    candidate.source_name = HIDDEN_CARD_NAME.to_string();
                }
            }
        }
    }

    if let WaitingFor::ManifestDreadChoice {
        player,
        ref cards,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::ManifestDreadChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                source_id,
            };
        }
    }

    // CR 732.2a: redact hidden-info legal targets in a `LoopShortcut` OFFER for a viewer who is
    // NOT the schema's proposer. The schema is built for the offer's public declaration; this
    // is the SOLE seam that removes a hidden-zone (hand/library) legal target from a viewer who
    // cannot legally see it. Public option sets (`ConvokeTaps` battlefield taps,
    // `TargetRef::Player`) are retained. The per-target drop reuses the EXACT hand-redaction
    // composite (`!is_visible_revealed_card && !private_look_visible`) keyed on each TARGET
    // object's owner + private zone — never the controller's visibility.
    if let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ref certificate,
        ref schema,
    } = state.waiting_for
    {
        if !can_view_private_for_player(proposer) {
            use crate::analysis::decision_template::{
                DecisionPoint, DecisionPointKind, ShortcutDecisionSchema,
            };
            use crate::types::ability::TargetRef;
            // A target object is hidden from this viewer iff it sits in a private zone whose
            // owner the viewer can't privately view AND it isn't otherwise revealed/peeked.
            let target_hidden = |id: ObjectId| -> bool {
                state.objects.get(&id).is_some_and(|obj| {
                    matches!(obj.zone, Zone::Hand | Zone::Library)
                        && !can_view_private_for_player(obj.owner)
                        && !is_visible_revealed_card(state, id)
                        && !private_look_visible.contains(&id)
                })
            };
            let points: Vec<DecisionPoint> = schema
                .points
                .iter()
                .map(|point| {
                    let kind = match &point.kind {
                        DecisionPointKind::Targets { legal_targets } => {
                            DecisionPointKind::Targets {
                                legal_targets: legal_targets
                                    .iter()
                                    .filter(|t| match t {
                                        TargetRef::Object(id) => !target_hidden(*id),
                                        TargetRef::Player(_) => true,
                                    })
                                    .cloned()
                                    .collect(),
                            }
                        }
                        DecisionPointKind::ConvokeTaps { tappable } => {
                            DecisionPointKind::ConvokeTaps {
                                tappable: tappable.clone(),
                            }
                        }
                        DecisionPointKind::Mode { available_modes } => DecisionPointKind::Mode {
                            available_modes: available_modes.clone(),
                        },
                        DecisionPointKind::MayChoice => DecisionPointKind::MayChoice,
                        DecisionPointKind::UnlessBreak => DecisionPointKind::UnlessBreak,
                    };
                    DecisionPoint {
                        slot: point.slot.clone(),
                        kind,
                    }
                })
                .collect();
            // CR 702.51a: recompute the convoke count from the redacted points so the invariant
            // "count == sum of this schema's tappable lengths" holds after visibility filtering
            // (ConvokeTaps are public battlefield objects and are not redacted, so this equals
            // the pre-filter count today; recomputing keeps it correct if that ever changes).
            let convoke_tappable_count = points
                .iter()
                .filter_map(|p| match &p.kind {
                    DecisionPointKind::ConvokeTaps { tappable } => Some(tappable.len()),
                    _ => None,
                })
                .sum();
            filtered.waiting_for = WaitingFor::LoopShortcut {
                proposer,
                predicted_winner,
                certificate: certificate.clone(),
                schema: ShortcutDecisionSchema {
                    iteration_count: schema.iteration_count.clone(),
                    points,
                    convoke_tappable_count,
                },
            };
        }
    }

    if let WaitingFor::DigChoice {
        player,
        library_owner,
        ref cards,
        keep_count,
        up_to,
        ref selectable_cards,
        kept_destination,
        rest_destination,
        source_id,
        enter_tapped,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::DigChoice {
                player,
                library_owner,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                keep_count,
                up_to,
                selectable_cards: selectable_cards.iter().map(|_| ObjectId(0)).collect(),
                kept_destination,
                rest_destination,
                source_id,
                enter_tapped,
            };
        }
    }

    if let WaitingFor::LearnChoice {
        player,
        ref hand_cards,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::LearnChoice {
                player,
                hand_cards: hand_cards.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    // CR 701.38 secret ballot (Truth or Consequences): withhold the running
    // tallies and ballots from EVERY viewer until the simultaneous reveal, so
    // neither later voters nor opponents can infer earlier secret votes. The
    // acting voter still sees their own `options`/`option_labels`. After
    // `VoteResolved` fires the public tally lives in the event log, so there is
    // no residual state leak.
    //
    // NOTE (D6 limitation): this scrubs the per-viewer snapshot only. The local
    // WASM AI computes over the unfiltered thread-local state and can therefore
    // read a human's earlier secret ballot — an accepted hidden-information gap
    // (the AI already sees full hidden state for its own search). Multiplayer
    // human↔human secrecy IS enforced here.
    if let WaitingFor::VoteChoice {
        player,
        remaining_votes,
        ref options,
        ref option_labels,
        ref remaining_voters,
        ref tallies,
        ref per_choice_effect,
        controller,
        source_id,
        actor,
        tally_mode,
        ref candidate_objects,
        ref outcome_template,
        visibility,
        ..
    } = state.waiting_for
    {
        if visibility == crate::types::ability::VoteVisibility::Secret {
            filtered.waiting_for = WaitingFor::VoteChoice {
                player,
                remaining_votes,
                options: options.clone(),
                option_labels: option_labels.clone(),
                remaining_voters: remaining_voters.clone(),
                tallies: vec![0; tallies.len()],
                ballots: crate::im::Vector::new(),
                per_choice_effect: per_choice_effect.clone(),
                controller,
                source_id,
                actor,
                tally_mode,
                candidate_objects: candidate_objects.clone(),
                outcome_template: outcome_template.clone(),
                visibility,
            };
        }
    }

    if let WaitingFor::SearchChoice {
        player,
        ref cards,
        count,
        reveal,
        up_to,
        allows_partial_find,
        ref constraint,
        ref split,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::SearchChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                reveal,
                up_to,
                allows_partial_find,
                constraint: constraint.clone(),
                split: split.clone(),
            };
        }
    }

    // CR 101.4a + CR 701.23i: A simultaneous multi-player library search keeps
    // each prior searcher's found cards private while later players decide.
    // `SearchChoice` above hides the current candidate list, but the protocol's
    // pending state also retains prior selections for deferred batch delivery;
    // redact those ids per selector so an observer cannot recover library
    // identities from `pending_scoped_library_search`.
    if let Some(pending) = filtered.pending_scoped_library_search.as_mut() {
        for (selector, selected) in &mut pending.selections {
            if !can_view_private_for_player(*selector) {
                *selected = selected.iter().map(|_| ObjectId(0)).collect();
            }
        }
    }
    if let Some(batch) = filtered.pending_search_found_batch.as_mut() {
        if !can_view_private_for_player(batch.searcher) {
            batch.remaining = batch.remaining.iter().map(|_| ObjectId(0)).collect();
            batch.survivors = batch.survivors.iter().map(|_| ObjectId(0)).collect();
        }
    }
    // CR 400.2 + CR 723.4: A nested zone-change replacement can park the
    // currently found hidden-library card in the batch completion sidecar.
    // Apply the same searcher/private-access boundary as the owning
    // `PendingSearchFoundBatch`; filtering mutates only this viewer copy.
    if state
        .pending_search_found_batch
        .as_ref()
        .is_some_and(|batch| !can_view_private_for_player(batch.searcher))
    {
        if let Some(crate::types::game_state::PendingBatchDeliveries {
            completion:
                Some(crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                    object_id,
                    ..
                }),
            ..
        }) = filtered.pending_batch_deliveries.as_mut()
        {
            *object_id = ObjectId(0);
        }
    }

    // CR 701.23a: The cultivate-class partition pick exposes the found set only
    // to the searcher; opponents see opaque ids (mirrors SearchChoice above).
    if let WaitingFor::SearchPartitionChoice {
        player,
        ref cards,
        primary_destination,
        primary_count,
        primary_enter_tapped,
        rest_destination,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::SearchPartitionChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                primary_destination,
                primary_count,
                primary_enter_tapped,
                rest_destination,
                source_id,
            };
        }
    }

    if let WaitingFor::OutsideGameChoice {
        player,
        source_id,
        reveal,
        up_to,
        destination,
        ..
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::OutsideGameChoice {
                player,
                source_id,
                choices: Vec::new(),
                count: 0,
                reveal,
                up_to,
                destination,
            };
        }
    }

    if let WaitingFor::ChooseFromZoneChoice {
        player,
        ref cards,
        count,
        up_to,
        ref constraint,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                up_to,
                constraint: constraint.clone(),
                source_id,
            };
        }
    }

    // CR 400.2 + CR 701.4a: A pending `BeholdChoice` carries the choosing player's
    // mixed-zone candidate set (battlefield-you-control ∪ HAND). The hand leg is a
    // hidden zone — exposing the raw candidate ids to an opponent would leak which
    // of the controller's hand cards are matching (e.g. which Dragons) BEFORE they
    // choose. Redact the candidate list to opaque placeholders for viewers who
    // cannot see the controller's private zones. The post-choice reveal of the
    // single chosen card flows through the separate `CardsRevealed` pipeline.
    if let WaitingFor::BeholdChoice {
        player,
        ref choices,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::BeholdChoice {
                player,
                choices: choices.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    // CR 400.2: Hand is a hidden zone. `FreeCastWindow` (Invoke Calamity) is the
    // first `CastOffer` kind whose `candidates` reference cards in the
    // controller's HAND (as well as the public graveyard). Exposing the raw
    // candidate ids to an opponent would leak which of the controller's hand
    // cards are eligible instant/sorcery spells within the MV budget. Redact the
    // candidate list to opaque placeholders for viewers who cannot see the
    // controller's private zones — `remaining_casts`, `remaining_mv_budget`, and
    // the rider stay public (CR 601.2 + CR 408 — the resolving spell is public).
    if let WaitingFor::CastOffer {
        player,
        kind:
            CastOfferKind::FreeCastWindow {
                ref candidates,
                remaining_casts,
                remaining_mv_budget,
                ref filter,
                ref zones,
                exile_instead_of_graveyard,
            },
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::FreeCastWindow {
                    candidates: candidates.iter().map(|_| ObjectId(0)).collect(),
                    remaining_casts,
                    remaining_mv_budget,
                    filter: filter.clone(),
                    zones: zones.clone(),
                    exile_instead_of_graveyard,
                },
            };
        }
    }

    // CR 400.2: Library and hand are hidden zones — opponents cannot see the
    // identities of cards there. The eligible-cards list for an alternative or
    // additional exile-from-hand cost (Force of Will and the rest of the
    // pitch-spell family) would leak hand contents to opponents (e.g.
    // `cards.len()` reveals the count of blue cards in the caster's hand minus
    // one). Redact `cards` to opaque placeholders for viewers who cannot see
    // the caster's hand. `count` and `pending_cast` are public (CR 601.2 +
    // CR 408 — the spell on the stack is public information).
    // The graveyard variant of `ExileForCost` is intentionally NOT redacted
    // because the graveyard is a public zone (CR 400.2).
    // CR 400.2: Hand and library are hidden zones. The eligible-objects list
    // for a `PayCost` choice can leak hidden-zone contents to opponents
    // (e.g. the count of blue cards in the caster's hand). Redact the
    // `choices` for viewers who cannot see the caster's private zones; `count`
    // and `resume` stay public (CR 601.2 + CR 408 — the spell on the stack is
    // public information). Public-zone choices (graveyard / battlefield) and
    // public-zone exile costs are intentionally NOT redacted.
    if let WaitingFor::PayCost {
        player,
        ref kind,
        ref choices,
        count,
        min_count,
        ref resume,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            // CR 400.2: redacted `choices` for the viewer, computed per `kind`.
            let redacted: Option<Vec<ObjectId>> = match kind {
                // Hand-pitch exile cost (Force of Will family): hand is hidden,
                // so opaque every choice. Graveyard exile is public — no redaction.
                PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Hand,
                } => Some(choices.iter().map(|_| ObjectId(0)).collect()),
                // Mana-ability exile cost: hidden only for hand/library zones.
                PayCostKind::ExileFromManaZone {
                    zone: Zone::Hand | Zone::Library,
                } => Some(vec![ObjectId(0); count]),
                // Behold from hand: drop the hand-card choices entirely (only
                // battlefield permanents remain visible to opponents).
                PayCostKind::Behold { .. } => Some(
                    choices
                        .iter()
                        .filter_map(|id| {
                            state
                                .objects
                                .get(id)
                                .filter(|obj| obj.zone == Zone::Hand)
                                .is_none()
                                .then_some(*id)
                        })
                        .collect(),
                ),
                // CR 400.2: Other PayCost kinds reveal only public-zone choices
                // and need no redaction. `ExilePermanent` (battlefield exile-cost,
                // Food Chain class) draws exclusively from the battlefield, a
                // public zone, so its choices fall through here unredacted.
                _ => None,
            };
            if let Some(redacted_choices) = redacted {
                filtered.waiting_for = WaitingFor::PayCost {
                    player,
                    kind: kind.clone(),
                    choices: redacted_choices,
                    count,
                    min_count,
                    resume: resume.clone(),
                };
            }
        }
    }

    // CR 400.2: Hand and library are hidden zones. The `options` on a
    // `CostTypeChoice` (Celestial Reunion's pre-cost "choose a creature type")
    // is the set of creature types the caster can actually pay for — computed by
    // `feasible_behold_creature_types` over beholdable cards, which INCLUDE the
    // caster's hand. Serialized in full, it leaks private hand contents to
    // opponents (e.g. offering "Goblin" reveals a Goblin is beholdable from hand)
    // before the behold selection is even made. Redact `options` to empty for
    // viewers who cannot see the caster's private zones; `choice_type` and
    // `pending_cast` are public (CR 400.2 — the stack is a public zone), and the
    // acting player still receives the full list to choose from.
    if let WaitingFor::CostTypeChoice {
        player,
        ref choice_type,
        ref options,
        ref pending_cast,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) && !options.is_empty() {
            filtered.waiting_for = WaitingFor::CostTypeChoice {
                player,
                choice_type: choice_type.clone(),
                options: Vec::new(),
                pending_cast: pending_cast.clone(),
            };
        }
    }

    if let WaitingFor::EffectZoneChoice {
        player,
        ref cards,
        count,
        min_count,
        up_to,
        source_id,
        effect_kind,
        zone,
        destination,
        enter_tapped,
        enter_transformed,
        enters_under_player,
        enters_attacking,
        owner_library,
        track_exiled_by_source,
        ref face_down_profile,
        ref enter_with_counters,
        ref conditional_enter_with_counters,
        count_param,
        library_position: None,
        is_cost_payment: _,
        enters_modified_if: _,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) && zone == Zone::Hand {
            filtered.waiting_for = WaitingFor::EffectZoneChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                min_count,
                up_to,
                source_id,
                effect_kind,
                zone,
                destination,
                enter_tapped,
                enter_transformed,
                enters_under_player,
                enters_attacking,
                owner_library,
                track_exiled_by_source,
                // Face-down entry characteristics are public effect parameters,
                // not private hand info — pass them through the redaction.
                face_down_profile: face_down_profile.clone(),
                enter_with_counters: enter_with_counters.clone(),
                conditional_enter_with_counters: conditional_enter_with_counters.clone(),
                count_param,
                library_position: None,
                is_cost_payment: false,
                enters_modified_if: None,
            };
        }
    }
    if let WaitingFor::DrawnThisTurnTopdeckChoice {
        player,
        ref cards,
        count,
        min_count,
        life_payment,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::DrawnThisTurnTopdeckChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                min_count,
                life_payment,
                source_id,
            };
        }
    }

    filtered.auto_pass.retain(|pid, _| *pid == viewer);
    filtered.phase_stops.retain(|pid, _| *pid == viewer);
    filtered
        .may_trigger_auto_choices
        .retain(|record| record.key.player == viewer);
    filtered.decision_templates.retain(|t| t.owner == viewer);
    filtered.priority_yields.retain(|y| y.player == viewer);
    filtered
        .lands_tapped_for_mana
        .retain(|pid, _| *pid == viewer);
    filtered
        .cards_drawn_this_turn
        .retain(|pid, _| can_view_private_for_player(*pid));
    filtered
        .outside_game_cards_brought_in
        .retain(|record| record.player == viewer);

    // CR 601.2 + CR 408: A spell being cast is on the stack and is public information —
    // caster, targets, chosen X values, and pending mana payment are all visible to
    // opponents. The old behavior of clearing `pending_cast` for non-casters was both
    // rules-incorrect and inconsistent with the inline `pending_cast` fields embedded in
    // `WaitingFor` variants (ChooseXValue, TargetSelection, etc.), which were already
    // leaking through unfiltered. `PendingCast` itself carries only public data
    // (object_id, card_id, ability, cost) — the card's identity is already visible via
    // the stack object.

    for pool in &mut filtered.deck_pools {
        if pool.player != viewer {
            // Per-seat redaction: replace the Arc'd decks with fresh empties.
            // Cheaper than `make_mut + clear` because we discard the contents;
            // the original Arcs remain shared by the unfiltered state and any
            // other viewer's filter.
            pool.registered_main = Arc::new(Vec::new());
            pool.registered_sideboard = Arc::new(Vec::new());
            pool.current_main = Arc::new(Vec::new());
            pool.current_sideboard = Arc::new(Vec::new());
            pool.registered_planar_deck = Arc::new(Vec::new());
            pool.registered_scheme_deck = Arc::new(Vec::new());
            pool.current_scheme_deck = Arc::new(Vec::new());
        }
    }

    // CR 603.3b + CR 400.2: Per-controller ordering pass — keep the
    // placement spine visible to everyone (groups, group sizes,
    // controllers, ordered flags) but strip each group's private
    // payload from viewers who are not that group's controller.
    if let Some(order) = filtered.pending_trigger_order.as_mut() {
        for group in &mut order.groups {
            if !can_view_private_for_player(group.controller) {
                for ctx in &mut group.triggers {
                    redact_pending_trigger_context_for_observer(ctx);
                }
            }
        }
    }

    // CR 603.3b + CR 400.2: Same redaction surface applies to the singleton
    // `pending_trigger` (the currently-targeting trigger) and its sidecar
    // `pending_trigger_event_batch` (full simultaneous-event set consumed when
    // it reaches the stack). Gate on the pending trigger's own controller.
    if let Some(pending) = filtered.pending_trigger.as_mut() {
        if !can_view_private_for_player(pending.controller) {
            redact_pending_trigger_for_observer(pending);
            filtered.pending_trigger_event_batch.clear();
        }
    }

    if let WaitingFor::TriggerTargetSelection {
        trigger_controller,
        trigger_event,
        trigger_events,
        ..
    } = &mut filtered.waiting_for
    {
        if trigger_controller.is_some_and(|controller| !can_view_private_for_player(controller)) {
            *trigger_event = None;
            trigger_events.clear();
        }
    }

    // CR 113.2c + CR 603.2 + CR 603.3b: `deferred_triggers` holds the FIFO
    // queue of same-pass triggers waiting on the active `pending_trigger` to
    // resolve. Each entry is a `PendingTriggerContext` with the same private
    // payload shape — redact per controller.
    for ctx in &mut filtered.deferred_triggers {
        if !can_view_private_for_player(ctx.pending.controller) {
            redact_pending_trigger_context_for_observer(ctx);
        }
    }

    filtered
}

/// CR 723.4 + CR 805.8: a player controlling another player sees the
/// information visible to that player; when turns are shared, controlling one
/// player controls that player's team. Reuse submitter authority so the same
/// team-turn boundary governs decisions and private information.
fn viewer_has_private_access_to_player(
    state: &GameState,
    viewer: PlayerId,
    player: PlayerId,
) -> bool {
    player == viewer || turn_control::authorized_submitter_for_player(state, player) == viewer
}

/// Returns a viewer-safe copy of `events` for wire broadcast.
///
/// `StateUpdate` / `GameStarted` carry a parallel `events` array alongside the
/// filtered `GameState`. Unlike the state snapshot, this array was broadcast
/// verbatim to every seat and spectator — leaking hidden library information
/// through `GameEvent::CardDrawn` (specific `object_id`) and
/// `GameEvent::ZoneChanged` records emitted on library → hand moves (the
/// `ZoneChangeRecord` embeds the full card name and type line). The structured
/// game log already excludes these events; this closes the same hole on the
/// raw event channel clients also consume.
pub fn filter_events_for_viewer(
    events: &[GameEvent],
    state: &GameState,
    viewer: PlayerId,
) -> Vec<GameEvent> {
    events
        .iter()
        .filter(|event| event_visible_to_viewer(event, state, viewer))
        .cloned()
        .collect()
}

fn event_visible_to_viewer(event: &GameEvent, state: &GameState, viewer: PlayerId) -> bool {
    let can_view_private_for_player =
        |player: PlayerId| viewer_has_private_access_to_player(state, viewer, player);

    match event {
        // Individual draws identify the exact library card — only viewers with
        // private-zone authority for the drawer may see them.
        GameEvent::CardDrawn { player_id, .. } => can_view_private_for_player(*player_id),
        GameEvent::ZoneChanged {
            object_id,
            from,
            to,
            record,
            ..
        } if *from == Some(Zone::Library) => library_zone_change_visible_to_viewer(
            state,
            viewer,
            *object_id,
            *to,
            record,
            &can_view_private_for_player,
        ),
        _ => true,
    }
}

/// Whether a library-origin `ZoneChanged` event may be sent to `viewer`.
///
/// The `ZoneChangeRecord` snapshots the card's real identity at move time, so
/// face-down manifest/cloak moves and face-down exiles must be gated the same
/// way `filter_state_for_viewer` gates the post-move object — not by a fixed
/// destination-zone allowlist.
fn library_zone_change_visible_to_viewer(
    state: &GameState,
    viewer: PlayerId,
    object_id: ObjectId,
    to: Zone,
    record: &crate::types::game_state::ZoneChangeRecord,
    can_view_private_for_player: &impl Fn(PlayerId) -> bool,
) -> bool {
    if matches!(to, Zone::Hand | Zone::Library) {
        return viewer_has_private_access_to_player(state, viewer, record.owner);
    }

    let Some(obj) = state.objects.get(&object_id) else {
        return true;
    };

    if obj.face_down {
        match to {
            Zone::Battlefield | Zone::Stack => {
                return can_view_private_for_player(obj.controller)
                    || viewer_may_look_at_face_down(state, object_id, can_view_private_for_player);
            }
            Zone::Exile => {
                return face_down_exile_visible_to_viewer(
                    state,
                    object_id,
                    obj,
                    can_view_private_for_player,
                );
            }
            _ => {}
        }
    }

    true
}

/// Mirrors the face-down exile redaction in `filter_state_for_viewer`.
fn face_down_exile_visible_to_viewer(
    state: &GameState,
    object_id: ObjectId,
    obj: &crate::game::game_object::GameObject,
    can_view_private_for_player: &impl Fn(PlayerId) -> bool,
) -> bool {
    use crate::types::game_state::ExileLinkKind;
    let foretell_ok = obj.foretold && can_view_private_for_player(obj.owner);
    let hideaway_lookable_by_viewer = state.exile_links.iter().any(|link| {
        link.exiled_id == object_id
            && link.kind == ExileLinkKind::HideawayLookable
            && state
                .objects
                .get(&link.source_id)
                .is_some_and(|src| can_view_private_for_player(src.controller))
    });
    foretell_ok || hideaway_lookable_by_viewer
}

/// CR 708.5: `viewer` may look at face-down permanent `obj_id` they do not
/// control if they control an active `MayLookAtFaceDown` permission whose
/// affected filter matches the permanent. The permission has two sources:
///   1. A printed continuous static (Found Footage) — scanned from the
///      battlefield. Its affected filter is resolved from the static's source
///      controller (the viewer), so `controller: Opponent` scopes to the
///      viewer's opponents.
///   2. A duration-bound transient continuous effect created by a resolving
///      activated ability (Lumbering Laundry's "Until end of turn, you may look
///      at face-down creatures you don't control any time"). The TCE's
///      `controller` is the viewer and its `affected` carries the same
///      face-down/controller filter, so it is read identically.
fn viewer_may_look_at_face_down(
    state: &GameState,
    obj_id: ObjectId,
    can_view_private_for_player: &impl Fn(PlayerId) -> bool,
) -> bool {
    use crate::types::ability::{ContinuousModification, Duration};
    use crate::types::statics::{StaticMode, StaticModeKind};
    // CR 708.5: O(1) presence gate covers ONLY the battlefield-static authority. The
    // duration-bound `transient_continuous_effects` scan below is a separate authority
    // the index does not track, so wrap the loop rather than early-returning `false`.
    if super::functioning_abilities::static_kind_present(state, StaticModeKind::MayLookAtFaceDown) {
        crate::game::perf_counters::record_static_full_scan(); // counter fires only on real scan
        for (source, def) in super::functioning_abilities::battlefield_active_statics(state) {
            if !matches!(def.mode, StaticMode::MayLookAtFaceDown) {
                continue;
            }
            if !can_view_private_for_player(source.controller) {
                continue;
            }
            let Some(filter) = def.affected.as_ref() else {
                continue;
            };
            let ctx = super::filter::FilterContext::from_source(state, source.id);
            if super::filter::matches_target_filter(state, obj_id, filter, &ctx) {
                return true;
            }
        }
    }

    // CR 708.5 + CR 608.2c + CR 611.2c: Duration-bound permission from a resolved
    // ability. CR 708.5 is the base own-permanent look right; CR 608.2c binds "you"
    // to the ability's controller at resolution; CR 611.2c makes this rules-modifying
    // effect's affected set dynamic (re-evaluated each query, not frozen at creation).
    for tce in &state.transient_continuous_effects {
        if !can_view_private_for_player(tce.controller) {
            continue;
        }
        let grants_look = tce.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MayLookAtFaceDown,
                }
            )
        });
        if !grants_look {
            continue;
        }
        // Honor the same duration/condition gates the static-mode TCE queries in
        // `static_abilities.rs` apply (a `ForAsLongAs` duration or explicit
        // `condition` must still hold this look).
        if let Duration::ForAsLongAs { condition } = &tce.duration {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        if let Some(condition) = &tce.condition {
            if !super::layers::evaluate_condition(state, condition, tce.controller, tce.source_id) {
                continue;
            }
        }
        // CR 608.2c: "you" is latched to the player who controlled the ability at
        // resolution (the stored `tce.controller`), NOT the source's current
        // battlefield controller. CR 611.2c: because this is a rules-modifying
        // continuous effect (it grants a look permission, it does not modify
        // characteristics or change control), its affected set stays dynamic — we
        // re-evaluate the affected filter (e.g. "you don't control" =
        // `ControllerRef::Opponent`) against that latched controller on each query.
        // A later control change of the source must not reinterpret who the looker
        // may see. `from_source` would derive `source_controller` from the current
        // object and silently rebind "you" to the new controller.
        let ctx = super::filter::FilterContext::from_source_with_controller(
            tce.source_id,
            tce.controller,
        );
        if super::filter::matches_target_filter(state, obj_id, &tce.affected, &ctx) {
            return true;
        }
    }
    false
}

fn is_visible_revealed_card(state: &GameState, obj_id: ObjectId) -> bool {
    state.revealed_cards.contains(&obj_id)
        || state.objects.get(&obj_id).is_some_and(|obj| {
            state.public_revealed_cards.contains(&obj_id) && obj.zone != Zone::Library
        })
}

fn hide_card(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.face_down = true;
        obj.name = HIDDEN_CARD_NAME.to_string();
        Arc::make_mut(&mut obj.abilities).clear();
        obj.keywords.clear();
        obj.base_keywords.clear();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = None;
        obj.color.clear();
        obj.base_color.clear();
        obj.trigger_definitions.clear();
        obj.replacement_definitions.clear();
        obj.static_definitions.clear();
        obj.casting_permissions.clear();
        obj.printed_ref = None;
        obj.base_printed_ref = None;
        obj.back_face = None;
        obj.token_image_ref = None;
        obj.source_related_token_ids.clear();
        obj.foretold = false;
    }
}

fn record_hidden_replacement_candidate_source(
    candidate_source_ids: Option<&HashSet<ObjectId>>,
    hidden_candidate_source_ids: &mut HashSet<ObjectId>,
    source_id: ObjectId,
) {
    if candidate_source_ids.is_some_and(|source_ids| source_ids.contains(&source_id)) {
        hidden_candidate_source_ids.insert(source_id);
    }
}

fn reveal_face_down_identity_to_controller(obj: &mut crate::game::game_object::GameObject) {
    if let Some(back_face) = &obj.back_face {
        obj.name = back_face.name.clone();
        obj.base_name = back_face.name.clone();
        obj.printed_ref = back_face.printed_ref.clone();
        obj.base_printed_ref = back_face.printed_ref.clone();
    }
}

fn redact_face_down_identity_from_observer(obj: &mut crate::game::game_object::GameObject) {
    obj.name = HIDDEN_CARD_NAME.to_string();
    obj.base_name = HIDDEN_CARD_NAME.to_string();
    obj.printed_ref = None;
    obj.base_printed_ref = None;
    obj.back_face = None;
}

/// CR 603.3b + CR 400.2: A pending trigger awaiting its
/// controller's ordering choice may carry private data —
/// the firing `GameEvent` can reference hidden-zone objects
/// (library look/scry/surveil/mill triggers), and the
/// modal/distribute/mode_abilities/description fields describe
/// the controller's not-yet-public choices. Strip every payload
/// an opponent has no rules-permission to see, leaving only
/// the public spine (source_id, controller, timestamp, ability,
/// condition, target_constraints, subject_match_count, die_result,
/// may_trigger_origin) plus the public scheduling metadata on the wrapping
/// context needed for the engine to keep running on
/// the wire and for the opponent's frontend to render an
/// "opponent is ordering N triggers" indicator.
fn redact_pending_trigger_for_observer(pending: &mut crate::game::triggers::PendingTrigger) {
    pending.trigger_event = None;
    pending.modal = None;
    pending.distribute = None;
    pending.mode_abilities.clear();
    pending.description = None;
}

/// CR 603.3b + CR 400.2: Wrapping-context variant of
/// [`redact_pending_trigger_for_observer`] that also clears the
/// `trigger_events` sidecar (the full simultaneous-event set for
/// batched triggers, which can reference hidden-zone objects). Scheduling
/// provenance is public metadata and is intentionally preserved.
fn redact_pending_trigger_context_for_observer(
    ctx: &mut crate::game::triggers::PendingTriggerContext,
) {
    redact_pending_trigger_for_observer(&mut ctx.pending);
    ctx.trigger_events.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::{apply, EngineError};
    use crate::game::morph::manifest;
    use crate::game::printed_cards::snapshot_object_face;
    use crate::game::replacement::{
        continue_replacement, replace_event, replacement_choice_waiting_for, ReplacementResult,
    };
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, BeholdCostAction, CostPaidObjectSnapshot, Effect,
        ReplacementDefinition, ResolvedAbility, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        AutoMayChoice, CastPaymentMode, CastingVariant, CostResume, ManaAbilityCostCursor,
        ManaAbilityCostResolutionMode, ManaAbilityResume, MayTriggerAutoChoiceKey,
        MayTriggerOrigin, PendingBeginGameAbility, PendingCast, PendingCostMoveCompletion,
        PendingCostMoveResume, PendingManaAbility, PendingScopedLibrarySearch,
        PendingSearchFoundBatch,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::proposed_event::ProposedEvent;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::{ExileCostSourceZone, Zone};
    use rand::RngCore;

    fn dummy_pending_cast(
        object_id: ObjectId,
        card_id: CardId,
        caster: PlayerId,
    ) -> Box<PendingCast> {
        Box::new(PendingCast {
            object_id,
            card_id,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Dummy".to_string(),
                    description: None,
                },
                vec![],
                object_id,
                caster,
            ),
            cost: ManaCost::NoCost,
            base_cost: None,
            declared_mana_additions: Vec::new(),
            activation_cost: None,
            activation_ability_index: None,
            pending_loyalty_activation_player: None,
            target_constraints: vec![],
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: crate::types::game_state::SpellCostSource::Other,
            additional_cost_payment_mode: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            deferred_sacrificed_permanents: Vec::new(),
            pinned_pool_units: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: crate::types::game_state::AssistState::NotOffered,
            activation_residual: crate::types::game_state::ActivationResidual::None,
            activation_target_selection:
                crate::types::game_state::ActivationTargetSelection::Pending,
            alt_cost_grant_source: None,
        })
    }

    fn dummy_pending_mana_ability(
        player: PlayerId,
        source_id: ObjectId,
    ) -> Box<PendingManaAbility> {
        Box::new(PendingManaAbility {
            player,
            source_id,
            ability_index: 0,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        })
    }

    #[test]
    fn redacts_rng_seed_from_every_viewer() {
        // A distinctive non-zero seed so a leak is unmistakable.
        let mut state = GameState::new_two_player(0x1234_5678_9abc_def0);
        assert_eq!(state.rng_seed, 0x1234_5678_9abc_def0);

        // Advance the ChaCha20 stream as gameplay would and snapshot the offset,
        // so `rng_word_pos` is non-zero. Issue #5466 sibling: a leaked word
        // offset hands an attacker the keystream alignment for free, so the
        // filter must redact the stream position as well as the seed.
        for _ in 0..5 {
            state.rng.next_u32();
        }
        state.capture_rng_word_pos();
        let source_word_pos = state.rng_word_pos;
        assert_ne!(
            source_word_pos, 0,
            "test precondition: stream position must be non-zero to prove redaction"
        );

        // Seat viewers and the non-seat spectator must never see the real seed
        // or the serialized stream position.
        for viewer in [PlayerId(0), PlayerId(1), PlayerId(u8::MAX)] {
            let filtered = filter_state_for_viewer(&state, viewer);
            assert_eq!(
                filtered.rng_seed, 0,
                "rng_seed must be redacted for viewer {viewer:?}"
            );
            assert_eq!(
                filtered.rng_word_pos, 0,
                "rng_word_pos must be redacted for viewer {viewer:?}"
            );
        }

        // The authoritative source state is untouched by filtering.
        assert_eq!(state.rng_seed, 0x1234_5678_9abc_def0);
        assert_eq!(state.rng_word_pos, source_word_pos);
    }

    #[test]
    fn liminal_entry_state_serializes_but_is_filtered_from_viewers() {
        let mut state = GameState::new_two_player(42);
        let entry_ref = ObjectId(99);
        state.liminal_entries.insert(
            entry_ref,
            crate::types::game_state::LiminalEntry {
                object: crate::game::game_object::GameObject::new(
                    entry_ref,
                    CardId(99),
                    PlayerId(0),
                    "Liminal Token".to_string(),
                    Zone::Battlefield,
                ),
                name: "Liminal Token".to_string(),
                source_id: ObjectId(1),
                controller: PlayerId(0),
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: None,
                remaining_count: 0,
                created_ids: Vec::new(),
                copy_resume: None,
                spec_resume: None,
                enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
                enter_with_counters: Vec::new(),
                kind: crate::types::game_state::LiminalEntryKind::Token,
                replacement_applied: std::collections::HashSet::new(),
            },
        );
        state.pending_liminal_entry_resume =
            Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                source_id: entry_ref,
                player: PlayerId(0),
                event: crate::types::proposed_event::ProposedEvent::TokenEntry {
                    entry_ref,
                    enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
                    enter_with_counters: Vec::new(),
                    applied: std::collections::HashSet::new(),
                },
            });

        let serialized = serde_json::to_string(&state).unwrap();
        let restored: GameState = serde_json::from_str(&serialized).unwrap();
        assert!(restored.liminal_entries.contains_key(&entry_ref));
        assert!(restored.pending_liminal_entry_resume.is_some());

        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        assert!(filtered.liminal_entries.is_empty());
        assert!(filtered.pending_liminal_entry_resume.is_none());
    }

    #[test]
    fn search_found_batch_is_visible_only_to_searcher() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.pending_search_found_batch = Some(PendingSearchFoundBatch {
            searcher: PlayerId(1),
            remaining: vec![ObjectId(101)],
            survivors: vec![ObjectId(102)],
            continuation: crate::types::game_state::PendingSearchFoundContinuation::Standard {
                split: None,
            },
            visibility: crate::types::game_state::SearchFoundVisibility::Private,
        });

        let searcher_view = filter_state_for_viewer(&state, PlayerId(1));
        let batch = searcher_view.pending_search_found_batch.unwrap();
        assert_eq!(batch.remaining, vec![ObjectId(101)]);
        assert_eq!(batch.survivors, vec![ObjectId(102)]);

        for viewer in [PlayerId(0), PlayerId(2)] {
            let batch = filter_state_for_viewer(&state, viewer)
                .pending_search_found_batch
                .expect("opaque batch remains serialized");
            assert_eq!(batch.remaining, vec![ObjectId(0)]);
            assert_eq!(batch.survivors, vec![ObjectId(0)]);
        }
    }

    #[test]
    fn filters_library_draw_events_for_non_drawer() {
        let state = GameState::new_two_player(42);
        let mut record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            ObjectId(99),
            Some(Zone::Library),
            Zone::Hand,
        );
        record.name = "Secret Card".to_string();
        record.owner = PlayerId(0);

        let events = vec![
            GameEvent::CardDrawn {
                player_id: PlayerId(0),
                object_id: ObjectId(99),
                nth_in_turn: 1,
                nth_in_step: 1,
            },
            GameEvent::ZoneChanged {
                object_id: ObjectId(99),
                from: Some(Zone::Library),
                to: Zone::Hand,
                record: Box::new(record),
            },
        ];

        let drawer = filter_events_for_viewer(&events, &state, PlayerId(0));
        assert_eq!(drawer.len(), 2);

        let opponent = filter_events_for_viewer(&events, &state, PlayerId(1));
        assert!(opponent.is_empty());
    }

    #[test]
    fn library_draw_events_visible_to_turn_controller() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        let event = GameEvent::CardDrawn {
            player_id: PlayerId(1),
            object_id: ObjectId(99),
            nth_in_turn: 1,
            nth_in_step: 1,
        };

        let controller =
            filter_events_for_viewer(std::slice::from_ref(&event), &state, PlayerId(0));
        assert_eq!(controller, vec![event]);
    }

    #[test]
    fn library_draw_events_visible_to_shared_team_turn_controller() {
        let active_player = PlayerId(0);
        let drawer = PlayerId(1);
        let turn_controller = PlayerId(2);
        let observer = PlayerId(3);
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.active_player = active_player;
        state.turn_decision_controller = Some(turn_controller);
        assert_eq!(
            turn_control::authorized_submitter_for_player(&state, drawer),
            turn_controller,
            "the shared-team turn controller must authorize decisions for the drawer"
        );

        let mut record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            ObjectId(99),
            Some(Zone::Library),
            Zone::Hand,
        );
        record.name = "Secret Teammate Card".to_string();
        record.owner = drawer;
        let events = vec![
            GameEvent::CardDrawn {
                player_id: drawer,
                object_id: ObjectId(99),
                nth_in_turn: 1,
                nth_in_step: 1,
            },
            GameEvent::ZoneChanged {
                object_id: ObjectId(99),
                from: Some(Zone::Library),
                to: Zone::Hand,
                record: Box::new(record),
            },
        ];

        assert_eq!(filter_events_for_viewer(&events, &state, drawer), events);
        assert_eq!(
            filter_events_for_viewer(&events, &state, turn_controller),
            events,
            "the controller of the active shared team must receive the teammate's private draw events"
        );
        assert!(
            filter_events_for_viewer(&events, &state, observer).is_empty(),
            "an observer without turn-control authority must not receive the teammate's private draw events"
        );
    }

    #[test]
    fn library_face_down_battlefield_zone_change_hidden_from_opponent() {
        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let _secret = create_object(
            &mut state,
            CardId(7),
            controller,
            "Secret Manifest".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        manifest(&mut state, controller, &mut events).unwrap();

        let controller_filtered = filter_events_for_viewer(&events, &state, controller);
        let lib_to_bf: Vec<_> = controller_filtered
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        from: Some(Zone::Library),
                        to: Zone::Battlefield,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(lib_to_bf.len(), 1);
        if let GameEvent::ZoneChanged { record, .. } = lib_to_bf[0] {
            assert_eq!(record.name, "Secret Manifest");
        } else {
            panic!("expected ZoneChanged");
        }

        let opponent = filter_events_for_viewer(&events, &state, PlayerId(1));
        assert!(opponent.iter().all(|e| !matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Library),
                to: Zone::Battlefield,
                ..
            }
        )));

        let spectator = filter_events_for_viewer(&events, &state, PlayerId(u8::MAX));
        assert!(spectator.iter().all(|e| !matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Library),
                to: Zone::Battlefield,
                ..
            }
        )));
    }

    #[test]
    fn library_to_exile_reveal_zone_change_stays_public() {
        let mut state = GameState::new_two_player(42);
        let owner = PlayerId(1);
        let card = create_object(
            &mut state,
            CardId(3),
            owner,
            "Cascade Card".to_string(),
            Zone::Exile,
        );

        let mut record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            card,
            Some(Zone::Library),
            Zone::Exile,
        );
        record.name = "Cascade Card".to_string();
        record.owner = owner;

        let events = vec![GameEvent::ZoneChanged {
            object_id: card,
            from: Some(Zone::Library),
            to: Zone::Exile,
            record: Box::new(record),
        }];

        let opponent = filter_events_for_viewer(&events, &state, PlayerId(0));
        assert_eq!(opponent.len(), 1);
        if let GameEvent::ZoneChanged { record, .. } = &opponent[0] {
            assert_eq!(record.name, "Cascade Card");
        } else {
            panic!("expected ZoneChanged");
        }
    }

    #[test]
    fn library_mill_zone_change_stays_public() {
        let state = GameState::new_two_player(42);
        let mut record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            ObjectId(7),
            Some(Zone::Library),
            Zone::Graveyard,
        );
        record.name = "Milled Card".to_string();
        record.owner = PlayerId(1);

        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(7),
            from: Some(Zone::Library),
            to: Zone::Graveyard,
            record: Box::new(record),
        }];

        let opponent = filter_events_for_viewer(&events, &state, PlayerId(0));
        assert_eq!(opponent.len(), 1);
        if let GameEvent::ZoneChanged { record, .. } = &opponent[0] {
            assert_eq!(record.name, "Milled Card");
        } else {
            panic!("expected ZoneChanged");
        }
    }

    #[test]
    fn filters_other_players_may_trigger_auto_choices() {
        let mut state = GameState::new_two_player(42);
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id: ObjectId(10),
                origin: MayTriggerOrigin::Printed { trigger_index: 0 },
            },
            AutoMayChoice::Accept,
        );
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(1),
                source_id: ObjectId(11),
                origin: MayTriggerOrigin::Printed { trigger_index: 0 },
            },
            AutoMayChoice::Decline,
        );

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(filtered.may_trigger_auto_choices.len(), 1);
        assert_eq!(filtered.may_trigger_auto_choices[0].key.player, PlayerId(0));
    }

    /// CR 603.3b: saved trigger-ordering templates are per-player private preference
    /// state — a viewer sees only their own, never an opponent's saved orderings.
    #[test]
    fn filters_other_players_decision_templates() {
        use crate::analysis::decision_template::{
            DecisionGroupKey, DecisionKind, DecisionTemplate, PinnedDecision, ReplayMode,
        };
        use crate::types::game_state::YieldTarget;

        let template = |owner: PlayerId, card_id: u64| {
            let src = YieldTarget::AllCopies {
                card_id: CardId(card_id),
                trigger_description: None,
            };
            DecisionTemplate {
                owner,
                decisions: vec![PinnedDecision::Order {
                    source: src.clone(),
                    pos: 0,
                }],
                replay: ReplayMode::Static,
                key: DecisionGroupKey::from_sources(&[src], DecisionKind::TriggerOrdering),
            }
        };

        let mut state = GameState::new_two_player(42);
        // Distinct keys (different card ids) so both templates coexist.
        state.set_trigger_order_template(template(PlayerId(0), 100));
        state.set_trigger_order_template(template(PlayerId(1), 200));
        assert_eq!(state.decision_templates.len(), 2);

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        // Own-kept AND other-removed: without the retain, P1's template leaks into P0's
        // view (len 2) or an inverted retain drops P0's own (len 0) — both fail here.
        assert_eq!(filtered.decision_templates.len(), 1);
        assert_eq!(filtered.decision_templates[0].owner, PlayerId(0));
    }

    /// CR 117.3d: priority yields are private preference state — a viewer sees
    /// only their own, never an opponent's.
    #[test]
    fn filters_other_players_priority_yields() {
        let mut state = GameState::new_two_player(42);
        state.add_priority_yield(
            PlayerId(0),
            crate::types::game_state::YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        state.add_priority_yield(
            PlayerId(1),
            crate::types::game_state::YieldTarget::AllCopies {
                card_id: CardId(10),
                trigger_description: None,
            },
        );

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(filtered.priority_yields.len(), 1);
        assert_eq!(filtered.priority_yields[0].player, PlayerId(0));
    }

    #[test]
    fn hidden_cards_redact_source_token_metadata() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Token Source".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&card_id).unwrap();
            obj.source_related_token_ids = vec!["secret-token-id".to_string()];
            obj.token_image_ref = Some(crate::types::card::TokenImageRef {
                scryfall_id: "secret-scryfall-id".to_string(),
                scryfall_oracle_id: Some("secret-oracle-id".to_string()),
                face_name: None,
                preset_id: "secret-preset-id".to_string(),
            });
        }

        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        let hidden = filtered.objects.get(&card_id).unwrap();

        assert_eq!(hidden.name, "Hidden Card");
        assert!(hidden.source_related_token_ids.is_empty());
        assert!(hidden.token_image_ref.is_none());
    }

    #[test]
    fn hidden_cards_redact_back_face_identity() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Front Face".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&card_id).unwrap();
            let mut back_face = snapshot_object_face(obj);
            back_face.name = "Secret Back Face".to_string();
            obj.back_face = Some(back_face);
        }

        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        let hidden = filtered.objects.get(&card_id).unwrap();

        assert_eq!(hidden.name, "Hidden Card");
        assert!(hidden.back_face.is_none());
    }

    #[test]
    fn search_choice_is_visible_to_turn_controller() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden Tutor Target".to_string(),
            Zone::Library,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
            split: None,
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        match filtered.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => assert_eq!(cards, vec![card_id]),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Hidden Tutor Target")
        );
    }

    /// CR 101.4a + CR 701.23i: In a three-player simultaneous library search,
    /// each selector sees only their own already-found cards and only the
    /// current searcher sees that search's candidates. The deferred delivery
    /// state must not leak a prior selector's library object ids to the third
    /// player while the current `SearchChoice` is correctly redacted.
    #[test]
    fn scoped_library_search_redacts_prior_selection_and_current_candidates_per_viewer() {
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let p0_selected = create_object(
            &mut state,
            CardId(1),
            p0,
            "P0 Secret Forest".to_string(),
            Zone::Library,
        );
        let p1_candidate = create_object(
            &mut state,
            CardId(2),
            p1,
            "P1 Secret Island".to_string(),
            Zone::Library,
        );
        let source_id = ObjectId(99);
        state.pending_scoped_library_search = Some(PendingScopedLibrarySearch {
            ability: Box::new(ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "test scoped search".to_string(),
                    description: None,
                },
                Vec::new(),
                source_id,
                p0,
            )),
            remaining_players: Vec::new(),
            selections: vec![(p0, vec![p0_selected])],
            current_player: Some(p1),
            after_scope: None,
        });
        state.waiting_for = WaitingFor::SearchChoice {
            player: p1,
            cards: vec![p1_candidate],
            count: 1,
            reveal: false,
            up_to: true,
            allows_partial_find: true,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
            split: None,
        };

        let p0_view = filter_state_for_viewer(&state, p0);
        let p0_pending = p0_view
            .pending_scoped_library_search
            .expect("P0 view retains the deferred search state");
        assert_eq!(p0_pending.selections, vec![(p0, vec![p0_selected])]);
        assert!(matches!(
            p0_view.waiting_for,
            WaitingFor::SearchChoice { cards, .. } if cards == vec![ObjectId(0)]
        ));

        let p1_view = filter_state_for_viewer(&state, p1);
        let p1_pending = p1_view
            .pending_scoped_library_search
            .expect("P1 view retains the deferred search state");
        assert_eq!(p1_pending.selections, vec![(p0, vec![ObjectId(0)])]);
        assert!(matches!(
            p1_view.waiting_for,
            WaitingFor::SearchChoice { cards, .. } if cards == vec![p1_candidate]
        ));

        let p2_view = filter_state_for_viewer(&state, p2);
        let p2_pending = p2_view
            .pending_scoped_library_search
            .expect("spectating player retains only the public pending-state shape");
        assert_eq!(p2_pending.selections, vec![(p0, vec![ObjectId(0)])]);
        assert!(matches!(
            p2_view.waiting_for,
            WaitingFor::SearchChoice { cards, .. } if cards == vec![ObjectId(0)]
        ));
    }

    #[test]
    fn public_reveal_memory_keeps_opponent_hand_card_visible() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Known Hand Card".to_string(),
            Zone::Hand,
        );
        state.public_revealed_cards.insert(card_id);

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Known Hand Card")
        );
    }

    #[test]
    fn public_reveal_memory_does_not_expose_library_order() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Known Library Card".to_string(),
            Zone::Library,
        );
        state.public_revealed_cards.insert(card_id);

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Hidden Card")
        );
    }

    /// Sandbox debug exposure: a viewer with debug permission in a sandbox
    /// game sees their own library card names (so the debug "move from
    /// library to hand" picker can identify a specific card). Opponents'
    /// libraries stay hidden — sandbox is a shared playground for your own
    /// materials, not an opponent-deck-leak. The FE alphabetizes the picker
    /// within each zone, so name exposure alone leaks no draw order.
    #[test]
    fn sandbox_debug_permitted_sees_own_library_but_not_opponent_library() {
        let mut state = GameState::new(FormatConfig::standard().with_sandbox(), 2, 42);
        state.debug_permitted.insert(PlayerId(0));
        state.debug_permitted.insert(PlayerId(1));
        let own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Library Card".to_string(),
            Zone::Library,
        );
        let opp = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Library Card".to_string(),
            Zone::Library,
        );

        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            filtered.objects.get(&own).map(|obj| obj.name.as_str()),
            Some("My Library Card"),
            "viewer must see their own library names in sandbox+permitted"
        );
        assert_eq!(
            filtered.objects.get(&opp).map(|obj| obj.name.as_str()),
            Some("Hidden Card"),
            "opponent's library stays hidden even in sandbox"
        );
    }

    /// Without the sandbox capability, debug permission alone must not
    /// expose the library — defense in depth against accidentally leaving
    /// `debug_permitted` populated in a non-sandbox game.
    #[test]
    fn non_sandbox_keeps_own_library_hidden_even_when_debug_permitted() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        state.debug_permitted.insert(PlayerId(0));
        let own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Library Card".to_string(),
            Zone::Library,
        );

        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            filtered.objects.get(&own).map(|obj| obj.name.as_str()),
            Some("Hidden Card"),
            "non-sandbox must keep library hidden regardless of debug_permitted"
        );
    }

    /// CR 400.7 + CR 122.2: A card that was publicly revealed in hand (e.g.
    /// by Duress, Telepathy, Coercion) and is then shuffled back into its
    /// owner's library becomes a new object. If that card is later drawn
    /// again, the persistent reveal memory must NOT leak into the new
    /// hand-zone object — opponents should not retroactively know the
    /// freshly drawn card's identity. This drives the cleanup in
    /// `apply_zone_exit_cleanup` (zones.rs) through real `move_to_zone`
    /// calls, not a shape assertion on the HashSet directly.
    #[test]
    fn public_reveal_memory_clears_when_card_changes_zones() {
        use crate::game::zones::move_to_zone;
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Duressed Card".to_string(),
            Zone::Hand,
        );
        state.public_revealed_cards.insert(card_id);

        // While in hand, the opponent (PlayerId(0)) sees it by name.
        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Duressed Card"),
            "reveal memory should show the card while it is in hand"
        );

        // Hand → Library: the reveal memory must be dropped at the zone
        // boundary. The library-zone gate in `is_visible_revealed_card`
        // would otherwise hide it incidentally — we check the underlying
        // set so the test would have caught the original bug.
        let mut events = Vec::new();
        move_to_zone(&mut state, card_id, Zone::Library, &mut events);
        assert!(
            !state.public_revealed_cards.contains(&card_id),
            "public_revealed_cards must be cleared on zone change (CR 400.7)"
        );

        // Library → Hand (draw the same storage id back). Without the fix,
        // the persistent flag would resurface visibility for the opponent.
        move_to_zone(&mut state, card_id, Zone::Hand, &mut events);
        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Hidden Card"),
            "re-drawn card must not inherit prior reveal state — it is a new object per CR 400.7"
        );
    }

    /// Unit 2, site #21 (multi-authority): `viewer_may_look_at_face_down` gates ONLY
    /// its battlefield `MayLookAtFaceDown` scan behind the O(1) presence index (wrap,
    /// not early-return), and falls through UNCHANGED to the duration-bound
    /// `transient_continuous_effects` authority the index does not track. Three cases:
    /// (a) a TCE grant with the index PRECISE-absent still permits the look — proving
    /// the wrap did not early-`return false` and suppress the TCE (revert-failing);
    /// (b) neither authority => no look; (c) a battlefield static (index present) falls
    /// through and permits the look.
    #[test]
    fn face_down_look_tce_survives_precise_battlefield_gate() {
        use crate::types::ability::{
            ContinuousModification, ControllerRef, Duration, StaticDefinition, TargetFilter,
            TypedFilter,
        };
        use crate::types::statics::{StaticMode, StaticModeKind};

        // Viewer P0; face-down creature controlled by opponent P1.
        let mut state = GameState::new_two_player(42);
        let face_down = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Face Down".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&face_down).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.face_down = true;
        }
        // Viewer P0 can see only their own private information.
        let can_view = |p: PlayerId| p == PlayerId(0);
        let opp_creature =
            || TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));

        // (b) Neither authority present, index precise => no look.
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            !crate::game::functioning_abilities::static_kind_present(
                &state,
                StaticModeKind::MayLookAtFaceDown
            ),
            "precondition: no battlefield MayLookAtFaceDown static"
        );
        assert!(
            !viewer_may_look_at_face_down(&state, face_down, &can_view),
            "no authority => the viewer may not look"
        );

        // (a) TCE grant (controller P0) with the battlefield index still absent.
        state.add_transient_continuous_effect(
            ObjectId(999),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            opp_creature(),
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MayLookAtFaceDown,
            }],
            None,
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            !crate::game::functioning_abilities::static_kind_present(
                &state,
                StaticModeKind::MayLookAtFaceDown
            ),
            "the TCE authority must NOT flip the battlefield-static presence index"
        );
        assert!(
            viewer_may_look_at_face_down(&state, face_down, &can_view),
            "TCE-granted look must survive the battlefield-static gate (revert-failing)"
        );

        // (c) Battlefield static (index present) — presence-positive fall-through.
        let mut state = GameState::new_two_player(42);
        let face_down = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Face Down".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&face_down).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.face_down = true;
        }
        let looker = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Found Footage".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&looker)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::MayLookAtFaceDown).affected(opp_creature()));
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            viewer_may_look_at_face_down(&state, face_down, &can_view),
            "a battlefield MayLookAtFaceDown static permits the look on fall-through"
        );
    }

    #[test]
    fn filtered_state_hides_pending_begin_game_queue() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Opening Hand Card".to_string(),
            Zone::Hand,
        );
        state
            .pending_begin_game_abilities
            .push(PendingBeginGameAbility {
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Hidden Begin Game Ability".to_string(),
                        description: None,
                    },
                    vec![],
                    source,
                    PlayerId(0),
                ),
            });
        state.resolving_begin_game_abilities = true;

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(filtered.pending_begin_game_abilities.is_empty());
        assert!(!filtered.resolving_begin_game_abilities);
        assert_eq!(state.pending_begin_game_abilities.len(), 1);
        assert!(state.resolving_begin_game_abilities);
    }

    #[test]
    fn search_choice_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden Tutor Target".to_string(),
            Zone::Library,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: crate::types::ability::SearchSelectionConstraint::None,
            split: None,
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(2));

        match filtered.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => assert_eq!(cards, vec![ObjectId(0)]),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn opponent_commander_in_command_zone_remains_visible() {
        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        let commander_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&commander_id).unwrap().is_commander = true;

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(filtered.command_zone, im::vector![commander_id]);
        let commander = filtered.objects.get(&commander_id).unwrap();
        assert_eq!(commander.name, "Opponent Commander");
        assert!(!commander.face_down);
        assert_eq!(commander.zone, Zone::Command);
        assert!(commander.is_commander);
    }

    #[test]
    fn supplementary_deck_cards_are_hidden_from_all_viewers() {
        let mut state = GameState::new_two_player(42);
        let plane_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Secret Plane".to_string(),
            Zone::Command,
        );
        state
            .objects
            .get_mut(&plane_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Plane);
        state.planar_deck.push_back(plane_id);

        let scheme_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Secret Scheme".to_string(),
            Zone::Command,
        );
        state
            .objects
            .get_mut(&scheme_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Scheme);
        state.scheme_deck.push_back(scheme_id);

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(
            filtered.objects.get(&plane_id).map(|obj| obj.name.as_str()),
            Some("Hidden Card")
        );
        assert_eq!(
            filtered
                .objects
                .get(&scheme_id)
                .map(|obj| obj.name.as_str()),
            Some("Hidden Card")
        );
        assert_eq!(filtered.planar_deck, im::vector![plane_id]);
        assert_eq!(filtered.scheme_deck, im::vector![scheme_id]);
    }

    // CR 601.2 + CR 408: A spell being cast is on the stack and is public information —
    // opponents see the caster, the spell, chosen targets, and mana payment progress
    // as it happens (the MTGA "Opponent is casting X" experience). The tests below guard
    // against regression of the pre-correction behavior that cleared `pending_cast` for
    // non-caster viewers, which was both rules-incorrect and inconsistent with the
    // inline `pending_cast` fields on `WaitingFor::{ChooseXValue, TargetSelection,
    // ModeChoice, ...}` that always leaked through unfiltered.

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_mana_payment() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        state.pending_cast = Some(dummy_pending_cast(ObjectId(10), CardId(1), PlayerId(0)));

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ManaPayment (CR 601.2 + CR 408)"
        );
        let pc = filtered.pending_cast.as_ref().unwrap();
        assert_eq!(pc.object_id, ObjectId(10));
        assert_eq!(pc.card_id, CardId(1));
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_choose_x_value() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(20), CardId(2), PlayerId(0));
        state.waiting_for = WaitingFor::ChooseXValue {
            player: PlayerId(0),
            min: 0,
            max: 5,
            pending_cast: pending.clone(),
            convoke_mode: None,
            x_cost_previews: vec![],
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ChooseXValue (CR 601.2 + CR 408)"
        );
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_target_selection() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(30), CardId(3), PlayerId(0));
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: pending.clone(),
            target_slots: vec![],
            mode_labels: Vec::new(),
            selection: Default::default(),
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during TargetSelection (CR 601.2 + CR 408)"
        );
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_mode_choice() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(40), CardId(4), PlayerId(0));
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: crate::types::ability::ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                ..Default::default()
            },
            pending_cast: pending.clone(),
            unavailable_modes: vec![],
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ModeChoice (CR 601.2 + CR 408)"
        );
    }

    /// CR 400.2: hand is a hidden zone. The eligible-cards list for an
    /// exile-from-hand cost reveals "blue cards in caster's hand − 1" to
    /// opponents and must be redacted, while the caster's own view is
    /// preserved.
    #[test]
    fn exile_from_hand_for_cost_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Blue Pitch Card".to_string(),
            Zone::Hand,
        );
        let pending = dummy_pending_cast(ObjectId(50), CardId(99), PlayerId(1));
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(1),
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Hand,
            },
            choices: vec![card_id],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell { spell: pending },
        };

        // Caster sees the real ID.
        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        match filtered_self.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone { zone },
                choices: cards,
                count,
                player,
                ..
            } => {
                assert_eq!(zone, ExileCostSourceZone::Hand);
                assert_eq!(cards, vec![card_id]);
                assert_eq!(count, 1);
                assert_eq!(player, PlayerId(1));
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }

        // Opponent sees a placeholder, but `count` and `resume` survive.
        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone { zone },
                choices: cards,
                count,
                player,
                resume:
                    CostResume::Spell {
                        spell: pending_cast,
                    },
                ..
            } => {
                assert_eq!(zone, ExileCostSourceZone::Hand);
                assert_eq!(cards, vec![ObjectId(0)]);
                assert_eq!(count, 1);
                assert_eq!(player, PlayerId(1));
                assert_eq!(pending_cast.object_id, ObjectId(50));
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }
    }

    #[test]
    fn exile_for_mana_ability_from_hand_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden mana cost card".to_string(),
            Zone::Hand,
        );
        let other_card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Other hidden mana cost card".to_string(),
            Zone::Hand,
        );
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(1),
            kind: PayCostKind::ExileFromManaZone { zone: Zone::Hand },
            choices: vec![card_id, other_card_id],
            count: 1,
            min_count: 0,
            resume: CostResume::ManaAbility {
                mana_ability: dummy_pending_mana_ability(PlayerId(1), ObjectId(50)),
            },
        };

        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        match filtered_self.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromManaZone { zone },
                choices: cards,
                count,
                ..
            } => {
                assert_eq!(zone, Zone::Hand);
                assert_eq!(cards, vec![card_id, other_card_id]);
                assert_eq!(count, 1);
            }
            other => panic!("expected PayCost ExileFromManaZone, got {other:?}"),
        }

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromManaZone { zone },
                choices: cards,
                count,
                ..
            } => {
                assert_eq!(zone, Zone::Hand);
                assert_eq!(cards, vec![ObjectId(0)]);
                assert_eq!(count, 1);
            }
            other => panic!("expected PayCost ExileFromManaZone, got {other:?}"),
        }
    }

    #[test]
    fn behold_for_cost_hides_matching_hand_choices_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let public_choice = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Public Dragon".to_string(),
            Zone::Battlefield,
        );
        let private_choice = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hidden Dragon".to_string(),
            Zone::Hand,
        );
        let pending = dummy_pending_cast(ObjectId(51), CardId(99), PlayerId(1));
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(1),
            kind: PayCostKind::Behold {
                action: BeholdCostAction::ChooseOrReveal,
            },
            choices: vec![public_choice, private_choice],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell { spell: pending },
        };

        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        match filtered_self.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::Behold { .. },
                choices,
                count,
                ..
            } => {
                assert_eq!(choices, vec![public_choice, private_choice]);
                assert_eq!(count, 1);
            }
            other => panic!("expected PayCost Behold, got {other:?}"),
        }

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::Behold { .. },
                choices,
                count,
                resume:
                    CostResume::Spell {
                        spell: pending_cast,
                    },
                ..
            } => {
                assert_eq!(choices, vec![public_choice]);
                assert_eq!(count, 1);
                assert_eq!(pending_cast.object_id, ObjectId(51));
            }
            other => panic!("expected PayCost Behold, got {other:?}"),
        }
    }

    /// Issue #1518 (Pithing Needle): a permanent's chosen card name is public
    /// information (CR 400.2) and MUST remain visible to opponents after the
    /// per-viewer redaction. `filter_state_for_viewer` only redacts cards in
    /// hidden zones; a face-up battlefield permanent keeps its
    /// `chosen_attributes` for every viewer, so the opponent can see which name
    /// was chosen.
    #[test]
    fn chosen_card_name_on_battlefield_permanent_is_visible_to_opponents() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let needle = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pithing Needle".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&needle)
            .unwrap()
            .chosen_attributes
            .push(crate::types::ability::ChosenAttribute::CardName(
                "Goblin Guide".to_string(),
            ));

        // The opponent (PlayerId(1)) must still see the chosen name.
        let filtered = filter_state_for_viewer(&state, PlayerId(1));
        let seen = &filtered.objects[&needle].chosen_attributes;
        assert!(
            seen.iter().any(|a| matches!(
                a,
                crate::types::ability::ChosenAttribute::CardName(name) if name == "Goblin Guide"
            )),
            "opponent must see the chosen card name on a battlefield permanent, got {seen:?}"
        );
    }

    #[test]
    fn drawn_this_turn_choice_private_tracking_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Drawn Secret".to_string(),
            Zone::Hand,
        );
        state
            .cards_drawn_this_turn
            .insert(PlayerId(1), vec![card_id]);
        state.waiting_for = WaitingFor::DrawnThisTurnTopdeckChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            min_count: 0,
            life_payment: 4,
            source_id: ObjectId(99),
        };

        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        assert_eq!(
            filtered_self.cards_drawn_this_turn.get(&PlayerId(1)),
            Some(&vec![card_id])
        );
        match filtered_self.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. } => {
                assert_eq!(cards, vec![card_id]);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        assert!(
            !filtered_opp
                .cards_drawn_this_turn
                .contains_key(&PlayerId(1)),
            "opponents must not learn which hidden hand cards were drawn this turn"
        );
        match filtered_opp.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0)]);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }
    }

    /// CR 400.2: Graveyard is a public zone. The escape eligibility list
    /// (`ExileForCost { zone: Graveyard, .. }`) must NOT be redacted for
    /// non-controller viewers.
    #[test]
    fn exile_for_cost_graveyard_is_not_redacted() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Escape Filler".to_string(),
            Zone::Graveyard,
        );
        let pending = dummy_pending_cast(ObjectId(50), CardId(99), PlayerId(1));
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(1),
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
            choices: vec![card_id],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell { spell: pending },
        };

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone { zone },
                choices: cards,
                ..
            } => {
                assert_eq!(zone, ExileCostSourceZone::Graveyard);
                assert_eq!(
                    cards,
                    vec![card_id],
                    "graveyard variant must NOT be redacted"
                );
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }
    }

    #[test]
    fn exile_for_mana_ability_graveyard_is_not_redacted() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Titans' Nest filler".to_string(),
            Zone::Graveyard,
        );
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(1),
            kind: PayCostKind::ExileFromManaZone {
                zone: Zone::Graveyard,
            },
            choices: vec![card_id],
            count: 1,
            min_count: 0,
            resume: CostResume::ManaAbility {
                mana_ability: dummy_pending_mana_ability(PlayerId(1), ObjectId(50)),
            },
        };

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromManaZone { zone },
                choices: cards,
                ..
            } => {
                assert_eq!(zone, Zone::Graveyard);
                assert_eq!(
                    cards,
                    vec![card_id],
                    "graveyard mana ability cost choices must NOT be redacted"
                );
            }
            other => panic!("expected PayCost ExileFromManaZone, got {other:?}"),
        }
    }

    #[test]
    fn choose_from_zone_choice_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Tracked Card".to_string(),
            Zone::Exile,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            up_to: false,
            constraint: None,
            source_id: ObjectId(99),
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(2));

        match filtered.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0)])
            }
            other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
        }
    }

    /// Heist (and any `ChooseFromZoneChoice` over a hidden zone like a
    /// library) must reveal the candidate card identities to the prompt
    /// player — the look step's reminder text is "Look at three random
    /// nonland cards", so without this the controller would be forced into
    /// a blind pick. A regression in the library-hiding loop (e.g. dropping
    /// the `choose_from_zone_hidden_visible` membership check) redacts the
    /// underlying object even though the cards ARRAY is still visible, so
    /// the UI/client reads "Hidden Card" for every candidate. This test
    /// pins both layers: the prompt-player view sees the real card name,
    /// and a non-prompt opponent still sees "Hidden Card" (proving the
    /// reveal is prompt-player-scoped, not a global leak).
    #[test]
    fn choose_from_zone_choice_library_cards_visible_to_prompt_player_only() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // P0 will be the heisting controller; P1 owns the library heisted.
        // Put a named nonland in P1's library and surface it via a
        // ChooseFromZoneChoice whose prompt player is P0.
        let card_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(1),
            "Heisted Bear".to_string(),
            Zone::Library,
        );
        state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(1))
            .unwrap()
            .library
            .push_back(card_id);
        state.active_player = PlayerId(0);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: PlayerId(0),
            cards: vec![card_id],
            count: 1,
            up_to: false,
            constraint: None,
            source_id: ObjectId(99),
        };

        // The prompt player must see the real card identity.
        let p0_view = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            p0_view.objects[&card_id].name, "Heisted Bear",
            "the prompt player must see the Heist candidate's real identity"
        );
        // The cards ARRAY is also intact for the prompt player (not redacted
        // to ObjectId(0) placeholders).
        match p0_view.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(
                    cards,
                    vec![card_id],
                    "the prompt player must see the real candidate ids"
                );
            }
            other => panic!("expected ChooseFromZoneChoice for P0, got {other:?}"),
        }

        // A non-prompt opponent still sees a redacted identity — the reveal
        // is prompt-player-scoped, not a global leak.
        let p1_view = filter_state_for_viewer(&state, PlayerId(1));
        assert_eq!(
            p1_view.objects[&card_id].name, "Hidden Card",
            "the non-prompt opponent must NOT see the Heist candidate identity"
        );
        match p1_view.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(
                    cards,
                    vec![ObjectId(0)],
                    "the non-prompt opponent must see redacted placeholder ids"
                );
            }
            other => panic!("expected ChooseFromZoneChoice for P1, got {other:?}"),
        }
    }

    #[test]
    fn trigger_target_selection_event_context_redacts_by_trigger_controller() {
        let trigger_event = crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(10),
            target: crate::types::ability::TargetRef::Object(ObjectId(20)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(1),
            trigger_controller: Some(PlayerId(0)),
            trigger_event: Some(trigger_event.clone()),
            trigger_events: vec![trigger_event.clone()],
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![crate::types::ability::TargetRef::Object(ObjectId(20))],
                optional: false,
                chooser: None,
            }],
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress::default(),
            source_id: Some(ObjectId(10)),
            description: Some("private trigger text".to_string()),
        };

        let controller_view = filter_state_for_viewer(&state, PlayerId(0));
        match controller_view.waiting_for {
            WaitingFor::TriggerTargetSelection {
                trigger_event: prompt_event,
                trigger_events,
                ..
            } => {
                assert_eq!(prompt_event, Some(trigger_event.clone()));
                assert_eq!(trigger_events, vec![trigger_event]);
            }
            other => panic!("expected trigger target selection, got {other:?}"),
        }

        let prompted_non_controller_view = filter_state_for_viewer(&state, PlayerId(1));
        match prompted_non_controller_view.waiting_for {
            WaitingFor::TriggerTargetSelection {
                trigger_event,
                trigger_events,
                description,
                ..
            } => {
                assert!(trigger_event.is_none());
                assert!(trigger_events.is_empty());
                assert_eq!(description.as_deref(), Some("private trigger text"));
            }
            other => panic!("expected trigger target selection, got {other:?}"),
        }
    }

    /// CR 903.10a: commander damage is public game state — every viewer
    /// (the dealing player, the receiving player, and every spectator) must
    /// see how much damage each commander has dealt to each player. The
    /// visibility filter must therefore preserve `commander_damage` verbatim
    /// for every viewer, and `derive_views` must populate the
    /// per-victim grouping irrespective of who is viewing.
    #[test]
    fn commander_damage_is_visible_to_every_viewer() {
        use crate::game::derived_views::derive_views;
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        let cmd = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Public Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd,
            damage: 7,
        });

        for viewer in [PlayerId(0), PlayerId(1)] {
            let filtered = filter_state_for_viewer(&state, viewer);
            assert_eq!(
                filtered.commander_damage.len(),
                1,
                "viewer {viewer:?} must see the commander-damage entry",
            );
            let views = derive_views(&filtered, Some(viewer));
            let from_p0 = views
                .commander_damage_by_attacker
                .get(&PlayerId(0))
                .unwrap_or_else(|| {
                    panic!("viewer {viewer:?} must see P0's attacker entry");
                });
            assert_eq!(from_p0.len(), 1);
            assert_eq!(from_p0[0].victim, PlayerId(1));
            assert_eq!(from_p0[0].damage, 7);
            assert_eq!(from_p0[0].commander, cmd);
        }
    }

    #[test]
    fn foretold_exile_card_identity_visible_only_to_owner() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Foretold Test".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&card_id).unwrap();
            obj.foretold = true;
            obj.face_down = true;
        }

        let owner_view = filter_state_for_viewer(&state, PlayerId(0));
        let owner_obj = owner_view.objects.get(&card_id).unwrap();
        assert_eq!(owner_obj.name, "Foretold Test");
        assert!(owner_obj.foretold);
        assert!(owner_obj.face_down);

        let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
        let opponent_obj = opponent_view.objects.get(&card_id).unwrap();
        assert_eq!(opponent_obj.name, "Hidden Card");
        assert!(!opponent_obj.foretold);
        assert!(opponent_obj.face_down);
        assert!(opponent_obj.casting_permissions.is_empty());
    }

    #[test]
    fn generic_face_down_exile_card_identity_hidden_from_everyone() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Necropotence Exile".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&card_id).unwrap().face_down = true;

        let owner_view = filter_state_for_viewer(&state, PlayerId(0));
        let owner_obj = owner_view.objects.get(&card_id).unwrap();
        assert_eq!(owner_obj.name, "Hidden Card");
        assert!(owner_obj.face_down);

        let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
        let opponent_obj = opponent_view.objects.get(&card_id).unwrap();
        assert_eq!(opponent_obj.name, "Hidden Card");
        assert!(opponent_obj.face_down);
    }

    /// Issue #2024 (Manifest): CR 708.5 — "At any time, you may look at a
    /// face-down permanent you control." A manifested (or morph/disguise/cloak)
    /// face-down battlefield permanent stores its real identity in `back_face`.
    /// The permanent's *controller* must keep that identity in their filtered
    /// view so the client can show them the face, while opponents must have it
    /// redacted (CR 708.5 — you can't look at a face-down permanent controlled
    /// by another player).
    #[test]
    fn face_down_battlefield_permanent_identity_visible_only_to_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let controller = PlayerId(0);
        let secret = create_object(
            &mut state,
            CardId(7),
            controller,
            "Secret Manifest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&secret).unwrap();
            obj.power = Some(5);
            obj.toughness = Some(4);
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }

        let mut events = Vec::new();
        manifest(&mut state, controller, &mut events).unwrap();

        // Server-side, the face-down 2/2 carries its real identity in back_face.
        assert!(state.objects[&secret].face_down);
        assert_eq!(state.objects[&secret].zone, Zone::Battlefield);
        let stored = state.objects[&secret].back_face.as_ref().unwrap();
        assert_eq!(stored.name, "Secret Manifest");

        // CR 708.5: the controller may look at their own face-down permanent —
        // their filtered view keeps the underlying identity in back_face.
        let controller_view = filter_state_for_viewer(&state, controller);
        let controller_obj = controller_view.objects.get(&secret).unwrap();
        assert!(controller_obj.face_down);
        assert_eq!(controller_obj.name, "Secret Manifest");
        assert_eq!(controller_obj.power, Some(2));
        assert_eq!(controller_obj.toughness, Some(2));
        let controller_back = controller_obj
            .back_face
            .as_ref()
            .expect("controller must retain back_face to look at their own manifest");
        assert_eq!(controller_back.name, "Secret Manifest");
        assert_eq!(controller_back.power, Some(5));

        // CR 708.5: an opponent can't look at it — back_face is redacted, but
        // the public 2/2 face is still shown.
        let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
        let opponent_obj = opponent_view.objects.get(&secret).unwrap();
        assert!(opponent_obj.face_down);
        assert_eq!(opponent_obj.name, "Hidden Card");
        assert!(
            opponent_obj.back_face.is_none(),
            "opponent must not see the manifested card's hidden identity"
        );
        assert_eq!(opponent_obj.power, Some(2));
        assert_eq!(opponent_obj.toughness, Some(2));
    }

    #[test]
    fn replacement_choice_redacts_hidden_candidate_source_for_non_actor() {
        let controller = PlayerId(0);
        let opponent = PlayerId(1);
        let mut state = GameState::new_two_player(42);
        let finality_source = create_object(
            &mut state,
            CardId(1),
            controller,
            "Secret Finality".to_string(),
            Zone::Battlefield,
        );
        let finality_back_face = snapshot_object_face(&state.objects[&finality_source]);
        let finality = state
            .objects
            .get_mut(&finality_source)
            .expect("finality permanent exists");
        finality.face_down = true;
        finality.back_face = Some(finality_back_face);
        finality.counters.insert(CounterType::Finality, 1);

        let redirect_source = create_object(
            &mut state,
            CardId(2),
            opponent,
            "Public Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .expect("competing redirect exists")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Library,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    conditional_enter_with_counters: Vec::new(),
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(Zone::Graveyard)]
        .into();

        let mut events = Vec::new();
        let result = replace_event(
            &mut state,
            ProposedEvent::zone_change(finality_source, Zone::Battlefield, Zone::Graveyard, None),
            &mut events,
        );
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(player) if player == controller),
            "the finality redirect and competing redirect must surface a real ordering choice"
        );
        state.waiting_for = replacement_choice_waiting_for(controller, &state);

        let controller_view = filter_state_for_viewer(&state, controller);
        assert!(
            controller_view
                .pending_replacement
                .as_ref()
                .is_some_and(|pending| {
                    pending
                        .candidates
                        .iter()
                        .any(|candidate| candidate.source == finality_source)
                }),
            "the authorized chooser must retain the real replacement continuation"
        );
        let controller_snapshot = serde_json::to_value(&controller_view)
            .expect("authorized viewer snapshot must serialize");
        assert!(
            controller_snapshot["pending_replacement"]["candidates"]
                .as_array()
                .is_some_and(|candidates| {
                    candidates
                        .iter()
                        .any(|candidate| candidate["source"] == finality_source.0)
                }),
            "the authorized viewer's serialized snapshot must retain the replacement source"
        );

        let finality_index = match &controller_view.waiting_for {
            WaitingFor::ReplacementChoice {
                candidate_count,
                candidates,
                ..
            } => {
                assert_eq!(*candidate_count, 2);
                let finality_index = candidates
                    .iter()
                    .position(|candidate| candidate.source_id == finality_source)
                    .expect("the controller must retain the finality candidate identity");
                assert_eq!(
                    candidates[finality_index].source_name, "Secret Finality",
                    "the controller must retain the hidden candidate's real identity"
                );
                assert!(
                    candidates.iter().any(|candidate| {
                        candidate.source_id == redirect_source
                            && candidate.source_name == "Public Redirect"
                    }),
                    "the actor must retain the public competing candidate"
                );
                finality_index
            }
            other => panic!("expected ReplacementChoice for controller, got {other:?}"),
        };

        let opponent_view = filter_state_for_viewer(&state, opponent);
        assert!(
            opponent_view.pending_replacement.is_none(),
            "an unauthorized viewer must not receive the replacement continuation"
        );
        let opponent_snapshot =
            serde_json::to_value(&opponent_view).expect("opponent viewer snapshot must serialize");
        assert!(
            opponent_snapshot["pending_replacement"].is_null(),
            "the serialized snapshot must omit the replacement continuation"
        );
        assert_eq!(
            opponent_view.objects[&finality_source].name, HIDDEN_CARD_NAME,
            "test precondition: the opponent must not see the face-down source"
        );
        match &opponent_view.waiting_for {
            WaitingFor::ReplacementChoice {
                candidate_count,
                candidates,
                ..
            } => {
                assert_eq!(*candidate_count, 2);
                assert_eq!(candidates[finality_index].source_id, ObjectId(0));
                assert_eq!(candidates[finality_index].source_name, HIDDEN_CARD_NAME);
                assert!(
                    !candidates
                        .iter()
                        .any(|candidate| candidate.source_id == finality_source),
                    "opponent must not receive the hidden finality source identifier"
                );
                assert!(
                    candidates.iter().any(|candidate| {
                        candidate.source_id == redirect_source
                            && candidate.source_name == "Public Redirect"
                    }),
                    "redaction must preserve visible replacement candidates"
                );
            }
            other => panic!("expected ReplacementChoice for opponent, got {other:?}"),
        }
        let serialized_candidates = opponent_snapshot["waiting_for"]["data"]["candidates"]
            .as_array()
            .expect("replacement candidates must serialize as an array");
        assert_eq!(
            serialized_candidates[finality_index]["source_id"],
            serde_json::Value::from(0),
            "the serialized waiting summary must not expose the hidden source"
        );
        assert!(
            !serialized_candidates
                .iter()
                .any(|candidate| { candidate["source_id"] == finality_source.0 }),
            "neither serialized replacement surface may expose the hidden source"
        );

        let ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) =
            continue_replacement(&mut state, finality_index, &mut events)
        else {
            panic!("the controller must be able to resolve the real finality candidate");
        };
        assert_eq!(to, Zone::Exile);
    }

    #[test]
    fn replacement_choice_shared_team_turn_controller_receives_private_continuation() {
        let active_player = PlayerId(0);
        let affected_teammate = PlayerId(1);
        let turn_controller = PlayerId(2);
        let unauthorized_opponent = PlayerId(3);
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.active_player = active_player;
        state.turn_decision_controller = Some(turn_controller);

        let hidden_source = create_object(
            &mut state,
            CardId(3),
            affected_teammate,
            "Secret Teammate Replacement".to_string(),
            Zone::Battlefield,
        );
        let hidden_back_face = snapshot_object_face(&state.objects[&hidden_source]);
        let hidden_object = state
            .objects
            .get_mut(&hidden_source)
            .expect("hidden replacement source exists");
        hidden_object.face_down = true;
        hidden_object.back_face = Some(hidden_back_face);
        hidden_object.counters.insert(CounterType::Finality, 1);

        let redirect_source = create_object(
            &mut state,
            CardId(4),
            unauthorized_opponent,
            "Public Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .expect("competing redirect exists")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Library,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: Vec::new(),
                    conditional_enter_with_counters: Vec::new(),
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .destination_zone(Zone::Graveyard)]
        .into();

        let mut events = Vec::new();
        let result = replace_event(
            &mut state,
            ProposedEvent::zone_change(hidden_source, Zone::Battlefield, Zone::Graveyard, None),
            &mut events,
        );
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(player) if player == affected_teammate),
            "the teammate's finality and competing redirect must surface a real ordering choice"
        );
        state.waiting_for = replacement_choice_waiting_for(affected_teammate, &state);

        assert_eq!(
            turn_control::authorized_submitter_for_player(&state, affected_teammate),
            turn_controller,
            "CR 723.5 + CR 805.8: controlling the active team's turn authorizes the controller for a teammate's choice"
        );

        let controller_view = filter_state_for_viewer(&state, turn_controller);
        assert!(
            controller_view.pending_replacement.is_some(),
            "the legal team-turn replacement chooser must retain the continuation"
        );
        let controller_source = controller_view
            .objects
            .get(&hidden_source)
            .expect("the hidden replacement source must remain visible to the controller");
        assert_eq!(controller_source.name, "Secret Teammate Replacement");
        assert_eq!(
            controller_source
                .back_face
                .as_ref()
                .expect("the controller must receive the face-down source identity")
                .name,
            "Secret Teammate Replacement"
        );
        let WaitingFor::ReplacementChoice { candidates, .. } = controller_view.waiting_for else {
            panic!("expected ReplacementChoice for the authorized turn controller");
        };
        let finality_index = candidates
            .iter()
            .position(|candidate| candidate.source_id == hidden_source)
            .expect("the controller must receive the face-down replacement source");
        assert_eq!(
            candidates[finality_index].source_name,
            "Secret Teammate Replacement"
        );

        let teammate_view = filter_state_for_viewer(&state, affected_teammate);
        assert!(
            teammate_view.pending_replacement.is_none(),
            "the controlled teammate must not receive the controller's replacement continuation"
        );

        let observer_view = filter_state_for_viewer(&state, unauthorized_opponent);
        assert!(
            observer_view.pending_replacement.is_none(),
            "an unauthorized opposing observer must not receive the continuation"
        );
        assert_eq!(
            observer_view.objects[&hidden_source].name, HIDDEN_CARD_NAME,
            "the observer must not receive the face-down source identity"
        );
        assert!(
            observer_view.objects[&hidden_source].back_face.is_none(),
            "the observer must not receive the face-down source's back face"
        );
        let WaitingFor::ReplacementChoice { candidates, .. } = observer_view.waiting_for else {
            panic!("expected ReplacementChoice for the observer");
        };
        assert_eq!(candidates[finality_index].source_id, ObjectId(0));
        assert_eq!(candidates[finality_index].source_name, HIDDEN_CARD_NAME);

        assert!(
            matches!(
                apply(
                    &mut state,
                    affected_teammate,
                    GameAction::ChooseReplacement {
                        index: finality_index,
                    },
                ),
                Err(EngineError::WrongPlayer)
            ),
            "the controlled teammate must not submit the controller's replacement choice"
        );
        apply(
            &mut state,
            turn_controller,
            GameAction::ChooseReplacement {
                index: finality_index,
            },
        )
        .expect("the controlling player must submit the visible replacement choice");
        assert_eq!(state.objects[&hidden_source].zone, Zone::Exile);
        assert!(state.pending_replacement.is_none());
    }

    /// CR 708.5 (Found Footage class): "You may look at face-down creatures your
    /// opponents control any time." A `MayLookAtFaceDown` static controlled by
    /// the viewer reveals the matched opponent's face-down identity to the
    /// viewer, while a viewer WITHOUT the static keeps the default redaction.
    /// Discriminating: the `found_back.name` assertion (the static is present)
    /// fails — back_face redacts to None — if the `viewer_may_look_at_face_down`
    /// branch in `filter_state_for_viewer` is removed.
    #[test]
    fn found_footage_reveals_opponent_face_down_to_static_controller_only() {
        use crate::types::ability::{
            ControllerRef, FilterProp, StaticDefinition, TargetFilter, TypedFilter,
        };
        use crate::types::statics::StaticMode;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let looker = PlayerId(0);
        let opponent = PlayerId(1);
        let turn_controller = PlayerId(2);

        // The opponent manifests a creature face down on the battlefield.
        let secret = create_object(
            &mut state,
            CardId(7),
            opponent,
            "Opposing Spy".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&secret).unwrap();
            obj.power = Some(5);
            obj.toughness = Some(4);
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }
        let mut events = Vec::new();
        manifest(&mut state, opponent, &mut events).unwrap();
        assert!(state.objects[&secret].face_down);

        // Without the static, the looker (an opponent of the controller) cannot
        // see the hidden identity — the CR 708.5 default redaction.
        let baseline = filter_state_for_viewer(&state, looker);
        assert!(
            baseline.objects[&secret].back_face.is_none(),
            "without Found Footage, the looker must not see the opponent's face-down identity"
        );

        // The looker now controls Found Footage: "you may look at face-down
        // creatures your opponents control any time."
        let found_footage = create_object(
            &mut state,
            CardId(0xF00),
            looker,
            "Found Footage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&found_footage).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(0);
            obj.static_definitions.push(
                StaticDefinition::new(StaticMode::MayLookAtFaceDown).affected(TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::Opponent)
                        .properties(vec![FilterProp::FaceDown]),
                )),
            );
        }

        // With the static, the looker now sees the opponent's hidden identity.
        let found_view = filter_state_for_viewer(&state, looker);
        let found_obj = found_view.objects.get(&secret).unwrap();
        assert!(
            found_obj.face_down,
            "the permanent is still face down (CR 708.2)"
        );
        let found_back = found_obj
            .back_face
            .as_ref()
            .expect("with Found Footage, the looker must see the opponent's face-down identity");
        assert_eq!(found_back.name, "Opposing Spy");
        assert_eq!(found_back.power, Some(5));

        // A player controlling the looker's turn gets the same private view the
        // looker would get, matching the rest of `filter_state_for_viewer`.
        state.active_player = looker;
        state.turn_decision_controller = Some(turn_controller);
        let controlled_turn_view = filter_state_for_viewer(&state, turn_controller);
        assert!(
            controlled_turn_view.objects[&secret].back_face.is_some(),
            "the turn controller must inherit the active player's look permission"
        );

        // The opponent (controller) still sees their own permanent regardless.
        let owner_view = filter_state_for_viewer(&state, opponent);
        assert!(owner_view.objects[&secret].back_face.is_some());
    }

    /// CR 708.5 + CR 611.2c + CR 514.2 (Lumbering Laundry class): the DURATION-BOUND
    /// look permission. Lumbering Laundry's "{2}: Until end of turn, you may look
    /// at face-down creatures you don't control any time." resolves into a
    /// `MayLookAtFaceDown` transient continuous effect controlled by the looker.
    /// While it is active the looker sees the matched opponent's face-down
    /// identity; once the `UntilEndOfTurn` TCE is pruned at cleanup, the default
    /// CR 708.5 redaction returns.
    ///
    /// Discriminating on two axes:
    ///   - the `granted_back.name` assertion (permission active) fails — back_face
    ///     redacts to None — if the transient-continuous-effect scan added to
    ///     `viewer_may_look_at_face_down` is removed (the printed-static scan
    ///     alone never sees this permission).
    ///   - the post-prune `back_face.is_none()` assertion fails if the permission
    ///     were modeled as permanent rather than bounded to end of turn.
    #[test]
    fn lumbering_laundry_reveals_opponent_face_down_until_end_of_turn() {
        use crate::types::ability::{
            ContinuousModification, ControllerRef, Duration, FilterProp, StaticDefinition,
            TargetFilter, TypedFilter,
        };
        use crate::types::statics::StaticMode;

        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let looker = PlayerId(0);
        let opponent = PlayerId(1);

        // The opponent manifests a creature face down on the battlefield.
        let secret = create_object(
            &mut state,
            CardId(7),
            opponent,
            "Laundered Spy".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&secret).unwrap();
            obj.power = Some(6);
            obj.toughness = Some(3);
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }
        let mut events = Vec::new();
        manifest(&mut state, opponent, &mut events).unwrap();
        assert!(state.objects[&secret].face_down);

        // Without any permission, the looker cannot see the hidden identity.
        let baseline = filter_state_for_viewer(&state, looker);
        assert!(
            baseline.objects[&secret].back_face.is_none(),
            "without the permission, the looker must not see the opponent's face-down identity"
        );

        // The looker activates Lumbering Laundry: resolution registers an
        // UntilEndOfTurn MayLookAtFaceDown transient continuous effect over the
        // "face-down creatures you don't control" filter, controlled by the looker.
        let source = create_object(
            &mut state,
            CardId(0x1A5),
            looker,
            "Lumbering Laundry".to_string(),
            Zone::Battlefield,
        );
        let affected = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::Opponent)
                .properties(vec![FilterProp::FaceDown]),
        );
        state.add_transient_continuous_effect(
            source,
            looker,
            Duration::UntilEndOfTurn,
            affected,
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MayLookAtFaceDown,
            }],
            None,
        );

        // With the permission active, the looker sees the opponent's identity.
        let granted_view = filter_state_for_viewer(&state, looker);
        let granted_obj = granted_view.objects.get(&secret).unwrap();
        assert!(
            granted_obj.face_down,
            "the permanent is still face down (CR 708.2)"
        );
        let granted_back = granted_obj.back_face.as_ref().expect(
            "with the duration-bound permission, the looker must see the opponent's face-down identity",
        );
        assert_eq!(granted_back.name, "Laundered Spy");
        assert_eq!(granted_back.power, Some(6));

        // The opponent's own face-down creature must NOT have the permission
        // stamped onto its own static definitions by the layer system (the
        // permission is player-scoped, not an object grant). The opponent always
        // sees their own permanent (CR 708.5) regardless.
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            !state.objects[&secret]
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::MayLookAtFaceDown),
            "the look permission must not be applied to the opponent's creature as an object grant"
        );

        // CR 514.2: prune the UntilEndOfTurn effect at cleanup — the permission
        // ends and the default redaction returns.
        crate::game::layers::prune_end_of_turn_effects(&mut state);
        let expired_view = filter_state_for_viewer(&state, looker);
        assert!(
            expired_view.objects[&secret].back_face.is_none(),
            "after end of turn the duration-bound permission must expire and redact again"
        );

        // Sanity: a `StaticDefinition` carrying the same mode on the source as a
        // PRINTED static (Found Footage path) would also expose the identity, so
        // the duration-bound and permanent forms share one visibility authority.
        let _ = StaticDefinition::new(StaticMode::MayLookAtFaceDown);
    }

    /// CR 608.2c: the controller of a resolved ability is the one its instructions
    /// bind, so "you" is fixed to the player who controlled the ability at
    /// resolution. CR 611.2c: as a rules-modifying continuous effect its affected
    /// set stays dynamic, so it keeps re-evaluating against that latched "you". The
    /// duration-bound look permission ("you may look at face-down creatures you
    /// don't control") must keep evaluating its `ControllerRef::Opponent` filter
    /// against the original looker even after Lumbering Laundry changes
    /// controller, so the original looker keeps seeing the same opponents'
    /// face-down creatures for the rest of the turn.
    ///
    /// Three players are needed to make the bug observable. The looker's OWN
    /// face-down permanents are always visible to them under the CR 708.5 base
    /// rule, so the wrong-side *gain* can't be seen on the looker's own creature.
    /// The discriminator is the *loss* of access to a creature controlled by the
    /// player the source moves to:
    ///   - CR 608.2c: Correct (filter "you" = stored `tce.controller` = P0): `Opponent`
    ///     matches every creature P0 doesn't control — both `p1_secret` and
    ///     `p2_secret` stay visible.
    ///   - Buggy (`from_source` rebinds "you" to the source's new controller P1):
    ///     `Opponent` now excludes P1's own creature, so `p1_secret` redacts
    ///     (`back_face` → `None`) and the first assertion fails. `p2_secret` is a
    ///     positive control proving the permission is still active (not merely
    ///     disabled), so a vacuous "permission turned off" regression can't pass.
    #[test]
    fn lumbering_laundry_look_permission_survives_source_control_change() {
        use crate::types::ability::{
            ContinuousModification, ControllerRef, Duration, FilterProp, TargetFilter, TypedFilter,
        };
        use crate::types::statics::StaticMode;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let looker = PlayerId(0);
        let opp_a = PlayerId(1);
        let opp_b = PlayerId(2);

        // Each opponent manifests a face-down creature.
        let p1_secret = create_object(
            &mut state,
            CardId(7),
            opp_a,
            "Laundered Spy".to_string(),
            Zone::Library,
        );
        let p2_secret = create_object(
            &mut state,
            CardId(8),
            opp_b,
            "Pressed Shirt".to_string(),
            Zone::Library,
        );
        for (id, p, t) in [(p1_secret, 6, 3), (p2_secret, 1, 1)] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(p);
            obj.toughness = Some(t);
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }
        let mut events = Vec::new();
        manifest(&mut state, opp_a, &mut events).unwrap();
        manifest(&mut state, opp_b, &mut events).unwrap();
        assert!(state.objects[&p1_secret].face_down);
        assert!(state.objects[&p2_secret].face_down);

        // The looker activates Lumbering Laundry: the resolved ability registers an
        // UntilEndOfTurn MayLookAtFaceDown TCE over "face-down creatures you don't
        // control", controlled by the looker.
        let source = create_object(
            &mut state,
            CardId(0x1A5),
            looker,
            "Lumbering Laundry".to_string(),
            Zone::Battlefield,
        );
        let affected = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::Opponent)
                .properties(vec![FilterProp::FaceDown]),
        );
        state.add_transient_continuous_effect(
            source,
            looker,
            Duration::UntilEndOfTurn,
            affected,
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MayLookAtFaceDown,
            }],
            None,
        );

        // Lumbering Laundry changes controller to P1 after the ability resolves
        // (e.g. a control-swap effect). The look permission belongs to the original
        // looker for the rest of the turn (CR 608.2c) — the source's new controller
        // is irrelevant to who "you" is.
        state.objects.get_mut(&source).unwrap().controller = opp_a;

        let view = filter_state_for_viewer(&state, looker);
        // Discriminator: the original looker still sees P1's face-down creature even
        // though the source is now controlled by P1.
        let p1_back = view.objects[&p1_secret].back_face.as_ref().expect(
            "after the source changes controller to P1, the original looker must still see P1's \
             face-down identity (CR 608.2c fixes \"you\" to the resolution-time controller)",
        );
        assert_eq!(p1_back.name, "Laundered Spy");
        // Positive control: P2's creature was visible before and stays visible,
        // proving the permission is still active rather than merely disabled.
        let p2_back = view.objects[&p2_secret]
            .back_face
            .as_ref()
            .expect("the looker continues to see P2's face-down identity");
        assert_eq!(p2_back.name, "Pressed Shirt");
    }

    /// CR 400.2 — Invoke Calamity's `FreeCastWindow` lists the controller's
    /// eligible HAND cards as candidates. An opponent viewer must NOT learn which
    /// hand card ids are eligible; the controller sees the real ids.
    #[test]
    fn free_cast_window_hides_hand_candidates_from_opponent() {
        let mut state = GameState::new_two_player(42);
        let hand_candidate = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand Sorcery".to_string(),
            Zone::Hand,
        );
        state.waiting_for = WaitingFor::CastOffer {
            player: PlayerId(0),
            kind: CastOfferKind::FreeCastWindow {
                candidates: vec![hand_candidate],
                remaining_casts: 2,
                remaining_mv_budget: Some(6),
                filter: crate::types::ability::TargetFilter::Any,
                zones: vec![Zone::Graveyard, Zone::Hand],
                exile_instead_of_graveyard: true,
            },
        };

        // The controller sees the real candidate ids (and the public scalars).
        let controller_view = filter_state_for_viewer(&state, PlayerId(0));
        match controller_view.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::FreeCastWindow {
                        candidates,
                        remaining_casts,
                        remaining_mv_budget,
                        ..
                    },
                ..
            } => {
                assert_eq!(candidates, vec![hand_candidate]);
                assert_eq!(remaining_casts, 2);
                assert_eq!(remaining_mv_budget, Some(6));
            }
            other => panic!("expected FreeCastWindow for controller, got {other:?}"),
        }

        // An opponent sees opaque placeholders, not the hand candidate id; the
        // public scalars (count, budget, rider) are preserved.
        let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
        match opponent_view.waiting_for {
            WaitingFor::CastOffer {
                kind:
                    CastOfferKind::FreeCastWindow {
                        candidates,
                        remaining_casts,
                        remaining_mv_budget,
                        exile_instead_of_graveyard,
                        ..
                    },
                ..
            } => {
                assert!(
                    !candidates.contains(&hand_candidate),
                    "opponent must not see the controller's hand candidate id"
                );
                assert_eq!(candidates, vec![ObjectId(0)]);
                assert_eq!(remaining_casts, 2);
                assert_eq!(remaining_mv_budget, Some(6));
                assert!(exile_instead_of_graveyard);
            }
            other => panic!("expected FreeCastWindow for opponent, got {other:?}"),
        }
    }

    /// CR 608.2d: The resolved yes/no answer to a `GuessSubject::Proposition`
    /// (`proposition_truth`) must never reach any viewer over the wire — for The
    /// Seventh Doctor the guesser IS the viewer receiving the `WaitingFor`, so an
    /// un-redacted answer would let them guess correctly every time. The engine
    /// resolves correctness on the unfiltered state, so it is stripped for all.
    #[test]
    fn opponent_guess_proposition_truth_is_redacted_for_all_viewers() {
        use crate::types::ability::ChoiceType;
        let mut state = GameState::new_two_player(42);
        // Source controlled by PlayerId(1); the guesser is PlayerId(0).
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "The Seventh Doctor".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::OpponentGuess {
            player: PlayerId(0),
            options: vec!["greater".to_string(), "not greater".to_string()],
            choice_type: ChoiceType::Labeled {
                options: vec!["greater".to_string(), "not greater".to_string()],
            },
            source_id: source,
            proposition_truth: Some(true),
        };

        for viewer in [PlayerId(0), PlayerId(1)] {
            let filtered = filter_state_for_viewer(&state, viewer);
            match filtered.waiting_for {
                WaitingFor::OpponentGuess {
                    proposition_truth, ..
                } => assert_eq!(
                    proposition_truth, None,
                    "proposition_truth must be stripped for viewer {viewer:?}"
                ),
                other => panic!("expected OpponentGuess, got {other:?}"),
            }
        }
        // The unfiltered state keeps the answer so the engine can resolve it.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OpponentGuess {
                proposition_truth: Some(true),
                ..
            }
        ));
    }

    /// CR 608.2d: For a `GuessSubject::CommittedChoice` (The Toymaker's Trap),
    /// only the MOST-RECENTLY committed number is hidden from the guesser — it is
    /// the secret of the pending guess. Numbers chosen on earlier upkeeps were
    /// already revealed ("then you reveal the number you chose") and stay public,
    /// so the guesser's client can still see which numbers are used up. The
    /// controller always sees the full committed history.
    #[test]
    fn opponent_guess_hides_only_last_committed_number_from_guesser() {
        use crate::types::ability::{ChoiceType, ChosenAttribute, NumberDistinctness};
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "The Toymaker's Trap".to_string(),
            Zone::Battlefield,
        );
        // Number(3) was revealed on a prior upkeep; Number(5) is this upkeep's
        // secret commit.
        state.objects.get_mut(&source).unwrap().chosen_attributes =
            vec![ChosenAttribute::Number(3), ChosenAttribute::Number(5)];
        state.waiting_for = WaitingFor::OpponentGuess {
            player: PlayerId(0),
            options: (1..=5).map(|n| n.to_string()).collect(),
            choice_type: ChoiceType::NumberRange {
                min: 1,
                max: 5,
                distinctness: NumberDistinctness::DistinctFromSourceHistory,
            },
            source_id: source,
            proposition_truth: None,
        };

        // Guesser (non-controller): the last committed number (5) is hidden, the
        // already-revealed earlier number (3) stays visible.
        let guesser_view = filter_state_for_viewer(&state, PlayerId(0));
        let guesser_attrs = &guesser_view.objects[&source].chosen_attributes;
        assert!(
            guesser_attrs.contains(&ChosenAttribute::Number(3)),
            "the earlier, already-revealed number must stay visible to the guesser"
        );
        assert!(
            !guesser_attrs.contains(&ChosenAttribute::Number(5)),
            "the pending-guess secret (last committed number) must be hidden"
        );

        // Controller: sees the full committed history.
        let controller_view = filter_state_for_viewer(&state, PlayerId(1));
        let controller_attrs = &controller_view.objects[&source].chosen_attributes;
        assert!(controller_attrs.contains(&ChosenAttribute::Number(3)));
        assert!(controller_attrs.contains(&ChosenAttribute::Number(5)));
    }

    #[test]
    fn paused_cost_move_resume_is_server_authoritative_for_unauthorized_viewers() {
        let mut state = GameState::new_two_player(42);
        state.next_object_id = 70_001;
        let hidden = create_object(
            &mut state,
            CardId(70_001),
            PlayerId(0),
            "Paused Cost Secret".to_string(),
            Zone::Hand,
        );
        let mut pending = *dummy_pending_mana_ability(PlayerId(0), ObjectId(70_002));
        pending.chosen_discards = vec![hidden];
        pending.chosen_exiled = vec![hidden];
        pending.cost_paid_object = Some(CostPaidObjectSnapshot {
            object_id: hidden,
            lki: state.objects[&hidden].snapshot_for_mana_spent(),
        });
        let mana_resume = PendingCostMoveResume::ManaAbilityPayment {
            pending: Box::new(pending),
            cursor: ManaAbilityCostCursor {
                remaining: Vec::new(),
                resolution_mode: ManaAbilityCostResolutionMode::Interactive,
                excluded_sources: Vec::new(),
                sub_cost_demand: None,
                next_tapper: 0,
                next_discard: 0,
                next_exiled: 0,
                next_sacrificed: 0,
                selected_exile_remaining: Some(vec![hidden]),
                deferred_cost_events: Vec::new(),
                current_action_deferred_start: 0,
                parent: None,
            },
        };
        let resumes = vec![
            PendingCostMoveResume::Cast {
                player: PlayerId(0),
                pending: Some(dummy_pending_cast(hidden, CardId(70_001), PlayerId(0))),
                chosen: vec![hidden],
                paused_at_index: 0,
                destination: Zone::Exile,
                completion: PendingCostMoveCompletion::FinishPending,
            },
            PendingCostMoveResume::Foretell {
                player: PlayerId(0),
                object_id: hidden,
                cost: ManaCost::generic(1),
                turn_foretold: 7,
            },
            PendingCostMoveResume::DelveManaPayment {
                player: PlayerId(0),
                fuel_id: hidden,
            },
            mana_resume,
        ];

        for resume in resumes {
            state.pending_cost_move_resume = Some(resume);
            let authoritative_resume = serde_json::to_string(&state.pending_cost_move_resume)
                .expect("the authoritative cost continuation serializes");
            assert!(
                authoritative_resume.contains("70001"),
                "the authoritative continuation contains the private object ID"
            );
            if matches!(
                &state.pending_cost_move_resume,
                Some(PendingCostMoveResume::ManaAbilityPayment { .. })
            ) {
                assert!(
                    authoritative_resume.contains("Paused Cost Secret"),
                    "the mana continuation contains the private cost-payment LKI"
                );
            }

            let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
            assert!(
                opponent_view.pending_cost_move_resume.is_none(),
                "a non-acting opponent must not receive a paused cost continuation"
            );
            let wire = serde_json::to_string(&opponent_view)
                .expect("the filtered multiplayer snapshot serializes");
            assert!(
                !wire.contains("\"pendingCostMoveResume\":{\"type\"")
                    && !wire.contains("\"pending_cost_move_resume\":{\"type\""),
                "the viewer snapshot must not serialize a paused continuation's IDs or LKI"
            );
        }

        assert!(
            state.pending_cost_move_resume.is_some(),
            "filtering must not alter the authoritative server continuation"
        );
    }
}
