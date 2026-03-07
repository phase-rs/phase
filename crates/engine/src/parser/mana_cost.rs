use std::str::FromStr;

use crate::types::mana::{ManaCost, ManaCostShard};

use super::ParseError;

pub fn parse(input: &str) -> Result<ManaCost, ParseError> {
    if input == "no cost" {
        return Ok(ManaCost::NoCost);
    }

    let mut shards = Vec::new();
    let mut generic: u32 = 0;

    for token in input.split_whitespace() {
        if let Ok(n) = token.parse::<u32>() {
            generic += n;
        } else {
            let shard = ManaCostShard::from_str(token)
                .map_err(|_| ParseError::InvalidManaCostShard(token.to_string()))?;
            shards.push(shard);
        }
    }

    Ok(ManaCost::Cost { shards, generic })
}

#[cfg(test)]
mod tests {
    use crate::types::mana::ManaCostShard;

    use super::*;

    #[test]
    fn parse_single_white() {
        let cost = parse("W").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_generic_and_colored() {
        let cost = parse("2 W U").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Blue],
                generic: 2,
            }
        );
    }

    #[test]
    fn parse_x_cost() {
        let cost = parse("X R R").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_hybrid() {
        let cost = parse("W/U").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::WhiteBlue],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_phyrexian() {
        let cost = parse("W/P").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::PhyrexianWhite],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_hybrid_phyrexian() {
        let cost = parse("B/G/P").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::PhyrexianBlackGreen],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_two_generic_hybrid() {
        let cost = parse("2/W").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::TwoWhite],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_snow() {
        let cost = parse("S").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Snow],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_colorless() {
        let cost = parse("C").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Colorless],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_no_cost() {
        let cost = parse("no cost").unwrap();
        assert_eq!(cost, ManaCost::NoCost);
    }

    #[test]
    fn parse_empty_is_zero_mana() {
        let cost = parse("").unwrap();
        assert_eq!(cost, ManaCost::zero());
    }

    #[test]
    fn parse_colorless_hybrid() {
        let cost = parse("C/W").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::ColorlessWhite],
                generic: 0,
            }
        );
    }

    #[test]
    fn parse_complex_cost() {
        let cost = parse("3 W W U/B").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                shards: vec![
                    ManaCostShard::White,
                    ManaCostShard::White,
                    ManaCostShard::BlueBlack,
                ],
                generic: 3,
            }
        );
    }
}
