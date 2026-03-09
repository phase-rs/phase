use thiserror::Error;

use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaCost, ManaCostShard, ManaPool, ManaType, ManaUnit};
use crate::types::player::PlayerId;

#[derive(Debug, Clone, Error, PartialEq)]
pub enum PaymentError {
    #[error("Insufficient mana")]
    InsufficientMana,
    #[error("Invalid cost")]
    InvalidCost,
}

/// Result of a phyrexian mana payment that used life instead of mana.
#[derive(Debug, Clone, PartialEq)]
pub struct LifePayment {
    pub player_id: PlayerId,
    pub amount: i32,
}

pub fn produce_mana(
    state: &mut GameState,
    source_id: ObjectId,
    mana_type: ManaType,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let unit = ManaUnit {
        color: mana_type,
        source_id,
        snow: false,
        restrictions: Vec::new(),
    };

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == player_id)
        .expect("player exists");
    player.mana_pool.add(unit);

    events.push(GameEvent::ManaAdded {
        player_id,
        mana_type,
        source_id,
    });
}

pub fn can_pay(pool: &ManaPool, cost: &ManaCost) -> bool {
    match cost {
        ManaCost::NoCost => true,
        ManaCost::Cost { shards, generic } => {
            // Clone pool to simulate payment
            let mut sim = pool.clone();
            // Pay colored shards first
            for shard in shards {
                match shard_to_mana_type(*shard) {
                    ShardRequirement::Single(mt) => {
                        if sim.spend(mt).is_none() {
                            return false;
                        }
                    }
                    ShardRequirement::Hybrid(a, b) => {
                        if sim.spend(a).is_none() && sim.spend(b).is_none() {
                            return false;
                        }
                    }
                    ShardRequirement::Phyrexian(_) => {
                        // Phyrexian can always be paid (with life as fallback)
                    }
                    ShardRequirement::TwoGenericHybrid(color) => {
                        // Pay 1 colored or 2 generic
                        if sim.spend(color).is_none() {
                            if sim.total() < 2 {
                                return false;
                            }
                            // Spend any 2
                            spend_any(&mut sim);
                            spend_any(&mut sim);
                        }
                    }
                    ShardRequirement::Generic(n) => {
                        for _ in 0..n {
                            if !spend_any(&mut sim) {
                                return false;
                            }
                        }
                    }
                    ShardRequirement::Snow => {
                        if !spend_snow(&mut sim) {
                            return false;
                        }
                    }
                    ShardRequirement::X => {
                        // X can be 0, so always satisfiable
                    }
                    ShardRequirement::ColorlessHybrid(color) => {
                        if sim.spend(ManaType::Colorless).is_none() && sim.spend(color).is_none() {
                            return false;
                        }
                    }
                    ShardRequirement::HybridPhyrexian(_, _) => {
                        // Phyrexian hybrid can always be paid (life fallback)
                    }
                }
            }
            // Pay generic
            for _ in 0..*generic {
                if !spend_any(&mut sim) {
                    return false;
                }
            }
            true
        }
    }
}

pub fn pay_cost(
    pool: &mut ManaPool,
    cost: &ManaCost,
) -> Result<(Vec<ManaUnit>, Vec<LifePayment>), PaymentError> {
    match cost {
        ManaCost::NoCost => Ok((Vec::new(), Vec::new())),
        ManaCost::Cost { shards, generic } => {
            let mut spent = Vec::new();
            let mut life_payments = Vec::new();

            // (a) Pay colored shards first (exact color match)
            for shard in shards {
                match shard_to_mana_type(*shard) {
                    ShardRequirement::Single(mt) => {
                        let unit = pool.spend(mt).ok_or(PaymentError::InsufficientMana)?;
                        spent.push(unit);
                    }
                    ShardRequirement::Hybrid(a, b) => {
                        let color = auto_pay_hybrid(pool, a, b);
                        let unit = pool.spend(color).ok_or(PaymentError::InsufficientMana)?;
                        spent.push(unit);
                    }
                    ShardRequirement::Phyrexian(color) => {
                        if let Some(unit) = pool.spend(color) {
                            spent.push(unit);
                        } else {
                            life_payments.push(LifePayment {
                                player_id: PlayerId(0), // Caller should set correct player
                                amount: 2,
                            });
                        }
                    }
                    ShardRequirement::TwoGenericHybrid(color) => {
                        if let Some(unit) = pool.spend(color) {
                            spent.push(unit);
                        } else {
                            // Pay 2 generic
                            for _ in 0..2 {
                                let unit =
                                    spend_any_unit(pool).ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                        }
                    }
                    ShardRequirement::Generic(n) => {
                        for _ in 0..n {
                            let unit =
                                spend_any_unit(pool).ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        }
                    }
                    ShardRequirement::Snow => {
                        let unit = spend_snow_unit(pool).ok_or(PaymentError::InsufficientMana)?;
                        spent.push(unit);
                    }
                    ShardRequirement::X => {
                        // X=0 by default; caller specifies X value separately
                    }
                    ShardRequirement::ColorlessHybrid(color) => {
                        if let Some(unit) = pool.spend(ManaType::Colorless) {
                            spent.push(unit);
                        } else {
                            let unit = pool.spend(color).ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        }
                    }
                    ShardRequirement::HybridPhyrexian(a, b) => {
                        // Try to pay with mana first (prefer the more available color)
                        let color = auto_pay_hybrid(pool, a, b);
                        if let Some(unit) = pool.spend(color) {
                            spent.push(unit);
                        } else {
                            life_payments.push(LifePayment {
                                player_id: PlayerId(0),
                                amount: 2,
                            });
                        }
                    }
                }
            }

            // (d) Pay generic from any remaining mana (prefer colorless first, then least-available color)
            for _ in 0..*generic {
                let unit = spend_any_unit(pool).ok_or(PaymentError::InsufficientMana)?;
                spent.push(unit);
            }

            Ok((spent, life_payments))
        }
    }
}

/// For a hybrid shard like W/U, returns the color with more available mana in the pool.
fn auto_pay_hybrid(pool: &ManaPool, a: ManaType, b: ManaType) -> ManaType {
    let count_a = pool.count_color(a);
    let count_b = pool.count_color(b);
    if count_a >= count_b {
        a
    } else {
        b
    }
}

/// Determine mana type for a basic land subtype.
pub fn land_subtype_to_mana_type(subtype: &str) -> Option<ManaType> {
    match subtype {
        "Plains" => Some(ManaType::White),
        "Island" => Some(ManaType::Blue),
        "Swamp" => Some(ManaType::Black),
        "Mountain" => Some(ManaType::Red),
        "Forest" => Some(ManaType::Green),
        _ => None,
    }
}

// --- Internal helpers ---

enum ShardRequirement {
    Single(ManaType),
    Hybrid(ManaType, ManaType),
    Phyrexian(ManaType),
    TwoGenericHybrid(ManaType),
    Generic(u32),
    Snow,
    X,
    ColorlessHybrid(ManaType),
    HybridPhyrexian(ManaType, ManaType),
}

fn shard_to_mana_type(shard: ManaCostShard) -> ShardRequirement {
    match shard {
        ManaCostShard::White => ShardRequirement::Single(ManaType::White),
        ManaCostShard::Blue => ShardRequirement::Single(ManaType::Blue),
        ManaCostShard::Black => ShardRequirement::Single(ManaType::Black),
        ManaCostShard::Red => ShardRequirement::Single(ManaType::Red),
        ManaCostShard::Green => ShardRequirement::Single(ManaType::Green),
        ManaCostShard::Colorless => ShardRequirement::Single(ManaType::Colorless),
        ManaCostShard::Snow => ShardRequirement::Snow,
        ManaCostShard::X => ShardRequirement::X,
        ManaCostShard::WhiteBlue => ShardRequirement::Hybrid(ManaType::White, ManaType::Blue),
        ManaCostShard::WhiteBlack => ShardRequirement::Hybrid(ManaType::White, ManaType::Black),
        ManaCostShard::BlueBlack => ShardRequirement::Hybrid(ManaType::Blue, ManaType::Black),
        ManaCostShard::BlueRed => ShardRequirement::Hybrid(ManaType::Blue, ManaType::Red),
        ManaCostShard::BlackRed => ShardRequirement::Hybrid(ManaType::Black, ManaType::Red),
        ManaCostShard::BlackGreen => ShardRequirement::Hybrid(ManaType::Black, ManaType::Green),
        ManaCostShard::RedWhite => ShardRequirement::Hybrid(ManaType::Red, ManaType::White),
        ManaCostShard::RedGreen => ShardRequirement::Hybrid(ManaType::Red, ManaType::Green),
        ManaCostShard::GreenWhite => ShardRequirement::Hybrid(ManaType::Green, ManaType::White),
        ManaCostShard::GreenBlue => ShardRequirement::Hybrid(ManaType::Green, ManaType::Blue),
        ManaCostShard::TwoWhite => ShardRequirement::TwoGenericHybrid(ManaType::White),
        ManaCostShard::TwoBlue => ShardRequirement::TwoGenericHybrid(ManaType::Blue),
        ManaCostShard::TwoBlack => ShardRequirement::TwoGenericHybrid(ManaType::Black),
        ManaCostShard::TwoRed => ShardRequirement::TwoGenericHybrid(ManaType::Red),
        ManaCostShard::TwoGreen => ShardRequirement::TwoGenericHybrid(ManaType::Green),
        ManaCostShard::PhyrexianWhite => ShardRequirement::Phyrexian(ManaType::White),
        ManaCostShard::PhyrexianBlue => ShardRequirement::Phyrexian(ManaType::Blue),
        ManaCostShard::PhyrexianBlack => ShardRequirement::Phyrexian(ManaType::Black),
        ManaCostShard::PhyrexianRed => ShardRequirement::Phyrexian(ManaType::Red),
        ManaCostShard::PhyrexianGreen => ShardRequirement::Phyrexian(ManaType::Green),
        ManaCostShard::PhyrexianWhiteBlue => {
            ShardRequirement::HybridPhyrexian(ManaType::White, ManaType::Blue)
        }
        ManaCostShard::PhyrexianWhiteBlack => {
            ShardRequirement::HybridPhyrexian(ManaType::White, ManaType::Black)
        }
        ManaCostShard::PhyrexianBlueBlack => {
            ShardRequirement::HybridPhyrexian(ManaType::Blue, ManaType::Black)
        }
        ManaCostShard::PhyrexianBlueRed => {
            ShardRequirement::HybridPhyrexian(ManaType::Blue, ManaType::Red)
        }
        ManaCostShard::PhyrexianBlackRed => {
            ShardRequirement::HybridPhyrexian(ManaType::Black, ManaType::Red)
        }
        ManaCostShard::PhyrexianBlackGreen => {
            ShardRequirement::HybridPhyrexian(ManaType::Black, ManaType::Green)
        }
        ManaCostShard::PhyrexianRedWhite => {
            ShardRequirement::HybridPhyrexian(ManaType::Red, ManaType::White)
        }
        ManaCostShard::PhyrexianRedGreen => {
            ShardRequirement::HybridPhyrexian(ManaType::Red, ManaType::Green)
        }
        ManaCostShard::PhyrexianGreenWhite => {
            ShardRequirement::HybridPhyrexian(ManaType::Green, ManaType::White)
        }
        ManaCostShard::PhyrexianGreenBlue => {
            ShardRequirement::HybridPhyrexian(ManaType::Green, ManaType::Blue)
        }
        ManaCostShard::ColorlessWhite => ShardRequirement::ColorlessHybrid(ManaType::White),
        ManaCostShard::ColorlessBlue => ShardRequirement::ColorlessHybrid(ManaType::Blue),
        ManaCostShard::ColorlessBlack => ShardRequirement::ColorlessHybrid(ManaType::Black),
        ManaCostShard::ColorlessRed => ShardRequirement::ColorlessHybrid(ManaType::Red),
        ManaCostShard::ColorlessGreen => ShardRequirement::ColorlessHybrid(ManaType::Green),
    }
}

fn spend_any(pool: &mut ManaPool) -> bool {
    spend_any_unit(pool).is_some()
}

fn spend_any_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if pool.mana.is_empty() {
        return None;
    }

    // Prefer colorless first, then least-available color
    if let Some(unit) = pool.spend(ManaType::Colorless) {
        return Some(unit);
    }

    // Find the color with least available mana and spend it
    let colors = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ];

    let mut best: Option<(ManaType, usize)> = None;
    for &color in &colors {
        let count = pool.count_color(color);
        if count > 0 {
            match best {
                None => best = Some((color, count)),
                Some((_, best_count)) if count < best_count => best = Some((color, count)),
                _ => {}
            }
        }
    }

    best.and_then(|(color, _)| pool.spend(color))
}

fn spend_snow(pool: &mut ManaPool) -> bool {
    spend_snow_unit(pool).is_some()
}

fn spend_snow_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if let Some(pos) = pool.mana.iter().position(|m| m.snow) {
        Some(pool.mana.swap_remove(pos))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;

    fn make_unit(color: ManaType) -> ManaUnit {
        ManaUnit {
            color,
            source_id: ObjectId(1),
            snow: false,
            restrictions: Vec::new(),
        }
    }

    fn pool_with(units: &[(ManaType, usize)]) -> ManaPool {
        let mut pool = ManaPool::default();
        for (color, count) in units {
            for _ in 0..*count {
                pool.add(make_unit(*color));
            }
        }
        pool
    }

    // --- produce_mana tests ---

    #[test]
    fn produce_mana_adds_to_pool() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(1),
            ManaType::Green,
            PlayerId(0),
            &mut events,
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn produce_mana_emits_mana_added_event() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Blue,
            PlayerId(1),
            &mut events,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GameEvent::ManaAdded {
                player_id: PlayerId(1),
                mana_type: ManaType::Blue,
                source_id: ObjectId(5),
            }
        ));
    }

    // --- can_pay tests ---

    #[test]
    fn can_pay_no_cost() {
        let pool = ManaPool::default();
        assert!(can_pay(&pool, &ManaCost::NoCost));
    }

    #[test]
    fn can_pay_zero_cost() {
        let pool = ManaPool::default();
        assert!(can_pay(&pool, &ManaCost::zero()));
    }

    #[test]
    fn can_pay_single_colored() {
        let pool = pool_with(&[(ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_fails_wrong_color() {
        let pool = pool_with(&[(ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(!can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_generic_with_any_color() {
        let pool = pool_with(&[(ManaType::Green, 3)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_colored_plus_generic() {
        let pool = pool_with(&[(ManaType::Blue, 2), (ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_insufficient_colored() {
        let pool = pool_with(&[(ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        };
        assert!(!can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_hybrid_either_color() {
        let pool_w = pool_with(&[(ManaType::White, 1)]);
        let pool_u = pool_with(&[(ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        assert!(can_pay(&pool_w, &cost));
        assert!(can_pay(&pool_u, &cost));
    }

    #[test]
    fn can_pay_phyrexian_always_payable() {
        let pool = ManaPool::default(); // Empty pool
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianWhite],
            generic: 0,
        };
        assert!(can_pay(&pool, &cost));
    }

    // --- pay_cost tests ---

    #[test]
    fn pay_cost_colored_shards() {
        let mut pool = pool_with(&[(ManaType::White, 2), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 0,
        };
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert!(life.is_empty());
        assert_eq!(pool.total(), 1); // 1 white left
    }

    #[test]
    fn pay_cost_generic_from_any() {
        let mut pool = pool_with(&[(ManaType::Green, 3)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn pay_cost_hybrid_prefers_more_available() {
        // 3 white, 1 blue -- should prefer white for W/U hybrid
        let mut pool = pool_with(&[(ManaType::White, 3), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].color, ManaType::White);
    }

    #[test]
    fn pay_cost_phyrexian_with_color_available() {
        let mut pool = pool_with(&[(ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianRed],
            generic: 0,
        };
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1);
        assert!(life.is_empty());
    }

    #[test]
    fn pay_cost_phyrexian_pays_life_when_no_color() {
        let mut pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 0,
        };
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
        assert!(spent.is_empty());
        assert_eq!(life.len(), 1);
        assert_eq!(life[0].amount, 2);
    }

    #[test]
    fn pay_cost_insufficient_returns_error() {
        let mut pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(pay_cost(&mut pool, &cost).is_err());
    }

    #[test]
    fn pay_cost_generic_prefers_colorless() {
        let mut pool = pool_with(&[(ManaType::Colorless, 1), (ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent[0].color, ManaType::Colorless);
    }

    // --- land_subtype_to_mana_type tests ---

    #[test]
    fn land_subtypes_map_correctly() {
        assert_eq!(land_subtype_to_mana_type("Plains"), Some(ManaType::White));
        assert_eq!(land_subtype_to_mana_type("Island"), Some(ManaType::Blue));
        assert_eq!(land_subtype_to_mana_type("Swamp"), Some(ManaType::Black));
        assert_eq!(land_subtype_to_mana_type("Mountain"), Some(ManaType::Red));
        assert_eq!(land_subtype_to_mana_type("Forest"), Some(ManaType::Green));
        assert_eq!(land_subtype_to_mana_type("Desert"), None);
    }
}
