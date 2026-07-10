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
//! already redacted), AND this only reports transitions BETWEEN public zones
//! plus life totals — so no hidden-zone movement (draws, random discards from
//! hand, library shuffles) is ever surfaced, even for the acting player's
//! opponents. CR 400.2 draws the public/hidden zone line.

use serde::{Deserialize, Serialize};

use crate::types::game_state::GameState;
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

/// The public, viewer-safe result of previewing an action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreviewDiff {
    pub life_deltas: Vec<LifeDelta>,
    pub zone_changes: Vec<ZoneChange>,
    /// Objects newly present in a public zone (e.g. a created token).
    pub created: Vec<ObjectId>,
}

/// CR 400.2: a zone whose contents every player can see. Hand and Library are
/// hidden; every other zone is public.
fn zone_is_public(zone: Zone) -> bool {
    !matches!(zone, Zone::Hand | Zone::Library)
}

/// Diff two snapshots (before/after an action) into the PUBLIC deltas a viewer
/// could legitimately observe.
///
/// Callers MUST pass `filter_state_for_viewer` outputs; on top of that this only
/// emits public-zone-to-public-zone transitions and life-total changes, so
/// hidden-zone movements never leak. Output ordering is deterministic (sorted by
/// id) for stable client rendering.
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
            Some(b) => {
                // Only surface transitions where BOTH ends are public — a
                // hand→battlefield cast or library→hand draw is elided so the
                // preview can't reveal a hidden card's identity or a random
                // draw/discard outcome.
                if b.zone != a.zone && zone_is_public(b.zone) && zone_is_public(a.zone) {
                    zone_changes.push(ZoneChange {
                        object_id: *id,
                        name: a.name.clone(),
                        controller: a.controller,
                        from: b.zone,
                        to: a.zone,
                    });
                }
            }
            None if zone_is_public(a.zone) => created.push(*id),
            None => {}
        }
    }
    zone_changes.sort_by_key(|z| z.object_id.0);
    created.sort_by_key(|id| id.0);

    PreviewDiff {
        life_deltas,
        zone_changes,
        created,
    }
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
    fn elides_hidden_zone_movements() {
        // Hand↔Library↔Battlefield transitions that touch a hidden zone must NOT
        // be reported — no draw/discard/cast leaks a hidden identity.
        let mut before = GameState::new_two_player(1);
        let mut after = before.clone();

        // Draw: library → hand (both partly hidden) — elided.
        before
            .objects
            .insert(ObjectId(30), obj(30, 0, "Secret", Zone::Library));
        after
            .objects
            .insert(ObjectId(30), obj(30, 0, "Secret", Zone::Hand));
        // Cast: hand → stack (hand is hidden) — elided.
        before
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Hand));
        after
            .objects
            .insert(ObjectId(31), obj(31, 0, "Bolt", Zone::Stack));
        // A newly-created object that lands in the LIBRARY must not count as created.
        after
            .objects
            .insert(ObjectId(32), obj(32, 0, "Milled", Zone::Library));

        let diff = compute_preview_diff(&before, &after);
        assert!(
            diff.zone_changes.is_empty(),
            "hidden-zone transitions must be elided: {:?}",
            diff.zone_changes
        );
        assert!(
            diff.created.is_empty(),
            "objects created in a hidden zone must not be reported"
        );
    }
}
