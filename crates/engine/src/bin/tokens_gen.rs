//! tokens-gen — one-shot translator that reads an mtgish token catalog dump
//! and emits `crates/engine/data/known-tokens.toml`, the phase-native canonical
//! source of debug-spawnable token presets.
//!
//! Lifecycle: the generated TOML is committed and authoritative going forward.
//! Hand-edits to the TOML are preserved across regenerations *only* by
//! diffing; rerunning `tokens-gen` with a refreshed mtgish dump will overwrite
//! the file. Inspect the diff before committing.
//!
//! Usage:
//!     cargo run --bin tokens-gen -- \
//!         --input data/scratch/mtgish-tokens.txt \
//!         --output crates/engine/data/known-tokens.toml

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use engine::game::token_presets::{
    PredefinedTokenKind, PresetFidelity, TokenCategory, TokenPreset,
};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::proposed_event::TokenCharacteristics;

// ──────────────────────────────────────────────────────────────────────────
// Mtgish Python-dict value parser
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Value {
    Str(String),
    Int(i64),
    List(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn as_int(&self) -> Option<i64> {
        if let Value::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }
    fn as_list(&self) -> Option<&[Value]> {
        if let Value::List(v) = self {
            Some(v)
        } else {
            None
        }
    }
    fn as_dict(&self) -> Option<&[(String, Value)]> {
        if let Value::Dict(v) = self {
            Some(v)
        } else {
            None
        }
    }
    fn dict_get(&self, key: &str) -> Option<&Value> {
        self.as_dict()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            src: s.as_bytes(),
            pos: 0,
        }
    }
    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }
    fn expect(&mut self, c: u8) -> Result<(), String> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "expected {:?} at pos {} (got {:?})",
                c as char,
                self.pos,
                self.peek().map(|b| b as char)
            ))
        }
    }
    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        let c = self.peek().ok_or_else(|| "unexpected EOF".to_string())?;
        match c {
            b'\'' | b'"' => self.parse_string().map(Value::Str),
            b'[' => self.parse_list(),
            b'{' => self.parse_dict(),
            b'-' | b'0'..=b'9' => self.parse_int().map(Value::Int),
            _ => Err(format!(
                "unexpected char {:?} at pos {}",
                c as char, self.pos
            )),
        }
    }
    fn parse_string(&mut self) -> Result<String, String> {
        let quote = self.src[self.pos];
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != quote {
            // Mtgish has no escape sequences in observed dumps; treat content verbatim.
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Err("unterminated string".to_string());
        }
        let s = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|e| e.to_string())?
            .to_string();
        self.pos += 1;
        Ok(s)
    }
    fn parse_int(&mut self) -> Result<i64, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())
    }
    fn parse_list(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::List(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                other => {
                    return Err(format!(
                        "expected ',' or ']' in list at pos {} (got {:?})",
                        self.pos, other
                    ))
                }
            }
        }
    }
    fn parse_dict(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Dict(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Dict(entries));
                }
                other => {
                    return Err(format!(
                        "expected ',' or '}}' in dict at pos {} (got {:?})",
                        self.pos, other
                    ))
                }
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Classification
// ──────────────────────────────────────────────────────────────────────────

/// Translate an mtgish color-list dict to an engine `Vec<ManaColor>` in WUBRG order.
fn extract_colors(v: &Value) -> Result<Vec<ManaColor>, String> {
    let kind = v
        .dict_get("_TokenColorList")
        .and_then(Value::as_str)
        .ok_or("missing _TokenColorList kind")?;
    match kind {
        "Colorless" => Ok(Vec::new()),
        "AllColors" => Ok(vec![
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ]),
        "Colors" => {
            let args = v
                .dict_get("args")
                .and_then(Value::as_list)
                .ok_or("Colors missing args")?;
            let mut out = Vec::new();
            for item in args {
                let s = item.as_str().ok_or("color arg not a string")?;
                let c = match s {
                    "White" => ManaColor::White,
                    "Blue" => ManaColor::Blue,
                    "Black" => ManaColor::Black,
                    "Red" => ManaColor::Red,
                    "Green" => ManaColor::Green,
                    other => return Err(format!("unknown color: {}", other)),
                };
                if !out.contains(&c) {
                    out.push(c);
                }
            }
            // CR 105.2c: Canonical color ordering (WUBRG).
            out.sort_by_key(|c| match c {
                ManaColor::White => 0,
                ManaColor::Blue => 1,
                ManaColor::Black => 2,
                ManaColor::Red => 3,
                ManaColor::Green => 4,
            });
            Ok(out)
        }
        // "TheChosenColor" cannot be statically resolved — caller drops entry.
        "TheChosenColor" => Err("color depends on runtime choice".to_string()),
        other => Err(format!("unknown _TokenColorList kind: {}", other)),
    }
}

fn extract_supertype(s: &str) -> Result<Supertype, String> {
    s.parse::<Supertype>()
        .map_err(|_| format!("unknown supertype: {}", s))
}

fn extract_core_types(items: &[Value]) -> Result<Vec<CoreType>, String> {
    items
        .iter()
        .map(|v| {
            let s = v.as_str().ok_or("core_type not a string")?;
            // CoreType doesn't have FromStr in engine; mirror common variants.
            match s {
                "Creature" => Ok(CoreType::Creature),
                "Artifact" => Ok(CoreType::Artifact),
                "Enchantment" => Ok(CoreType::Enchantment),
                "Land" => Ok(CoreType::Land),
                "Planeswalker" => Ok(CoreType::Planeswalker),
                "Battle" => Ok(CoreType::Battle),
                other => Err(format!("unknown core_type: {}", other)),
            }
        })
        .collect()
}

/// Extract simple keyword-only rules from an mtgish rules list. Returns
/// `(keywords, has_complex)` where `has_complex` is true if any non-keyword
/// rule (TriggerA, Activated, PermanentLayerEffect, Equip, etc.) is present.
fn extract_keywords_and_complexity(rules: &[Value]) -> (Vec<Keyword>, bool) {
    let mut kws = Vec::new();
    let mut complex = false;
    for rule in rules {
        let Some(d) = rule.as_dict() else {
            complex = true;
            continue;
        };
        let kind = d
            .iter()
            .find(|(k, _)| k == "_Rule")
            .and_then(|(_, v)| v.as_str());
        let has_args = d.iter().any(|(k, _)| k == "args");
        match (kind, has_args) {
            (Some(name), false) => {
                if let Some(kw) = mtgish_keyword_to_engine(name) {
                    if !kws.contains(&kw) {
                        kws.push(kw);
                    }
                } else {
                    complex = true;
                }
            }
            _ => complex = true,
        }
    }
    (kws, complex)
}

/// Map an mtgish bare-keyword rule name to an engine `Keyword`. Only the
/// non-parameterized keywords are handled here; parameterized variants
/// (Ward N, Annihilator N, Protection-From-X, etc.) carry args and therefore
/// fall through to `complex=true`.
fn mtgish_keyword_to_engine(name: &str) -> Option<Keyword> {
    Some(match name {
        "Flying" => Keyword::Flying,
        "FirstStrike" => Keyword::FirstStrike,
        "DoubleStrike" => Keyword::DoubleStrike,
        "Trample" => Keyword::Trample,
        "Vigilance" => Keyword::Vigilance,
        "Haste" => Keyword::Haste,
        "Deathtouch" => Keyword::Deathtouch,
        "Lifelink" => Keyword::Lifelink,
        "Reach" => Keyword::Reach,
        "Defender" => Keyword::Defender,
        "Menace" => Keyword::Menace,
        "Intimidate" => Keyword::Intimidate,
        "Shroud" => Keyword::Shroud,
        "Hexproof" => Keyword::Hexproof,
        "Indestructible" => Keyword::Indestructible,
        "Flash" => Keyword::Flash,
        "Fear" => Keyword::Fear,
        "Skulk" => Keyword::Skulk,
        "Decayed" => Keyword::Decayed,
        // Landwalk variants ("Islandwalk", "Mountainwalk", etc.) are
        // parameterized in the engine (`Keyword::Landwalk(String)`) — not
        // simple bare keywords. Translate them with the basic-land name.
        "Plainswalk" => Keyword::Landwalk("Plains".to_string()),
        "Islandwalk" => Keyword::Landwalk("Island".to_string()),
        "Swampwalk" => Keyword::Landwalk("Swamp".to_string()),
        "Mountainwalk" => Keyword::Landwalk("Mountain".to_string()),
        "Forestwalk" => Keyword::Landwalk("Forest".to_string()),
        _ => return None,
    })
}

fn classify_category(types: &[CoreType], subtypes: &[String]) -> Result<TokenCategory, String> {
    // Predefined-ability artifact tokens: keyed by subtype, abilities
    // attached at runtime by `predefined_token_abilities`. Restricted to
    // artifact subtypes — Eldrazi Spawn (also predefined) is a Creature
    // and falls through to TokenCategory::Creature below.
    let has_creature = types.contains(&CoreType::Creature);
    let predefined: Vec<PredefinedTokenKind> = if has_creature {
        Vec::new()
    } else {
        subtypes
            .iter()
            .filter_map(|s| match s.as_str() {
                "Treasure" => Some(PredefinedTokenKind::Treasure),
                "Food" => Some(PredefinedTokenKind::Food),
                "Gold" => Some(PredefinedTokenKind::Gold),
                "Clue" => Some(PredefinedTokenKind::Clue),
                "Blood" => Some(PredefinedTokenKind::Blood),
                "Powerstone" => Some(PredefinedTokenKind::Powerstone),
                "Map" => Some(PredefinedTokenKind::Map),
                _ => None,
            })
            .collect()
    };
    if let [kind] = predefined.as_slice() {
        return Ok(TokenCategory::PredefinedArtifact { kind: kind.clone() });
    }
    // CR 110.4: Permanent type dispatch.
    let has_artifact = types.contains(&CoreType::Artifact);
    let has_enchantment = types.contains(&CoreType::Enchantment);
    let has_land = types.contains(&CoreType::Land);
    let aura = subtypes.iter().any(|s| s == "Aura");
    let equipment = subtypes.iter().any(|s| s == "Equipment");
    let vehicle = subtypes.iter().any(|s| s == "Vehicle");
    if has_creature {
        return Ok(TokenCategory::Creature);
    }
    if aura {
        return Ok(TokenCategory::Aura);
    }
    if equipment && has_artifact {
        return Ok(TokenCategory::Equipment);
    }
    if vehicle && has_artifact {
        return Ok(TokenCategory::Vehicle);
    }
    if has_enchantment {
        return Ok(TokenCategory::Enchantment);
    }
    if has_land {
        return Ok(TokenCategory::Land);
    }
    if has_artifact {
        return Ok(TokenCategory::Artifact);
    }
    Err(format!(
        "unclassifiable: types={:?} subtypes={:?}",
        types, subtypes
    ))
}

// ──────────────────────────────────────────────────────────────────────────
// Mtgish entry → TokenPreset
// ──────────────────────────────────────────────────────────────────────────

/// Parsed shape of one mtgish `_CreatableToken` entry, normalized to a
/// uniform record. Each `_CreatableToken` kind has a different positional
/// `args` order, so the kind-specific extractor populates these fields.
#[derive(Debug)]
struct Entry {
    #[allow(dead_code)] // Retained for debug output on parse failures.
    kind: String,
    name: Option<String>,
    supertypes: Vec<Supertype>,
    core_types: Vec<CoreType>,
    subtypes: Vec<String>,
    colors: Vec<ManaColor>,
    power: Option<i32>,
    toughness: Option<i32>,
    rules: Vec<Value>,
}

fn extract_subtypes(v: &Value) -> Result<Vec<String>, String> {
    // Plain list (e.g., from ArtifactToken's subtype slot) or a wrapped
    // `_CreatureTokenSubtypes` dict (from CreatureToken).
    if let Some(items) = v.as_list() {
        return items
            .iter()
            .map(|s| {
                s.as_str()
                    .map(|s| s.to_string())
                    .ok_or("subtype not str".to_string())
            })
            .collect();
    }
    if let Some(d) = v.as_dict() {
        if d.iter().any(|(k, _)| k == "_CreatureTokenSubtypes") {
            let args = v
                .dict_get("args")
                .and_then(Value::as_list)
                .ok_or("_CreatureTokenSubtypes missing args")?;
            return args
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(|s| s.to_string())
                        .ok_or("subtype not str".to_string())
                })
                .collect();
        }
    }
    Err("unrecognized subtypes shape".to_string())
}

fn extract_pt(v: &Value) -> Result<(Option<i32>, Option<i32>), String> {
    let kind = v
        .dict_get("_PT")
        .and_then(Value::as_str)
        .ok_or("missing _PT kind")?;
    match kind {
        "PT" => {
            let args = v
                .dict_get("args")
                .and_then(Value::as_list)
                .ok_or("PT missing args")?;
            if args.len() != 2 {
                return Err(format!("PT args wrong length: {}", args.len()));
            }
            let p = args[0].as_int().ok_or("P not int")? as i32;
            let t = args[1].as_int().ok_or("T not int")? as i32;
            Ok((Some(p), Some(t)))
        }
        "ZeroPT" => Ok((Some(0), Some(0))),
        // Effect-bound P/T — skip these from the catalog (caller drops).
        "ManualPT" | "PTOfExiledCard" | "PTOfGraveyardCard" | "PTOfPermanent" | "PTX" => {
            Err("effect-bound P/T not viable as catalog preset".to_string())
        }
        other => Err(format!("unknown _PT kind: {}", other)),
    }
}

fn parse_supertypes_arg(v: &Value) -> Result<Vec<Supertype>, String> {
    let items = v.as_list().ok_or("supertypes arg not a list")?;
    items
        .iter()
        .map(|s| {
            let s = s.as_str().ok_or("supertype not str")?;
            extract_supertype(s)
        })
        .collect()
}

fn parse_entry(v: &Value) -> Result<Entry, String> {
    let kind = v
        .dict_get("_CreatableToken")
        .and_then(Value::as_str)
        .ok_or("missing _CreatableToken")?
        .to_string();
    let args = v
        .dict_get("args")
        .and_then(Value::as_list)
        .ok_or("missing args")?;
    let mut e = Entry {
        kind: kind.clone(),
        name: None,
        supertypes: Vec::new(),
        core_types: Vec::new(),
        subtypes: Vec::new(),
        colors: Vec::new(),
        power: None,
        toughness: None,
        rules: Vec::new(),
    };
    match kind.as_str() {
        // CreatureToken: [pt, types_list, colors, subtypes_wrapper]
        "CreatureToken" => {
            let (p, t) = extract_pt(&args[0])?;
            e.power = p;
            e.toughness = t;
            e.core_types = extract_core_types(args[1].as_list().ok_or("types not list")?)?;
            e.colors = extract_colors(&args[2])?;
            e.subtypes = extract_subtypes(&args[3])?;
        }
        // CreatureTokenWithAbilities: [pt, types, colors, subtypes, rules]
        "CreatureTokenWithAbilities" => {
            let (p, t) = extract_pt(&args[0])?;
            e.power = p;
            e.toughness = t;
            e.core_types = extract_core_types(args[1].as_list().ok_or("types not list")?)?;
            e.colors = extract_colors(&args[2])?;
            e.subtypes = extract_subtypes(&args[3])?;
            e.rules = args[4].as_list().ok_or("rules not list")?.to_vec();
        }
        // NamedCreatureToken: [name, pt, supertypes, types, colors, subtypes]
        // NamedCreatureTokenWithAbilities: [..., rules]
        "NamedCreatureToken" | "NamedCreatureTokenWithAbilities" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            let (p, t) = extract_pt(&args[1])?;
            e.power = p;
            e.toughness = t;
            e.supertypes = parse_supertypes_arg(&args[2])?;
            e.core_types = extract_core_types(args[3].as_list().ok_or("types not list")?)?;
            e.colors = extract_colors(&args[4])?;
            e.subtypes = extract_subtypes(&args[5])?;
            if kind == "NamedCreatureTokenWithAbilities" {
                e.rules = args[6].as_list().ok_or("rules not list")?.to_vec();
            }
        }
        // LegendaryNamedCreatureToken[WithAbilities]: identical to Named*
        // but the Legendary supertype is implicit. Engine handles that via
        // the supertype list — caller adds Legendary.
        "LegendaryNamedCreatureToken" | "LegendaryNamedCreatureTokenWithAbilities" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            let (p, t) = extract_pt(&args[1])?;
            e.power = p;
            e.toughness = t;
            e.supertypes = parse_supertypes_arg(&args[2])?;
            if !e.supertypes.contains(&Supertype::Legendary) {
                e.supertypes.insert(0, Supertype::Legendary);
            }
            e.core_types = extract_core_types(args[3].as_list().ok_or("types not list")?)?;
            e.colors = extract_colors(&args[4])?;
            e.subtypes = extract_subtypes(&args[5])?;
            if kind == "LegendaryNamedCreatureTokenWithAbilities" {
                e.rules = args[6].as_list().ok_or("rules not list")?.to_vec();
            }
        }
        // LegendaryNamedCreatureTokenWithCopyEffects: [name, pt, supertypes,
        // types, colors, subtypes, copy_effects]. Treat copy_effects as
        // complex rules.
        "LegendaryNamedCreatureTokenWithCopyEffects" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            let (p, t) = extract_pt(&args[1])?;
            e.power = p;
            e.toughness = t;
            e.supertypes = parse_supertypes_arg(&args[2])?;
            if !e.supertypes.contains(&Supertype::Legendary) {
                e.supertypes.insert(0, Supertype::Legendary);
            }
            e.core_types = extract_core_types(args[3].as_list().ok_or("types not list")?)?;
            e.colors = extract_colors(&args[4])?;
            e.subtypes = extract_subtypes(&args[5])?;
            // Mark complex via a placeholder rule
            e.rules.push(Value::Dict(vec![(
                "_Rule".to_string(),
                Value::Str("CopyEffects".to_string()),
            )]));
            e.rules.push(Value::Dict(vec![]));
        }
        // ArtifactToken: [name, supertypes, subtypes, colors, rules]
        "ArtifactToken" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            e.supertypes = parse_supertypes_arg(&args[1])?;
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = extract_subtypes(&args[2])?;
            e.colors = extract_colors(&args[3])?;
            e.rules = args[4].as_list().ok_or("rules not list")?.to_vec();
        }
        // ArtifactTokenWithNoRules: [name, supertypes, subtypes, colors]
        "ArtifactTokenWithNoRules" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            e.supertypes = parse_supertypes_arg(&args[1])?;
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = extract_subtypes(&args[2])?;
            e.colors = extract_colors(&args[3])?;
        }
        // ArtifactVehicleToken: [supertypes, subtypes, colors, rules, pt]
        "ArtifactVehicleToken" => {
            e.supertypes = parse_supertypes_arg(&args[0])?;
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = extract_subtypes(&args[1])?;
            if !e.subtypes.iter().any(|s| s == "Vehicle") {
                e.subtypes.push("Vehicle".to_string());
            }
            e.colors = extract_colors(&args[2])?;
            e.rules = args[3].as_list().ok_or("rules not list")?.to_vec();
            let (p, t) = extract_pt(&args[4])?;
            e.power = p;
            e.toughness = t;
        }
        // NamedArtifactVehicleToken: [name, supertypes, subtypes, colors, rules, pt]
        "NamedArtifactVehicleToken" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            e.supertypes = parse_supertypes_arg(&args[1])?;
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = extract_subtypes(&args[2])?;
            if !e.subtypes.iter().any(|s| s == "Vehicle") {
                e.subtypes.push("Vehicle".to_string());
            }
            e.colors = extract_colors(&args[3])?;
            e.rules = args[4].as_list().ok_or("rules not list")?.to_vec();
            let (p, t) = extract_pt(&args[5])?;
            e.power = p;
            e.toughness = t;
        }
        // EnchantmentToken: [name, supertypes, subtypes, colors, rules]
        "EnchantmentToken" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            e.supertypes = parse_supertypes_arg(&args[1])?;
            e.core_types = vec![CoreType::Enchantment];
            e.subtypes = extract_subtypes(&args[2])?;
            e.colors = extract_colors(&args[3])?;
            e.rules = args[4].as_list().ok_or("rules not list")?.to_vec();
        }
        // NamedLandTokenWithNoAbilities: [name, supertypes, subtypes, colors]
        "NamedLandTokenWithNoAbilities" => {
            e.name = Some(args[0].as_str().ok_or("name not str")?.to_string());
            e.supertypes = parse_supertypes_arg(&args[1])?;
            e.core_types = vec![CoreType::Land];
            e.subtypes = extract_subtypes(&args[2])?;
            e.colors = extract_colors(&args[3])?;
        }
        // TreasureToken / FoodToken / GoldToken: shorthand for a single
        // predefined-subtype artifact. mtgish's args here are typically just
        // counts; we materialize a canonical body and let the engine attach
        // abilities by subtype.
        "TreasureToken" => {
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = vec!["Treasure".to_string()];
        }
        "FoodToken" => {
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = vec!["Food".to_string()];
        }
        "GoldToken" => {
            e.core_types = vec![CoreType::Artifact];
            e.subtypes = vec!["Gold".to_string()];
        }
        // OracleToken / NumberTokens: not viable as static catalog entries.
        "OracleToken" | "NumberTokens" => {
            return Err(format!("{} not catalogable", kind));
        }
        other => return Err(format!("unsupported _CreatableToken kind: {}", other)),
    }
    Ok(e)
}

fn display_name_for(e: &Entry) -> String {
    if let Some(n) = &e.name {
        return n.clone();
    }
    // Default token display name: subtype list (CR 111.4). Fall back to
    // a comma-joined subtype list when multiple are present.
    if e.subtypes.is_empty() {
        "Token".to_string()
    } else {
        e.subtypes.join(" ")
    }
}

fn id_for(name: &str, e: &Entry, taken: &mut BTreeMap<String, u32>) -> String {
    let pt = match (e.power, e.toughness) {
        (Some(p), Some(t)) => format!("-{}-{}", p, t),
        _ => String::new(),
    };
    let colors_chord: String = e
        .colors
        .iter()
        .map(|c| match c {
            ManaColor::White => 'w',
            ManaColor::Blue => 'u',
            ManaColor::Black => 'b',
            ManaColor::Red => 'r',
            ManaColor::Green => 'g',
        })
        .collect();
    let colors_seg = if colors_chord.is_empty() {
        "c".to_string()
    } else {
        colors_chord
    };
    let slug = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = format!("{}{}-{}", slug, pt, colors_seg);
    let counter = taken.entry(base.clone()).or_insert(0);
    *counter += 1;
    if *counter == 1 {
        base
    } else {
        format!("{}-{}", base, counter)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// TOML serialization
// ──────────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct CatalogFile {
    token: Vec<TokenPreset>,
}

// ──────────────────────────────────────────────────────────────────────────
// Driver
// ──────────────────────────────────────────────────────────────────────────

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let args: Vec<String> = std::env::args().collect();
    let mut input = None;
    let mut output = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                input = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--output" => {
                output = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            other => return Err(format!("unknown arg: {}", other)),
        }
    }
    Ok((
        input.ok_or("--input required")?,
        output.ok_or("--output required")?,
    ))
}

fn main() -> ExitCode {
    let (input, output) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("usage: tokens-gen --input <mtgish.txt> --output <known-tokens.toml>");
            return ExitCode::FAILURE;
        }
    };
    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to read {}: {}", input.display(), e);
            return ExitCode::FAILURE;
        }
    };

    let mut presets: Vec<TokenPreset> = Vec::new();
    let mut id_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut skipped: Vec<(usize, String)> = Vec::new();

    for (lineno, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        if !trimmed.contains("_CreatableToken") {
            continue;
        }
        let mut p = Parser::new(trimmed);
        let v = match p.parse_value() {
            Ok(v) => v,
            Err(e) => {
                skipped.push((lineno + 1, format!("parse error: {}", e)));
                continue;
            }
        };
        let entry = match parse_entry(&v) {
            Ok(e) => e,
            Err(e) => {
                skipped.push((lineno + 1, e));
                continue;
            }
        };
        let (keywords, complex) = extract_keywords_and_complexity(&entry.rules);
        let category = match classify_category(&entry.core_types, &entry.subtypes) {
            Ok(c) => c,
            Err(e) => {
                skipped.push((lineno + 1, e));
                continue;
            }
        };
        let fidelity = if complex {
            PresetFidelity::PartialMissingAbilities
        } else {
            PresetFidelity::Full
        };
        let display_name = display_name_for(&entry);
        let id = id_for(&display_name, &entry, &mut id_counts);
        let body = TokenCharacteristics {
            display_name,
            power: entry.power,
            toughness: entry.toughness,
            core_types: entry.core_types,
            subtypes: entry.subtypes,
            supertypes: entry.supertypes,
            colors: entry.colors,
            keywords,
        };
        presets.push(TokenPreset {
            id,
            category,
            fidelity,
            body,
        });
    }

    // Deterministic emit order: by category, then id.
    presets.sort_by(|a, b| {
        category_sort_key(&a.category)
            .cmp(&category_sort_key(&b.category))
            .then_with(|| a.id.cmp(&b.id))
    });

    let file = CatalogFile { token: presets };
    let serialized = match toml::to_string_pretty(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("toml serialize failed: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if let Some(parent) = output.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::write(&output, serialized) {
        eprintln!("write {} failed: {}", output.display(), e);
        return ExitCode::FAILURE;
    }
    eprintln!(
        "tokens-gen: wrote {} presets to {} ({} entries skipped)",
        file.token.len(),
        output.display(),
        skipped.len()
    );
    if !skipped.is_empty() {
        eprintln!("skipped entries:");
        for (lineno, msg) in skipped.iter().take(30) {
            eprintln!("  line {}: {}", lineno, msg);
        }
        if skipped.len() > 30 {
            eprintln!("  ... and {} more", skipped.len() - 30);
        }
    }
    ExitCode::SUCCESS
}

fn category_sort_key(cat: &TokenCategory) -> u8 {
    match cat {
        TokenCategory::PredefinedArtifact { .. } => 0,
        TokenCategory::Creature => 1,
        TokenCategory::Aura => 2,
        TokenCategory::Equipment => 3,
        TokenCategory::Vehicle => 4,
        TokenCategory::Enchantment => 5,
        TokenCategory::Land => 6,
        TokenCategory::Artifact => 7,
    }
}
