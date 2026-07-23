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
    /// Lightning Bolts placed into hand via the scenario harness
    /// (`GameScenario::add_bolt_to_hand` — production `Effect::DealDamage`).
    #[serde(default)]
    pub lightning_bolts: Vec<LightningBoltSpec>,
    /// Optional RNG seed (defaults to 42).
    #[serde(default)]
    pub seed: Option<u64>,
}

fn default_phase() -> String {
    "PreCombatMain".to_string()
}

/// A Lightning Bolt in hand, addressable by handle for cast steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightningBoltSpec {
    /// Local handle referenced by cast steps (`"bolt"`).
    pub id: String,
    pub player: u8,
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
///
/// Steps must drive production engine APIs (`GameAction` / `GameRunner` helpers).
/// Direct `GameState` field writes are not allowed as scenario steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ScenarioStep {
    /// Pass priority once for the current player.
    PassPriority,
    /// Pass priority for both players (AP then NAP).
    PassBoth,
    /// Cast a Lightning Bolt from hand through the casting pipeline
    /// (`GameAction::CastSpell` + target selection). Does not resolve.
    CastLightningBolt {
        spell: String,
        #[serde(default)]
        target_player: Option<u8>,
        #[serde(default)]
        target_creature: Option<String>,
    },
    /// Resolve the top stack object (`GameRunner::resolve_top`).
    ResolveTop,
    /// Advance until the stack is empty.
    AdvanceUntilStackEmpty,
    /// Check state-based actions by passing priority once (CR 704.3).
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
    /// The stack is empty (CR 405 / CR 608).
    StackIsEmpty,
    /// The player who currently holds priority (CR 117).
    PriorityPlayer { player: u8 },
    /// Player's library size equals `count` (CR 401).
    LibraryCountEquals { player: u8, count: usize },
    /// Player's poison counter total equals `count` (CR 122 / CR 704.5c).
    PlayerPoison { player: u8, count: u32 },
    /// Named creature is a declared attacker this combat (CR 508).
    AttackerDeclared { creature: String },
    /// Named creature is a declared blocker this combat (CR 509).
    BlockerDeclared { creature: String },
    /// Named creature has the given keyword after CR 613 layers (CR 702).
    CreatureHasKeyword { creature: String, keyword: String },
    /// Named object handle is in the command zone (CR 408).
    InCommandZone { handle: String },
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
                        life: 3,
                        hand: vec![],
                        library_top: vec![],
                    },
                ],
                creatures: vec![],
                lightning_bolts: vec![LightningBoltSpec {
                    id: "bolt".into(),
                    player: 0,
                }],
                seed: Some(42),
            }),
            steps: vec![
                ScenarioStep::CastLightningBolt {
                    spell: "bolt".into(),
                    target_player: Some(1),
                    target_creature: None,
                },
                ScenarioStep::ResolveTop,
            ],
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
