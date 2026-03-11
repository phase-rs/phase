use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Zone {
    Library,
    Hand,
    Battlefield,
    Graveyard,
    Stack,
    Exile,
    Command,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_has_all_seven_mtg_zones() {
        let zones = [
            Zone::Library,
            Zone::Hand,
            Zone::Battlefield,
            Zone::Graveyard,
            Zone::Stack,
            Zone::Exile,
            Zone::Command,
        ];
        assert_eq!(zones.len(), 7);
    }

    #[test]
    fn zone_serializes_as_string() {
        let zone = Zone::Battlefield;
        let json = serde_json::to_value(zone).unwrap();
        assert_eq!(json, "Battlefield");
    }

    #[test]
    fn zone_roundtrips() {
        let zone = Zone::Graveyard;
        let serialized = serde_json::to_string(&zone).unwrap();
        let deserialized: Zone = serde_json::from_str(&serialized).unwrap();
        assert_eq!(zone, deserialized);
    }
}
