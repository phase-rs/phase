use crate::types::ability::{
    Effect, EffectError, EffectKind, ParentTargetMissingReason, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// Whose library a `RevealTop` reveals from, bound once for both [`resolve`]
/// and [`optional_reveal_is_infeasible`].
enum RevealSubjects {
    /// "Two target players each reveal the top card of their library" (Parker
    /// Luck): every targeted player reveals their own top card.
    EachTargeted(Vec<PlayerId>),
    /// One player, or `None` when the player reference names no one
    /// (CR 608.2b — see `resolve_player_for_context_ref`).
    One(Option<PlayerId>),
}

/// CR 701.20 + CR 601.2c: "two target players each reveal the top card of their
/// library" (Parker Luck). When the reveal is a true `Player`-target reveal with
/// more than one chosen player, every targeted player reveals their own top card
/// and the full ordered set lands in `last_revealed_ids`. Guarded on the targeted
/// `Player` filter + `> 1` chosen players so single-target reveals and
/// context-ref (`Controller`/`ScopedPlayer`) `player_scope` reveals (Duskmantle
/// Seer) are byte-unchanged.
///
/// CR 115.1: Mirror Draw/Mill/Discard — context-ref filters (Controller,
/// DefendingPlayer, etc.) must consult state slots, not `ability.targets`,
/// so a chained "reveal top of your library" sub-ability does not inherit
/// the parent's Player target and reveal from the wrong library.
fn reveal_subjects(
    state: &GameState,
    ability: &ResolvedAbility,
    player_filter: &TargetFilter,
) -> RevealSubjects {
    let targeted_players: Vec<PlayerId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        })
        .collect();
    if matches!(player_filter, TargetFilter::Player) && targeted_players.len() > 1 {
        RevealSubjects::EachTargeted(targeted_players)
    } else {
        RevealSubjects::One(super::resolve_player_for_context_ref(
            state,
            ability,
            player_filter,
        ))
    }
}

/// CR 608.2d + CR 701.20a: "you may reveal the top card of your library" is an
/// impossible option when it would reveal no card at all — every library it
/// reads is empty, or it names no player. CR 608.2d's empty-library exception
/// covers drawing only (CR 121.3). Fewer cards than the count is not
/// impossible: the reveal does as much as possible (CR 609.3).
pub(crate) fn optional_reveal_is_infeasible(state: &GameState, ability: &ResolvedAbility) -> bool {
    let Effect::RevealTop { count, player } = &ability.effect else {
        return false;
    };
    if *count == 0 {
        return true;
    }
    match reveal_subjects(state, ability, player) {
        // WATCH-POINT N2 mirror: `resolve` skips a missing player or an empty
        // library individually, so the reveal is impossible only when it skips
        // every targeted player.
        RevealSubjects::EachTargeted(players) => players.into_iter().all(|pid| {
            state
                .players
                .get(pid.0 as usize)
                .is_none_or(|player| state.library_of(player.id).is_empty())
        }),
        RevealSubjects::One(None) => true,
        RevealSubjects::One(Some(pid)) => state.library_of(pid).is_empty(),
    }
}

/// CR 701.20a: Reveal the top card(s) of a player's library.
///
/// Resolves the `player` target filter (typically `DefendingPlayer` or `Controller`)
/// into a PlayerId, then takes the top `count` cards from that player's library,
/// marks them as revealed, and emits `CardsRevealed`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, player_filter) = match &ability.effect {
        Effect::RevealTop { count, player } => (*count as usize, player.clone()),
        _ => return Err(EffectError::MissingParam("RevealTop count".to_string())),
    };

    state.last_parent_target_missing_reason = None;

    let target_player = match reveal_subjects(state, ability, &player_filter) {
        RevealSubjects::EachTargeted(targeted_players) => {
            let mut accumulated: Vec<crate::types::identifiers::ObjectId> = Vec::new();
            for pid in targeted_players {
                let Some(player) = state.players.get(pid.0 as usize) else {
                    continue;
                };
                // WATCH-POINT N2: skip an empty library INDIVIDUALLY — never
                // early-return, or a first empty library would suppress every later
                // player's reveal (CR 608.2b fail-closed per player).
                let library = state.library_of(player.id);
                if library.is_empty() {
                    continue;
                }
                let count_n = count.min(library.len());
                let revealed_ids: Vec<_> = library.iter().take(count_n).copied().collect();
                // CR 701.20b: Revealing a card doesn't cause it to leave its zone.
                for &card_id in &revealed_ids {
                    state.revealed_cards.insert(card_id);
                }
                let card_names: Vec<String> = revealed_ids
                    .iter()
                    .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
                    .collect();
                events.push(GameEvent::CardsRevealed {
                    player: pid,
                    card_ids: revealed_ids.clone(),
                    card_names,
                });
                accumulated.extend(revealed_ids);
            }
            // CR 108.3 + CR 608.2c: the full ordered set drives owner-keyed per-player
            // binding and the OtherRevealedCard by-exclusion cross-loss.
            super::publish_reveal_result(state, accumulated);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Reveal,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        RevealSubjects::One(None) => {
            super::publish_reveal_result(state, Vec::new());
            state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
            return Ok(());
        }
        RevealSubjects::One(Some(target_player)) => target_player,
    };

    let library = state.library_of(target_player);
    if library.is_empty() {
        super::publish_reveal_result(state, Vec::new());
        state.last_parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // Take the top `count` cards (library[0] = top, per zones.rs convention)
    let count = count.min(library.len());
    let revealed_ids: Vec<_> = library.iter().take(count).copied().collect();

    // CR 701.20b: Revealing a card doesn't cause it to leave the zone it's in.
    for &card_id in &revealed_ids {
        state.revealed_cards.insert(card_id);
    }

    // Store revealed IDs for sub_ability condition/target injection
    super::publish_reveal_result(state, revealed_ids.clone());

    // Emit event with card names
    let card_names: Vec<String> = revealed_ids
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: target_player,
        card_ids: revealed_ids,
        card_names,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_reveal_top_ability(
        controller: PlayerId,
        target_player: PlayerId,
        count: u32,
    ) -> ResolvedAbility {
        // Non-context-ref filter so the explicit `TargetRef::Player` in
        // `ability.targets` legitimately wins (mirrors "target player reveals
        // the top card of their library" cards).
        ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::Any,
                count,
            },
            vec![TargetRef::Player(target_player)],
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_top_marks_top_card_as_revealed() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.contains(&card1));
    }

    #[test]
    fn reveal_top_emits_cards_revealed_event() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { card_names, .. } => Some(card_names.clone()),
            _ => None,
        });
        assert_eq!(revealed, Some(vec!["Mountain".to_string()]));
    }

    #[test]
    fn reveal_top_empty_library_is_noop() {
        let mut state = GameState::new_two_player(42);
        // Player 1 has no library

        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn reveal_top_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Library,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Island".to_string(),
            Zone::Library,
        );
        let _card3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Library,
        );

        // library = [card1(top), card2, card3(bottom)]
        let ability = make_reveal_top_ability(PlayerId(0), PlayerId(1), 2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Top 2 cards (library[0..2]) should be revealed
        assert!(state.revealed_cards.contains(&card1));
        assert!(state.revealed_cards.contains(&card2));
        assert_eq!(state.revealed_cards.len(), 2);
    }

    #[test]
    fn reveal_top_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained RevealTop with `player: Controller`
        // must reveal the spell controller's library, not the parent's
        // inherited Player target.
        let mut state = GameState::new_two_player(42);
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealTop {
                player: TargetFilter::Controller,
                count: 1,
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.revealed_cards.contains(&p0_top),
            "P0's library top should be revealed (Controller filter resolves to caster)"
        );
        assert!(
            !state.revealed_cards.contains(&p1_top),
            "P1's library top must NOT be revealed — inherited parent target must not override Controller filter"
        );
    }

    fn library_card(state: &mut GameState, owner: PlayerId, card: u64) -> ObjectId {
        create_object(
            state,
            CardId(card),
            owner,
            format!("Library Card {card}"),
            Zone::Library,
        )
    }

    fn optional_reveal(
        player: TargetFilter,
        count: u32,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::RevealTop { player, count },
            targets,
            ObjectId(100),
            PlayerId(0),
        );
        ability.optional = true;
        ability
    }

    /// U2-a/b — CR 608.2d + CR 701.20a + CR 609.3: "you may reveal the top card
    /// of your library" is impossible with an empty library, but a library
    /// smaller than the count still reveals what it has.
    #[test]
    fn optional_reveal_is_infeasible_only_when_it_would_reveal_nothing() {
        let mut state = GameState::new_two_player(42);
        let reveal_one = optional_reveal(TargetFilter::Controller, 1, vec![]);
        assert!(
            optional_reveal_is_infeasible(&state, &reveal_one),
            "an empty library has no top card to reveal"
        );

        library_card(&mut state, PlayerId(0), 1);
        let reveal_three = optional_reveal(TargetFilter::Controller, 3, vec![]);
        assert!(
            !optional_reveal_is_infeasible(&state, &reveal_one),
            "reach: one card is revealable"
        );
        assert!(
            !optional_reveal_is_infeasible(&state, &reveal_three),
            "fewer cards than the count reveals as many as possible (CR 609.3)"
        );
    }

    /// U2-c — CR 701.20a + CR 601.2c: "two target players each reveal …" is
    /// feasible while any targeted library has a card, and impossible when every
    /// targeted library is empty.
    #[test]
    fn optional_each_targeted_reveal_needs_one_nonempty_library() {
        let reveal = optional_reveal(
            TargetFilter::Player,
            1,
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
        );
        let mut state = GameState::new_two_player(42);
        assert!(
            optional_reveal_is_infeasible(&state, &reveal),
            "both targeted libraries are empty"
        );
        library_card(&mut state, PlayerId(1), 1);
        assert!(
            !optional_reveal_is_infeasible(&state, &reveal),
            "the second targeted player still reveals a card"
        );
    }

    /// U2-d — CR 608.2b + CR 608.2d: a reveal whose player reference names no
    /// one (an unannounced declared player) reveals nothing, so it is impossible
    /// even when libraries have cards.
    #[test]
    fn optional_reveal_naming_no_player_is_infeasible() {
        let mut state = GameState::new_two_player(42);
        library_card(&mut state, PlayerId(0), 1);
        library_card(&mut state, PlayerId(1), 2);
        let reveal = optional_reveal(
            TargetFilter::DeclaredPlayer {
                group: crate::types::ability::ChosenGroupId(
                    crate::types::ability::ChosenGroupId::DECLARED_PLAYER_BASE,
                ),
            },
            1,
            vec![],
        );
        assert!(optional_reveal_is_infeasible(&state, &reveal));
    }
}
