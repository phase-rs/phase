use std::collections::HashMap;

/// Intermediate AST for a single Forge card face.
///
/// This struct captures the raw parsed content of a Forge `.txt` card script
/// (or one side of a DFC split by `ALTERNATE`). Translation into phase.rs
/// types happens in `translate.rs`.
#[derive(Debug, Clone)]
pub(crate) struct ForgeCard {
    pub name: String,
    pub mana_cost: Option<String>,
    pub types: Option<String>,
    pub pt: Option<String>,
    pub colors: Option<String>,
    /// `A:` lines — spell/activated abilities.
    pub abilities: Vec<ForgeAbilityLine>,
    /// `T:` lines — trigger definitions.
    pub triggers: Vec<ForgeAbilityLine>,
    /// `S:` lines — static abilities.
    pub statics: Vec<ForgeAbilityLine>,
    /// `R:` lines — replacement effects.
    pub replacements: Vec<ForgeAbilityLine>,
    /// `K:` lines — keywords.
    pub keywords: Vec<String>,
    /// `SVar:` declarations — name → value.
    pub svars: HashMap<String, String>,
    /// Oracle text (for diagnostics only — not used in translation).
    pub oracle_text: Option<String>,
    /// Back face for DFC cards (parsed from content after `ALTERNATE`).
    /// Used by `ForgeIndex::parse_card()` to select the correct face.
    #[allow(dead_code)]
    pub alternate: Option<Box<ForgeCard>>,
}

/// A single Forge ability/trigger/static/replacement line, parsed into
/// pipe-delimited segments of `Key$ Value` pairs.
#[derive(Debug, Clone)]
pub(crate) struct ForgeAbilityLine {
    /// The raw line text (for error messages and graceful degradation).
    pub raw: String,
    /// Parsed parameters from the `Key$ Value | Key$ Value` segments.
    pub params: ForgeParams,
}

/// Key-value parameter bag parsed from a Forge ability line.
///
/// Forge uses `Key$ Value` notation separated by ` | `. This struct provides
/// typed accessors for common lookups.
#[derive(Debug, Clone, Default)]
pub(crate) struct ForgeParams {
    entries: HashMap<String, String>,
}

impl ForgeParams {
    pub fn new(entries: HashMap<String, String>) -> Self {
        Self { entries }
    }

    /// Get a string value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    /// Check if a key is present (regardless of value).
    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Get the effect type (the `SP$` or `DB$` prefix value).
    pub fn effect_type(&self) -> Option<&str> {
        self.get("SP").or_else(|| self.get("DB"))
    }
}

/// Errors that can occur during Forge → phase.rs translation.
#[derive(Debug, Clone)]
pub(crate) enum ForgeTranslateError {
    /// A Forge effect type we don't yet translate.
    UnsupportedEffect(String),
    /// A required parameter was missing.
    MissingParam { param: String, context: String },
    /// A filter string we couldn't parse.
    #[allow(dead_code)]
    UnparsableFilter(String),
    /// A Cost$ token we don't recognize.
    UnparsableCost(String),
    /// SVar references form a cycle.
    CyclicSvar(String),
    /// An SVar was referenced but not defined.
    MissingSvar(String),
    /// A trigger mode we don't yet translate.
    UnsupportedTriggerMode(String),
    /// A static mode we don't yet translate.
    UnsupportedStaticMode(String),
    /// A replacement event we don't yet translate.
    UnsupportedReplacementEvent(String),
    /// Generic translation failure.
    Other(String),
}

impl std::fmt::Display for ForgeTranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedEffect(e) => write!(f, "unsupported effect: {e}"),
            Self::MissingParam { param, context } => {
                write!(f, "missing param '{param}' in {context}")
            }
            Self::UnparsableFilter(s) => write!(f, "unparsable filter: {s}"),
            Self::UnparsableCost(s) => write!(f, "unparsable cost: {s}"),
            Self::CyclicSvar(s) => write!(f, "cyclic SVar: {s}"),
            Self::MissingSvar(s) => write!(f, "missing SVar: {s}"),
            Self::UnsupportedTriggerMode(m) => write!(f, "unsupported trigger mode: {m}"),
            Self::UnsupportedStaticMode(m) => write!(f, "unsupported static mode: {m}"),
            Self::UnsupportedReplacementEvent(e) => {
                write!(f, "unsupported replacement event: {e}")
            }
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}
