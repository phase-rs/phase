use serde::{Deserialize, Serialize};

use super::player::PlayerId;

/// CR 602.2 + CR 605.1a + CR 606.2: Discriminates which kind of activated
/// ability was activated, so "Whenever you activate a loyalty ability"
/// triggers (CR 606.2), "that isn't a mana ability" qualifiers (CR 605.1a), and
/// ordinary activation triggers share the single `GameEvent::AbilityActivated`
/// event family. The three kinds partition activated abilities: CR 605.1a
/// excludes loyalty abilities from being mana abilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ActivatedAbilityKind {
    /// CR 602.2: An ordinary (non-loyalty, non-mana) activated ability.
    #[default]
    Normal,
    /// CR 606.1 + CR 606.2: A loyalty ability of a planeswalker.
    Loyalty,
    /// CR 605.1a + CR 605.3: An activated mana ability. Activating it follows
    /// CR 602.2 like any other activation, but it resolves immediately without
    /// using the stack (CR 605.3b).
    Mana,
}

impl ActivatedAbilityKind {
    /// CR 606.1 + CR 605.1a: The single classifier of an activated ability's
    /// kind, from the ability definition bound when it was announced. Never
    /// reads live game state, so an ability removed from its source (a granted
    /// ability, a source that left) is still classified as it was activated.
    pub fn of_definition(def: &super::ability::AbilityDefinition) -> Self {
        if def
            .cost
            .as_ref()
            .is_some_and(super::ability::is_loyalty_ability_cost)
        {
            Self::Loyalty
        } else if crate::types::ability::is_mana_ability(def) {
            Self::Mana
        } else {
            Self::Normal
        }
    }

    /// CR 602.5 + CR 606.1: Whether an activation of this kind falls under a
    /// `CantBeActivated` prohibition's ability-kind axis. That axis predates the
    /// `Mana` kind and distinguishes loyalty from non-loyalty abilities, so a
    /// `Normal` requirement means "non-loyalty" and keeps covering mana
    /// abilities. Mana abilities are carved out on the separate
    /// `ActivationExemption::ManaAbilities` axis (CR 605.1a).
    pub fn satisfies_prohibition_kind(self, required: Self) -> bool {
        match required {
            Self::Normal => matches!(self, Self::Normal | Self::Mana),
            Self::Loyalty => self == Self::Loyalty,
            Self::Mana => self == Self::Mana,
        }
    }
}

/// Avatar crossover: The four elemental bending types, tracked per-turn on each player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BendingType {
    Fire,
    Air,
    Earth,
    Water,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PlayerActionKind {
    /// A player accepted a resolution-time optional effect.
    AcceptedOptionalEffect,
    SearchedLibrary,
    Scry,
    Surveil,
    CollectEvidence,
    /// CR 701.24a: A player shuffled their library.
    ShuffledLibrary,
    /// CR 701.34a: A player proliferated.
    Proliferate,
    /// CR 701.16a: A player investigated (created a Clue token).
    Investigate,
    /// CR 701.61a: A player foraged by exiling three cards from their graveyard
    /// or sacrificing a Food.
    Forage,
    /// A player completed a draw instruction that delivered at least
    /// one card. Emitted once per settled draw INSTRUCTION (at draw-sequence
    /// completion), not once per card — so a multi-card draw records a single
    /// event. Recorded so "for each opponent who drew a card this way" (Cut a
    /// Deal) resolves via `PlayerFilter::PerformedActionThisWay` — a count over
    /// players, not objects — and so `PlayerActionsThisTurn { Draw }` would count
    /// draw events rather than cards. `player_actions_this_way` (a set) counts the
    /// drawing player once; a draw that delivered no card (empty library, or every
    /// unit replaced away) emits nothing because that player did not draw.
    Draw,
}

/// CR 701.30d: Result of a clash — whether the controller won, lost, or tied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClashResult {
    Won,
    Lost,
    Tied,
}

impl ClashResult {
    /// CR 701.30d: A clash's `result` is stated from the clash controller's
    /// perspective (the player who initiated the clash). Re-express it from
    /// `player`'s perspective, returning `None` if `player` did not participate.
    /// The controller sees `self`; the opponent sees the mirror (Won ⇄ Lost, Tied
    /// unchanged).
    ///
    /// Single source of truth shared by resolution-time "if you won" gating
    /// (`event_outcome_was_won_by_controller`) and trigger MATCHING
    /// (`match_clash`'s `clash_result` gate) so both agree on who won.
    pub fn for_player(
        self,
        clash_controller: PlayerId,
        opponent: PlayerId,
        player: PlayerId,
    ) -> Option<ClashResult> {
        if player == clash_controller {
            Some(self)
        } else if player == opponent {
            Some(match self {
                ClashResult::Won => ClashResult::Lost,
                ClashResult::Lost => ClashResult::Won,
                ClashResult::Tied => ClashResult::Tied,
            })
        } else {
            None
        }
    }
}
