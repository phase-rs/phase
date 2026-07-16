//! Non-mutating action preview (issue #5468).
//!
//! Diffs the PUBLIC deltas an action would produce — life-total changes and
//! public-zone object transitions — so an embedder can drive hover-preview UX
//! ("this kills that", "you take 4") without committing the action. The caller
//! runs the action on a throwaway clone (never rendered) and passes the
//! before/after snapshots here.
//!
//! Hidden-information safety is guaranteed two ways: callers pass
//! `filter_state_for_viewer` outputs (hands/libraries/face-down identities
//! already redacted, including in `ZoneChange.name`), AND a transition is
//! surfaced only when at least one endpoint is a public zone — so a
//! fully-hidden hand↔library move (a draw) is never surfaced. CR 400.2 draws
//! the public/hidden zone line.

use serde::{Deserialize, Serialize};

use crate::ai_support::legal_actions_for_viewer;
use crate::game::engine::{apply_for_simulation, EngineError};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// A single object's public zone transition (e.g. `Battlefield → Graveyard` is a
/// death; `Stack → Graveyard` is a resolved/countered spell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChange {
    pub object_id: ObjectId,
    pub name: String,
    pub controller: PlayerId,
    pub from: Zone,
    pub to: Zone,
}

/// A player's life-total change (`delta` is signed: negative = life lost).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeDelta {
    pub player: PlayerId,
    pub delta: i32,
}

/// An object that ceased to exist during the action (`from` = the public zone it
/// left). CR 111.7 / CR 704.5d: a token that leaves the battlefield/stack is
/// removed by state-based actions, so it is simply gone from the after-snapshot —
/// the headline "this kills that" case when the victim is a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeasedObject {
    pub object_id: ObjectId,
    pub name: String,
    pub controller: PlayerId,
    pub from: Zone,
}

/// The public, viewer-safe result of previewing an action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreviewDiff {
    pub life_deltas: Vec<LifeDelta>,
    pub zone_changes: Vec<ZoneChange>,
    /// Objects newly present in a public zone (e.g. a created token).
    pub created: Vec<ObjectId>,
    /// Objects that were in a public zone before and ceased to exist (e.g. a
    /// token that died), so they appear in neither `after.objects` nor
    /// `zone_changes`.
    pub ceased: Vec<CeasedObject>,
}

/// CR 400.2: a zone whose contents every player can see. Hand and Library are
/// hidden; every other zone is public.
fn zone_is_public(zone: Zone) -> bool {
    !matches!(zone, Zone::Hand | Zone::Library)
}

/// Diff two snapshots (before/after an action) into the PUBLIC deltas a viewer
/// could legitimately observe.
///
/// Callers MUST pass `filter_state_for_viewer` outputs, so any identity the
/// viewer may not see (opponents' hands, libraries, face-down cards) is already
/// redacted in the snapshots and in `ZoneChange.name`. On top of that, a
/// transition is only surfaced when at least ONE endpoint is a public zone: a
/// departure from a public zone is public information even if the destination is
/// hidden (a bounce), and an arrival into a public zone is public even if the
/// origin was hidden (a cast from hand, a mill). Only fully-hidden
/// hand↔library moves (draws, hand shuffles) are elided, since neither endpoint
/// is observable. Output ordering is deterministic (sorted by id).
pub fn compute_preview_diff(before: &GameState, after: &GameState) -> PreviewDiff {
    let mut life_deltas = Vec::new();
    for a in &after.players {
        if let Some(b) = before.players.iter().find(|p| p.id == a.id) {
            if a.life != b.life {
                life_deltas.push(LifeDelta {
                    player: a.id,
                    delta: a.life - b.life,
                });
            }
        }
    }
    life_deltas.sort_by_key(|l| l.player.0);

    let mut zone_changes = Vec::new();
    let mut created = Vec::new();
    for (id, a) in &after.objects {
        match before.objects.get(id) {
            // Observable iff at least one endpoint is public (fully-hidden
            // hand↔library moves are elided).
            Some(b) if b.zone != a.zone && (zone_is_public(b.zone) || zone_is_public(a.zone)) => {
                zone_changes.push(ZoneChange {
                    object_id: *id,
                    name: a.name.clone(),
                    controller: a.controller,
                    from: b.zone,
                    to: a.zone,
                });
            }
            Some(_) => {}
            None if zone_is_public(a.zone) => created.push(*id),
            None => {}
        }
    }

    // Objects present before and gone after ceased to exist (CR 111.7 /
    // CR 704.5d — a token leaving battlefield/stack via SBAs). Only report those
    // that were in a public zone, so a token dying on the battlefield ("this
    // kills that") surfaces while a hidden-zone disappearance stays hidden.
    let mut ceased = Vec::new();
    for (id, b) in &before.objects {
        if !after.objects.contains_key(id) && zone_is_public(b.zone) {
            ceased.push(CeasedObject {
                object_id: *id,
                name: b.name.clone(),
                controller: b.controller,
                from: b.zone,
            });
        }
    }

    zone_changes.sort_by_key(|z| z.object_id.0);
    created.sort_by_key(|id| id.0);
    ceased.sort_by_key(|c| c.object_id.0);

    PreviewDiff {
        life_deltas,
        zone_changes,
        created,
        ceased,
    }
}

/// Returns the mana sources the automatic payment path uses for `action`,
/// without changing the live game state.
///
/// This accepts only an exact, engine-offered automatic `CastSpell` action.
/// Simulating that action keeps the preview on the same payment path as a real
/// cast; this layer deliberately does not reconstruct casts or invoke the
/// payment resolver itself. If an X spell pauses to announce its value, the
/// drag preview continues with X = 0: drag has no announced X value, and zero
/// is the rules-defined default for an unchosen X outside the stack.
/// Other player choices still return an empty list because they determine the
/// final payment path.
pub fn preview_auto_payment_sources(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<Vec<ObjectId>, EngineError> {
    let GameAction::CastSpell {
        object_id,
        payment_mode: CastPaymentMode::Auto,
        ..
    } = action
    else {
        return Ok(Vec::new());
    };

    if !legal_actions_for_viewer(state, actor)
        .0
        .iter()
        .any(|candidate| candidate == action)
    {
        return Ok(Vec::new());
    }

    let mut sim = state.clone();
    // Auto-pass is a convenience setting for future priority windows, not part
    // of casting payment. A preview must stop at the cast transaction rather
    // than continuing through unrelated automatic passes.
    sim.auto_pass.clear();
    let result = apply_for_simulation(&mut sim, actor, action.clone())?;
    // CR 107.3g: Before this card is cast, its unannounced {X} is 0. Preserve
    // that drag-time view by advancing the throwaway cast simulation at X=0.
    let mut events = result.events;
    if matches!(&result.waiting_for, WaitingFor::ChooseXValue { min: 0, .. }) {
        let x_result = apply_for_simulation(&mut sim, actor, GameAction::ChooseX { value: 0 })?;
        events.extend(x_result.events);
    }
    Ok(mana_source_ids_before_spell_cast(&events, *object_id))
}

fn mana_source_ids_before_spell_cast(events: &[GameEvent], object_id: ObjectId) -> Vec<ObjectId> {
    let Some(spell_cast_index) = events.iter().position(|event| {
        matches!(event, GameEvent::SpellCast { object_id: cast_object_id, .. } if *cast_object_id == object_id)
    }) else {
        return Vec::new();
    };

    let mut sources = events
        .iter()
        .take(spell_cast_index)
        .filter_map(|event| match event {
            GameEvent::ManaAdded { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source_id| source_id.0);
    sources.dedup();
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::identifiers::CardId;

    fn obj(id: u64, owner: u8, name: &str, zone: Zone) -> GameObject {
        GameObject::new(
            ObjectId(id),
            CardId(id),
            PlayerId(owner),
            name.to_string(),
            zone,
        )
    }

    #[test]
    fn reports_life_delta_and_public_zone_change() {
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();

        // A creature on the battlefield in `before` dies (→ graveyard) in `after`,
        // and player 1 loses 4 life.
        before
            .objects
            .insert(ObjectId(10), obj(10, 0, "Bear", Zone::Battlefield));
        after
            .objects
            .insert(ObjectId(10), obj(10, 0, "Bear", Zone::Graveyard));
        after.players[1].life = before.players[1].life - 4;

        let diff = compute_preview_diff(&before, &after);
        assert_eq!(
            diff.zone_changes,
            vec![ZoneChange {
                object_id: ObjectId(10),
                name: "Bear".to_string(),
                controller: PlayerId(0),
                from: Zone::Battlefield,
                to: Zone::Graveyard,
            }]
        );
        assert_eq!(
            diff.life_deltas,
            vec![LifeDelta {
                player: PlayerId(1),
                delta: -4,
            }]
        );
        assert!(diff.created.is_empty());
    }

    #[test]
    fn reports_created_token_in_public_zone() {
        let before = GameState::new_two_player(1);
        let mut after = before.clone();
        after
            .objects
            .insert(ObjectId(20), obj(20, 0, "Soldier", Zone::Battlefield));

        let diff = compute_preview_diff(&before, &after);
        assert_eq!(diff.created, vec![ObjectId(20)]);
        assert!(diff.zone_changes.is_empty());
    }

    #[test]
    fn reports_public_endpoint_transitions_but_elides_pure_hidden_moves() {
        // Widened predicate (#5491 review): a transition with at least one PUBLIC
        // endpoint is observable and reported; only fully-hidden hand↔library
        // moves (draws) are elided. A creation into a hidden zone is not reported.
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();

        // Cast: hand → stack (arrival into a public zone) — REPORTED.
        before
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Hand));
        after
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Stack));
        // Bounce: battlefield → hand (departure from a public zone) — REPORTED.
        before
            .objects
            .insert(ObjectId(32), obj(32, 0, "Bear", Zone::Battlefield));
        after
            .objects
            .insert(ObjectId(32), obj(32, 0, "Bear", Zone::Hand));
        // Draw: library → hand (both hidden) — ELIDED.
        before
            .objects
            .insert(ObjectId(33), obj(33, 0, "Secret", Zone::Library));
        after
            .objects
            .insert(ObjectId(33), obj(33, 0, "Secret", Zone::Hand));
        // Milled into the library (hidden arrival) — not `created`.
        after
            .objects
            .insert(ObjectId(34), obj(34, 0, "Milled", Zone::Library));

        let diff = compute_preview_diff(&before, &after);
        let changed: Vec<u64> = diff.zone_changes.iter().map(|z| z.object_id.0).collect();
        assert_eq!(
            changed,
            vec![31, 32],
            "cast + bounce reported, draw elided: {:?}",
            diff.zone_changes
        );
        assert!(
            diff.created.is_empty(),
            "hidden-zone creation is not reported"
        );
    }

    #[test]
    fn reports_ceased_token_but_not_hidden_disappearance() {
        // #5491 review blocker: a token that dies (removed from `after.objects` by
        // state-based actions, CR 111.7 / 704.5d) must still surface — the headline
        // "this kills that" case. A disappearance from a hidden zone must not.
        let mut before = GameState::new_two_player(1);
        let after = before.clone();
        // These exist only in `before` → they ceased to exist during the action.
        before
            .objects
            .insert(ObjectId(40), obj(40, 0, "Zombie Token", Zone::Battlefield));
        before
            .objects
            .insert(ObjectId(41), obj(41, 0, "Milled Token", Zone::Library));

        let diff = compute_preview_diff(&before, &after);
        assert_eq!(
            diff.ceased,
            vec![CeasedObject {
                object_id: ObjectId(40),
                name: "Zombie Token".to_string(),
                controller: PlayerId(0),
                from: Zone::Battlefield,
            }],
            "public-zone token death surfaces; hidden-zone disappearance stays hidden",
        );
    }
}
