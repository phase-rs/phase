//! Engine-authored presentation projections over `GameState`.
//!
//! These "derived views" are computed just-in-time at serialization
//! boundaries (the WASM getter, the server-core broadcast) and sent to
//! clients alongside the raw state. Display consumers (React components)
//! consume the pre-grouped shape directly and never compute game logic
//! themselves — per CLAUDE.md's "engine owns all logic" invariant.
//!
//! Contrast with `crates/engine/src/game/derived.rs`, which contains
//! engine-internal state derivation (summoning sickness, commander damage
//! aggregation, etc.). This module is a thin presentation-facing wrapper
//! that composes those helpers into a client-ready shape.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::game::stack::{stack_display_groups, StackDisplayGroup};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// A single commander-damage badge the HUD renders: which victim received
/// `damage` from `commander` (the ObjectId is stable across zone changes
/// because commanders live in `state.objects` for the life of the game).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageView {
    pub victim: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

/// Engine-authored projections used by the display layer. Keep this struct
/// small — every field becomes mandatory payload on every state snapshot
/// the client receives. Add a new field only when the frontend would
/// otherwise have to compute game logic (a CLAUDE.md violation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedViews {
    /// Commander damage grouped by the attacking commander's current
    /// controller. Each inner entry preserves per-commander identity so
    /// partner commanders under one controller render as separate badges.
    /// Empty in non-Commander formats (see `derive_views` JIT short-circuit).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commander_damage_by_attacker: BTreeMap<PlayerId, Vec<CommanderDamageView>>,

    /// Engine-authored coalesced view of the stack. Adjacent entries with
    /// the same (source, kind, description, targets) signature collapse
    /// into one `StackDisplayGroup` with a `count`. Empty when the stack
    /// is empty (JIT short-circuit). The frontend renders one card + ×N
    /// badge per group and never re-implements the grouping rule.
    /// Authoritative grouping lives in `game::stack::stack_display_groups`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack_display_groups: Vec<StackDisplayGroup>,
}

/// Serialize-only wrapper: the WASM getter passes `&GameState` by reference
/// to avoid an O(n) clone of `state.objects` and other owned collections
/// (GameState is not rpds-backed at the top level). The wire shape is
/// `{ state: <GameState>, derived: <DerivedViews> }`.
#[derive(Debug, Serialize)]
pub struct ClientGameStateRef<'a> {
    pub state: &'a GameState,
    pub derived: DerivedViews,
}

impl<'a> ClientGameStateRef<'a> {
    /// Wrap a borrowed `GameState` with its derived projections.
    /// Invoke AFTER any viewer-side filtering (e.g. `filter_state_for_player`)
    /// so the derived shape reflects what the viewer will actually see.
    pub fn wrap(state: &'a GameState) -> Self {
        Self {
            state,
            derived: derive_views(state),
        }
    }
}

/// Owned counterpart for deserialize paths (round-trip tests, any future
/// state-restore flow that ingests the wire format). The JSON shape matches
/// `ClientGameStateRef` exactly — fields named identically, no
/// `#[serde(flatten)]` — so serialize/deserialize round-trip is lossless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientGameState {
    pub state: GameState,
    pub derived: DerivedViews,
}

/// Compute all engine-authored projections over `state`. Runs in O(damage
/// entries) per call; the JIT short-circuit for non-Commander formats
/// (where `commander_damage_threshold` is `None`) keeps the cost at exactly
/// zero for the overwhelmingly common case.
///
/// CR 903.10a: commander damage is public information tracked per commander
/// — no viewer-based redaction is applied here, and the grouping runs
/// unconditionally for every Commander-format game regardless of who is
/// viewing. Partner commanders under the same controller each get their
/// own `CommanderDamageView` entry, not a summed total.
pub fn derive_views(state: &GameState) -> DerivedViews {
    let mut views = DerivedViews::default();

    // JIT short-circuit: grouping an empty stack is free, but this also
    // avoids the per-entry allocation path entirely for the dominant case
    // (no spells/abilities in flight).
    if !state.stack.is_empty() {
        views.stack_display_groups = stack_display_groups(state);
    }

    if state.format_config.commander_damage_threshold.is_none() {
        return views;
    }
    for &victim in &state.seat_order {
        for (attacker, entries) in super::derived::commander_damage_received(state, victim) {
            views
                .commander_damage_by_attacker
                .entry(attacker)
                .or_default()
                .extend(
                    entries
                        .into_iter()
                        .map(|(commander, damage)| CommanderDamageView {
                            victim,
                            commander,
                            damage,
                        }),
                );
        }
    }
    views
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::CommanderDamageEntry;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    fn setup_commander_game(num_players: u8) -> GameState {
        let mut state = GameState::new(FormatConfig::commander(), num_players, 42);
        for player_idx in 0..num_players {
            for i in 0..5 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        state
    }

    /// JIT short-circuit: non-Commander formats must return an empty view
    /// without walking `state.commander_damage`. Verifies the map is empty
    /// even when the flat list has entries (defensive; this shouldn't
    /// happen in practice, but the early-return must not depend on the
    /// data being empty).
    #[test]
    fn derive_views_empty_for_non_commander_format() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // Push a phantom entry to prove the short-circuit doesn't inspect it.
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: ObjectId(1),
            damage: 21,
        });

        let views = derive_views(&state);
        assert!(
            views.commander_damage_by_attacker.is_empty(),
            "non-Commander format must short-circuit regardless of stored damage entries"
        );
    }

    /// Four-player pod: P0 receives damage from two different opponents'
    /// commanders. The view must key entries by the attacking commander's
    /// controller, preserving per-commander granularity for the HUD.
    #[test]
    fn derive_views_groups_by_attacker_in_four_player_pod() {
        let mut state = setup_commander_game(4);
        let cmd_p1 = create_object(
            &mut state,
            CardId(1001),
            PlayerId(1),
            "P1 Commander".into(),
            Zone::Command,
        );
        let cmd_p2 = create_object(
            &mut state,
            CardId(1002),
            PlayerId(2),
            "P2 Commander".into(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd_p1).unwrap().is_commander = true;
        state.objects.get_mut(&cmd_p2).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd_p1,
            damage: 7,
        });
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd_p2,
            damage: 11,
        });

        let views = derive_views(&state);
        let from_p1 = views
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 should have an entry");
        let from_p2 = views
            .commander_damage_by_attacker
            .get(&PlayerId(2))
            .expect("P2 should have an entry");
        assert_eq!(from_p1.len(), 1);
        assert_eq!(from_p1[0].damage, 7);
        assert_eq!(from_p1[0].victim, PlayerId(0));
        assert_eq!(from_p1[0].commander, cmd_p1);
        assert_eq!(from_p2.len(), 1);
        assert_eq!(from_p2[0].damage, 11);
    }

    /// Partner commanders (two commanders under the same controller) must
    /// remain separate entries — CR 903.10a tracks commander damage per
    /// commander identity, so summing them would misreport the SBA-lethal
    /// progress when one partner is at 20 damage and the other at 5.
    #[test]
    fn derive_views_respects_partner_commanders() {
        let mut state = setup_commander_game(2);
        let partner_a = create_object(
            &mut state,
            CardId(2001),
            PlayerId(1),
            "Partner A".into(),
            Zone::Command,
        );
        let partner_b = create_object(
            &mut state,
            CardId(2002),
            PlayerId(1),
            "Partner B".into(),
            Zone::Command,
        );
        state.objects.get_mut(&partner_a).unwrap().is_commander = true;
        state.objects.get_mut(&partner_b).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: partner_a,
            damage: 20,
        });
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: partner_b,
            damage: 5,
        });

        let views = derive_views(&state);
        let from_p1 = views
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 should have an entry");
        assert_eq!(
            from_p1.len(),
            2,
            "partner commanders must stay as separate entries, not be summed"
        );
        let damages: Vec<u32> = from_p1.iter().map(|e| e.damage).collect();
        assert!(damages.contains(&20));
        assert!(damages.contains(&5));
    }

    /// Stack grouping rides alongside commander damage in the same derived
    /// view: one `derive_views` pass populates both. The detailed grouping
    /// behavior (coalescing rules, target-aware keys, keyword-action opt-
    /// outs) is covered by the dedicated tests in `game::stack`; this test
    /// only verifies wiring — that `derive_views` invokes the grouper when
    /// the stack is non-empty and short-circuits when it is.
    #[test]
    fn derive_views_wires_stack_display_groups() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(4001),
            PlayerId(0),
            "Scute Swarm".into(),
            Zone::Battlefield,
        );
        let mk_effect = || Effect::Unimplemented {
            name: "test".into(),
            description: None,
        };
        for i in 0..2u64 {
            state.stack.push_back(StackEntry {
                id: ObjectId(9000 + i),
                source_id: source,
                controller: PlayerId(0),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(ResolvedAbility::new(
                        mk_effect(),
                        vec![],
                        source,
                        PlayerId(0),
                    )),
                    condition: None,
                    trigger_event: None,
                    description: Some("landfall".into()),
                },
            });
        }

        let views = derive_views(&state);
        assert_eq!(
            views.stack_display_groups.len(),
            1,
            "identical adjacent triggers must coalesce into one group"
        );
        assert_eq!(views.stack_display_groups[0].count, 2);

        state.stack.clear();
        let empty = derive_views(&state);
        assert!(
            empty.stack_display_groups.is_empty(),
            "empty-stack short-circuit must leave the group vec empty"
        );
    }

    /// Wire-format round-trip: the JSON produced from `ClientGameStateRef`
    /// must deserialize cleanly into `ClientGameState`. This guarantees the
    /// frontend's hand-maintained TypeScript type can consume what the
    /// WASM boundary produces.
    #[test]
    fn client_game_state_roundtrips_through_json() {
        let mut state = setup_commander_game(2);
        let cmd = create_object(
            &mut state,
            CardId(3001),
            PlayerId(1),
            "Roundtrip Cmdr".into(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: cmd,
            damage: 14,
        });

        let wrapped = ClientGameStateRef::wrap(&state);
        let json = serde_json::to_string(&wrapped).expect("serialize");
        let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
        let from_p1 = round
            .derived
            .commander_damage_by_attacker
            .get(&PlayerId(1))
            .expect("P1 entry survives round-trip");
        assert_eq!(from_p1[0].damage, 14);
    }
}
