use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaPool {
    pub white: u32,
    pub blue: u32,
    pub black: u32,
    pub red: u32,
    pub green: u32,
    pub colorless: u32,
}

impl ManaPool {
    pub fn add(&mut self, color: ManaColor, amount: u32) {
        match color {
            ManaColor::White => self.white += amount,
            ManaColor::Blue => self.blue += amount,
            ManaColor::Black => self.black += amount,
            ManaColor::Red => self.red += amount,
            ManaColor::Green => self.green += amount,
        }
    }

    pub fn total(&self) -> u32 {
        self.white + self.blue + self.black + self.red + self.green + self.colorless
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mana_color_serializes_as_string() {
        let color = ManaColor::White;
        let json = serde_json::to_value(color).unwrap();
        assert_eq!(json, "White");
    }

    #[test]
    fn all_mana_colors_serialize() {
        let colors = [
            (ManaColor::White, "White"),
            (ManaColor::Blue, "Blue"),
            (ManaColor::Black, "Black"),
            (ManaColor::Red, "Red"),
            (ManaColor::Green, "Green"),
        ];
        for (color, expected) in colors {
            let json = serde_json::to_value(color).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn mana_pool_default_is_empty() {
        let pool = ManaPool::default();
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn mana_pool_add_increases_correct_color() {
        let mut pool = ManaPool::default();
        pool.add(ManaColor::Blue, 3);
        assert_eq!(pool.blue, 3);
        assert_eq!(pool.total(), 3);
    }

    #[test]
    fn mana_pool_add_multiple_colors() {
        let mut pool = ManaPool::default();
        pool.add(ManaColor::White, 2);
        pool.add(ManaColor::Red, 1);
        pool.add(ManaColor::Green, 3);
        assert_eq!(pool.total(), 6);
        assert_eq!(pool.white, 2);
        assert_eq!(pool.red, 1);
        assert_eq!(pool.green, 3);
    }

    #[test]
    fn mana_pool_total_includes_colorless() {
        let pool = ManaPool {
            colorless: 5,
            ..Default::default()
        };
        assert_eq!(pool.total(), 5);
    }
}
