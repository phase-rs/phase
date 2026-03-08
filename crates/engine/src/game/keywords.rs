use std::str::FromStr;

use crate::game::game_object::GameObject;
use crate::types::keywords::Keyword;

/// Check if a game object has a specific keyword, using discriminant-based matching
/// for simple keywords (ignoring associated data for parameterized variants).
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    obj.keywords
        .iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}

/// Convenience: check for Flying.
pub fn has_flying(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flying)
}

/// Convenience: check for Haste.
pub fn has_haste(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Haste)
}

/// Convenience: check for Flash.
pub fn has_flash(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flash)
}

/// Convenience: check for Hexproof.
pub fn has_hexproof(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Hexproof)
}

/// Convenience: check for Shroud.
pub fn has_shroud(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Shroud)
}

/// Convenience: check for Indestructible.
pub fn has_indestructible(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Indestructible)
}

/// Batch parse keyword strings into typed Keyword values.
/// Used when creating GameObjects from parsed card data.
pub fn parse_keywords(keyword_strings: &[String]) -> Vec<Keyword> {
    keyword_strings
        .iter()
        .map(|s| Keyword::from_str(s).unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn has_keyword_simple_match() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        assert!(has_keyword(&obj, &Keyword::Flying));
        assert!(!has_keyword(&obj, &Keyword::Haste));
    }

    #[test]
    fn has_keyword_discriminant_matching() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Kicker("1G".to_string()));
        // Discriminant match -- doesn't care about the param value
        assert!(has_keyword(&obj, &Keyword::Kicker("X".to_string())));
        assert!(!has_keyword(&obj, &Keyword::Cycling("2".to_string())));
    }

    #[test]
    fn convenience_functions() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        obj.keywords.push(Keyword::Flash);
        obj.keywords.push(Keyword::Hexproof);
        obj.keywords.push(Keyword::Shroud);
        obj.keywords.push(Keyword::Indestructible);

        assert!(has_flying(&obj));
        assert!(has_haste(&obj));
        assert!(has_flash(&obj));
        assert!(has_hexproof(&obj));
        assert!(has_shroud(&obj));
        assert!(has_indestructible(&obj));
    }

    #[test]
    fn parse_keywords_known() {
        let strings = vec!["Flying".to_string(), "Haste".to_string(), "Deathtouch".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed, vec![Keyword::Flying, Keyword::Haste, Keyword::Deathtouch]);
    }

    #[test]
    fn parse_keywords_parameterized() {
        let strings = vec!["Kicker:1G".to_string(), "Ward:2".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed[0], Keyword::Kicker("1G".to_string()));
        assert_eq!(parsed[1], Keyword::Ward("2".to_string()));
    }

    #[test]
    fn parse_keywords_unknown() {
        let strings = vec!["NotReal".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed[0], Keyword::Unknown("NotReal".to_string()));
    }

    #[test]
    fn has_keyword_method_on_game_object() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Indestructible);
        assert!(obj.has_keyword(&Keyword::Indestructible));
        assert!(!obj.has_keyword(&Keyword::Flying));
    }
}
