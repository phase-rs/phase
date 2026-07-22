//! Declarative CR scenario fixture schema (TOML).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Lifecycle status of a scenario fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioStatus {
    /// Auto-generated from CompRules; not yet executable.
    #[default]
    Skeleton,
    /// Fully wired setup/steps/assertions; runner will execute.
    Executable,
    /// Definitional / tournament / non-scenario-testable rule.
    NotApplicable,
    /// Intentionally deferred pending engine primitives.
    Deferred,
}

/// Top-level TOML fixture document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioFile {
    /// CR rule number, e.g. `"704.5a"`.
    pub rule: String,
    /// Three-digit section.
    pub section: u32,
    /// Short human title (usually the section title + rule number).
    #[serde(default)]
    pub title: String,
    /// Fixture lifecycle.
    #[serde(default)]
    pub status: ScenarioStatus,
    /// First-line CompRules text (or curated paraphrase).
    #[serde(default)]
    pub text: String,
    /// Optional notes for agents extending coverage.
    #[serde(default)]
    pub notes: String,
    /// Board/setup declaration (required for `executable`).
    #[serde(default)]
    pub setup: Option<SetupSpec>,
    /// Ordered steps to drive the engine.
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
    /// Post-condition assertions.
    #[serde(default)]
    pub assertions: Vec<AssertionSpec>,
    /// Extra metadata (tags, related cards, …).
    #[serde(default)]
    pub meta: IndexMap<String, String>,
}

/// Initial game configuration for an executable scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SetupSpec {
    /// Starting phase (serde name of `engine::types::phase::Phase`).
    #[serde(default = "default_phase")]
    pub phase: String,
    /// Per-player starting state.
    #[serde(default)]
    pub players: Vec<PlayerSetup>,
    /// Battlefield creatures.
    #[serde(default)]
    pub creatures: Vec<CreatureSpec>,
    /// Optional RNG seed (defaults to 42).
    #[serde(default)]
    pub seed: Option<u64>,
}

fn default_phase() -> String {
    "PreCombatMain".to_string()
}

/// Per-player setup fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerSetup {
    pub id: u8,
    #[serde(default = "default_life")]
    pub life: i32,
    /// Cards to put into hand by name (vanilla placeholders).
    #[serde(default)]
    pub hand: Vec<String>,
    /// Library top cards (first entry is top).
    #[serde(default)]
    pub library_top: Vec<String>,
}

fn default_life() -> i32 {
    20
}

/// A creature placed on the battlefield during setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatureSpec {
    /// Local handle referenced by steps/assertions (`"bear"`).
    pub id: String,
    pub player: u8,
    #[serde(default = "default_creature_name")]
    pub name: String,
    pub power: i32,
    pub toughness: i32,
    /// Damage already marked on the creature before steps run.
    #[serde(default)]
    pub damage_marked: u32,
    /// Keyword names to grant (e.g. `"Flying"`).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// If true, creature enters with summoning sickness.
    #[serde(default)]
    pub summoning_sickness: bool,
}

fn default_creature_name() -> String {
    "Creature".to_string()
}

/// A single runner step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ScenarioStep {
    /// Pass priority once for the current player.
    PassPriority,
    /// Pass priority for both players (AP then NAP).
    PassBoth,
    /// Deal a fixed amount of damage to a named creature (direct mark + SBA check via pass).
    MarkDamage { creature: String, amount: u32 },
    /// Set a player's life total.
    SetLife { player: u8, life: i32 },
    /// Deal damage to a player by reducing life (simulates resolved damage effect).
    DamagePlayer { player: u8, amount: i32 },
    /// Advance until the stack is empty (no-op if empty).
    AdvanceUntilStackEmpty,
    /// Check state-based actions by passing priority once.
    CheckSbas,
}

/// Post-condition assertion kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssertionSpec {
    /// Player life equals `life`.
    PlayerLife { player: u8, life: i32 },
    /// Named creature is in the given zone (`Battlefield`, `Graveyard`, …).
    CreatureZone { creature: String, zone: String },
    /// Named creature still exists on the battlefield.
    CreatureOnBattlefield { creature: String },
    /// Named creature is in its controller's graveyard.
    CreatureInGraveyard { creature: String },
    /// Game is over; optional winner player id.
    GameOver {
        #[serde(default)]
        winner: Option<u8>,
    },
    /// Game is not over.
    GameNotOver,
    /// Current phase matches the serde name.
    PhaseIs { phase: String },
    /// Player has at least `count` cards in hand.
    HandCountAtLeast { player: u8, count: usize },
    /// Player hand size equals `count`.
    HandCountEquals { player: u8, count: usize },
    /// Named creature's marked damage equals `damage`.
    CreatureDamage { creature: String, damage: u32 },
}

impl ScenarioFile {
    /// Build a skeleton fixture from a CompRules entry.
    pub fn skeleton(rule: &str, section: u32, text: &str, section_title: &str) -> Self {
        Self {
            rule: rule.to_string(),
            section,
            title: format!("{section_title} — CR {rule}"),
            status: ScenarioStatus::Skeleton,
            text: text.to_string(),
            notes: String::new(),
            setup: None,
            steps: Vec::new(),
            assertions: Vec::new(),
            meta: IndexMap::new(),
        }
    }

    pub fn is_runnable(&self) -> bool {
        self.status == ScenarioStatus::Executable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_executable_fixture() {
        let scenario = ScenarioFile {
            rule: "704.5a".into(),
            section: 704,
            title: "SBA life loss".into(),
            status: ScenarioStatus::Executable,
            text: "A player with 0 or less life loses the game.".into(),
            notes: String::new(),
            setup: Some(SetupSpec {
                phase: "PreCombatMain".into(),
                players: vec![
                    PlayerSetup {
                        id: 0,
                        life: 20,
                        hand: vec![],
                        library_top: vec![],
                    },
                    PlayerSetup {
                        id: 1,
                        life: 0,
                        hand: vec![],
                        library_top: vec![],
                    },
                ],
                creatures: vec![],
                seed: Some(42),
            }),
            steps: vec![ScenarioStep::CheckSbas],
            assertions: vec![AssertionSpec::GameOver { winner: Some(0) }],
            meta: IndexMap::new(),
        };

        let toml_str = toml::to_string_pretty(&scenario).unwrap();
        let back: ScenarioFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.rule, "704.5a");
        assert!(back.is_runnable());
        assert_eq!(back.assertions.len(), 1);
    }
}
