use crate::types::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};

use super::ParseError;

pub fn parse_ability(_raw: &str) -> Result<AbilityDefinition, ParseError> {
    todo!("ability parser not yet implemented")
}

pub fn parse_trigger(_raw: &str) -> Result<TriggerDefinition, ParseError> {
    todo!("trigger parser not yet implemented")
}

pub fn parse_static(_raw: &str) -> Result<StaticDefinition, ParseError> {
    todo!("static parser not yet implemented")
}

pub fn parse_replacement(_raw: &str) -> Result<ReplacementDefinition, ParseError> {
    todo!("replacement parser not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests will be added in Task 2
}
