use crate::types::game_state::GameState;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;

/// Count a player's devotion to the given colors.
///
/// Devotion is the number of mana symbols among mana costs of permanents
/// the player controls that are of any of the specified colors (CR 700.5).
/// A hybrid symbol like {W/U} counts as one symbol toward both white and blue,
/// but only contributes 1 to multi-color devotion (e.g., devotion to white and blue).
pub fn count_devotion(state: &GameState, player: PlayerId, colors: &[ManaColor]) -> u32 {
    let mut total = 0u32;
    for &id in &state.battlefield {
        let obj = match state.objects.get(&id) {
            Some(o) if o.controller == player => o,
            _ => continue,
        };
        if let ManaCost::Cost { ref shards, .. } = obj.mana_cost {
            for shard in shards {
                if colors.iter().any(|c| shard.contributes_to(*c)) {
                    total += 1;
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn devotion_counts_matching_shards() {
        let mut state = setup();
        // Permanent with cost {U}{U} → 2 devotion to blue
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Frostburn Weird".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        };

        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Blue]), 2);
        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Red]), 0);
    }

    #[test]
    fn devotion_hybrid_counts_once_for_multicolor() {
        let mut state = setup();
        // Permanent with cost {U/B} → 1 devotion to blue+black (not 2)
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nightveil Specter".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::BlueBlack,
                ManaCostShard::BlueBlack,
                ManaCostShard::BlueBlack,
            ],
            generic: 0,
        };

        // Each {U/B} contributes to blue AND black, so devotion to blue = 3
        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Blue]), 3);
        // Devotion to "blue and black" counts each symbol once if it matches either
        assert_eq!(
            count_devotion(&state, PlayerId(0), &[ManaColor::Blue, ManaColor::Black]),
            3
        );
    }

    #[test]
    fn devotion_ignores_opponent_permanents() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };

        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Blue]), 0);
    }

    #[test]
    fn devotion_sums_across_permanents() {
        let mut state = setup();
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Perm {}", i),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            };
        }
        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Red]), 3);
    }

    #[test]
    fn devotion_no_cost_contributes_zero() {
        let mut state = setup();
        let _id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Token".to_string(),
            Zone::Battlefield,
        );
        // Default mana cost is ManaCost::zero() (empty shards, 0 generic)
        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::White]), 0);
    }

    #[test]
    fn devotion_mixed_costs() {
        let mut state = setup();
        // Mogis-like scenario: devotion to black and red
        // Perm 1: {B}{B}{R} → 3 symbols matching black or red
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Perm1".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id1).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Red,
            ],
            generic: 1,
        };
        // Perm 2: {U}{B} → 1 matching (the {B})
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Perm2".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id2).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 0,
        };

        assert_eq!(
            count_devotion(&state, PlayerId(0), &[ManaColor::Black, ManaColor::Red]),
            4
        );
    }

    #[test]
    fn devotion_phyrexian_counts() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Phyrexian Card".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue, ManaCostShard::Blue],
            generic: 1,
        };
        assert_eq!(count_devotion(&state, PlayerId(0), &[ManaColor::Blue]), 2);
    }

    #[test]
    fn contributes_to_exhaustive() {
        // Verify basic shards
        assert!(ManaCostShard::White.contributes_to(ManaColor::White));
        assert!(!ManaCostShard::White.contributes_to(ManaColor::Blue));

        // Hybrid counts for both
        assert!(ManaCostShard::WhiteBlue.contributes_to(ManaColor::White));
        assert!(ManaCostShard::WhiteBlue.contributes_to(ManaColor::Blue));
        assert!(!ManaCostShard::WhiteBlue.contributes_to(ManaColor::Black));

        // Two-generic hybrid
        assert!(ManaCostShard::TwoRed.contributes_to(ManaColor::Red));
        assert!(!ManaCostShard::TwoRed.contributes_to(ManaColor::Green));

        // Colorless/generic never count
        assert!(!ManaCostShard::Colorless.contributes_to(ManaColor::White));
        assert!(!ManaCostShard::Snow.contributes_to(ManaColor::Blue));
        assert!(!ManaCostShard::X.contributes_to(ManaColor::Red));
    }
}
