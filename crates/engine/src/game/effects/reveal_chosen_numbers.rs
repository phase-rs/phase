use crate::types::ability::{Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// CR 101.4 + CR 608.2c: Publish the numbers `players` secretly chose earlier in
/// this resolution — the runtime half of "then all players reveal those numbers
/// simultaneously" (Wheel of Misfortune), "then you reveal the number you chose"
/// (The Toymaker's Trap), "Then those numbers are revealed" (Menacing Ogre).
///
/// The transition is typed, not a visibility flag: each named player's
/// `ChosenAttribute::Number` (private — `game::visibility` redacts it from every
/// other viewer) becomes `ChosenAttribute::RevealedNumber` (public). Because
/// privacy is a property of the attribute kind, this single conversion is what
/// makes the card's reveal instruction observable, and a card that never reveals
/// keeps its numbers secret with no extra bookkeeping.
///
/// CR 101.4: the reveal is SIMULTANEOUS, so every player is converted before the
/// event is emitted and one event carries the whole set. Iteration is in APNAP
/// order purely so the event's contents are deterministic (CR 101.4 fixes that
/// order for the choices themselves); no game action is sequenced by it.
///
/// CR 609.3: naming a player who chose no number does as much as possible —
/// nothing. That is what lets Wheel of Misfortune's `players: All` be correct on
/// a table where a card's choosers were only a subset (Life at Stake).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let players = match &ability.effect {
        Effect::RevealChosenNumbers { players } => players.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected RevealChosenNumbers effect".to_string(),
            ))
        }
    };

    let candidates: Vec<PlayerId> = crate::game::players::apnap_order_from(
        state,
        ability.starting_with.clone(),
        ability.controller,
    )
    .into_iter()
    .filter(|pid| {
        super::matches_player_scope(state, *pid, &players, ability.controller, ability.source_id)
    })
    .collect();

    let mut numbers: Vec<(PlayerId, u8)> = Vec::new();
    for pid in candidates {
        if let Some(player) = state.players.iter_mut().find(|p| p.id == pid) {
            if let Some(value) = player.reveal_chosen_number() {
                numbers.push((pid, value));
            }
        }
    }

    // CR 613.1: a per-player published value can gate statics/filters that read
    // it, so re-run layers for the same reason the per-player anchor bind does.
    if !numbers.is_empty() {
        crate::game::layers::mark_layers_full(state);
        events.push(GameEvent::ChosenNumbersRevealed { numbers });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// The default population when a card does not name one. "Reveal those numbers"
/// with no subject means every player who chose (CR 101.4).
pub(crate) fn default_players() -> PlayerFilter {
    PlayerFilter::All
}
